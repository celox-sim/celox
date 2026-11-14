//! Loops: unrolled when the trip count is known, counted SLT folds
//! otherwise.

use super::*;

/// The run-time loop folds being executed.
#[derive(Default)]
pub(super) struct Folds {
    /// The depth of run-time loop folds being executed.
    pub(super) folding: usize,
    /// The depth of loop bodies being executed only to find the state they
    /// write; their runtime events are not recorded.
    pub(super) probing: usize,
    /// The runtime events of the innermost run-time loop being folded, in
    /// statement order.
    pub(super) loop_effects: Option<Vec<SLTForEffect>>,
}

impl Comb<'_, '_> {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn exec_loop(
        &mut self,
        mut store: Store,
        frames: &[Frame],
        kind: &sv::ir::LoopKind<sv::ir::Expr>,
        init: &[sv::ir::Stmt],
        condition: Option<&sv::ir::Expr>,
        step: &[sv::ir::Stmt],
        body: &[sv::ir::Stmt],
    ) -> Result<Store, sv::AnalyzerError> {
        let break_flag = self.flag("break");
        let continue_flag = self.flag("continue");
        let mut loop_frames = frames.to_vec();
        loop_frames.push(Frame::Loop {
            break_flag,
            continue_flag,
        });

        let canonical = match kind {
            sv::ir::LoopKind::For => canonical_for_loop(init, condition, step),
            _ => None,
        };
        // An `initial` block runs as a process when it cannot be folded into
        // initial state, so a loop whose trip count is not known before it
        // runs goes there instead of being unrolled until the limit.
        if self.initial {
            let counted = match kind {
                sv::ir::LoopKind::Repeat(_) => true,
                sv::ir::LoopKind::For => canonical.as_ref().is_some_and(|canonical| {
                    let mut written = HashSet::default();
                    written_names(body, &mut written);
                    !written.contains(&canonical.var)
                }),
                sv::ir::LoopKind::While | sv::ir::LoopKind::DoWhile => {
                    match counted_while_loop(condition, body) {
                        Some((counted, rest)) => {
                            !self.loop_needs_fold(&store, frames, &counted, rest)?
                        }
                        None => false,
                    }
                }
                sv::ir::LoopKind::Forever | sv::ir::LoopKind::EventBlock => false,
            };
            if !counted {
                return Err(unsupported(
                    "initial block loop without a static trip count",
                ));
            }
        }
        if let Some(canonical) = canonical
            && self.loop_needs_fold(&store, frames, &canonical, body)?
        {
            self.folds.folding += 1;
            let folded = self.fold_loop(
                store,
                frames,
                &loop_frames,
                &canonical,
                body,
                break_flag,
                continue_flag,
            );
            self.folds.folding -= 1;
            return folded;
        }

        store = self.exec_block(store, frames, init)?;
        self.set_flag(&mut store, break_flag, false)?;
        let mut remaining_repeat = match kind {
            sv::ir::LoopKind::Repeat(count) => {
                let (node, _) = self.eval(&mut store, frames, count, None)?;
                let (value, _) = slt_const(self.arena, &mut self.consts, node)
                    .ok_or_else(|| unsupported("repeat count that depends on run-time values"))?;
                Some(
                    value
                        .to_usize()
                        .ok_or_else(|| unsupported("repeat count"))?,
                )
            }
            _ => None,
        };
        let mut first = true;
        let mut symbolic_iterations = 0usize;
        // Whether every variable the step writes is local to the loop.
        let step_is_local = {
            let mut declared = HashSet::default();
            for stmt in init {
                if let sv::ir::Stmt::Local { name, .. } = stmt {
                    declared.insert(name.clone());
                }
            }
            let mut stepped = HashSet::default();
            written_names(step, &mut stepped);
            !stepped.is_empty() && stepped.iter().all(|name| declared.contains(name))
        };
        loop {
            // The loop condition (not for the first pass of a do-while).
            let proceed = match (kind, condition) {
                (sv::ir::LoopKind::DoWhile, _) if first => None,
                (sv::ir::LoopKind::Repeat(_), _) => {
                    let remaining = remaining_repeat.as_mut().expect("repeat count");
                    if *remaining == 0 {
                        break;
                    }
                    *remaining -= 1;
                    None
                }
                (sv::ir::LoopKind::Forever | sv::ir::LoopKind::EventBlock, _) => None,
                (_, Some(condition)) => {
                    let (node, sources) = self.eval(
                        &mut store,
                        frames,
                        &floating::condition(condition, self.m.variables, self.m.name_to_id),
                        None,
                    )?;
                    let truth = slt_truth(self.arena, node)?;
                    match slt_bool(self.arena, &mut self.consts, truth) {
                        Some(false) => break,
                        Some(true) => None,
                        None => Some((truth, sources)),
                    }
                }
                (_, None) => None,
            };
            first = false;
            self.unrolled += 1;
            if self.unrolled > MAX_UNROLLED_ITERATIONS {
                // The budget is the process's, which earlier loops may have
                // spent, and a loop whose first conditions fold may still
                // depend on values later: the process runs as a guarded unit,
                // which executes its loops as the values make them stop.
                return Err(unsupported(RUNTIME_LOOP_CONDITION));
            }
            // A run-time condition, or a `break` taken on some path, guards
            // the iteration.
            let broken = self.read(&store, break_flag, full(1))?;
            let broken_value = slt_bool(self.arena, &mut self.consts, broken.0);
            if broken_value == Some(true) {
                break;
            }
            let mut guard = proceed;
            if broken_value.is_none() {
                let active = (slt_not(self.arena, &mut self.consts, broken.0)?, broken.1);
                guard = Some(match guard {
                    None => active,
                    Some((node, mut sources)) => {
                        sources.extend(active.1);
                        (
                            slt_and(self.arena, &mut self.consts, node, active.0)?,
                            sources,
                        )
                    }
                });
            }
            if guard.is_some() {
                symbolic_iterations += 1;
                if symbolic_iterations > MAX_SYMBOLIC_ITERATIONS {
                    return Err(unsupported(RUNTIME_LOOP_CONDITION));
                }
            }
            let mut iteration = store.fork();
            self.set_flag(&mut iteration, continue_flag, false)?;
            if let Some(guard) = &guard {
                self.path.guards.push(guard.clone());
            }
            let executed = self.exec_block(iteration, &loop_frames, body);
            if guard.is_some() {
                self.path.guards.pop();
            }
            iteration = executed?;
            // `continue` ends only this iteration; the step still runs.
            self.set_flag(&mut iteration, continue_flag, false)?;
            store = match &guard {
                None => iteration,
                Some(guard) => self.merge(guard, &iteration, &store)?,
            };
            // The step of loop-scoped control variables runs unconditionally:
            // once the loop has stopped on a path, their values are no longer
            // observable there, and keeping them constant keeps the loop
            // bounded. Other steps run only where the loop is still active.
            if step_is_local {
                store = self.exec_block(store, frames, step)?;
            } else {
                let broken_after = self.read(&store, break_flag, full(1))?;
                let mut step_guard = (
                    slt_not(self.arena, &mut self.consts, broken_after.0)?,
                    broken_after.1,
                );
                if let Some(guard) = &guard {
                    let mut sources = step_guard.1;
                    sources.extend(guard.1.iter().copied());
                    step_guard = (
                        slt_and(self.arena, &mut self.consts, step_guard.0, guard.0)?,
                        sources,
                    );
                }
                store = match slt_bool(self.arena, &mut self.consts, step_guard.0) {
                    Some(true) => self.exec_block(store, frames, step)?,
                    Some(false) => store,
                    None => {
                        let stepped = self.exec_block(store.fork(), frames, step)?;
                        self.merge(&step_guard, &stepped, &store)?
                    }
                };
            }
            // A `return` inside the loop ends it as well, and so does a
            // `disable` of a named block around it.
            if let Some(active) = self.active_around_loop(&store, frames)? {
                match slt_bool(self.arena, &mut self.consts, active.0) {
                    Some(false) => break,
                    Some(true) => {}
                    None => {
                        // Later iterations run only while neither was taken.
                        let (node, sources) = self.read(&store, break_flag, full(1))?;
                        let returned = slt_not(self.arena, &mut self.consts, active.0)?;
                        let node = slt_or(self.arena, &mut self.consts, node, returned)?;
                        let mut sources = sources;
                        sources.extend(active.1);
                        self.write(&mut store, break_flag, full(1), (node, sources))?;
                    }
                }
            }
        }
        Ok(store)
    }

