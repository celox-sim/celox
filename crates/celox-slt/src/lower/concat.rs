//! Concatenation lowering and common-zero guard planning.

use super::*;

impl SLTToSIRLowerer {
    fn zero_required_from_both(
        lhs: &ZeroControllerFacts,
        rhs: &ZeroControllerFacts,
    ) -> ZeroControllerFacts {
        match (lhs.unconditional_zero, rhs.unconditional_zero) {
            (true, true) => ZeroControllerFacts {
                unconditional_zero: true,
                guards: crate::HashSet::default(),
            },
            (true, false) => rhs.clone(),
            (false, true) => lhs.clone(),
            (false, false) => {
                let (smaller, larger) = if lhs.guards.len() <= rhs.guards.len() {
                    (&lhs.guards, &rhs.guards)
                } else {
                    (&rhs.guards, &lhs.guards)
                };
                ZeroControllerFacts {
                    unconditional_zero: false,
                    guards: smaller
                        .iter()
                        .copied()
                        .filter(|guard| larger.contains(guard))
                        .collect(),
                }
            }
        }
    }

    fn zero_from_either(
        lhs: &ZeroControllerFacts,
        rhs: &ZeroControllerFacts,
    ) -> ZeroControllerFacts {
        if lhs.unconditional_zero || rhs.unconditional_zero {
            return ZeroControllerFacts {
                unconditional_zero: true,
                guards: crate::HashSet::default(),
            };
        }
        let mut guards = lhs.guards.clone();
        guards.extend(rhs.guards.iter().copied());
        ZeroControllerFacts {
            unconditional_zero: false,
            guards,
        }
    }

    /// Compute exact two-state zero controllers for the small algebra used by
    /// guarded lane vectors. A controller `g` is present only when assuming
    /// the one-bit value `g == 0` proves every bit of `node` is zero.
    fn zero_controller_facts<A: Hash + Eq + Clone + std::fmt::Debug>(
        &self,
        root: NodeId,
        arena: &SLTNodeArena<A>,
        memo: &mut crate::HashMap<NodeId, ZeroControllerFacts>,
    ) -> ZeroControllerFacts {
        if let Some(facts) = memo.get(&root) {
            return facts.clone();
        }

        // Explicit postorder avoids consuming the native stack on procedural
        // expression chains. Each node in this concat cone is analyzed once.
        let mut stack = vec![(root, false)];
        while let Some((node, expanded)) = stack.pop() {
            if memo.contains_key(&node) {
                continue;
            }
            if !expanded {
                stack.push((node, true));
                for child in Self::node_children(node, arena).into_iter().rev() {
                    if !memo.contains_key(&child) {
                        stack.push((child, false));
                    }
                }
                continue;
            }

            let child = |node: NodeId| {
                memo.get(&node)
                    .cloned()
                    .expect("zero-controller postorder must analyze children first")
            };
            let mut facts = match arena.get(node) {
                SLTNode::Constant(value, mask, _, _) => ZeroControllerFacts {
                    unconditional_zero: value.is_zero() && mask.is_zero(),
                    guards: crate::HashSet::default(),
                },
                SLTNode::Input { .. } => ZeroControllerFacts::default(),
                SLTNode::Binary(lhs, op, rhs) => {
                    let lhs = child(*lhs);
                    let rhs = child(*rhs);
                    match op {
                        BinaryOp::And | BinaryOp::LogicAnd | BinaryOp::Mul => {
                            Self::zero_from_either(&lhs, &rhs)
                        }
                        BinaryOp::Or
                        | BinaryOp::Xor
                        | BinaryOp::Add
                        | BinaryOp::Sub
                        | BinaryOp::LogicOr => Self::zero_required_from_both(&lhs, &rhs),
                        BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar => lhs,
                        BinaryOp::Eq
                        | BinaryOp::Ne
                        | BinaryOp::EqCase
                        | BinaryOp::NeCase
                        | BinaryOp::EqWildcard
                        | BinaryOp::NeWildcard
                        | BinaryOp::LtU
                        | BinaryOp::LtS
                        | BinaryOp::LeU
                        | BinaryOp::LeS
                        | BinaryOp::GtU
                        | BinaryOp::GtS
                        | BinaryOp::GeU
                        | BinaryOp::GeS
                        | BinaryOp::DivU
                        | BinaryOp::DivS
                        | BinaryOp::RemU
                        | BinaryOp::RemS => ZeroControllerFacts::default(),
                    }
                }
                SLTNode::Unary(op, inner) => match op {
                    UnaryOp::Ident
                    | UnaryOp::ToTwoState
                    | UnaryOp::Minus
                    | UnaryOp::And
                    | UnaryOp::Or
                    | UnaryOp::Xor
                    | UnaryOp::PopCount => child(*inner),
                    UnaryOp::LogicNot
                    | UnaryOp::BitNot
                    | UnaryOp::CountLeadingZeros
                    | UnaryOp::CountTrailingZeros => ZeroControllerFacts::default(),
                },
                SLTNode::Capture { expr, .. } => child(*expr),
                SLTNode::Slice { expr, .. } => child(*expr),
                SLTNode::Concat(parts) => {
                    let mut combined = ZeroControllerFacts {
                        unconditional_zero: true,
                        guards: crate::HashSet::default(),
                    };
                    for (part, _) in parts {
                        combined = Self::zero_required_from_both(&combined, &child(*part));
                    }
                    combined
                }
                SLTNode::Mux {
                    then_expr,
                    else_expr,
                    ..
                } => Self::zero_required_from_both(&child(*then_expr), &child(*else_expr)),
                SLTNode::ForFold { .. } | SLTNode::ForFoldGroup { .. } => {
                    ZeroControllerFacts::default()
                }
            };

            let shared = self.cost_cache.borrow().fanout[node.0] > 1;
            if !facts.unconditional_zero
                && shared
                && self.get_width(node, arena) == 1
                && !matches!(arena.get(node), SLTNode::Constant(..))
            {
                // This shared compound value covers every zero case of its
                // descendant controllers and possibly more. Keeping only the
                // maximal value prevents a leaf from winning on a tiny local
                // cost difference and keeps deep conjunction sets linear.
                facts.guards.clear();
                facts.guards.insert(node);
            }
            memo.insert(node, facts);
        }

        memo.get(&root)
            .cloned()
            .expect("zero-controller root must be produced by its postorder")
    }

