//! Mux profitability, branch planning, shared-value hoisting, and lowering.

use super::*;

impl SLTToSIRLowerer {
    pub(super) fn contains_div_rem<A: Hash + Eq + Clone + std::fmt::Debug>(
        &self,
        node: NodeId,
        arena: &SLTNodeArena<A>,
    ) -> bool {
        self.prepare_cost_cache(arena);
        if let Some(result) = self.cost_cache.borrow().contains_div_rem[node.0] {
            return result;
        }
        self.note_analysis_visits(1);
        let excluded = {
            let cache = self.cost_cache.borrow();
            cache.initially_materialized[node.0] || cache.fanout[node.0] > 1
        };
        let result = !excluded
            && (matches!(
                arena.get(node),
                SLTNode::Binary(
                    _,
                    BinaryOp::DivU | BinaryOp::DivS | BinaryOp::RemU | BinaryOp::RemS,
                    _,
                )
            ) || Self::node_children(node, arena)
                .into_iter()
                .any(|child| self.contains_div_rem(child, arena)));
        let mut cache = self.cost_cache.borrow_mut();
        cache.touch(node);
        cache.contains_div_rem[node.0] = Some(result);
        result
    }

    pub(super) fn static_true_probability<A: Hash + Eq + Clone>(
        cond: NodeId,
        arena: &SLTNodeArena<A>,
    ) -> StaticBranchProbability {
        match arena.get(cond) {
            SLTNode::Unary(UnaryOp::LogicNot, inner) => {
                Self::static_true_probability(*inner, arena).inverted()
            }
            SLTNode::Unary(UnaryOp::Ident, inner) => Self::static_true_probability(*inner, arena),
            SLTNode::Binary(
                lhs,
                op @ (BinaryOp::Eq | BinaryOp::Ne | BinaryOp::EqWildcard | BinaryOp::NeWildcard),
                rhs,
            ) if try_const_eval(*lhs, arena).is_some() || try_const_eval(*rhs, arena).is_some() => {
                // Ball and Larus, "Branch Prediction for Free" (PLDI 1993),
                // predict equality-to-constant tests false.  Their complete
                // static heuristic reports a 20% average miss rate; use that
                // measured uncertainty as the 20/80 local prior.  This affects
                // expected executed cost, never whether analysis is allowed to
                // stop or how large a CFG may become.
                let equality = StaticBranchProbability {
                    true_weight: 1,
                    total_weight: 5,
                };
                if matches!(*op, BinaryOp::Eq | BinaryOp::EqWildcard) {
                    equality
                } else {
                    equality.inverted()
                }
            }
            _ => StaticBranchProbability::EVEN,
        }
    }

    pub(super) fn guarded_true_probability<A: Hash + Eq + Clone>(
        guard: NodeId,
        arena: &SLTNodeArena<A>,
    ) -> StaticBranchProbability {
        let mut probability = StaticBranchProbability {
            true_weight: 1,
            total_weight: 1,
        };
        let mut visited = crate::HashSet::default();
        let mut work = vec![guard];
        while let Some(node) = work.pop() {
            if !visited.insert(node) {
                continue;
            }
            match arena.get(node) {
                SLTNode::Binary(lhs, BinaryOp::LogicAnd, rhs) => {
                    work.extend([*lhs, *rhs]);
                }
                SLTNode::Unary(UnaryOp::Ident, inner) => work.push(*inner),
                _ => {
                    probability =
                        probability.conjunction(Self::static_true_probability(node, arena));
                }
            }
        }
        probability
    }

    pub(super) fn mux_cfg_is_profitable(
        then_cost: u128,
        else_cost: u128,
        result_width: usize,
        probability: StaticBranchProbability,
    ) -> bool {
        Self::mux_cfg_is_profitable_with_extra_cost(
            then_cost,
            else_cost,
            result_width,
            probability,
            0,
        )
    }

