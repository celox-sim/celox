//! Loops: unrolled when the trip count is a small constant, SIR loops otherwise.

use super::*;

impl Ff<'_, '_> {
    pub(super) fn exec_loop(
        &mut self,
        kind: &sv::ir::LoopKind<sv::ir::Expr>,
        init: &[sv::ir::Stmt],
        condition: Option<&sv::ir::Expr>,
        step: &[sv::ir::Stmt],
        body: &[sv::ir::Stmt],
    ) -> Result<bool, sv::AnalyzerError> {
        if !(self.observation.observe_blocking_assignments && self.kernel.is_some())
            && let sv::ir::LoopKind::For = kind
            && let Some(canonical) = canonical_for_loop(init, condition, step)
            // A variable that other processes can read or write has its
            // value at each iteration only while none of them can run.
            && self
                .m
                .id(&canonical.var)
                .is_some_and(|id| self.m.is_hidden(id) || runs_without_others(body))
            && let Some((values, last)) = self.unrolled_values(&canonical, condition, step, body)?
        {
            return self.unroll(&canonical, &values, last, body);
        }
        self.runtime_loop(kind, init, condition, step, body)
    }

    /// The loop-variable values of a counted loop with constant bounds, when
    /// it is small enough to unroll, and the value that ends it.
    pub(super) fn unrolled_values(
        &mut self,
        canonical: &CanonicalLoop,
        condition: Option<&sv::ir::Expr>,
        step: &[sv::ir::Stmt],
        body: &[sv::ir::Stmt],
    ) -> Result<Option<(Vec<(String, i128)>, i128)>, sv::AnalyzerError> {
        let mut written = HashSet::default();
        written_names(body, &mut written);
        if written.contains(&canonical.var) {
            return Ok(None);
        }
        let id = self.m.id(&canonical.var).expect("loop variable");
        let (width, signed) = (self.m.var(id).width, self.m.var(id).signed);
        let start_signed = self.expr_signed(&canonical.start);
        let Some((mut start, start_width)) = self.eval_const(&canonical.start)? else {
            return Ok(None);
        };
        // The initializer widens to the loop variable by its own signedness.
        if start_signed
            && start_width > 0
            && start_width < width
            && start.bit(start_width as u64 - 1)
        {
            start |= ((BigUint::from(1u8) << width) - BigUint::from(1u8))
                ^ ((BigUint::from(1u8) << start_width) - BigUint::from(1u8));
        }
        let Some(condition) = condition else {
            return Ok(None);
        };
        let [sv::ir::Stmt::Assign { rhs: step_rhs, .. }] = step else {
            return Ok(None);
        };
        let mask = (BigUint::from(1u8) << width) - BigUint::from(1u8);
        let mut value = start & &mask;
        let mut values = Vec::new();
        let saved = self.scope.overlay.remove(&canonical.var);
        let result = loop {
            let numeric = if signed && value.bit(width as u64 - 1) {
                (num_bigint::BigInt::from(value.clone()) - (num_bigint::BigInt::from(1u8) << width))
                    .to_i128()
            } else {
                value.to_i128()
            };
            let Some(numeric) = numeric else { break None };
            let literal = (typed_literal(&value, width, signed), numeric);
            self.scope
                .overlay
                .insert(canonical.var.clone(), literal.clone());
            let Some((proceed, _)) = self.eval_const(condition)? else {
                break None;
            };
            if proceed.is_zero() {
                break Some((values, numeric));
            }
            values.push(literal);
            if values.len() + self.unrolled > MAX_UNROLLED_ITERATIONS {
                break None;
            }
            let Some((next, _)) = self.eval_const(step_rhs)? else {
                break None;
            };
            value = next & &mask;
        };
        self.scope.overlay.remove(&canonical.var);
        if let Some(saved) = saved {
            self.scope.overlay.insert(canonical.var.clone(), saved);
        }
        Ok(result)
    }

