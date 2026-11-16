//! Statements: sequencing, branches, named blocks and jumps, and the
//! condition under which the rest of a block still runs.

use super::*;

impl Comb<'_, '_> {
    /// `None` when every jump flag of the innermost loop and function is known
    /// clear; otherwise the condition that execution is still active.
    pub(super) fn active(
        &mut self,
        store: &Store,
        frames: &[Frame],
    ) -> Result<Option<Value>, sv::AnalyzerError> {
        let mut flags = Vec::new();
        let mut seen_loop = false;
        for frame in frames.iter().rev() {
            match frame {
                Frame::Loop {
                    break_flag,
                    continue_flag,
                } => {
                    if !seen_loop {
                        flags.push(*break_flag);
                        flags.push(*continue_flag);
                        seen_loop = true;
                    }
                }
                Frame::Block { exit_flag, .. } => flags.push(*exit_flag),
                Frame::Function { return_flag, .. } => {
                    flags.push(*return_flag);
                    break;
                }
            }
        }
        let mut any: Option<Value> = None;
        for flag in flags {
            if store.get(&flag).is_none() {
                continue;
            }
            let (node, sources) = self.read(store, flag, full(1))?;
            if slt_bool(self.arena, &mut self.consts, node) == Some(false) {
                continue;
            }
            any = Some(match any {
                None => (node, sources),
                Some((other, mut other_sources)) => {
                    other_sources.extend(sources);
                    (
                        slt_or(self.arena, &mut self.consts, other, node)?,
                        other_sources,
                    )
                }
            });
        }
        match any {
            None => Ok(None),
            Some((node, sources)) => Ok(Some((
                slt_not(self.arena, &mut self.consts, node)?,
                sources,
            ))),
        }
    }

    pub(super) fn exec_block(
        &mut self,
        mut store: Store,
        frames: &[Frame],
        stmts: &[sv::ir::Stmt],
    ) -> Result<Store, sv::AnalyzerError> {
        for (index, stmt) in stmts.iter().enumerate() {
            store = self.exec(store, frames, stmt)?;
            if index + 1 < stmts.len() && stmt_may_jump(stmt, false) {
                if let Some(active) = self.active(&store, frames)? {
                    return match slt_bool(self.arena, &mut self.consts, active.0) {
                        Some(false) => Ok(store),
                        Some(true) => self.exec_block(store, frames, &stmts[index + 1..]),
                        None => {
                            self.path.guards.push(active.clone());
                            let rest = self.exec_block(store.fork(), frames, &stmts[index + 1..]);
                            self.path.guards.pop();
                            self.merge(&active, &rest?, &store)
                        }
                    };
                }
            }
        }
        Ok(store)
    }