    fn mux_cfg_is_profitable_with_extra_cost(
        then_cost: u128,
        else_cost: u128,
        result_width: usize,
        probability: StaticBranchProbability,
        extra_always_executed_cost: u128,
    ) -> bool {
        // Native and Cranelift both pay for a conditional transfer, the taken
        // arm's merge transfer, and a result phi copy.  With no dynamic profile,
        // predict the more likely edge and charge a 16-cycle x86 branch miss on
        // the less likely edge.  All terms are scaled by total_weight, so this
        // remains exact integer expected-cost arithmetic.
        const CONTROL_COST: u128 = 3;
        const MISPREDICT_COST: u128 = 16;
        const PHI_COPY_COST_PER_CHUNK: u128 = 2;

        let false_weight = probability.total_weight - probability.true_weight;
        let select_cost = Self::chunks(result_width);
        let skipped_cost = false_weight
            .saturating_mul(then_cost)
            .saturating_add(probability.true_weight.saturating_mul(else_cost))
            .saturating_add(probability.total_weight.saturating_mul(select_cost));
        let predictable_misses = probability.true_weight.min(false_weight);
        let introduced_cost = probability
            .total_weight
            .saturating_mul(
                CONTROL_COST
                    .saturating_add(
                        PHI_COPY_COST_PER_CHUNK.saturating_mul(Self::chunks(result_width)),
                    )
                    .saturating_add(extra_always_executed_cost),
            )
            .saturating_add(predictable_misses.saturating_mul(MISPREDICT_COST));
        skipped_cost > introduced_cost
    }

    fn mux_cfg_plan<A: Hash + Eq + Clone + std::fmt::Debug>(
        &self,
        cond: NodeId,
        then_expr: NodeId,
        else_expr: NodeId,
        result_width: usize,
        arena: &SLTNodeArena<A>,
        materialized: &crate::HashMap<NodeId, RegisterId>,
        allow_cache: bool,
    ) -> Option<MuxCfgPlan> {
        let empty_materialized = crate::HashMap::default();
        let materialized = if allow_cache {
            materialized
        } else {
            &empty_materialized
        };
        // Control flow selects one arm, while a four-state Mux bitwise-merges
        // both arms for X/Z conditions. No expression shape may bypass this
        // semantic policy.
        if self.four_state {
            self.with_mux_stats(|stats| stats.kept_four_state += 1);
            return None;
        }
        if !self.is_speculatable_pure(then_expr, arena)
            || !self.is_speculatable_pure(else_expr, arena)
        {
            self.with_mux_stats(|stats| stats.kept_impure += 1);
            return None;
        }
        let forced =
            self.contains_div_rem(then_expr, arena) || self.contains_div_rem(else_expr, arena);
        if !forced && !allow_cache {
            self.with_mux_stats(|stats| stats.kept_dynamic_env += 1);
            return None;
        }

        let probability = Self::static_true_probability(cond, arena);
        let then_cost = self.owned_tree_cost(then_expr, arena);
        let else_cost = self.owned_tree_cost(else_expr, arena);
        self.with_mux_stats(|stats| {
            stats.record_cost(then_cost, else_cost);
            stats.biased_conditions += usize::from(probability != StaticBranchProbability::EVEN);
        });
        if !forced && !Self::mux_cfg_is_profitable(then_cost, else_cost, result_width, probability)
        {
            self.with_mux_stats(|stats| stats.record_unprofitable(then_cost, else_cost));
            return None;
        }

        let shared_nodes = match self.shared_mux_nodes(then_expr, else_expr, arena, materialized) {
            Some(shared) => shared,
            None if forced => Vec::new(),
            None => {
                self.with_mux_stats(|stats| stats.kept_deep_shared += 1);
                return None;
            }
        };
        self.with_mux_stats(|stats| {
            if forced {
                stats.cfg_div_rem += 1;
            } else {
                stats.cfg_cost += 1;
            }
        });
        Some(MuxCfgPlan { shared_nodes })
    }