    pub(super) fn unroll(
        &mut self,
        canonical: &CanonicalLoop,
        values: &[(String, i128)],
        last: i128,
        body: &[sv::ir::Stmt],
    ) -> Result<bool, sv::AnalyzerError> {
        self.unrolled += values.len();
        // The loop variable may be declared outside the loop, which then reads
        // its value: the one `break` left, or the one that ended the loop.
        let id = self.m.id(&canonical.var).expect("loop variable");
        let exit = self.b.new_block();
        let saved = self.scope.overlay.remove(&canonical.var);
        let mut reachable = true;
        for literal in values {
            let next = self.b.new_block();
            self.store_constant(id, literal.1);
            self.scope
                .overlay
                .insert(canonical.var.clone(), literal.clone());
            self.scope.loops.push(LoopBlocks {
                break_block: exit,
                continue_block: next,
            });
            let falls = self.exec_block(body);
            self.scope.loops.pop();
            if falls? {
                self.jump(next);
            }
            self.b.switch_to_block(next);
            reachable = true;
        }
        self.scope.overlay.remove(&canonical.var);
        if let Some(saved) = saved {
            self.scope.overlay.insert(canonical.var.clone(), saved);
        }
        let _ = reachable;
        self.store_constant(id, last);
        self.jump(exit);
        self.b.switch_to_block(exit);
        Ok(true)
    }

    /// Store `value`, truncated to the variable's width, into `id`.
    pub(super) fn store_constant(&mut self, id: SourceVarId, value: i128) {
        let width = self.m.var(id).width;
        let modulus = num_bigint::BigInt::from(1u8) << width;
        let value = ((num_bigint::BigInt::from(value) % &modulus) + &modulus) % &modulus;
        let register = self.b.alloc_bit(width, false);
        self.b.emit(SIRInstruction::Imm(
            register,
            SIRValue::new(value.to_biguint().expect("a non-negative value")),
        ));
        let offset = sv_memory_offset(self.m.var(id), 0, width);
        self.store(id, offset, width, register);
    }

