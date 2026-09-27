//! Lowering cost caches, traversal, ownership, and speculation analysis.

use super::*;

impl SLTToSIRLowerer {
    pub(super) fn reset_cost_cache<A: Hash + Eq + Clone>(
        &self,
        root: NodeId,
        arena: &SLTNodeArena<A>,
        materialized: &crate::HashMap<NodeId, RegisterId>,
        honor_materialized: bool,
    ) {
        self.reset_cost_cache_roots(
            std::slice::from_ref(&root),
            arena,
            materialized,
            honor_materialized,
        );
    }

    pub(super) fn reset_cost_cache_roots<A: Hash + Eq + Clone>(
        &self,
        roots: &[NodeId],
        arena: &SLTNodeArena<A>,
        materialized: &crate::HashMap<NodeId, RegisterId>,
        honor_materialized: bool,
    ) {
        let node_count = arena.len();
        let mut cache = self.cost_cache.borrow_mut();
        cache.reset(node_count);
        cache.traversal_work.clear();
        cache.traversal_work.extend_from_slice(roots);
        #[cfg(test)]
        {
            cache.analysis_node_visits = 0;
        }

        while let Some(node) = cache.traversal_work.pop() {
            if cache.traversal_seen[node.0] {
                continue;
            }
            cache.touch(node);
            cache.traversal_seen[node.0] = true;
            #[cfg(test)]
            {
                cache.analysis_node_visits += 1;
            }
            if honor_materialized && materialized.contains_key(&node) {
                cache.initially_materialized[node.0] = true;
                continue;
            }
            for child in Self::node_children(node, arena) {
                cache.touch(child);
                cache.fanout[child.0] = cache.fanout[child.0].saturating_add(1);
                cache.traversal_work.push(child);
            }
        }
        self.cache_insert_log.borrow_mut().clear();
        self.region_slice_cache.borrow_mut().clear();
        self.region_slice_cache_insert_log.borrow_mut().clear();
    }

    pub(super) fn cache_transaction(&self) -> LowerCacheTransaction {
        LowerCacheTransaction {
            node_insertions: self.cache_insert_log.borrow().len(),
            region_slice_insertions: self.region_slice_cache_insert_log.borrow().len(),
        }
    }

    #[cfg(test)]
    pub(super) fn note_analysis_visits(&self, visits: usize) {
        let mut cache = self.cost_cache.borrow_mut();
        cache.analysis_node_visits = cache.analysis_node_visits.saturating_add(visits);
    }

    #[cfg(not(test))]
    #[inline(always)]
    pub(super) fn note_analysis_visits(&self, _visits: usize) {}

    #[cfg(test)]
    pub(super) fn analysis_node_visits(&self) -> usize {
        self.cost_cache.borrow().analysis_node_visits
    }

    pub(super) fn rollback_cache(
        &self,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
        transaction: LowerCacheTransaction,
    ) {
        let mut log = self.cache_insert_log.borrow_mut();
        for node in log.drain(transaction.node_insertions..) {
            cache.remove(&node);
        }
        let mut region_log = self.region_slice_cache_insert_log.borrow_mut();
        let mut region_cache = self.region_slice_cache.borrow_mut();
        for key in region_log.drain(transaction.region_slice_insertions..) {
            region_cache.remove(&key);
        }
    }

    /// Return the cache entries created by the most recent top-level `lower`
    /// call. A scheduler-owned control arm keeps them available to subsequent
    /// paths in that arm, then removes them before lowering the sibling arm.
    pub(crate) fn take_scheduled_region_insertions(&self) -> Vec<NodeId> {
        std::mem::take(&mut *self.cache_insert_log.borrow_mut())
    }

    pub(super) fn prepare_cost_cache<A: Hash + Eq + Clone>(&self, arena: &SLTNodeArena<A>) {
        let mut cache = self.cost_cache.borrow_mut();
        if cache.tree_costs.len() < arena.len() {
            cache.resize(arena.len());
        }
    }