    fn mux_slice_cfg_plan<A: Hash + Eq + Clone + std::fmt::Debug>(
        &self,
        cond: NodeId,
        then_expr: NodeId,
        else_expr: NodeId,
        access: &BitAccess,
        arena: &SLTNodeArena<A>,
        materialized: &crate::HashMap<NodeId, RegisterId>,
        allow_cache: bool,
    ) -> Option<MuxCfgPlan> {
        if self.four_state {
            self.with_mux_stats(|stats| stats.kept_four_state += 1);
            return None;
        }
        if !self.is_speculatable_pure(then_expr, arena)
            || !self.is_speculatable_pure(else_expr, arena)
        {
            self.with_mux_stats(|stats| stats.kept_impure += 1);
            return None;
        }
        let empty_materialized = crate::HashMap::default();
        let materialized = if allow_cache {
            materialized
        } else {
            &empty_materialized
        };
        let forced =
            self.contains_div_rem(then_expr, arena) || self.contains_div_rem(else_expr, arena);
        let shared_nodes = match self.shared_mux_nodes(then_expr, else_expr, arena, materialized) {
            Some(shared) => shared,
            None if forced => Vec::new(),
            None => {
                self.with_mux_stats(|stats| stats.kept_deep_shared += 1);
                return None;
            }
        };
        if !forced {
            let then_cost = self.owned_slice_lower_cost(then_expr, arena);
            let else_cost = self.owned_slice_lower_cost(else_expr, arena);
            let probability = Self::static_true_probability(cond, arena);
            self.with_mux_stats(|stats| {
                stats.record_cost(then_cost, else_cost);
                stats.biased_conditions +=
                    usize::from(probability != StaticBranchProbability::EVEN);
            });
            // Slice lowering can be cheaper than computing the corresponding
            // full shared node.  Charge the entire full hoist as additional
            // always-executed work; this deliberately underestimates the
            // transformation's benefit and prevents optimistic branchification.
            let shared_hoist_cost = shared_nodes
                .iter()
                .map(|node| self.estimated_tree_cost(*node, arena))
                .fold(0u128, u128::saturating_add);
            if !Self::mux_cfg_is_profitable_with_extra_cost(
                then_cost,
                else_cost,
                access.msb - access.lsb + 1,
                probability,
                shared_hoist_cost,
            ) {
                self.with_mux_stats(|stats| stats.record_unprofitable(then_cost, else_cost));
                return None;
            }
        }
        self.with_mux_stats(|stats| {
            if forced {
                stats.cfg_slice_div_rem += 1;
            } else {
                stats.cfg_slice_cost += 1;
            }
        });
        Some(MuxCfgPlan { shared_nodes })
    }

    fn constant_condition<A: Hash + Eq + Clone>(
        cond: NodeId,
        arena: &SLTNodeArena<A>,
    ) -> Option<bool> {
        let (value, mask) = try_const_eval(cond, arena)?;
        (mask == BigUint::from(0u8)).then(|| value != BigUint::from(0u8))
    }