    pub(super) fn runtime_loop(
        &mut self,
        kind: &sv::ir::LoopKind<sv::ir::Expr>,
        init: &[sv::ir::Stmt],
        condition: Option<&sv::ir::Expr>,
        step: &[sv::ir::Stmt],
        body: &[sv::ir::Stmt],
    ) -> Result<bool, sv::AnalyzerError> {
        if !self.exec_block(init)? {
            return Ok(false);
        }
        // A `repeat` count is evaluated once, into a hidden counter.
        let counter = match kind {
            sv::ir::LoopKind::Repeat(count) => {
                let signed = self.expr_signed(count);
                let (id, name) = self.m.temp("repeat", 64, false, false);
                let value = if self.kernel.is_some() {
                    // A count with an unknown bit, or a negative one, runs
                    // the body zero times (IEEE 1800-2023 12.7.2).
                    let count = if signed {
                        sv::ir::Expr::Mux {
                            condition: Box::new(sv::ir::Expr::Binary {
                                left: Box::new(count.clone()),
                                op: sv::ir::BinaryOp::Lt,
                                right: Box::new(sv::ir::Expr::Literal("0".to_string())),
                            }),
                            then_expr: Box::new(sv::ir::Expr::Literal("0".to_string())),
                            else_expr: Box::new(count.clone()),
                        }
                    } else {
                        count.clone()
                    };
                    let mut arena = SLTNodeArena::new();
                    let node = self.expr_slt(&count, None, &mut arena)?;
                    let original = self.lower_slt(&arena, node)?;
                    let node =
                        coerce_node_width(&mut arena, node, Some(64), signed).map_err(slt_error)?;
                    let value = self.lower_slt(&arena, node)?;
                    self.zero_if_unknown(value, original)
                } else {
                    self.eval(count, Some((64, signed)))?
                };
                self.store(id, SIROffset::Static(0), 64, value);
                Some(name)
            }
            _ => None,
        };
        let header = self.b.new_block();
        let body_block = self.b.new_block();
        let latch = self.b.new_block();
        let exit = self.b.new_block();
        if matches!(kind, sv::ir::LoopKind::DoWhile) {
            self.jump(body_block);
        } else {
            self.jump(header);
        }
        // Header: the loop condition.
        self.b.switch_to_block(header);
        let test = match (kind, &counter) {
            (_, Some(counter)) => Some(sv::ir::Expr::Binary {
                left: Box::new(sv::ir::Expr::Ident(counter.clone())),
                op: sv::ir::BinaryOp::Ne,
                right: Box::new(sv::ir::Expr::Literal("64'd0".to_string())),
            }),
            (sv::ir::LoopKind::Forever | sv::ir::LoopKind::EventBlock, _) => None,
            _ => condition.cloned(),
        };
        match test {
            Some(test) => {
                let (truth, _) = self.eval_truth(&test)?;
                self.b.seal_block(SIRTerminator::Branch {
                    cond: truth,
                    true_block: (body_block, Vec::new()),
                    false_block: (exit, Vec::new()),
                });
            }
            None => self.jump(body_block),
        }
        // Body.
        self.b.switch_to_block(body_block);
        if let Some(counter) = &counter {
            let decrement = sv::ir::Expr::Binary {
                left: Box::new(sv::ir::Expr::Ident(counter.clone())),
                op: sv::ir::BinaryOp::Sub,
                right: Box::new(sv::ir::Expr::Literal("64'd1".to_string())),
            };
            self.assign(&sv::ir::LValue::Ident(counter.clone()), &decrement)?;
        }
        self.scope.loops.push(LoopBlocks {
            break_block: exit,
            continue_block: latch,
        });
        let falls = self.exec_block(body);
        self.scope.loops.pop();
        if falls? {
            self.jump(latch);
        }
        // A kernel loop that may not suspend yields now and then, so the
        // runtime charges it against the time slot's budget instead of
        // spinning in the kernel.
        let header = if self.kernel.is_some() && !always_suspends(body) {
            self.yielding_back_edge(header)?
        } else {
            header
        };
        // Latch: the step, then the condition again. A counted loop whose
        // step leaves its variable unchanged would never end; it stops the
        // simulation with an error instead.
        self.b.switch_to_block(latch);
        let progress = match kind {
            sv::ir::LoopKind::For => canonical_for_loop(init, condition, step)
                .and_then(|canonical| Some((canonical.var.clone(), self.m.id(&canonical.var)?))),
            _ => None,
        };
        let previous = match &progress {
            Some((var, id)) => {
                let (width, signed) = (self.m.var(*id).width, self.m.var(*id).signed);
                let (temp, temp_name) = self.m.temp("progress", width, signed, false);
                let value = self.eval(&sv::ir::Expr::Ident(var.clone()), Some((width, signed)))?;
                self.store(temp, SIROffset::Static(0), width, value);
                Some(temp_name)
            }
            None => None,
        };
        if self.exec_block(step)? {
            if let (Some((var, id)), Some(previous)) = (&progress, previous) {
                let stalled = sv::ir::Expr::Binary {
                    left: Box::new(sv::ir::Expr::Ident(var.clone())),
                    op: sv::ir::BinaryOp::EqCase,
                    right: Box::new(sv::ir::Expr::Ident(previous)),
                };
                let (stalled, _) = self.eval_truth(&stalled)?;
                let error_block = self.b.new_block();
                self.b.seal_block(SIRTerminator::Branch {
                    cond: stalled,
                    true_block: (error_block, Vec::new()),
                    false_block: (header, Vec::new()),
                });
                self.b.switch_to_block(error_block);
                let source = self.m.var(*id).path[0].clone();
                let code = self.m.runtime_error(format!(
                    "Non-progressing for loop in always_ff (loop variable `{source}`)"
                ));
                if let Some(info) = self.m.runtime.errors.get_mut(&code) {
                    info.signals.push(*id);
                }
                self.b.seal_block(SIRTerminator::Error(code));
            } else {
                self.jump(header);
            }
        }
        self.b.switch_to_block(exit);
        Ok(true)
    }

