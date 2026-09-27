//! Counted-loop lowering, joint fold groups, and invariant captures.

use super::*;

impl SLTToSIRLowerer {
    /// Lower several independent recovered folds as one counted loop.
    ///
    /// This entry point is intentionally transactional: a rejected family
    /// leaves both the builder and the materialization cache unchanged.  The
    /// scheduler remains responsible for proving that the roots are mutually
    /// unordered in its dependency graph; this method rechecks every local
    /// property needed by the joint loop itself.
    pub fn lower_fold_groups_jointly<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        roots: &[NodeId],
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
    ) -> bool {
        if roots.is_empty()
            || roots.iter().any(|root| cache.contains_key(root))
            || roots.iter().copied().collect::<crate::HashSet<_>>().len() != roots.len()
        {
            return false;
        }
        let Some(specs) = roots
            .iter()
            .copied()
            .map(|root| FoldGroupLowerSpec::from_root(root, arena))
            .collect::<Option<Vec<_>>>()
        else {
            return false;
        };
        // A word-level scan is strictly cheaper than putting this group back
        // into a shared counted loop.  Leave it for ordinary single-root
        // lowering, which can apply the algebraic plan without weakening the
        // joint-lowering transaction.
        if !self.four_state
            && specs
                .iter()
                .any(|spec| match_slt_or_scan_plan(spec, arena).is_some())
        {
            return false;
        }
        if !Self::joint_fold_group_specs_are_legal(&specs, arena) {
            return false;
        }

        self.reset_cost_cache_roots(roots, arena, cache, true);
        let results = self.lower_fold_group_specs(builder, arena, cache, &specs, None, true);
        debug_assert_eq!(results.len(), roots.len());
        for (&root, result) in roots.iter().zip(results) {
            let previous = cache.insert(root, result);
            debug_assert!(previous.is_none());
            self.cache_insert_log.borrow_mut().push(root);
        }
        true
    }

    fn joint_fold_group_specs_are_legal<A: Hash + Eq + Clone>(
        specs: &[FoldGroupLowerSpec<'_, A>],
        arena: &SLTNodeArena<A>,
    ) -> bool {
        let Some(first) = specs.first() else {
            return false;
        };
        if specs.iter().any(|spec| {
            spec.loop_width != first.loop_width
                || spec.loop_signed != first.loop_signed
                || spec.start != first.start
                || spec.step != first.step
                || spec.trip_count != first.trip_count
                || spec.entry_guard != first.entry_guard
                || spec.loop_width == 0
                || spec.trip_count == 0
                || spec.states.is_empty()
        }) {
            return false;
        }

        let mut state_owners: crate::HashMap<A, Vec<(usize, BitAccess)>> =
            crate::HashMap::default();
        for (owner, spec) in specs.iter().enumerate() {
            for state in spec.states {
                if specs
                    .iter()
                    .any(|candidate| *candidate.loop_var == state.target.id)
                {
                    return false;
                }
                let owners = state_owners.entry(state.target.id.clone()).or_default();
                if owners
                    .iter()
                    .any(|(_, access)| access.overlaps(&state.target.access))
                {
                    return false;
                }
                owners.push((owner, state.target.access));
            }
        }

        let mut preheader_visited = crate::HashSet::default();
        if !Self::joint_fold_tree_is_legal(
            first.entry_guard,
            None,
            specs,
            &state_owners,
            arena,
            &mut preheader_visited,
        ) {
            return false;
        }
        let mut update_visited = (0..specs.len())
            .map(|_| crate::HashSet::default())
            .collect::<Vec<_>>();
        for (owner, spec) in specs.iter().enumerate() {
            for state in spec.states {
                if !Self::joint_fold_tree_is_legal(
                    state.initial,
                    None,
                    specs,
                    &state_owners,
                    arena,
                    &mut preheader_visited,
                ) || !Self::joint_fold_tree_is_legal(
                    state.update,
                    Some(owner),
                    specs,
                    &state_owners,
                    arena,
                    &mut update_visited[owner],
                ) {
                    return false;
                }
            }
        }
        true
    }

    fn joint_fold_tree_is_legal<A: Hash + Eq + Clone>(
        root: NodeId,
        update_owner: Option<usize>,
        specs: &[FoldGroupLowerSpec<'_, A>],
        state_owners: &crate::HashMap<A, Vec<(usize, BitAccess)>>,
        arena: &SLTNodeArena<A>,
        visited: &mut crate::HashSet<NodeId>,
    ) -> bool {
        let mut work = vec![root];
        while let Some(node) = work.pop() {
            if !visited.insert(node) {
                continue;
            }
            match arena.get(node) {
                SLTNode::Input {
                    variable,
                    index,
                    access,
                    ..
                } => {
                    if let (Some(owner), Some(owners)) = (update_owner, state_owners.get(variable))
                    {
                        let overlaps = owners
                            .iter()
                            .filter(|(_, target)| !index.is_empty() || target.overlaps(access));
                        for (state_owner, _) in overlaps {
                            if owner != *state_owner {
                                return false;
                            }
                        }
                    }
                    if let Some(owner) = update_owner {
                        for spec in specs {
                            if *spec.loop_var == *variable
                                && (*spec.loop_var != *specs[owner].loop_var || !index.is_empty())
                            {
                                return false;
                            }
                        }
                    }
                    work.extend(index.iter().map(|entry| entry.node));
                }
                SLTNode::Constant(..) => {}
                SLTNode::Binary(lhs, _, rhs) => {
                    work.push(*lhs);
                    work.push(*rhs);
                }
                SLTNode::Unary(_, inner) | SLTNode::Capture { expr: inner, .. } => {
                    work.push(*inner)
                }
                SLTNode::Mux {
                    cond,
                    then_expr,
                    else_expr,
                } => {
                    work.push(*cond);
                    work.push(*then_expr);
                    work.push(*else_expr);
                }
                SLTNode::Concat(parts) => {
                    work.extend(parts.iter().map(|(part, _)| *part));
                }
                SLTNode::Slice { expr, .. } => work.push(*expr),
                SLTNode::ForFold { .. } | SLTNode::ForFoldGroup { .. } => return false,
            }
        }
        true
    }

    fn fold_node_is_invariant<A: Hash + Eq + Clone>(
        node: NodeId,
        rebound_variables: &crate::HashSet<&A>,
        arena: &SLTNodeArena<A>,
        memo: &mut crate::HashMap<NodeId, bool>,
    ) -> bool {
        if let Some(&invariant) = memo.get(&node) {
            return invariant;
        }
        let invariant = match arena.get(node) {
            SLTNode::Input {
                variable, index, ..
            } => {
                !rebound_variables.contains(variable)
                    && index.iter().all(|entry| {
                        Self::fold_node_is_invariant(entry.node, rebound_variables, arena, memo)
                    })
            }
            SLTNode::Constant(..) => true,
            SLTNode::Binary(lhs, _, rhs) => {
                Self::fold_node_is_invariant(*lhs, rebound_variables, arena, memo)
                    && Self::fold_node_is_invariant(*rhs, rebound_variables, arena, memo)
            }
            SLTNode::Unary(_, inner)
            | SLTNode::Capture { expr: inner, .. }
            | SLTNode::Slice { expr: inner, .. } => {
                Self::fold_node_is_invariant(*inner, rebound_variables, arena, memo)
            }
            SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } => {
                Self::fold_node_is_invariant(*cond, rebound_variables, arena, memo)
                    && Self::fold_node_is_invariant(*then_expr, rebound_variables, arena, memo)
                    && Self::fold_node_is_invariant(*else_expr, rebound_variables, arena, memo)
            }
            SLTNode::Concat(parts) => parts.iter().all(|(part, _)| {
                Self::fold_node_is_invariant(*part, rebound_variables, arena, memo)
            }),
            SLTNode::ForFold { .. } | SLTNode::ForFoldGroup { .. } => false,
        };
        memo.insert(node, invariant);
        invariant
    }

    fn fold_capture_is_total<A: Hash + Eq + Clone>(
        node: NodeId,
        arena: &SLTNodeArena<A>,
        memo: &mut crate::HashMap<NodeId, bool>,
    ) -> bool {
        if let Some(&total) = memo.get(&node) {
            return total;
        }
        let total = match arena.get(node) {
            SLTNode::Binary(
                _,
                BinaryOp::DivU | BinaryOp::DivS | BinaryOp::RemU | BinaryOp::RemS,
                _,
            )
            | SLTNode::ForFold { .. }
            | SLTNode::ForFoldGroup { .. } => false,
            _ => Self::node_children(node, arena)
                .into_iter()
                .all(|child| Self::fold_capture_is_total(child, arena, memo)),
        };
        memo.insert(node, total);
        total
    }

    fn fold_invariant_capture_frontier<A: Hash + Eq + Clone>(
        specs: &[FoldGroupLowerSpec<'_, A>],
        arena: &SLTNodeArena<A>,
    ) -> Vec<NodeId> {
        let mut rebound_variables = crate::HashSet::default();
        for spec in specs {
            rebound_variables.insert(spec.loop_var);
            rebound_variables.extend(spec.states.iter().map(|state| &state.target.id));
        }
        let mut invariant_memo = crate::HashMap::default();
        let mut total_memo = crate::HashMap::default();
        let mut captures = crate::HashSet::default();
        let mut pending = specs
            .iter()
            .flat_map(|spec| spec.states.iter().map(|state| state.update))
            .collect::<Vec<_>>();
        let mut visited = crate::HashSet::default();
        while let Some(node) = pending.pop() {
            if !visited.insert(node) {
                continue;
            }
            let invariant =
                Self::fold_node_is_invariant(node, &rebound_variables, arena, &mut invariant_memo);
            if invariant
                && Self::fold_capture_is_total(node, arena, &mut total_memo)
                && !matches!(arena.get(node), SLTNode::Constant(..))
            {
                captures.insert(node);
                continue;
            }
            pending.extend(Self::node_children(node, arena));
        }
        let mut captures = captures.into_iter().collect::<Vec<_>>();
        captures.sort_unstable();
        captures
    }

    pub(super) fn lower_bound<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        bound: &SLTLoopBound,
        _canonical_width: usize,
        width: usize,
        signed: bool,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
        env: Option<&LowerEnv<'_, A>>,
    ) -> RegisterId {
        match bound {
            SLTLoopBound::Const(v) => {
                let reg = builder.alloc_bit(width, signed);
                builder.emit(SIRInstruction::Imm(reg, SIRValue::new(*v as u64)));
                reg
            }
            SLTLoopBound::Expr(node) => {
                let reg = self.lower_inner(builder, *node, arena, cache, env, env.is_none());
                let source_signed = self.get_bound_signed(*node, arena);
                let extend_signed = source_signed && signed;
                let sized = self.cast_reg_width_ext(builder, reg, width, extend_signed);
                if extend_signed == signed {
                    sized
                } else {
                    let dest = builder.alloc_bit(width, signed);
                    builder.emit(SIRInstruction::Unary(dest, UnaryOp::Ident, sized));
                    dest
                }
            }
        }
    }

    pub(super) fn bound_width(bound: &SLTLoopBound) -> usize {
        match bound {
            SLTLoopBound::Const(v) => {
                let bits = usize::BITS as usize - v.leading_zeros() as usize;
                bits.max(1)
            }
            SLTLoopBound::Expr(_) => 0,
        }
    }

    fn step_math_width(base_width: usize, step_op: SLTStepOp, step: usize) -> usize {
        match step_op {
            SLTStepOp::Add => {
                let step_bits = (usize::BITS as usize - step.leading_zeros() as usize).max(1);
                base_width.saturating_add(step_bits)
            }
            SLTStepOp::Mul => {
                let step_bits = (usize::BITS as usize - step.leading_zeros() as usize).max(1);
                base_width.saturating_add(step_bits)
            }
            SLTStepOp::Shl => base_width.saturating_add(step.max(1)),
            SLTStepOp::BitOr | SLTStepOp::BitXor => base_width,
        }
    }

    fn truncate_usize_to_width(value: usize, width: usize) -> usize {
        if width >= usize::BITS as usize {
            value
        } else if width == 0 {
            0
        } else {
            value & ((1usize << width) - 1)
        }
    }

    fn bigint_payload(value: &BigInt, width: usize) -> BigUint {
        let modulus = BigInt::from(1u8) << width;
        let mut wrapped = value % &modulus;
        if wrapped < BigInt::from(0u8) {
            wrapped += modulus;
        }
        wrapped
            .to_biguint()
            .expect("a modulo-reduced loop value must be non-negative")
    }

    pub(super) fn pack_fold_group_states<A>(
        &self,
        builder: &mut SIRBuilder<A>,
        states: &[RegisterId],
    ) -> RegisterId {
        debug_assert!(!states.is_empty());
        let width = states
            .iter()
            .map(|state| builder.register(state).width())
            .sum();
        let packed = builder.alloc_logic(width);
        builder.emit(SIRInstruction::Concat(packed, states.to_vec()));
        packed
    }

    /// Lower one or more independent, fixed-trip-count multi-state folds.
    ///
    /// The loop body sees one immutable set of block parameters, so every
    /// update is computed from the previous iteration and the backedge applies
    /// all updates simultaneously.  The counter is a remaining-iteration
    /// count: it cannot stall and needs neither a safety cap nor an Error exit.
    pub(super) fn lower_fold_group_specs<
        'env,
        A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display,
    >(
        &self,
        builder: &mut SIRBuilder<A>,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
        specs: &[FoldGroupLowerSpec<'_, A>],
        outer_env: Option<&'env LowerEnv<'env, A>>,
        allow_cache: bool,
    ) -> Vec<RegisterId> {
        let first = specs
            .first()
            .expect("joint fold lowering requires at least one group");
        debug_assert!(first.loop_width > 0);
        debug_assert!(first.trip_count > 0);
        debug_assert!(specs.iter().all(|spec| !spec.states.is_empty()));
        debug_assert!(specs.iter().all(|spec| {
            spec.loop_width == first.loop_width
                && spec.loop_signed == first.loop_signed
                && spec.start == first.start
                && spec.step == first.step
                && spec.trip_count == first.trip_count
                && spec.entry_guard == first.entry_guard
        }));

        if !self.four_state
            && outer_env.is_none()
            && specs.len() == 1
            && let Some(plan) = match_slt_or_scan_plan(first, arena)
        {
            return vec![self.lower_or_scan_plan(builder, arena, cache, first, plan, allow_cache)];
        }

        let guard = self.lower_inner(
            builder,
            first.entry_guard,
            arena,
            cache,
            outer_env,
            allow_cache,
        );
        let mut group_ranges = Vec::with_capacity(specs.len());
        let mut state_count = 0usize;
        for spec in specs {
            let start = state_count;
            state_count = state_count
                .checked_add(spec.states.len())
                .expect("verified joint fold state count must fit usize");
            group_ranges.push(start..state_count);
        }
        let initial_states: Vec<_> = specs
            .iter()
            .flat_map(|spec| spec.states)
            .map(|state| {
                let initial =
                    self.lower_inner(builder, state.initial, arena, cache, outer_env, allow_cache);
                self.cast_reg_width(
                    builder,
                    initial,
                    state.target.access.msb - state.target.access.lsb + 1,
                )
            })
            .collect();
        let initial_packed = if self.four_state {
            group_ranges
                .iter()
                .map(|range| {
                    Some(self.pack_fold_group_states(builder, &initial_states[range.clone()]))
                })
                .collect::<Vec<_>>()
        } else {
            vec![None; specs.len()]
        };
        let remaining_width =
            (usize::BITS as usize - first.trip_count.leading_zeros() as usize).max(1);
        let initial_remaining = builder.alloc_bit(remaining_width, false);
        let zero = builder.alloc_bit(remaining_width, false);
        let one = builder.alloc_bit(remaining_width, false);
        let initial_loop_value = builder.alloc_bit(first.loop_width, first.loop_signed);
        let step_value = builder.alloc_bit(first.loop_width, first.loop_signed);
        let body_remaining = builder.alloc_bit(remaining_width, false);
        let body_loop_value = builder.alloc_bit(first.loop_width, first.loop_signed);
        let body_states: Vec<_> = specs
            .iter()
            .flat_map(|spec| spec.states)
            .map(|state| builder.alloc_logic(state.target.access.msb - state.target.access.lsb + 1))
            .collect();
        let exit_states: Vec<_> = specs
            .iter()
            .flat_map(|spec| spec.states)
            .map(|state| builder.alloc_logic(state.target.access.msb - state.target.access.lsb + 1))
            .collect();
        let body = builder.new_block_with(
            std::iter::once(body_remaining)
                .chain(std::iter::once(body_loop_value))
                .chain(body_states.iter().copied())
                .collect(),
        );
        let exit = builder.new_block_with(exit_states.clone());

        let capture_nodes = Self::fold_invariant_capture_frontier(specs, arena);
        let needs_capture_block = !capture_nodes.is_empty()
            && (!allow_cache || capture_nodes.iter().any(|node| !cache.contains_key(node)));
        let enter = needs_capture_block.then(|| builder.new_block());
        let emit_loop_setup = |builder: &mut SIRBuilder<A>| {
            builder.emit(SIRInstruction::Imm(
                initial_remaining,
                SIRValue::new(BigUint::from(first.trip_count)),
            ));
            builder.emit(SIRInstruction::Imm(zero, SIRValue::new(0u8)));
            builder.emit(SIRInstruction::Imm(one, SIRValue::new(1u8)));
            builder.emit(SIRInstruction::Imm(
                initial_loop_value,
                SIRValue::new(Self::bigint_payload(first.start, first.loop_width)),
            ));
            builder.emit(SIRInstruction::Imm(
                step_value,
                SIRValue::new(Self::bigint_payload(first.step, first.loop_width)),
            ));
        };
        let initial_body_args = || {
            std::iter::once(initial_remaining)
                .chain(std::iter::once(initial_loop_value))
                .chain(initial_states.iter().copied())
                .collect::<Vec<_>>()
        };
        let mut captured_values = crate::HashMap::default();
        if let Some(enter) = enter {
            builder.seal_block(SIRTerminator::Branch {
                cond: guard,
                true_block: (enter, Vec::new()),
                false_block: (exit, initial_states.clone()),
            });
            builder.switch_to_block(enter);
            let capture_transaction = self.cache_transaction();
            if allow_cache {
                for node in capture_nodes {
                    let value = cache.get(&node).copied().unwrap_or_else(|| {
                        self.lower_inner(builder, node, arena, cache, outer_env, true)
                    });
                    captured_values.insert(node, value);
                }
                self.rollback_cache(cache, capture_transaction);
            } else {
                let mut capture_cache = crate::HashMap::default();
                for node in capture_nodes {
                    let value =
                        self.lower_inner(builder, node, arena, &mut capture_cache, outer_env, true);
                    captured_values.insert(node, value);
                }
                self.rollback_cache(&mut capture_cache, capture_transaction);
            }
            emit_loop_setup(builder);
            builder.seal_block(SIRTerminator::Jump(body, initial_body_args()));
        } else {
            for node in capture_nodes {
                let value = *cache
                    .get(&node)
                    .expect("a capture without an enter block must already dominate the loop");
                captured_values.insert(node, value);
            }
            emit_loop_setup(builder);
            builder.seal_block(SIRTerminator::Branch {
                cond: guard,
                true_block: (body, initial_body_args()),
                false_block: (exit, initial_states.clone()),
            });
        }

        builder.switch_to_block(body);
        let mut env_inputs = crate::HashMap::default();
        for (state, value) in specs
            .iter()
            .flat_map(|spec| spec.states)
            .zip(body_states.iter().copied())
        {
            env_inputs.insert(state.target.clone(), value);
        }
        for spec in specs {
            env_inputs.insert(
                VarAtomBase::new(spec.loop_var.clone(), 0, first.loop_width - 1),
                body_loop_value,
            );
        }
        let env = LowerEnv {
            inputs: env_inputs,
            parent: outer_env,
        };
        let mut local_cache = captured_values;
        let local_cache_transaction = self.cache_transaction();
        let next_states: Vec<_> = specs
            .iter()
            .flat_map(|spec| spec.states)
            .map(|state| {
                // The loop body uses its own environment-scoped cache.  A
                // child that was materialized while lowering a guard or an
                // initial value is not reusable here: its value may depend on
                // the loop variable or an old carried state.  Rebuild the cost
                // model from the cache that lower_inner will actually use so
                // mux profitability and mandatory lazy Div/Rem lowering do not
                // mistake an unavailable outer value for a body-local one.
                self.reset_cost_cache(state.update, arena, &local_cache, true);
                let next = self.lower_inner(
                    builder,
                    state.update,
                    arena,
                    &mut local_cache,
                    Some(&env),
                    true,
                );
                self.cast_reg_width(
                    builder,
                    next,
                    state.target.access.msb - state.target.access.lsb + 1,
                )
            })
            .collect();
        // These entries are valid only under this loop body's state/counter
        // environment.  Keep the local CSE results, but remove their tracking
        // records before returning to the caller's global cache transaction.
        self.cache_insert_log
            .borrow_mut()
            .truncate(local_cache_transaction.node_insertions);
        self.region_slice_cache_insert_log
            .borrow_mut()
            .truncate(local_cache_transaction.region_slice_insertions);

        let next_remaining = builder.alloc_bit(remaining_width, false);
        builder.emit(SIRInstruction::Binary(
            next_remaining,
            body_remaining,
            BinaryOp::Sub,
            one,
        ));
        let has_more = builder.alloc_bit(1, false);
        builder.emit(SIRInstruction::Binary(
            has_more,
            next_remaining,
            BinaryOp::Ne,
            zero,
        ));
        // The final value of this addition is unobserved when `has_more` is
        // false. Computing it eagerly lets the conditional edge carry the next
        // loop parameters directly, avoiding an extra hot advance block and
        // unconditional jump on every taken iteration.
        let next_loop_value = builder.alloc_bit(first.loop_width, first.loop_signed);
        builder.emit(SIRInstruction::Binary(
            next_loop_value,
            body_loop_value,
            BinaryOp::Add,
            step_value,
        ));
        builder.seal_block(SIRTerminator::Branch {
            cond: has_more,
            true_block: (
                body,
                std::iter::once(next_remaining)
                    .chain(std::iter::once(next_loop_value))
                    .chain(next_states.iter().copied())
                    .collect(),
            ),
            false_block: (exit, next_states.clone()),
        });

        builder.switch_to_block(exit);
        let mut results = Vec::with_capacity(specs.len());
        for (range, initial) in group_ranges.iter().zip(initial_packed) {
            let candidate = self.pack_fold_group_states(builder, &exit_states[range.clone()]);
            if let Some(initial) = initial {
                let result = builder.alloc_logic(builder.register(&candidate).width());
                builder.emit(SIRInstruction::Mux(result, guard, candidate, initial));
                results.push(result);
            } else {
                results.push(candidate);
            }
        }
        results
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn lower_for_fold<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
        loop_var: &A,
        loop_width: usize,
        loop_signed: bool,
        start: &SLTLoopBound,
        end: &SLTLoopBound,
        inclusive: bool,
        step: usize,
        step_op: SLTStepOp,
        reverse: bool,
        result: &crate::SLTForFoldResult<A>,
        initials: &[crate::SLTForUpdate<A>],
        updates: &[crate::SLTForUpdate<A>],
        effects: &[crate::SLTForEffect],
        continue_cond: NodeId,
        parent_env: Option<&LowerEnv<'_, A>>,
    ) -> RegisterId {
        let mut counter_width = loop_width.max(1);
        counter_width = counter_width.max(Self::bound_width(start));
        counter_width = counter_width.max(Self::bound_width(end));
        if let SLTLoopBound::Expr(node) = start {
            counter_width = counter_width.max(self.get_width(*node, arena));
        }
        if let SLTLoopBound::Expr(node) = end {
            counter_width = counter_width.max(self.get_width(*node, arena));
        }

        let widen_inclusive = inclusive && !loop_signed;
        let compare_width = if widen_inclusive {
            counter_width + 1
        } else {
            counter_width
        };

        let start_reg = self.lower_bound(
            builder,
            start,
            loop_width,
            compare_width,
            loop_signed,
            arena,
            cache,
            parent_env,
        );
        let end_reg = self.lower_bound(
            builder,
            end,
            loop_width,
            compare_width,
            loop_signed,
            arena,
            cache,
            parent_env,
        );
        let one_reg = builder.alloc_bit(compare_width, loop_signed);
        builder.emit(SIRInstruction::Imm(one_reg, SIRValue::new(1u64)));
        let end_limit = if widen_inclusive {
            let reg = builder.alloc_bit(compare_width, loop_signed);
            builder.emit(SIRInstruction::Binary(reg, end_reg, BinaryOp::Add, one_reg));
            reg
        } else {
            end_reg
        };

        let init_source = if reverse && !inclusive {
            let reg = builder.alloc_bit(compare_width, loop_signed);
            builder.emit(SIRInstruction::Binary(reg, end_reg, BinaryOp::Sub, one_reg));
            reg
        } else if reverse {
            end_reg
        } else {
            start_reg
        };
        // SystemVerilog declares the induction variable as a fixed-width
        // `int`, so initialization is an assignment to that visible width.
        let init_visible = self.cast_reg_width_ext(builder, init_source, loop_width, loop_signed);
        let init_counter =
            self.cast_reg_width_ext(builder, init_visible, compare_width, loop_signed);

        let mut initial_states: Vec<RegisterId> = initials
            .iter()
            .zip(updates.iter())
            .map(|(init, update)| {
                let reg = self.lower_inner(
                    builder,
                    init.expr,
                    arena,
                    cache,
                    parent_env,
                    parent_env.is_none(),
                );
                let width = update.target.access.msb - update.target.access.lsb + 1;
                self.cast_reg_width(builder, reg, width)
            })
            .collect();
        if let crate::SLTForFoldResult::Transient { initial, update } = result {
            let reg = self.lower_inner(
                builder,
                *initial,
                arena,
                cache,
                parent_env,
                parent_env.is_none(),
            );
            initial_states.push(self.cast_reg_width(builder, reg, self.get_width(*update, arena)));
        }

        let transient_width = match result {
            crate::SLTForFoldResult::State(_) => None,
            crate::SLTForFoldResult::Transient { update, .. } => {
                Some(self.get_width(*update, arena))
            }
        };

        let header_counter = builder.alloc_bit(compare_width, loop_signed);
        let mut header_states: Vec<_> = updates
            .iter()
            .map(|update| {
                let width = update.target.access.msb - update.target.access.lsb + 1;
                builder.alloc_logic(width)
            })
            .collect();
        if let Some(width) = transient_width {
            header_states.push(builder.alloc_logic(width));
        }
        let body_counter = builder.alloc_bit(compare_width, loop_signed);
        let mut body_states: Vec<_> = updates
            .iter()
            .map(|update| {
                let width = update.target.access.msb - update.target.access.lsb + 1;
                builder.alloc_logic(width)
            })
            .collect();
        if let Some(width) = transient_width {
            body_states.push(builder.alloc_logic(width));
        }
        let mut exit_states: Vec<_> = updates
            .iter()
            .map(|update| {
                let width = update.target.access.msb - update.target.access.lsb + 1;
                builder.alloc_logic(width)
            })
            .collect();
        if let Some(width) = transient_width {
            exit_states.push(builder.alloc_logic(width));
        }

        let header_params = std::iter::once(header_counter)
            .chain(header_states.iter().copied())
            .collect();
        let body_params = std::iter::once(body_counter)
            .chain(body_states.iter().copied())
            .collect();
        let header_block = builder.new_block_with(header_params);
        let body_block = builder.new_block_with(body_params);
        let exit_block = builder.new_block_with(exit_states.clone());

        builder.seal_block(SIRTerminator::Jump(
            header_block,
            std::iter::once(init_counter)
                .chain(initial_states.iter().copied())
                .collect(),
        ));

        builder.switch_to_block(header_block);
        if reverse {
            let in_range = builder.alloc_bit(1, false);
            builder.emit(SIRInstruction::Binary(
                in_range,
                header_counter,
                if loop_signed {
                    BinaryOp::GeS
                } else {
                    BinaryOp::GeU
                },
                start_reg,
            ));
            builder.seal_block(SIRTerminator::Branch {
                cond: in_range,
                true_block: (
                    body_block,
                    std::iter::once(header_counter)
                        .chain(header_states.iter().copied())
                        .collect(),
                ),
                false_block: (exit_block, header_states.clone()),
            });
        } else {
            let cond = builder.alloc_bit(1, false);
            builder.emit(SIRInstruction::Binary(
                cond,
                header_counter,
                if loop_signed {
                    if inclusive {
                        BinaryOp::LeS
                    } else {
                        BinaryOp::LtS
                    }
                } else {
                    BinaryOp::LtU
                },
                end_limit,
            ));
            builder.seal_block(SIRTerminator::Branch {
                cond,
                true_block: (
                    body_block,
                    std::iter::once(header_counter)
                        .chain(header_states.iter().copied())
                        .collect(),
                ),
                false_block: (exit_block, header_states.clone()),
            });
        }

        builder.switch_to_block(body_block);
        let loop_value = body_counter;
        let loop_value_trunc =
            self.cast_reg_width_ext(builder, loop_value, loop_width, loop_signed);

        let mut env_inputs = crate::HashMap::default();
        for (update, state_reg) in updates.iter().zip(body_states.iter().copied()) {
            env_inputs.insert(update.target.clone(), state_reg);
        }
        env_inputs.insert(
            VarAtomBase::new(loop_var.clone(), 0, loop_width - 1),
            loop_value_trunc,
        );
        let env = LowerEnv {
            inputs: env_inputs,
            parent: parent_env,
        };
        let mut local_cache = crate::HashMap::default();
        self.lower_for_effects(builder, arena, &mut local_cache, &env, effects);
        let mut next_states: Vec<_> = updates
            .iter()
            .map(|update| {
                let reg = self.lower_inner(
                    builder,
                    update.expr,
                    arena,
                    &mut local_cache,
                    Some(&env),
                    false,
                );
                let width = update.target.access.msb - update.target.access.lsb + 1;
                self.cast_reg_width(builder, reg, width)
            })
            .collect();
        if let crate::SLTForFoldResult::Transient { update, .. } = result {
            let reg =
                self.lower_inner(builder, *update, arena, &mut local_cache, Some(&env), false);
            next_states.push(self.cast_reg_width(builder, reg, self.get_width(*update, arena)));
        }

        let continue_reg = self.lower_inner(
            builder,
            continue_cond,
            arena,
            &mut local_cache,
            Some(&env),
            false,
        );

        let progress_block = builder.new_block();
        builder.seal_block(SIRTerminator::Branch {
            cond: continue_reg,
            true_block: (progress_block, vec![]),
            false_block: (exit_block, next_states.clone()),
        });
        builder.switch_to_block(progress_block);

        if reverse {
            let reverse_width = Self::step_math_width(compare_width, SLTStepOp::Add, step);
            let current_math =
                self.cast_reg_width_ext(builder, body_counter, reverse_width, loop_signed);
            let start_math =
                self.cast_reg_width_ext(builder, start_reg, reverse_width, loop_signed);
            let reverse_step = builder.alloc_bit(reverse_width, loop_signed);
            builder.emit(SIRInstruction::Imm(
                reverse_step,
                SIRValue::new(step as u64),
            ));
            let next_raw = builder.alloc_bit(reverse_width, loop_signed);
            builder.emit(SIRInstruction::Binary(
                next_raw,
                current_math,
                BinaryOp::Sub,
                reverse_step,
            ));
            let next_visible = self.cast_reg_width_ext(builder, next_raw, loop_width, loop_signed);
            let next_math =
                self.cast_reg_width_ext(builder, next_visible, reverse_width, loop_signed);
            let decreasing = builder.alloc_bit(1, false);
            builder.emit(SIRInstruction::Binary(
                decreasing,
                next_math,
                if loop_signed {
                    BinaryOp::LtS
                } else {
                    BinaryOp::LtU
                },
                current_math,
            ));
            let range_check_block = builder.new_block();
            let stall_block = builder.new_block();
            builder.seal_block(SIRTerminator::Branch {
                cond: decreasing,
                true_block: (range_check_block, vec![]),
                false_block: (stall_block, vec![]),
            });
            builder.switch_to_block(range_check_block);
            let in_range = builder.alloc_bit(1, false);
            builder.emit(SIRInstruction::Binary(
                in_range,
                next_math,
                if loop_signed {
                    BinaryOp::GeS
                } else {
                    BinaryOp::GeU
                },
                start_math,
            ));
            let next_counter =
                self.cast_reg_width_ext(builder, next_math, compare_width, loop_signed);
            builder.seal_block(SIRTerminator::Branch {
                cond: in_range,
                true_block: (
                    header_block,
                    std::iter::once(next_counter)
                        .chain(next_states.iter().copied())
                        .collect(),
                ),
                false_block: (exit_block, next_states.clone()),
            });
            builder.switch_to_block(stall_block);
            builder.seal_block(SIRTerminator::Error(1));
        } else {
            let math_width = Self::step_math_width(compare_width, step_op, step);
            let step_width = if matches!(step_op, SLTStepOp::BitOr | SLTStepOp::BitXor) {
                loop_width
            } else {
                math_width
            };
            let current_step =
                self.cast_reg_width_ext(builder, body_counter, step_width, loop_signed);
            let step_reg = builder.alloc_bit(step_width, loop_signed);
            let step_value = Self::truncate_usize_to_width(step, step_width);
            builder.emit(SIRInstruction::Imm(
                step_reg,
                SIRValue::new(step_value as u64),
            ));
            let next_step = builder.alloc_bit(step_width, loop_signed);
            let op = match step_op {
                SLTStepOp::Add => BinaryOp::Add,
                SLTStepOp::Mul => BinaryOp::Mul,
                SLTStepOp::Shl => BinaryOp::Shl,
                SLTStepOp::BitOr => BinaryOp::Or,
                SLTStepOp::BitXor => BinaryOp::Xor,
            };
            builder.emit(SIRInstruction::Binary(
                next_step,
                current_step,
                op,
                step_reg,
            ));
            let current_math =
                self.cast_reg_width_ext(builder, current_step, math_width, loop_signed);
            // The emitted SystemVerilog loop variable has `loop_width` bits
            // (`int`/i32 for Veryl). Apply the compound assignment at that
            // width before checking progress or the loop bound; otherwise a
            // widened host counter can step through values the emitted loop
            // can never represent and incorrectly terminate.
            let next_visible = self.cast_reg_width_ext(builder, next_step, loop_width, loop_signed);
            let next_math = self.cast_reg_width_ext(builder, next_visible, math_width, loop_signed);

            let progress = builder.alloc_bit(1, false);
            builder.emit(SIRInstruction::Binary(
                progress,
                next_math,
                BinaryOp::Ne,
                current_math,
            ));
            let check_block = builder.new_block();
            let stall_block = builder.new_block();
            builder.seal_block(SIRTerminator::Branch {
                cond: progress,
                true_block: (check_block, vec![]),
                false_block: (stall_block, vec![]),
            });

            builder.switch_to_block(check_block);
            let increasing = builder.alloc_bit(1, false);
            builder.emit(SIRInstruction::Binary(
                increasing,
                next_math,
                if loop_signed {
                    BinaryOp::GtS
                } else {
                    BinaryOp::GtU
                },
                current_math,
            ));
            let range_check_block = builder.new_block();
            builder.seal_block(SIRTerminator::Branch {
                cond: increasing,
                true_block: (range_check_block, vec![]),
                false_block: (stall_block, vec![]),
            });

            builder.switch_to_block(range_check_block);
            let end_math = self.cast_reg_width_ext(builder, end_limit, math_width, loop_signed);
            let in_range = builder.alloc_bit(1, false);
            builder.emit(SIRInstruction::Binary(
                in_range,
                next_math,
                if loop_signed {
                    if inclusive {
                        BinaryOp::LeS
                    } else {
                        BinaryOp::LtS
                    }
                } else {
                    BinaryOp::LtU
                },
                end_math,
            ));
            let next_counter =
                self.cast_reg_width_ext(builder, next_math, compare_width, loop_signed);
            builder.seal_block(SIRTerminator::Branch {
                cond: in_range,
                true_block: (
                    header_block,
                    std::iter::once(next_counter)
                        .chain(next_states.iter().copied())
                        .collect(),
                ),
                false_block: (exit_block, next_states.clone()),
            });

            builder.switch_to_block(stall_block);
            builder.seal_block(SIRTerminator::Error(1));
        }

        builder.switch_to_block(exit_block);
        let result_idx = match result {
            crate::SLTForFoldResult::State(result) => updates
                .iter()
                .position(|update| update.target == *result)
                .expect("ForFold result target must be present in updates"),
            crate::SLTForFoldResult::Transient { .. } => updates.len(),
        };
        exit_states[result_idx]
    }

    fn lower_for_effects<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
        env: &LowerEnv<'_, A>,
        effects: &[crate::SLTForEffect],
    ) {
        for effect in effects {
            let crate::SLTForEffect::Event {
                site_id,
                guard,
                emit_on_true,
                args,
                fatal_error_code,
            } = effect
            else {
                let crate::SLTForEffect::Runner(runner) = effect else {
                    unreachable!()
                };
                self.lower_inner(builder, *runner, arena, cache, Some(env), false);
                continue;
            };
            let emit = |builder: &mut SIRBuilder<A>,
                        this: &Self,
                        cache: &mut crate::HashMap<NodeId, RegisterId>| {
                let args = args
                    .iter()
                    .map(|arg| this.lower_inner(builder, *arg, arena, cache, Some(env), false))
                    .collect();
                builder.emit(SIRInstruction::CombCaptureEvent {
                    site_id: *site_id,
                    args,
                    fatal_error_code: *fatal_error_code,
                    consume_enabled: false,
                });
            };
            if let Some(guard) = guard {
                let cond = self.lower_inner(builder, *guard, arena, cache, Some(env), false);
                let branch_cond = if *emit_on_true {
                    cond
                } else {
                    let inverted = builder.alloc_bit(1, false);
                    builder.emit(SIRInstruction::Unary(inverted, UnaryOp::LogicNot, cond));
                    inverted
                };
                let event_block = builder.new_block();
                let done_block = builder.new_block();
                builder.seal_block(SIRTerminator::Branch {
                    cond: branch_cond,
                    true_block: (event_block, vec![]),
                    false_block: (done_block, vec![]),
                });
                builder.switch_to_block(event_block);
                emit(builder, self, cache);
                builder.seal_block(SIRTerminator::Jump(done_block, vec![]));
                builder.switch_to_block(done_block);
            } else {
                emit(builder, self, cache);
            }
        }
    }
}