    pub(super) fn exec(
        &mut self,
        mut store: Store,
        frames: &[Frame],
        stmt: &sv::ir::Stmt,
    ) -> Result<Store, sv::AnalyzerError> {
        match stmt {
            sv::ir::Stmt::ProceduralAssign { .. } | sv::ir::Stmt::Deassign { .. } => {
                unreachable!("the analyzer lowers procedural continuous assignments")
            }
            sv::ir::Stmt::DisableFork => Ok(store),
            sv::ir::Stmt::Fork { .. }
            | sv::ir::Stmt::WaitFork
            | sv::ir::Stmt::AssertionRegion { .. } => {
                Err(unsupported("fork control outside a timed process"))
            }
            sv::ir::Stmt::Assign {
                lhs,
                rhs,
                nonblocking,
            } => {
                if *nonblocking && !self.allow_nonblocking {
                    return Err(unsupported("nonblocking assignment inside always_comb"));
                }
                self.assign(&mut store, frames, lhs, rhs)?;
                Ok(store)
            }
            sv::ir::Stmt::AssignConcat {
                parts,
                rhs,
                nonblocking,
            } => {
                if *nonblocking && !self.allow_nonblocking {
                    return Err(unsupported("nonblocking assignment inside always_comb"));
                }
                self.assign_concat(&mut store, frames, parts, rhs)?;
                Ok(store)
            }
            // A timing control suspends a process; this lowering has none.
            sv::ir::Stmt::DeferActive
            | sv::ir::Stmt::DelayedNonblocking { .. }
            | sv::ir::Stmt::Delay { .. }
            | sv::ir::Stmt::WaitEvent(_)
            | sv::ir::Stmt::WaitImplicit { .. }
            | sv::ir::Stmt::Wait(_) => Err(unsupported("procedural timing control")),
            sv::ir::Stmt::Local { name, init } => {
                let id = self
                    .m
                    .id(name)
                    .ok_or_else(|| unsupported(format!("local `{name}`")))?;
                let width = self.m.var(id).width;
                match init {
                    Some(init) => {
                        self.assign(
                            &mut store,
                            frames,
                            &sv::ir::LValue::Ident(name.clone()),
                            init,
                        )?;
                    }
                    None => {
                        let node = self.default_value(id)?;
                        self.write(&mut store, id, full(width), (node, Sources::default()))?;
                    }
                }
                Ok(store)
            }
            sv::ir::Stmt::If {
                condition,
                then_body,
                else_body,
            } => {
                let (node, sources) = self.eval(
                    &mut store,
                    frames,
                    &floating::condition(condition, self.m.variables, self.m.name_to_id),
                    None,
                )?;
                let truth = slt_truth(self.arena, node)?;
                match slt_bool(self.arena, &mut self.consts, truth).or_else(|| self.known(truth)) {
                    Some(true) => self.exec_block(store, frames, then_body),
                    Some(false) => self.exec_block(store, frames, else_body),
                    None => {
                        self.path.facts.push((truth, true, sources.clone()));
                        let then_store = self.exec_block(store.fork(), frames, then_body);
                        self.path.facts.pop();
                        let then_store = then_store?;
                        self.path.facts.push((truth, false, sources.clone()));
                        let else_store = self.exec_block(store, frames, else_body);
                        self.path.facts.pop();
                        let else_store = else_store?;
                        self.merge(&(truth, sources), &then_store, &else_store)
                    }
                }
            }
            sv::ir::Stmt::Case {
                kind,
                selector,
                items,
                default,
            } => self.exec_case(store, frames, *kind, selector, items, default.as_deref()),
            sv::ir::Stmt::Loop {
                kind,
                init,
                condition,
                step,
                body,
            } => self.exec_loop(store, frames, kind, init, condition.as_ref(), step, body),
            sv::ir::Stmt::Break | sv::ir::Stmt::Continue => {
                // The innermost loop of this subroutine; named blocks are
                // transparent to it.
                let mut flag = None;
                for frame in frames.iter().rev() {
                    match frame {
                        Frame::Loop {
                            break_flag,
                            continue_flag,
                        } => {
                            flag = Some(if matches!(stmt, sv::ir::Stmt::Break) {
                                *break_flag
                            } else {
                                *continue_flag
                            });
                            break;
                        }
                        Frame::Block { .. } => {}
                        Frame::Function { .. } => break,
                    }
                }
                let flag = flag.ok_or_else(|| unsupported("break or continue outside a loop"))?;
                self.set_flag(&mut store, flag, true)?;
                Ok(store)
            }
            sv::ir::Stmt::Block { id, body } => {
                let exit_flag = self.flag("block-exit");
                self.set_flag(&mut store, exit_flag, false)?;
                let mut frames = frames.to_vec();
                frames.push(Frame::Block { id: *id, exit_flag });
                self.exec_block(store, &frames, body)
            }
            sv::ir::Stmt::DisableBlock(id) => {
                // The named block executing in this subroutine.
                let mut flag = None;
                for frame in frames.iter().rev() {
                    match frame {
                        Frame::Block {
                            id: block,
                            exit_flag,
                        } if block == id => {
                            flag = Some(*exit_flag);
                            break;
                        }
                        Frame::Function { .. } => break,
                        _ => {}
                    }
                }
                let flag = flag.ok_or_else(|| {
                    unsupported("disable of a block that is not executing in this process")
                })?;
                self.set_flag(&mut store, flag, true)?;
                Ok(store)
            }
            sv::ir::Stmt::Return(value) => {
                let (return_flag, return_var) = frames
                    .iter()
                    .rev()
                    .find_map(|frame| match frame {
                        Frame::Function {
                            return_flag,
                            return_var,
                        } => Some((*return_flag, return_var.clone())),
                        Frame::Loop { .. } | Frame::Block { .. } => None,
                    })
                    .ok_or_else(|| unsupported("return outside a subroutine"))?;
                if let (Some(value), Some(return_var)) = (value, return_var) {
                    self.assign(
                        &mut store,
                        frames,
                        &sv::ir::LValue::Ident(return_var),
                        value,
                    )?;
                }
                self.set_flag(&mut store, return_flag, true)?;
                Ok(store)
            }
            sv::ir::Stmt::Call {
                name,
                args,
                arg_order,
            } => {
                if self.m.dpi_imports.contains_key(name) {
                    return Err(unsupported(format!(
                        "DPI-C function `{name}` called in combinational logic"
                    )));
                }
                if !self.m.subroutines.contains_key(name) {
                    return Err(unsupported(format!("call of `{name}`")));
                }
                self.call(&mut store, frames, name, args, arg_order)?;
                Ok(store)
            }
            sv::ir::Stmt::Eval(expr) => {
                self.eval(&mut store, frames, expr, None)?;
                Ok(store)
            }
            sv::ir::Stmt::SystemTask {
                task: sv::ir::SystemTask::Readmem(format),
                args,
                site,
                ..
            } => {
                self.readmem(&mut store, frames, *format, args, site.as_ref())?;
                Ok(store)
            }
            // IEEE 1800-2023 24.7: `$exit` outside a program block is
            // ignored, with a warning.
            sv::ir::Stmt::SystemTask {
                task: sv::ir::SystemTask::Exit,
                site,
                ..
            } => {
                self.m.warn(
                    crate::FrontendWarningKind::ExitOutsideProgram,
                    site.as_ref(),
                );
                Ok(store)
            }
            sv::ir::Stmt::SystemTask { task, .. } if self.initial => Err(unsupported(format!(
                "system task `{task}` inside an initial block"
            ))),
            sv::ir::Stmt::SystemTask {
                task,
                args,
                site,
                assertion,
            } => {
                self.system_task(
                    &mut store,
                    frames,
                    *task,
                    args,
                    site.as_ref(),
                    assertion.as_ref(),
                )?;
                Ok(store)
            }
        }
    }
}