    /// The back edge of a kernel loop that may not suspend: count the
    /// iteration, yield every 2^[`LOOP_YIELD_BITS`] of them, and continue at
    /// `header`. Returns the block the loop's back edges jump to.
    pub(super) fn yielding_back_edge(
        &mut self,
        header: BlockId,
    ) -> Result<BlockId, sv::AnalyzerError> {
        let back = self.b.new_block();
        let (counter, _) = self.m.temp("loop", LOOP_YIELD_BITS, false, false);
        self.b.switch_to_block(back);
        let count = self.b.alloc_bit(LOOP_YIELD_BITS, false);
        self.b.emit(SIRInstruction::Load(
            count,
            stable(counter),
            SIROffset::Static(0),
            LOOP_YIELD_BITS,
        ));
        let one = self.b.alloc_bit(LOOP_YIELD_BITS, false);
        self.b.emit(SIRInstruction::Imm(one, SIRValue::new(1u8)));
        let next = self.b.alloc_bit(LOOP_YIELD_BITS, false);
        self.b
            .emit(SIRInstruction::Binary(next, count, BinaryOp::Add, one));
        self.store(counter, SIROffset::Static(0), LOOP_YIELD_BITS, next);
        let zero = self.b.alloc_bit(LOOP_YIELD_BITS, false);
        self.b.emit(SIRInstruction::Imm(zero, SIRValue::new(0u8)));
        let wrapped = self.b.alloc_bit(1, false);
        self.b
            .emit(SIRInstruction::Binary(wrapped, next, BinaryOp::Eq, zero));
        let yield_block = self.b.new_block();
        self.b.seal_block(SIRTerminator::Branch {
            cond: wrapped,
            true_block: (yield_block, Vec::new()),
            false_block: (header, Vec::new()),
        });
        self.b.switch_to_block(yield_block);
        self.with_kernel(ProcessKernelBuilder::yield_now)
            .map_err(kernel_error)?;
        self.jump(header);
        Ok(back)
    }
}

/// Iterations, as a power of two, between the yields of a kernel loop that
/// may not suspend.
pub(super) const LOOP_YIELD_BITS: usize = 16;

/// Whether `body` runs from its first statement to its last with no other
/// process, and nothing but its own assignments that could write a variable
/// it reads, in between: it has no timing control and calls nothing that
/// has an effect, which a subroutine, a method of a process and a system
/// function that writes an argument, as `$random(seed)`, have.
fn runs_without_others(body: &[sv::ir::Stmt]) -> bool {
    let mut alone = true;
    let mut calls = Vec::new();
    for stmt in body {
        stmt.walk(&mut |stmt| {
            alone &= !matches!(
                stmt,
                sv::ir::Stmt::Delay { .. }
                    | sv::ir::Stmt::DelayedNonblocking { .. }
                    | sv::ir::Stmt::WaitEvent(_)
                    | sv::ir::Stmt::WaitImplicit { .. }
                    | sv::ir::Stmt::Wait(_)
                    | sv::ir::Stmt::Fork { .. }
                    | sv::ir::Stmt::WaitFork
                    | sv::ir::Stmt::Call { .. }
                    | sv::ir::Stmt::SystemTask { .. }
            );
            stmt_calls(stmt, &mut calls);
        });
    }
    // A sampled value is that of the start of the time slot (16.9.3), not
    // the value an iteration gives the variable.
    alone
        && calls.iter().all(|(callee, _)| {
            !callee.is_effectful()
                && !matches!(callee, sv::ir::Callee::System(function) if function.reads_sampled_values())
        })
}

/// Whether every pass through `stmts` suspends the process.
pub(super) fn always_suspends(stmts: &[sv::ir::Stmt]) -> bool {
    stmts.iter().any(|stmt| {
        matches!(
            stmt,
            sv::ir::Stmt::Delay { .. }
                | sv::ir::Stmt::WaitEvent(_)
                | sv::ir::Stmt::WaitImplicit { .. }
        )
    })
}
