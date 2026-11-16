//! Statements: sequencing, branches, named blocks and `disable`.

use super::*;

/// The constructs the statement being lowered is nested in, innermost
/// last. The branches of a fork are lowered outside those of the parent:
/// the `Fork` arm of [`Ff::exec`] and [`Ff::fork_in_unit`] each set aside
/// the ones they do and put them back after the branches.
#[derive(Default)]
pub(super) struct Scope {
    pub(super) loops: Vec<LoopBlocks>,
    /// Named blocks a `disable` can leave: their ids and exit blocks.
    pub(super) blocks: Vec<BlockEntry>,
    pub(super) functions: Vec<FunctionBlocks>,
    /// Loop variables of unrolled loops and their literal values.
    pub(super) overlay: HashMap<String, (String, i128)>,
}

impl Ff<'_, '_> {
    /// Leave every named block an earlier `disable` ended while this
    /// activation was suspended: at a statement boundary, an activation
    /// whose snapshot no longer matches the block's generation jumps to
    /// the block's exit instead of executing the statement.
    pub(super) fn check_disabled_blocks(&mut self) -> Result<(), sv::AnalyzerError> {
        let epochs: Vec<(BlockId, String, String)> = self
            .scope
            .blocks
            .iter()
            .filter_map(|entry| {
                entry
                    .epoch
                    .as_ref()
                    .map(|(epoch, snapshot)| (entry.exit, epoch.clone(), snapshot.clone()))
            })
            .collect();
        for (exit, epoch, snapshot) in epochs {
            let changed = sv::ir::Expr::Binary {
                left: Box::new(sv::ir::Expr::Ident(snapshot)),
                op: sv::ir::BinaryOp::Ne,
                right: Box::new(sv::ir::Expr::Ident(epoch)),
            };
            let (truth, _) = self.eval_truth(&changed)?;
            let next = self.b.new_block();
            self.b.seal_block(SIRTerminator::Branch {
                cond: truth,
                true_block: (exit, Vec::new()),
                false_block: (next, Vec::new()),
            });
            self.b.switch_to_block(next);
        }
        Ok(())
    }