    fn guarded_concat_root_is_supported<A: Hash + Eq + Clone>(
        root: NodeId,
        arena: &SLTNodeArena<A>,
    ) -> bool {
        let mut visited = crate::HashSet::default();
        let mut work = vec![root];
        while let Some(node) = work.pop() {
            if !visited.insert(node) {
                continue;
            }
            match arena.get(node) {
                SLTNode::Binary(
                    _,
                    BinaryOp::DivU | BinaryOp::DivS | BinaryOp::RemU | BinaryOp::RemS,
                    _,
                )
                | SLTNode::ForFold { .. }
                | SLTNode::ForFoldGroup { .. } => return false,
                _ => work.extend(Self::node_children(node, arena)),
            }
        }
        true
    }

    fn guarded_concat_region_cost<A: Hash + Eq + Clone + std::fmt::Debug>(
        &self,
        root: NodeId,
        guard: NodeId,
        arena: &SLTNodeArena<A>,
        materialized: &crate::HashMap<NodeId, RegisterId>,
    ) -> (u128, u128) {
        let mut guard_closure = crate::HashSet::default();
        let mut guard_work = vec![guard];
        while let Some(node) = guard_work.pop() {
            if guard_closure.insert(node) {
                guard_work.extend(Self::node_children(node, arena));
            }
        }

        let mut visited = crate::HashSet::default();
        let mut live_through = crate::HashSet::default();
        let mut work = vec![root];
        let mut cost = 0u128;
        while let Some(node) = work.pop() {
            if !visited.insert(node) {
                continue;
            }
            // The guard and its dependencies are evaluated in the dominator.
            // If one is also reached outside the guard expression, the true
            // arm consumes that already-materialized value as a live-through.
            if guard_closure.contains(&node) || materialized.contains_key(&node) {
                live_through.insert(node);
                continue;
            }

            cost = cost.saturating_add(self.intrinsic_node_cost(node, arena));
            // A nested Mux may already lower to control flow. Counting only
            // the Mux itself and none of its condition/arms is a conservative
            // lower bound on work skipped by the new outer branch.
            if !matches!(arena.get(node), SLTNode::Mux { .. }) {
                work.extend(Self::node_children(node, arena));
            }
        }
        let live_through_cost = live_through
            .into_iter()
            .map(|node| Self::chunks(self.get_width(node, arena)))
            .fold(0u128, u128::saturating_add);
        (cost, live_through_cost)
    }