    fn hoist_shared_mux_nodes<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        plan: &MuxCfgPlan,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
        env: Option<&LowerEnv<'_, A>>,
        allow_cache: bool,
    ) {
        if !allow_cache {
            return;
        }
        self.with_mux_stats(|stats| stats.shared_nodes_hoisted += plan.shared_nodes.len());
        for &node in &plan.shared_nodes {
            self.lower_inner(builder, node, arena, cache, env, true);
        }
    }

    /// Cost-directed reverse if-conversion for symbolic expression DAGs.
    ///
    /// Cheap pure muxes remain `SIRInstruction::Mux`.  When the expected work
    /// skipped by preserving control exceeds branch, prediction, and phi-copy
    /// costs, the arms are lowered into separate CFG blocks.  Division and
    /// remainder remain a correctness case: an unselected zero divisor must
    /// never reach a native divide instruction.
    pub(super) fn lower_mux_inner<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        cond: NodeId,
        then_expr: NodeId,
        else_expr: NodeId,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
        env: Option<&LowerEnv<'_, A>>,
        allow_cache: bool,
    ) -> RegisterId {
        self.with_mux_stats(|stats| stats.normal_seen += 1);
        let then_width = self.get_width(then_expr, arena);
        let else_width = self.get_width(else_expr, arena);
        let res_width = then_width.max(else_width);

        if let Some(take_then) = Self::constant_condition(cond, arena) {
            self.with_mux_stats(|stats| stats.constant_folded += 1);
            let selected = if take_then { then_expr } else { else_expr };
            let value = self.lower_inner(builder, selected, arena, cache, env, allow_cache);
            return self.cast_reg_width(builder, value, res_width);
        }

        let cond_reg = self.lower_inner(builder, cond, arena, cache, env, allow_cache);
        if let Some(plan) = self.mux_cfg_plan(
            cond,
            then_expr,
            else_expr,
            res_width,
            arena,
            cache,
            allow_cache,
        ) {
            self.hoist_shared_mux_nodes(builder, &plan, arena, cache, env, allow_cache);
            return self.lower_mux_cfg(
                builder,
                cond_reg,
                then_expr,
                else_expr,
                res_width,
                arena,
                cache,
                env,
                allow_cache,
            );
        }

        let then_val = self.lower_inner(builder, then_expr, arena, cache, env, allow_cache);
        let else_val = self.lower_inner(builder, else_expr, arena, cache, env, allow_cache);

        // Use Mux instruction: preserves Z in 4-state, branchless select in 2-state.
        // Backends handle value and mask selection independently.
        let result = builder.alloc_logic(res_width);
        builder.emit(SIRInstruction::Mux(result, cond_reg, then_val, else_val));

        result
    }

    #[allow(clippy::too_many_arguments)]
    fn lower_mux_cfg<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        cond_reg: RegisterId,
        then_expr: NodeId,
        else_expr: NodeId,
        result_width: usize,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
        env: Option<&LowerEnv<'_, A>>,
        allow_cache: bool,
    ) -> RegisterId {
        let result = builder.alloc_logic(result_width);
        let then_block = builder.new_block();
        let else_block = builder.new_block();
        let merge_block = builder.new_block_with(vec![result]);

        builder.seal_block(SIRTerminator::Branch {
            cond: cond_reg,
            true_block: (then_block, vec![]),
            false_block: (else_block, vec![]),
        });

        let then_transaction = self.cache_transaction();
        builder.switch_to_block(then_block);
        let then_val = self.lower_inner(builder, then_expr, arena, cache, env, allow_cache);
        let then_val = self.cast_reg_width(builder, then_val, result_width);
        builder.seal_block(SIRTerminator::Jump(merge_block, vec![then_val]));
        if allow_cache {
            self.rollback_cache(cache, then_transaction);
        }

        let else_transaction = self.cache_transaction();
        builder.switch_to_block(else_block);
        let else_val = self.lower_inner(builder, else_expr, arena, cache, env, allow_cache);
        let else_val = self.cast_reg_width(builder, else_val, result_width);
        builder.seal_block(SIRTerminator::Jump(merge_block, vec![else_val]));
        if allow_cache {
            self.rollback_cache(cache, else_transaction);
        }

        builder.switch_to_block(merge_block);
        result
    }

    pub(super) fn lower_region_slice_mux_inner<
        A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display,
    >(
        &self,
        builder: &mut SIRBuilder<A>,
        cond: NodeId,
        then_expr: NodeId,
        else_expr: NodeId,
        access: &BitAccess,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
        env: Option<&LowerEnv<'_, A>>,
        allow_cache: bool,
    ) -> RegisterId {
        self.with_mux_stats(|stats| stats.slice_seen += 1);
        let result_width = access.msb - access.lsb + 1;
        if let Some(take_then) = Self::constant_condition(cond, arena) {
            self.with_mux_stats(|stats| stats.constant_folded += 1);
            return self.lower_region_slice_inner(
                builder,
                if take_then { then_expr } else { else_expr },
                access,
                arena,
                cache,
                env,
                allow_cache,
            );
        }

        let cond_reg = self.lower_inner(builder, cond, arena, cache, env, allow_cache);
        if let Some(plan) = self.mux_slice_cfg_plan(
            cond,
            then_expr,
            else_expr,
            access,
            arena,
            cache,
            allow_cache,
        ) {
            self.hoist_shared_mux_nodes(builder, &plan, arena, cache, env, allow_cache);
            let result = builder.alloc_logic(result_width);
            let then_block = builder.new_block();
            let else_block = builder.new_block();
            let merge_block = builder.new_block_with(vec![result]);

            builder.seal_block(SIRTerminator::Branch {
                cond: cond_reg,
                true_block: (then_block, vec![]),
                false_block: (else_block, vec![]),
            });

            let then_transaction = self.cache_transaction();
            builder.switch_to_block(then_block);
            let then_value = self.lower_region_slice_inner(
                builder,
                then_expr,
                access,
                arena,
                cache,
                env,
                allow_cache,
            );
            builder.seal_block(SIRTerminator::Jump(merge_block, vec![then_value]));
            if allow_cache {
                self.rollback_cache(cache, then_transaction);
            }

            let else_transaction = self.cache_transaction();
            builder.switch_to_block(else_block);
            let else_value = self.lower_region_slice_inner(
                builder,
                else_expr,
                access,
                arena,
                cache,
                env,
                allow_cache,
            );
            builder.seal_block(SIRTerminator::Jump(merge_block, vec![else_value]));
            if allow_cache {
                self.rollback_cache(cache, else_transaction);
            }

            builder.switch_to_block(merge_block);
            return result;
        }

        let then_value = self.lower_region_slice_inner(
            builder,
            then_expr,
            access,
            arena,
            cache,
            env,
            allow_cache,
        );
        let else_value = self.lower_region_slice_inner(
            builder,
            else_expr,
            access,
            arena,
            cache,
            env,
            allow_cache,
        );
        let result = builder.alloc_logic(result_width);
        builder.emit(SIRInstruction::Mux(
            result, cond_reg, then_value, else_value,
        ));
        result
    }
}