    /// Lower a statement list; `false` when control cannot fall off its end.
    pub(super) fn exec_block(&mut self, stmts: &[sv::ir::Stmt]) -> Result<bool, sv::AnalyzerError> {
        for stmt in stmts {
            if self.scope.blocks.iter().any(|entry| entry.epoch.is_some()) {
                self.check_disabled_blocks()?;
            }
            if !self.exec(stmt)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub(super) fn jump(&mut self, target: BlockId) {
        self.b.seal_block(SIRTerminator::Jump(target, Vec::new()));
    }

    pub(super) fn exec(&mut self, stmt: &sv::ir::Stmt) -> Result<bool, sv::AnalyzerError> {
        if matches!(
            stmt,
            sv::ir::Stmt::Delay { .. }
                | sv::ir::Stmt::WaitEvent(_)
                | sv::ir::Stmt::WaitImplicit { .. }
                | sv::ir::Stmt::Wait(_)
        ) && (self.kernel.is_none()
            && !(matches!(
                stmt,
                sv::ir::Stmt::Delay { .. } | sv::ir::Stmt::WaitEvent(_) | sv::ir::Stmt::Wait(_)
            ) && self.can_request())
            || self
                .call_stack
                .active_calls
                .iter()
                .skip(self.call_stack.timing_call_boundary)
                .any(|name| {
                    self.m
                        .subroutine(name)
                        .is_some_and(|subroutine| !subroutine.is_task)
                }))
        {
            return Err(unsupported("timed task call outside a suspendable process"));
        }
        match stmt {
            sv::ir::Stmt::ProceduralAssign { .. }
            | sv::ir::Stmt::Deassign { .. }
            | sv::ir::Stmt::Force { .. }
            | sv::ir::Stmt::Release { .. } => {
                unreachable!("the analyzer lowers procedural continuous assignments and forces")
            }
            // The store follows the force controls, which the force set
            // before it, and the release cleared (see
            // `procedural_forces`).
            sv::ir::Stmt::ForceStore { lhs, rhs } => {
                self.assign(lhs, rhs)?;
                self.observe_assignment(std::slice::from_ref(lhs))?;
                Ok(true)
            }
            sv::ir::Stmt::DeferActive => {
                self.with_kernel(ProcessKernelBuilder::defer_active)
                    .map_err(kernel_error)?;
                Ok(true)
            }
            sv::ir::Stmt::DelayedNonblocking {
                control,
                assignment,
            } => {
                let sv::ir::Stmt::Delay {
                    amount,
                    ticks_per_unit,
                    ..
                } = control.as_ref()
                else {
                    return Err(unsupported("nonblocking intra-assignment event control"));
                };
                let delay = self
                    .eval_const_with_width(amount, Some(PROCESS_DELAY_WIDTH))?
                    .and_then(|(value, _)| u64::try_from(value).ok())
                    .and_then(|value| value.checked_mul(*ticks_per_unit))
                    .ok_or_else(|| {
                        unsupported("nonconstant or overflowing nonblocking assignment delay")
                    })?;
                match assignment.as_ref() {
                    sv::ir::Stmt::Assign {
                        lhs,
                        rhs,
                        nonblocking: true,
                    } => self.nonblocking_delayed(std::slice::from_ref(lhs), rhs, delay)?,
                    sv::ir::Stmt::AssignConcat {
                        parts,
                        rhs,
                        nonblocking: true,
                    } => self.nonblocking_delayed(parts, rhs, delay)?,
                    _ => return Err(unsupported("nonblocking delayed assignment target")),
                }
                Ok(true)
            }
            // A unit stages the update at a function NBA site.
            sv::ir::Stmt::Assign {
                lhs,
                rhs,
                nonblocking: true,
            } if self.kernel.is_some() || self.can_suspend_unit() => {
                self.nonblocking(std::slice::from_ref(lhs), rhs)?;
                Ok(true)
            }
            sv::ir::Stmt::AssignConcat {
                parts,
                rhs,
                nonblocking: true,
            } if self.kernel.is_some() || self.can_suspend_unit() => {
                self.nonblocking(parts, rhs)?;
                Ok(true)
            }
            sv::ir::Stmt::Assign { lhs, rhs, .. } => {
                self.assign(lhs, rhs)?;
                self.observe_assignment(std::slice::from_ref(lhs))?;
                Ok(true)
            }
            sv::ir::Stmt::AssignConcat { parts, rhs, .. } => {
                self.assign_concat(parts, rhs)?;
                self.observe_assignment(parts)?;
                Ok(true)
            }
            sv::ir::Stmt::Delay {
                amount,
                ticks_per_unit,
                ticks_per_precision,
                precision_steps_per_tick,
            } => {
                self.delay(
                    amount,
                    *ticks_per_unit,
                    *ticks_per_precision,
                    *precision_steps_per_tick,
                )?;
                Ok(true)
            }
            sv::ir::Stmt::WaitEvent(items) => {
                self.wait_event(items)?;
                Ok(true)
            }
            sv::ir::Stmt::WaitImplicit { reads, always_comb } => {
                self.wait_implicit(reads, *always_comb)?;
                Ok(true)
            }
            sv::ir::Stmt::Wait(condition) => {
                self.wait_level(condition)?;
                Ok(true)
            }
            sv::ir::Stmt::AssertionRegion { reactive } => {
                self.with_kernel(|kernel| kernel.assertion_region(*reactive))
                    .map_err(kernel_error)?;
                Ok(true)
            }
            sv::ir::Stmt::WaitFork | sv::ir::Stmt::DisableFork
                if self.kernel.is_none() && self.can_suspend_unit() =>
            {
                // A unit acts on the children of the process it runs in.
                let status = if matches!(stmt, sv::ir::Stmt::WaitFork) {
                    celox_design::ProcessStatus::WaitFork
                } else {
                    celox_design::ProcessStatus::DisableFork
                };
                let request = self.b.alloc_bit(PROCESS_DELAY_WIDTH, false);
                self.b
                    .emit(SIRInstruction::Imm(request, SIRValue::new(0u64)));
                self.suspend_unit_with(status, request)?;
                Ok(true)
            }
            sv::ir::Stmt::WaitFork | sv::ir::Stmt::DisableFork => {
                if self.kernel.is_none() {
                    return if matches!(stmt, sv::ir::Stmt::DisableFork) {
                        Ok(true)
                    } else {
                        Err(unsupported("fork control outside a timed process"))
                    };
                }
                if matches!(stmt, sv::ir::Stmt::WaitFork) {
                    self.with_kernel(ProcessKernelBuilder::wait_fork)
                        .map_err(kernel_error)?;
                } else {
                    self.with_kernel(ProcessKernelBuilder::disable_fork)
                        .map_err(kernel_error)?;
                }
                Ok(true)
            }
            sv::ir::Stmt::Fork { join, branches }
                if self.kernel.is_none() && self.can_suspend_unit() =>
            {
                self.fork_in_unit(*join, branches)?;
                Ok(true)
            }
            sv::ir::Stmt::Fork { join, branches } => {
                if self.kernel.is_none() {
                    return Err(unsupported("fork outside a timed process"));
                }
                self.activation.forked = true;
                let (entries, continuation) = self
                    .with_kernel(|kernel| kernel.fork(branches.len(), *join))
                    .map_err(kernel_error)?;
                self.b.seal_block(SIRTerminator::Return);
                let loops = std::mem::take(&mut self.scope.loops);
                let blocks = std::mem::take(&mut self.scope.blocks);
                let functions = std::mem::take(&mut self.scope.functions);
                let overlay = std::mem::take(&mut self.scope.overlay);
                let timing_boundary = self.call_stack.timing_call_boundary;
                self.call_stack.timing_call_boundary = self.call_stack.active_calls.len();
                for (entry, branch) in entries.into_iter().zip(branches) {
                    self.b.switch_to_block(entry);
                    if self.exec_block(branch)? {
                        self.with_kernel(ProcessKernelBuilder::complete);
                    }
                }
                self.call_stack.timing_call_boundary = timing_boundary;
                self.scope.loops = loops;
                self.scope.blocks = blocks;
                self.scope.functions = functions;
                self.scope.overlay = overlay;
                self.b.switch_to_block(continuation);
                Ok(true)
            }
            sv::ir::Stmt::Local { name, init } => {
                let id = self
                    .m
                    .id(name)
                    .ok_or_else(|| unsupported(format!("local `{name}`")))?;
                self.postponed_locals.insert(id);
                if self.activation.forked && self.kernel.is_some() {
                    let slot = match self
                        .activation
                        .frame_locals
                        .iter()
                        .position(|&local| local == id)
                    {
                        Some(slot) => slot,
                        None => {
                            self.activation.frame_locals.push(id);
                            self.activation.frame_locals.len() - 1
                        }
                    };
                    self.with_kernel(|kernel| kernel.local(slot))
                        .map_err(kernel_error)?;
                }
                match init {
                    Some(init) => self.assign(&sv::ir::LValue::Ident(name.clone()), init)?,
                    None => self.init_default(id),
                }
                // Each execution of the declaration makes a new variable,
                // in a new box when a fork shares it.
                if let Some(shared) = self
                    .call_stack
                    .shared_boxes
                    .iter()
                    .rev()
                    .find(|shared| shared.var == *name)
                    .cloned()
                {
                    self.open_box(&shared)?;
                }
                Ok(true)
            }
            sv::ir::Stmt::If {
                condition,
                then_body,
                else_body,
            } => {
                let (truth, constant) = self.eval_truth(condition)?;
                match constant {
                    Some(true) => return self.exec_block(then_body),
                    Some(false) => return self.exec_block(else_body),
                    None => {}
                }
                let then_block = self.b.new_block();
                let else_block = self.b.new_block();
                let join = self.b.new_block();
                self.b.seal_block(SIRTerminator::Branch {
                    cond: truth,
                    true_block: (then_block, Vec::new()),
                    false_block: (else_block, Vec::new()),
                });
                self.b.switch_to_block(then_block);
                let then_falls = self.exec_block(then_body)?;
                if then_falls {
                    self.jump(join);
                }
                self.b.switch_to_block(else_block);
                let else_falls = self.exec_block(else_body)?;
                if else_falls {
                    self.jump(join);
                }
                self.b.switch_to_block(join);
                if !then_falls && !else_falls {
                    // The join block is unreachable; give it a terminator.
                    self.b.seal_block(SIRTerminator::Return);
                    return Ok(false);
                }
                Ok(true)
            }
            sv::ir::Stmt::Case {
                kind,
                selector,
                items,
                default,
            } => self.exec_case(*kind, selector, items, default.as_deref()),
            sv::ir::Stmt::Loop {
                kind,
                init,
                condition,
                step,
                body,
            } => self.exec_loop(kind, init, condition.as_ref(), step, body),
            sv::ir::Stmt::Break | sv::ir::Stmt::Continue => {
                let blocks = self
                    .scope
                    .loops
                    .last()
                    .ok_or_else(|| unsupported("break or continue outside a loop"))?;
                if self
                    .scope
                    .functions
                    .last()
                    .is_some_and(|function| function.loop_depth == self.scope.loops.len())
                {
                    return Err(unsupported("break or continue outside a loop"));
                }
                let target = if matches!(stmt, sv::ir::Stmt::Break) {
                    blocks.break_block
                } else {
                    blocks.continue_block
                };
                self.jump(target);
                Ok(false)
            }
            sv::ir::Stmt::Block { id, body } => {
                let exit = self.b.new_block();
                // A timed activation can be suspended inside the block
                // while another activation of it runs, so a block its own
                // execution disables needs the generation check. Every
                // `disable` of the block is a statement of its body: a
                // recursive activation runs the same statements.
                let mut disabled = false;
                for stmt in body {
                    stmt.walk(&mut |stmt| {
                        disabled |=
                            matches!(stmt, sv::ir::Stmt::DisableBlock(block) if block == id);
                    });
                }
                let epoch = if self.kernel.is_some() && disabled {
                    let epoch = self.m.disable_epoch(*id);
                    let (_, snapshot) = self.m.temp("disable_snapshot", 64, false, false);
                    self.assign(
                        &sv::ir::LValue::Ident(snapshot.clone()),
                        &sv::ir::Expr::Ident(epoch.clone()),
                    )?;
                    Some((epoch, snapshot))
                } else {
                    None
                };
                self.scope.blocks.push(BlockEntry {
                    id: *id,
                    exit,
                    epoch,
                });
                let falls_through = self.exec_block(body);
                self.scope.blocks.pop();
                if falls_through? {
                    self.jump(exit);
                }
                self.b.switch_to_block(exit);
                Ok(true)
            }
            sv::ir::Stmt::DisableBlock(id) => {
                // The block must be executing in this process, in this
                // subroutine inlining: a target outside them (an outer
                // block of a fork parent, say) is not lowered.
                let depth = self
                    .scope
                    .functions
                    .last()
                    .map_or(0, |function| function.block_depth);
                let entry = self.scope.blocks[depth..]
                    .iter()
                    .rev()
                    .find(|entry| entry.id == *id)
                    .ok_or_else(|| {
                        unsupported("disable of a block that is not executing in this process")
                    })?;
                let (exit, epoch) = (entry.exit, entry.epoch.clone());
                // End every other active activation too: they find the
                // generation advanced at their next statement.
                if let Some((epoch, _)) = epoch {
                    let advanced = sv::ir::Expr::Binary {
                        left: Box::new(sv::ir::Expr::Ident(epoch.clone())),
                        op: sv::ir::BinaryOp::Add,
                        right: Box::new(sv::ir::Expr::Literal("1".into())),
                    };
                    self.assign(&sv::ir::LValue::Ident(epoch), &advanced)?;
                }
                self.jump(exit);
                Ok(false)
            }
            sv::ir::Stmt::Return(value) => {
                let function = self
                    .scope
                    .functions
                    .last()
                    .ok_or_else(|| unsupported("return outside a subroutine"))?;
                let (return_block, return_var) =
                    (function.return_block, function.return_var.clone());
                if let (Some(value), Some(return_var)) = (value, return_var) {
                    self.assign(&sv::ir::LValue::Ident(return_var), value)?;
                }
                self.jump(return_block);
                Ok(false)
            }
            sv::ir::Stmt::Call {
                name,
                args,
                arg_order,
            } => {
                if let Some(import) = self.m.dpi_imports.get(name).cloned() {
                    self.dpi_call(&import, args, arg_order)?;
                    return Ok(true);
                }
                if !self.m.subroutines.contains_key(name) {
                    return Err(unsupported(format!("call of `{name}`")));
                }
                self.call(name, args, arg_order)?;
                Ok(true)
            }
            sv::ir::Stmt::Eval(expr) => {
                if place_store_target(expr).is_some() {
                    self.store_place(expr)?;
                } else {
                    self.eval(expr, None)?;
                }
                Ok(true)
            }
            sv::ir::Stmt::SystemTask {
                task,
                args,
                site,
                assertion,
            } => {
                // System-task output writes (including readmem) have their own
                // lowering. Until they expose safe per-update boundaries, a
                // watched automatic destination is rejected like expression
                // copy-out rather than losing intermediate changes.
                self.observation.implicit_write_depth += 1;
                let result = self.system_task(*task, args, site.as_ref(), assertion.as_ref());
                self.observation.implicit_write_depth -= 1;
                result
            }
        }
    }

    pub(super) fn exec_case(
        &mut self,
        kind: sv::ir::CaseKind,
        selector: &sv::ir::Expr,
        items: &[sv::ir::CaseItem],
        default: Option<&[sv::ir::Stmt]>,
    ) -> Result<bool, sv::AnalyzerError> {
        // Evaluate the selector once.
        let selector = if self.m.calls(selector) {
            self.hoist(selector)?
        } else {
            selector.clone()
        };
        let join = self.b.new_block();
        let mut any_falls = false;
        for item in items {
            let mut terms = Vec::new();
            for label in &item.labels {
                terms.push(match (kind, label) {
                    (sv::ir::CaseKind::Inside, label) => sv::ir::Expr::Inside {
                        expr: Box::new(selector.clone()),
                        items: vec![match label {
                            sv::ir::CaseLabel::Value(value) => {
                                sv::ir::InsideItem::Value(value.clone())
                            }
                            sv::ir::CaseLabel::Range { low, high } => sv::ir::InsideItem::Range {
                                low: low.clone(),
                                high: high.clone(),
                            },
                        }],
                    },
                    (sv::ir::CaseKind::X, sv::ir::CaseLabel::Value(value)) => {
                        sv::ir::Expr::Binary {
                            left: Box::new(sv::ir::Expr::Binary {
                                left: Box::new(selector.clone()),
                                op: sv::ir::BinaryOp::EqWildcard,
                                right: Box::new(value.clone()),
                            }),
                            op: sv::ir::BinaryOp::NeCase,
                            right: Box::new(sv::ir::Expr::Literal("1'b0".to_string())),
                        }
                    }
                    (_, sv::ir::CaseLabel::Value(value)) => sv::ir::Expr::Binary {
                        left: Box::new(selector.clone()),
                        op: if kind == sv::ir::CaseKind::Exact {
                            sv::ir::BinaryOp::EqCase
                        } else {
                            sv::ir::BinaryOp::EqWildcard
                        },
                        right: Box::new(value.clone()),
                    },
                    (_, sv::ir::CaseLabel::Range { .. }) => {
                        return Err(unsupported("range label outside case inside"));
                    }
                });
            }
            let condition = terms
                .into_iter()
                .reduce(|left, right| sv::ir::Expr::Binary {
                    left: Box::new(left),
                    op: sv::ir::BinaryOp::LogicOr,
                    right: Box::new(right),
                })
                .unwrap_or_else(|| sv::ir::Expr::Literal("1'b0".to_string()));
            let (truth, constant) = self.eval_truth(&condition)?;
            match constant {
                Some(false) => continue,
                Some(true) => {
                    let falls = self.exec_block(&item.body)?;
                    if falls {
                        self.jump(join);
                    } else {
                        // Control left through a jump; continue in a dead block.
                    }
                    self.b.switch_to_block(join);
                    if !falls && !any_falls {
                        self.b.seal_block(SIRTerminator::Return);
                        return Ok(false);
                    }
                    return Ok(true);
                }
                None => {}
            }
            let item_block = self.b.new_block();
            let next = self.b.new_block();
            self.b.seal_block(SIRTerminator::Branch {
                cond: truth,
                true_block: (item_block, Vec::new()),
                false_block: (next, Vec::new()),
            });
            self.b.switch_to_block(item_block);
            if self.exec_block(&item.body)? {
                any_falls = true;
                self.jump(join);
            }
            self.b.switch_to_block(next);
        }
        let default_falls = match default {
            Some(default) => self.exec_block(default)?,
            None => true,
        };
        if default_falls {
            any_falls = true;
            self.jump(join);
        }
        self.b.switch_to_block(join);
        if !any_falls {
            self.b.seal_block(SIRTerminator::Return);
            return Ok(false);
        }
        Ok(true)
    }
}