    fn guarded_concat_net_benefit<A: Hash + Eq + Clone + std::fmt::Debug>(
        &self,
        root: NodeId,
        guard: NodeId,
        arena: &SLTNodeArena<A>,
        materialized: &crate::HashMap<NodeId, RegisterId>,
    ) -> Option<u128> {
        const CONTROL_COST: u128 = 3;
        const MISPREDICT_COST: u128 = 16;
        const PHI_COPY_COST_PER_CHUNK: u128 = 2;
        const LIVE_THROUGH_COST_PER_CHUNK: u128 = 1;

        let probability = Self::guarded_true_probability(guard, arena);
        let false_weight = probability.total_weight - probability.true_weight;
        let (skippable_cost, live_through_chunks) =
            self.guarded_concat_region_cost(root, guard, arena, materialized);
        let result_chunks = Self::chunks(self.get_width(root, arena));
        let saved_scaled = false_weight.saturating_mul(skippable_cost);
        let introduced_scaled = probability
            .total_weight
            .saturating_mul(
                CONTROL_COST
                    .saturating_add(result_chunks.saturating_mul(PHI_COPY_COST_PER_CHUNK))
                    .saturating_add(
                        live_through_chunks.saturating_mul(LIVE_THROUGH_COST_PER_CHUNK),
                    ),
            )
            .saturating_add(false_weight.saturating_mul(result_chunks))
            .saturating_add(
                probability
                    .true_weight
                    .min(false_weight)
                    .saturating_mul(MISPREDICT_COST),
            );
        (saved_scaled > introduced_scaled).then(|| saved_scaled - introduced_scaled)
    }

    pub(super) fn guarded_concat_plan<A: Hash + Eq + Clone + std::fmt::Debug>(
        &self,
        root: NodeId,
        arena: &SLTNodeArena<A>,
        materialized: &crate::HashMap<NodeId, RegisterId>,
    ) -> Option<GuardedConcatPlan> {
        if self.four_state || !Self::guarded_concat_root_is_supported(root, arena) {
            return None;
        }

        let mut memo = crate::HashMap::default();
        let facts = self.zero_controller_facts(root, arena, &mut memo);
        if facts.unconditional_zero {
            return None;
        }
        let mut guards = facts.guards.into_iter().collect::<Vec<_>>();
        guards.sort_unstable();

        let mut best = None;
        for guard in guards {
            if guard == root || self.get_width(guard, arena) != 1 {
                continue;
            }
            let Some(net_benefit_scaled) =
                self.guarded_concat_net_benefit(root, guard, arena, materialized)
            else {
                continue;
            };
            let candidate = GuardedConcatPlan {
                guard,
                net_benefit_scaled,
            };
            let replace = best.as_ref().is_none_or(|current: &GuardedConcatPlan| {
                candidate.net_benefit_scaled > current.net_benefit_scaled
                    || candidate.net_benefit_scaled == current.net_benefit_scaled
                        && candidate.guard < current.guard
            });
            if replace {
                best = Some(candidate);
            }
        }
        best
    }