    /// The flags of the jumps that leave a loop with the frames `frames`
    /// around it, besides its own `break`: the exit flags of the named
    /// blocks around it in its subroutine (9.6.2) and that subroutine's
    /// `return` flag. An enclosing loop has no flag the loop can set.
    pub(super) fn flags_around_loop(frames: &[Frame]) -> Vec<SourceVarId> {
        let mut flags = Vec::new();
        for frame in frames.iter().rev() {
            match frame {
                Frame::Loop { .. } => {}
                Frame::Block { exit_flag, .. } => flags.push(*exit_flag),
                Frame::Function { return_flag, .. } => {
                    flags.push(*return_flag);
                    break;
                }
            }
        }
        flags
    }

    /// The condition that none of the [`Self::flags_around_loop`] is set;
    /// `None` when every one is known clear.
    pub(super) fn active_around_loop(
        &mut self,
        store: &Store,
        frames: &[Frame],
    ) -> Result<Option<Value>, sv::AnalyzerError> {
        let mut any: Option<Value> = None;
        for flag in Self::flags_around_loop(frames) {
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

    /// Whether a counted loop must be folded: its bounds depend on run-time
    /// values, or it runs more iterations than may be unrolled.
    pub(super) fn loop_needs_fold(
        &mut self,
        store: &Store,
        frames: &[Frame],
        canonical: &CanonicalLoop,
        body: &[sv::ir::Stmt],
    ) -> Result<bool, sv::AnalyzerError> {
        let mut written = HashSet::default();
        written_names(body, &mut written);
        if written.contains(&canonical.var) {
            return Ok(false);
        }
        // Only inspect the bounds: their runtime events belong to the
        // evaluation that follows.
        let mut probe = store.fork();
        self.folds.probing += 1;
        let bounds = self
            .eval(&mut probe, frames, &canonical.start, None)
            .and_then(|start| Ok((start, self.loop_end(&mut probe, frames, canonical)?)));
        self.folds.probing -= 1;
        let ((start, _), (end, _)) = bounds?;
        let (Some((start, _)), Some((end, _))) = (
            slt_const(self.arena, &mut self.consts, start),
            slt_const(self.arena, &mut self.consts, end),
        ) else {
            return Ok(true);
        };
        // Constant bounds: estimate the trip count of an additive loop.
        let start = start.to_f64().unwrap_or(f64::MAX);
        let end = end.to_f64().unwrap_or(f64::MAX);
        let trips = if canonical.step_op == celox_slt::SLTStepOp::Add {
            ((end - start).abs() / canonical.step.max(1) as f64).ceil()
        } else {
            0.0
        };
        Ok(trips > (MAX_UNROLLED_ITERATIONS - self.unrolled.min(MAX_UNROLLED_ITERATIONS)) as f64)
    }

    /// The bound of a counted loop. It is an operand of the relational loop
    /// condition, so it is evaluated at the width of the comparison with the
    /// loop variable, and signed only when both operands are (IEEE 1800-2023
    /// 11.6.1, 11.8.1 and 11.8.2).
    pub(super) fn loop_end(
        &mut self,
        store: &mut Store,
        frames: &[Frame],
        canonical: &CanonicalLoop,
    ) -> Result<Value, sv::AnalyzerError> {
        let var = sv::ir::Expr::Ident(canonical.var.clone());
        let signed = self.expr_signed(&var) && self.expr_signed(&canonical.end);
        let context = self
            .m
            .comparison_width(&var, &canonical.end)
            .map(|width| (width, signed));
        self.eval(store, frames, &canonical.end, context)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn fold_loop(
        &mut self,
        mut store: Store,
        frames: &[Frame],
        loop_frames: &[Frame],
        canonical: &CanonicalLoop,
        body: &[sv::ir::Stmt],
        break_flag: SourceVarId,
        continue_flag: SourceVarId,
    ) -> Result<Store, sv::AnalyzerError> {
        let loop_var = self
            .m
            .id(&canonical.var)
            .ok_or_else(|| unsupported(format!("loop variable `{}`", canonical.var)))?;
        let (loop_width, loop_signed) = {
            let var = self.m.var(loop_var);
            (var.width, var.signed)
        };
        // The initializer is assigned to the loop variable: it widens by its
        // own signedness (IEEE 1800-2023 10.7, 11.8.1).
        let start_signed = self.expr_signed(&canonical.start);
        let (start, start_sources) = self.eval(
            &mut store,
            frames,
            &canonical.start,
            Some((loop_width, start_signed)),
        )?;
        let start = coerce_node_width(self.arena, start, Some(loop_width), start_signed)
            .map_err(slt_error)?;
        let end_signed = self.expr_signed(&canonical.end);
        let (mut end, end_sources) = self.loop_end(&mut store, frames, canonical)?;
        let mut inclusive = matches!(
            canonical.compare,
            sv::ir::BinaryOp::Le | sv::ir::BinaryOp::Ge
        );
        if canonical.compare == sv::ir::BinaryOp::Gt {
            // `v > end` is `v >= end + 1`.
            let width = self.width(end) + 1;
            let wide =
                coerce_node_width(self.arena, end, Some(width), end_signed).map_err(slt_error)?;
            let one = slt_constant(self.arena, BigUint::from(1u8), width, end_signed)?;
            end = self.alloc(SLTNode::Binary(wide, BinaryOp::Add, one))?;
            inclusive = true;
        }
        let (start_bound, end_bound) = if canonical.decreasing {
            (
                SLTLoopBound::TypedExpr {
                    node: end,
                    signed: end_signed,
                },
                SLTLoopBound::TypedExpr {
                    node: start,
                    signed: loop_signed,
                },
            )
        } else {
            (
                SLTLoopBound::TypedExpr {
                    node: start,
                    signed: loop_signed,
                },
                SLTLoopBound::TypedExpr {
                    node: end,
                    signed: end_signed,
                },
            )
        };
        // The reverse fold counts down from the start value itself.
        let inclusive = if canonical.decreasing {
            true
        } else {
            inclusive
        };

        // Find the state the body writes, then run it with that state read
        // from the loop-carried values.
        let mut probe = store.fork();
        let loop_input = self.alloc(SLTNode::Input {
            variable: loop_var,
            signed: loop_signed,
            index: Vec::new(),
            access: full(loop_width),
        })?;
        probe.remove(&loop_var);
        let _ = loop_input;
        self.set_flag(&mut probe, break_flag, false)?;
        self.set_flag(&mut probe, continue_flag, false)?;
        let saved_unrolled = self.unrolled;
        let first_temp = self.m.next_id;
        self.folds.probing += 1;
        let probe_after = self.exec_block(probe.fork(), loop_frames, body);
        self.folds.probing -= 1;
        let probe_after = probe_after?;
        self.unrolled = saved_unrolled;
        let is_state = |id: &SourceVarId| {
            *id != loop_var && *id != break_flag && *id != continue_flag && id.0 < first_temp.0
        };
        let mut carried: Vec<SourceVarId> = probe_after
            .differing_keys(&probe)
            .copied()
            .filter(is_state)
            .collect();
        for id in probe_after.keys() {
            if probe.get(id).is_none() && is_state(id) && !carried.contains(id) {
                carried.push(*id);
            }
        }
        carried.sort_by_key(|id| id.0);

        let mut iteration = store.fork();
        iteration.remove(&loop_var);
        for id in &carried {
            iteration.remove(id);
        }
        self.set_flag(&mut iteration, break_flag, false)?;
        self.set_flag(&mut iteration, continue_flag, false)?;
        let observer_start = self.events.observers.len();
        let outer_effects = self.folds.loop_effects.replace(Vec::new());
        let after = self.exec_block(iteration, loop_frames, body);
        let effects =
            std::mem::replace(&mut self.folds.loop_effects, outer_effects).unwrap_or_default();
        let after = after?;

        let mut initials = Vec::with_capacity(carried.len());
        let mut updates = Vec::with_capacity(carried.len());
        let mut initial_sources = Sources::default();
        let mut update_sources = Sources::default();
        for id in &carried {
            let width = self.m.var(*id).width;
            let target = VarAtomBase::new(*id, 0, width - 1);
            let (initial, sources) = self.read(&store, *id, full(width))?;
            initial_sources.extend(sources);
            let (update, sources) = self.read(&after, *id, full(width))?;
            update_sources.extend(sources);
            initials.push(SLTForUpdate {
                target,
                expr: initial,
            });
            updates.push(SLTForUpdate {
                target,
                expr: update,
            });
        }
        // The fold stops after an iteration that breaks, returns or
        // disables a named block around the loop.
        let (broke, broke_sources) = self.read(&after, break_flag, full(1))?;
        let mut stop = broke;
        let mut stop_sources = broke_sources;
        for flag in Self::flags_around_loop(frames) {
            if after.get(&flag).is_some() {
                let (left, sources) = self.read(&after, flag, full(1))?;
                stop = slt_or(self.arena, &mut self.consts, stop, left)?;
                stop_sources.extend(sources);
            }
        }
        let continue_cond = slt_not(self.arena, &mut self.consts, stop)?;

        let bound: HashSet<SourceVarId> = carried.iter().copied().chain([loop_var]).collect();
        let mut sources: Sources = start_sources;
        sources.extend(end_sources);
        sources.extend(initial_sources);
        sources.extend(
            update_sources
                .into_iter()
                .filter(|source| !bound.contains(&source.id)),
        );
        sources.extend(
            stop_sources
                .into_iter()
                .filter(|source| !bound.contains(&source.id)),
        );

        if !effects.is_empty() {
            // The events follow every value the iterations depend on, such
            // as the bounds, even when no output does.
            self.events
                .effect_sensitivity
                .extend(sources.iter().copied());
            // A runner repeats the loop to emit its events, iteration by
            // iteration, with the loop-carried state of each iteration.
            let result = match carried.first() {
                Some(id) => {
                    SLTForFoldResult::State(VarAtomBase::new(*id, 0, self.m.var(*id).width - 1))
                }
                None => {
                    let live = self.constant(1, 1)?;
                    SLTForFoldResult::Transient {
                        initial: live,
                        update: live,
                    }
                }
            };
            let runner = self.alloc(SLTNode::ForFold {
                loop_var,
                loop_width,
                loop_signed,
                start: start_bound.clone(),
                end: end_bound.clone(),
                inclusive,
                step: canonical.step,
                step_op: canonical.step_op,
                reverse: canonical.decreasing,
                result,
                initials: initials.clone(),
                updates: updates.clone(),
                effects,
                continue_cond,
            })?;
            match &mut self.folds.loop_effects {
                Some(outer) => outer.push(SLTForEffect::Runner(runner)),
                None => {
                    let observer = self
                        .events
                        .observers
                        .get_mut(observer_start)
                        .ok_or_else(|| unsupported("runtime event of a run-time loop"))?;
                    observer.loop_runner = Some(runner);
                }
            }
        }
        if carried.is_empty() {
            return Ok(store);
        }
        for id in &carried {
            let width = self.m.var(*id).width;
            let target = VarAtomBase::new(*id, 0, width - 1);
            let node = self.alloc(SLTNode::ForFold {
                loop_var,
                loop_width,
                loop_signed,
                start: start_bound.clone(),
                end: end_bound.clone(),
                inclusive,
                step: canonical.step,
                step_op: canonical.step_op,
                reverse: canonical.decreasing,
                result: SLTForFoldResult::State(target),
                initials: initials.clone(),
                updates: updates.clone(),
                effects: Vec::new(),
                continue_cond,
            })?;
            let own: Sources = sources
                .iter()
                .copied()
                .filter(|source| source.id != *id)
                .collect();
            self.write(&mut store, *id, full(width), (node, own))?;
        }
        Ok(store)
    }
}

/// Iterations unrolled under a run-time loop condition before the loop is
/// considered unbounded.
pub(super) const MAX_SYMBOLIC_ITERATIONS: usize = 1024;

/// Why the symbolic form refuses a loop it cannot bound; the process then
/// runs as a guarded unit (see `lower_comb_processes`).
pub(in crate::lowering) const RUNTIME_LOOP_CONDITION: &str =
    "loop condition that depends on run-time values";