    pub(super) fn node_children<A: Hash + Eq + Clone>(
        node: NodeId,
        arena: &SLTNodeArena<A>,
    ) -> Vec<NodeId> {
        match arena.get(node) {
            SLTNode::Input { index, .. } => index.iter().map(|entry| entry.node).collect(),
            SLTNode::Constant(..) => Vec::new(),
            SLTNode::Binary(lhs, _, rhs) => vec![*lhs, *rhs],
            SLTNode::Unary(_, inner) => vec![*inner],
            SLTNode::Capture { expr, .. } => vec![*expr],
            SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } => vec![*cond, *then_expr, *else_expr],
            SLTNode::Concat(parts) => parts.iter().map(|(part, _)| *part).collect(),
            SLTNode::Slice { expr, .. } => vec![*expr],
            SLTNode::ForFold {
                start,
                end,
                result,
                initials,
                updates,
                effects,
                continue_cond,
                ..
            } => {
                let mut children = Vec::new();
                if let SLTLoopBound::Expr(node) = start {
                    children.push(*node);
                }
                if let SLTLoopBound::Expr(node) = end {
                    children.push(*node);
                }
                if let crate::SLTForFoldResult::Transient { initial, update } = result {
                    children.push(*initial);
                    children.push(*update);
                }
                children.extend(initials.iter().map(|update| update.expr));
                children.extend(updates.iter().map(|update| update.expr));
                for effect in effects {
                    match effect {
                        crate::SLTForEffect::Event { guard, args, .. } => {
                            children.extend(*guard);
                            children.extend(args.iter().copied());
                        }
                        crate::SLTForEffect::Runner(runner) => children.push(*runner),
                    }
                }
                children.push(*continue_cond);
                children
            }
            SLTNode::ForFoldGroup {
                entry_guard,
                states,
                ..
            } => std::iter::once(*entry_guard)
                .chain(
                    states
                        .iter()
                        .flat_map(|state| [state.initial, state.update]),
                )
                .collect(),
        }
    }

    pub(super) fn chunks(width: usize) -> u128 {
        width.div_ceil(64).max(1) as u128
    }

    fn binary_operation_cost(op: BinaryOp, width: usize) -> u128 {
        let chunks = Self::chunks(width);
        match op {
            BinaryOp::And
            | BinaryOp::Or
            | BinaryOp::Xor
            | BinaryOp::LogicAnd
            | BinaryOp::LogicOr => chunks,
            BinaryOp::Add | BinaryOp::Sub => 3 * chunks,
            BinaryOp::Mul => 5 * chunks.saturating_mul(chunks),
            BinaryOp::DivU | BinaryOp::DivS | BinaryOp::RemU | BinaryOp::RemS => {
                12 * chunks.saturating_mul(chunks)
            }
            BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar => 4 * chunks,
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
            | BinaryOp::GeS => 3 * chunks,
        }
    }

    /// Runtime work introduced by this node itself.  Child work is accounted
    /// separately so hash-consed descendants can be counted exactly once.
    pub(super) fn intrinsic_node_cost<A: Hash + Eq + Clone + std::fmt::Debug>(
        &self,
        node: NodeId,
        arena: &SLTNodeArena<A>,
    ) -> u128 {
        match arena.get(node) {
            SLTNode::Input { access, index, .. } => {
                let chunks = Self::chunks(access.msb - access.lsb + 1);
                3 * chunks + u128::from(!index.is_empty()) * 3
            }
            SLTNode::Constant(_, _, width, _) => Self::chunks(*width),
            SLTNode::Binary(lhs, op, rhs) => {
                let width = self.get_width(*lhs, arena).max(self.get_width(*rhs, arena));
                Self::binary_operation_cost(*op, width)
            }
            SLTNode::Unary(op, inner) => {
                let chunks = Self::chunks(self.get_width(*inner, arena));
                match op {
                    UnaryOp::PopCount => 2 * chunks + 1,
                    UnaryOp::CountLeadingZeros | UnaryOp::CountTrailingZeros => 3 * chunks + 1,
                    _ => 2 * chunks,
                }
            }
            SLTNode::Capture { .. } => 0,
            SLTNode::Mux {
                then_expr,
                else_expr,
                ..
            } => Self::chunks(
                self.get_width(*then_expr, arena)
                    .max(self.get_width(*else_expr, arena)),
            ),
            SLTNode::Concat(parts) => {
                let width = parts.iter().map(|(_, width)| *width).sum();
                Self::chunks(width) + parts.len() as u128
            }
            SLTNode::Slice { access, .. } => 2 * Self::chunks(access.msb - access.lsb + 1),
            // A fold contains at least a loop test, a backedge, loop-carried
            // values, and an exit edge.  Its child DAG is still counted below;
            // this fixed cost represents the control operation itself rather
            // than an input-size or iteration cap.
            SLTNode::ForFold { updates, .. } => 8 + 2 * updates.len() as u128,
            SLTNode::ForFoldGroup { states, .. } => 6 + 2 * states.len() as u128,
        }
    }

    /// Cheap, memoized upper bound used only to avoid building reachability
    /// sets for muxes that cannot possibly pay for a branch.  It may count a
    /// shared descendant more than once; the final decision below never does.
    pub(super) fn estimated_tree_cost<A: Hash + Eq + Clone + std::fmt::Debug>(
        &self,
        node: NodeId,
        arena: &SLTNodeArena<A>,
    ) -> u128 {
        self.prepare_cost_cache(arena);
        if let Some(cost) = self.cost_cache.borrow().tree_costs[node.0] {
            return cost;
        }
        self.note_analysis_visits(1);
        let mut cost = self.intrinsic_node_cost(node, arena);
        for child in Self::node_children(node, arena) {
            cost = cost.saturating_add(self.estimated_tree_cost(child, arena));
        }
        let mut cache = self.cost_cache.borrow_mut();
        cache.touch(node);
        cache.tree_costs[node.0] = Some(cost);
        cost
    }

    pub(super) fn is_nontrivial_node<A: Hash + Eq + Clone>(
        node: NodeId,
        arena: &SLTNodeArena<A>,
    ) -> bool {
        !matches!(
            arena.get(node),
            SLTNode::Input { .. } | SLTNode::Constant(..)
        )
    }

    /// Cost which is provably owned by this node in the current top-level DAG.
    /// A node with more than one incoming DAG edge is excluded together with
    /// its descendants: charging it to either mux arm could mistake shared CSE
    /// work for conditionally skippable work.  The memo makes all nested mux
    /// queries constant-time after one traversal of the top-level DAG.
    pub(super) fn owned_tree_cost<A: Hash + Eq + Clone + std::fmt::Debug>(
        &self,
        node: NodeId,
        arena: &SLTNodeArena<A>,
    ) -> u128 {
        self.prepare_cost_cache(arena);
        if let Some(cost) = self.cost_cache.borrow().owned_costs[node.0] {
            return cost;
        }
        self.note_analysis_visits(1);
        let excluded = {
            let cache = self.cost_cache.borrow();
            cache.initially_materialized[node.0] || cache.fanout[node.0] > 1
        };
        let mut cost = if excluded {
            0
        } else {
            self.intrinsic_node_cost(node, arena)
        };
        if !excluded {
            for child in Self::node_children(node, arena) {
                cost = cost.saturating_add(self.owned_tree_cost(child, arena));
            }
        }
        let mut cache = self.cost_cache.borrow_mut();
        cache.touch(node);
        cache.owned_costs[node.0] = Some(cost);
        cost
    }

    /// Width-independent lower bound for region-slice lowering.  A Slice node
    /// may compose into its child without emitting an instruction, while every
    /// other non-materialized node emits at least its one-chunk operation.
    pub(super) fn owned_slice_lower_cost<A: Hash + Eq + Clone + std::fmt::Debug>(
        &self,
        node: NodeId,
        arena: &SLTNodeArena<A>,
    ) -> u128 {
        self.prepare_cost_cache(arena);
        if let Some(cost) = self.cost_cache.borrow().owned_slice_lower_costs[node.0] {
            return cost;
        }
        self.note_analysis_visits(1);
        let excluded = {
            let cache = self.cost_cache.borrow();
            cache.initially_materialized[node.0] || cache.fanout[node.0] > 1
        };
        let mut cost = if excluded {
            0
        } else {
            match arena.get(node) {
                SLTNode::Slice { .. } => 0,
                SLTNode::Binary(_, op, _) => Self::binary_operation_cost(*op, 1),
                SLTNode::Unary(..) => 1,
                SLTNode::Capture { .. } => 0,
                SLTNode::ForFold { updates, .. } => 8 + 2 * updates.len() as u128,
                SLTNode::ForFoldGroup { states, .. } => 6 + 2 * states.len() as u128,
                SLTNode::Input { .. }
                | SLTNode::Constant(..)
                | SLTNode::Mux { .. }
                | SLTNode::Concat(..) => 1,
            }
        };
        if !excluded {
            for child in Self::node_children(node, arena) {
                cost = cost.saturating_add(self.owned_slice_lower_cost(child, arena));
            }
        }
        let mut cache = self.cost_cache.borrow_mut();
        cache.touch(node);
        cache.owned_slice_lower_costs[node.0] = Some(cost);
        cost
    }

    pub(super) fn contains_shared_nontrivial<A: Hash + Eq + Clone>(
        &self,
        node: NodeId,
        arena: &SLTNodeArena<A>,
    ) -> bool {
        self.prepare_cost_cache(arena);
        if let Some(result) = self.cost_cache.borrow().contains_shared_nontrivial[node.0] {
            return result;
        }
        self.note_analysis_visits(1);
        let (materialized, fanout) = {
            let cache = self.cost_cache.borrow();
            (cache.initially_materialized[node.0], cache.fanout[node.0])
        };
        let result = !materialized
            && ((fanout > 1 && Self::is_nontrivial_node(node, arena))
                || Self::node_children(node, arena)
                    .into_iter()
                    .any(|child| self.contains_shared_nontrivial(child, arena)));
        let mut cache = self.cost_cache.borrow_mut();
        cache.touch(node);
        cache.contains_shared_nontrivial[node.0] = Some(result);
        result
    }

    fn direct_shared_candidates<A: Hash + Eq + Clone>(
        &self,
        node: NodeId,
        arena: &SLTNodeArena<A>,
        materialized: &crate::HashMap<NodeId, RegisterId>,
    ) -> crate::HashSet<NodeId> {
        let candidates = std::iter::once(node)
            .chain(Self::node_children(node, arena))
            .collect::<Vec<_>>();
        self.note_analysis_visits(candidates.len());
        candidates
            .into_iter()
            .filter(|candidate| {
                !materialized.contains_key(candidate)
                    && self.cost_cache.borrow().fanout[candidate.0] > 1
                    && Self::is_nontrivial_node(*candidate, arena)
            })
            .collect()
    }

    fn arm_has_only_direct_shared<A: Hash + Eq + Clone>(
        &self,
        node: NodeId,
        arena: &SLTNodeArena<A>,
        materialized: &crate::HashMap<NodeId, RegisterId>,
        allowed_shared: &crate::HashSet<NodeId>,
    ) -> bool {
        if materialized.contains_key(&node) || allowed_shared.contains(&node) {
            return true;
        }
        let node_is_shared =
            self.cost_cache.borrow().fanout[node.0] > 1 && Self::is_nontrivial_node(node, arena);
        if node_is_shared {
            return false;
        }
        let children = Self::node_children(node, arena);
        self.note_analysis_visits(children.len().max(1));
        children.into_iter().all(|child| {
            materialized.contains_key(&child)
                || allowed_shared.contains(&child)
                || !self.contains_shared_nontrivial(child, arena)
        })
    }

    /// Find shared expressions without walking either entire arm.  Only a
    /// common root or direct operand is hoisted.  If a deeper shared expression
    /// exists, the mux remains a Select; this conservative rule preserves CSE
    /// and keeps analysis linear for long nested priority-mux chains.
    pub(super) fn shared_mux_nodes<A: Hash + Eq + Clone>(
        &self,
        then_expr: NodeId,
        else_expr: NodeId,
        arena: &SLTNodeArena<A>,
        materialized: &crate::HashMap<NodeId, RegisterId>,
    ) -> Option<Vec<NodeId>> {
        let then_candidates = self.direct_shared_candidates(then_expr, arena, materialized);
        let else_candidates = self.direct_shared_candidates(else_expr, arena, materialized);
        let shared = then_candidates
            .intersection(&else_candidates)
            .copied()
            .collect::<crate::HashSet<_>>();
        if !self.arm_has_only_direct_shared(then_expr, arena, materialized, &shared)
            || !self.arm_has_only_direct_shared(else_expr, arena, materialized, &shared)
        {
            return None;
        }
        let mut shared = shared.into_iter().collect::<Vec<_>>();
        shared.sort_unstable_by_key(|node| std::cmp::Reverse(node.0));
        Some(shared)
    }

    pub(super) fn is_speculatable_pure<A: Hash + Eq + Clone>(
        &self,
        node: NodeId,
        arena: &SLTNodeArena<A>,
    ) -> bool {
        self.prepare_cost_cache(arena);
        if let Some(result) = self.cost_cache.borrow().is_speculatable_pure[node.0] {
            return result;
        }
        self.note_analysis_visits(1);
        // Fold nodes carry a scoped loop environment and lower to CFG.  They
        // must not be hoisted or cloned as an ordinary mux-arm expression;
        // ForFold can additionally emit effects and Error exits.  All other
        // SLT nodes lower to read-only/value instructions.
        let result = !matches!(
            arena.get(node),
            SLTNode::ForFold { .. } | SLTNode::ForFoldGroup { .. }
        ) && Self::node_children(node, arena)
            .into_iter()
            .all(|child| self.is_speculatable_pure(child, arena));
        let mut cache = self.cost_cache.borrow_mut();
        cache.touch(node);
        cache.is_speculatable_pure[node.0] = Some(result);
        result
    }
}