    #[allow(clippy::too_many_arguments)]
    fn lower_guarded_concat_cfg<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        plan: GuardedConcatPlan,
        parts: &[(NodeId, usize)],
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
    ) -> RegisterId {
        let guard = self.lower_inner(builder, plan.guard, arena, cache, None, true);
        let width = parts.iter().map(|(_, width)| *width).sum();
        let result = builder.alloc_logic(width);
        let true_block = builder.new_block();
        let false_block = builder.new_block();
        let merge_block = builder.new_block_with(vec![result]);
        builder.seal_block(SIRTerminator::Branch {
            cond: guard,
            true_block: (true_block, Vec::new()),
            false_block: (false_block, Vec::new()),
        });

        let true_transaction = self.cache_transaction();
        builder.switch_to_block(true_block);
        let true_value = self.lower_concat_eager_inner(builder, parts, arena, cache, None, true);
        builder.seal_block(SIRTerminator::Jump(merge_block, vec![true_value]));
        self.rollback_cache(cache, true_transaction);

        builder.switch_to_block(false_block);
        let zero = builder.alloc_logic(width);
        builder.emit(SIRInstruction::Imm(zero, SIRValue::new(0u64)));
        builder.seal_block(SIRTerminator::Jump(merge_block, vec![zero]));

        builder.switch_to_block(merge_block);
        result
    }

    pub(super) fn lower_concat_inner<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        node: NodeId,
        parts: &[(NodeId, usize)],
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
        env: Option<&LowerEnv<'_, A>>,
        allow_cache: bool,
    ) -> RegisterId {
        // Fast path: if all parts are constants, fold into a single wide Imm.
        if env.is_none()
            && let Some(reg) = self.try_fold_const_concat(builder, parts, arena)
        {
            return reg;
        }

        if env.is_none()
            && allow_cache
            && let Some(plan) = self.guarded_concat_plan(node, arena, cache)
        {
            return self.lower_guarded_concat_cfg(builder, plan, parts, arena, cache);
        }

        self.lower_concat_eager_inner(builder, parts, arena, cache, env, allow_cache)
    }

    fn lower_concat_eager_inner<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        parts: &[(NodeId, usize)],
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
        env: Option<&LowerEnv<'_, A>>,
        allow_cache: bool,
    ) -> RegisterId {
        // Use SIR Concat instruction directly. This preserves Z bits in 4-state
        // mode (unlike the Shl+Or pattern which converts Z to X through Binary Or
        // normalization). Concat args are [MSB, ..., LSB] — same order as `parts`.
        let total_width: usize = parts.iter().map(|(_, w)| w).sum();
        let part_regs: Vec<RegisterId> = parts
            .iter()
            .map(|(node, width)| {
                let reg = self.lower_inner(builder, *node, arena, cache, env, allow_cache);
                self.cast_reg_width(builder, reg, *width)
            })
            .collect();
        let result = builder.alloc_logic(total_width);
        builder.emit(SIRInstruction::Concat(result, part_regs));
        result
    }

    /// Try to fold a Concat of all-constant parts into a single wide Imm.
    /// Recursively evaluates each part to check if it's a compile-time constant.
    fn try_fold_const_concat<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        parts: &[(NodeId, usize)],
        arena: &SLTNodeArena<A>,
    ) -> Option<RegisterId> {
        let mut const_parts: Vec<(BigUint, BigUint, usize)> = Vec::with_capacity(parts.len());
        for (node_id, width) in parts {
            let (val, mask) = try_const_eval(*node_id, arena)?;
            const_parts.push((val, mask, *width));
        }

        // Build the combined value and mask (parts are MSB-first, reverse for LSB-first).
        let mut combined_val = BigUint::from(0u32);
        let mut combined_mask = BigUint::from(0u32);
        let mut total_width = 0usize;
        for (val, mask, width) in const_parts.iter().rev() {
            let width_mask = if *width >= 64 {
                (BigUint::from(1u64) << width) - 1u64
            } else {
                BigUint::from((1u64 << width) - 1)
            };
            combined_val |= (val & &width_mask) << total_width;
            combined_mask |= (mask & &width_mask) << total_width;
            total_width += *width;
        }

        let reg = if combined_mask.is_zero() {
            builder.alloc_bit(total_width, false)
        } else {
            builder.alloc_logic(total_width)
        };
        builder.emit(SIRInstruction::Imm(
            reg,
            SIRValue::new_four_state(combined_val, combined_mask),
        ));
        Some(reg)
    }
}
