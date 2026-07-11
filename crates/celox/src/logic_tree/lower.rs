use crate::ir::{
    BinaryOp, BitAccess, RegisterId, SIRBuilder, SIRInstruction, SIROffset, SIRTerminator,
    SIRValue, UnaryOp, VarAtomBase,
};
use crate::logic_tree::{NodeId, SLTLoopBound, SLTNode, SLTNodeArena, comb::SLTStepOp};
use num_bigint::BigUint;
use std::cell::RefCell;
use std::hash::Hash;

/// Try to evaluate an SLT node as a compile-time constant.
/// Returns `Some((value, mask))` if the entire subtree is constant, `None` otherwise.
fn try_const_eval<A: Hash + Eq + Clone>(
    node_id: NodeId,
    arena: &SLTNodeArena<A>,
) -> Option<(BigUint, BigUint)> {
    match arena.get(node_id) {
        SLTNode::Constant(val, mask, _width, _signed) => Some((val.clone(), mask.clone())),
        SLTNode::Binary(lhs, op, rhs) => {
            let (lv, lm) = try_const_eval(*lhs, arena)?;
            let (rv, rm) = try_const_eval(*rhs, arena)?;
            // Only fold 2-state (no X/Z) constants for safety.
            if lm != BigUint::from(0u32) || rm != BigUint::from(0u32) {
                return None;
            }
            let result = match op {
                BinaryOp::And => &lv & &rv,
                BinaryOp::Or => &lv | &rv,
                BinaryOp::Xor => &lv ^ &rv,
                BinaryOp::Add => &lv + &rv,
                BinaryOp::Sub if lv >= rv => &lv - &rv,
                BinaryOp::Sub => return None,
                _ => return None,
            };
            Some((result, BigUint::from(0u32)))
        }
        SLTNode::Unary(_, _) => None,
        SLTNode::Concat(parts) => {
            let mut combined_val = BigUint::from(0u32);
            let mut total_width = 0usize;
            for (part_node, part_width) in parts.iter().rev() {
                let (v, m) = try_const_eval(*part_node, arena)?;
                if m != BigUint::from(0u32) {
                    return None;
                }
                let width_mask = if *part_width >= 64 {
                    (BigUint::from(1u64) << part_width) - 1u64
                } else {
                    BigUint::from((1u64 << part_width) - 1)
                };
                combined_val |= (&v & &width_mask) << total_width;
                total_width += part_width;
            }
            Some((combined_val, BigUint::from(0u32)))
        }
        SLTNode::Slice { expr, access } => {
            let (v, m) = try_const_eval(*expr, arena)?;
            if m != BigUint::from(0u32) {
                return None;
            }
            let width = access.msb - access.lsb + 1;
            let shifted = &v >> access.lsb;
            let width_mask = if width >= 64 {
                (BigUint::from(1u64) << width) - 1u64
            } else {
                BigUint::from((1u64 << width) - 1)
            };
            Some((shifted & width_mask, BigUint::from(0u32)))
        }
        _ => None, // Input, Mux — not constant
    }
}

#[derive(Default)]
struct LoweringCostCache {
    tree_costs: Vec<Option<u128>>,
    contains_div_rem: Vec<Option<bool>>,
    fanout: Vec<usize>,
    initially_materialized: Vec<bool>,
    owned_costs: Vec<Option<u128>>,
    owned_slice_lower_costs: Vec<Option<u128>>,
    contains_shared_nontrivial: Vec<Option<bool>>,
    is_speculatable_pure: Vec<Option<bool>>,
    #[cfg(test)]
    analysis_node_visits: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct StaticBranchProbability {
    true_weight: u128,
    total_weight: u128,
}

impl StaticBranchProbability {
    const EVEN: Self = Self {
        true_weight: 1,
        total_weight: 2,
    };

    fn inverted(self) -> Self {
        Self {
            true_weight: self.total_weight - self.true_weight,
            total_weight: self.total_weight,
        }
    }
}

struct MuxCfgPlan {
    /// Nodes used by both arms which were not already materialized.  They must
    /// be evaluated once in the dominator before the control-flow split.
    shared_nodes: Vec<NodeId>,
}

#[derive(Default)]
struct MuxLowerStats {
    normal_seen: usize,
    slice_seen: usize,
    constant_folded: usize,
    cfg_cost: usize,
    cfg_div_rem: usize,
    cfg_slice_cost: usize,
    cfg_slice_div_rem: usize,
    shared_nodes_hoisted: usize,
    kept_four_state: usize,
    kept_impure: usize,
    kept_dynamic_env: usize,
    kept_unprofitable: usize,
    kept_deep_shared: usize,
    biased_conditions: usize,
    owned_cost_sum: u128,
    owned_cost_max: u128,
    unprofitable_cost_buckets: [usize; 7],
}

impl MuxLowerStats {
    fn record_cost(&mut self, then_cost: u128, else_cost: u128) {
        let total = then_cost.saturating_add(else_cost);
        self.owned_cost_sum = self.owned_cost_sum.saturating_add(total);
        self.owned_cost_max = self.owned_cost_max.max(total);
    }

    fn record_unprofitable(&mut self, then_cost: u128, else_cost: u128) {
        self.kept_unprofitable += 1;
        let total = then_cost.saturating_add(else_cost);
        let bucket = match total {
            0..=7 => 0,
            8..=15 => 1,
            16..=31 => 2,
            32..=63 => 3,
            64..=127 => 4,
            128..=255 => 5,
            _ => 6,
        };
        self.unprofitable_cost_buckets[bucket] += 1;
    }
}

pub struct SLTToSIRLowerer {
    four_state: bool,
    cost_cache: RefCell<LoweringCostCache>,
    cache_insert_log: RefCell<Vec<NodeId>>,
    mux_stats: Option<RefCell<MuxLowerStats>>,
}

struct LowerEnv<A: Hash + Eq + Clone> {
    inputs: crate::HashMap<VarAtomBase<A>, RegisterId>,
}

impl SLTToSIRLowerer {
    pub fn new(four_state: bool) -> Self {
        Self {
            four_state,
            cost_cache: RefCell::new(LoweringCostCache::default()),
            cache_insert_log: RefCell::new(Vec::new()),
            mux_stats: std::env::var_os("CELOX_MUX_LOWER_STATS")
                .is_some()
                .then(|| RefCell::new(MuxLowerStats::default())),
        }
    }

    #[inline(always)]
    fn with_mux_stats(&self, update: impl FnOnce(&mut MuxLowerStats)) {
        if let Some(stats) = &self.mux_stats {
            update(&mut stats.borrow_mut());
        }
    }

    /// Recursively expand SLT nodes into SIR instructions
    pub fn lower<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        node: NodeId,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
    ) -> RegisterId {
        self.reset_cost_cache(node, arena, cache, true);
        self.lower_inner(builder, node, arena, cache, None, true)
    }

    pub fn lower_with_inputs<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        node: NodeId,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
        inputs: crate::HashMap<VarAtomBase<A>, RegisterId>,
    ) -> RegisterId {
        self.reset_cost_cache(node, arena, cache, false);
        let env = LowerEnv { inputs };
        self.lower_inner(builder, node, arena, cache, Some(&env), false)
    }

    pub fn lower_region_slice<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        node: NodeId,
        access: BitAccess,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
    ) -> RegisterId {
        self.reset_cost_cache(node, arena, cache, true);
        let node_width = self.get_width(node, arena);
        if access.lsb == 0 && access.msb + 1 == node_width {
            return self.lower(builder, node, arena, cache);
        }
        self.lower_region_slice_inner(builder, node, &access, arena, cache)
    }

    fn lower_inner<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        node: NodeId,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
        env: Option<&LowerEnv<A>>,
        allow_cache: bool,
    ) -> RegisterId {
        if allow_cache {
            if let Some(reg) = cache.get(&node) {
                return *reg;
            }
        }

        let reg = match arena.get(node) {
            SLTNode::Input {
                variable: id,
                index,
                access,
                ..
            } => {
                if let Some(env) = env
                    && let Some(reg) =
                        self.lookup_override(builder, arena, cache, env, id, index, access)
                {
                    reg
                } else {
                    self.lower_input(builder, id, index, access, arena, cache, env)
                }
            }
            SLTNode::Constant(val, mask, width, _signed) => {
                let reg = builder.alloc_bit(*width, false);
                builder.emit(SIRInstruction::Imm(
                    reg,
                    SIRValue::new_four_state(val.clone(), mask.clone()),
                ));
                reg
            }
            SLTNode::Binary(lhs, op, rhs) => {
                let l = self.lower_inner(builder, *lhs, arena, cache, env, allow_cache);
                let r = self.lower_inner(builder, *rhs, arena, cache, env, allow_cache);
                let width = self.get_width(node, arena);
                let dest = builder.alloc_logic(width);
                builder.emit(SIRInstruction::Binary(dest, l, *op, r));
                dest
            }
            SLTNode::Unary(op, inner) => {
                let i = self.lower_inner(builder, *inner, arena, cache, env, allow_cache);
                let width = self.get_width(node, arena);
                let dest = builder.alloc_logic(width);
                builder.emit(SIRInstruction::Unary(dest, *op, i));
                dest
            }
            SLTNode::Slice { expr, access } => {
                self.lower_slice_inner(builder, *expr, access, arena, cache, env, allow_cache)
            }
            SLTNode::Concat(parts) => {
                self.lower_concat_inner(builder, parts, arena, cache, env, allow_cache)
            }
            SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } => self.lower_mux_inner(
                builder,
                *cond,
                *then_expr,
                *else_expr,
                arena,
                cache,
                env,
                allow_cache,
            ),
            SLTNode::ForFold {
                loop_var,
                loop_width,
                loop_signed,
                start,
                end,
                inclusive,
                step,
                step_op,
                reverse,
                result,
                initials,
                updates,
                effects,
                continue_cond,
            } => self.lower_for_fold(
                builder,
                arena,
                cache,
                loop_var,
                *loop_width,
                *loop_signed,
                start,
                end,
                *inclusive,
                *step,
                *step_op,
                *reverse,
                result,
                initials,
                updates,
                effects,
                *continue_cond,
            ),
        };

        if allow_cache {
            let previous = cache.insert(node, reg);
            debug_assert!(previous.is_none());
            self.cache_insert_log.borrow_mut().push(node);
        }
        reg
    }

    fn lower_input<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        id: &A,
        index: &[crate::logic_tree::comb::SLTIndex],
        access: &BitAccess,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
        env: Option<&LowerEnv<A>>,
    ) -> RegisterId {
        let width = access.msb - access.lsb + 1;
        let dest = builder.alloc_logic(width);

        if !index.is_empty() {
            let off_reg = builder.alloc_bit(64, false);
            builder.emit(SIRInstruction::Imm(
                off_reg,
                SIRValue::new(access.lsb as u64),
            ));

            let mut total_dynamic = None;
            for idx_entry in index {
                let mut idx_val =
                    self.lower_inner(builder, idx_entry.node, arena, cache, env, env.is_none());

                if idx_entry.stride > 1 {
                    let stride_reg = builder.alloc_bit(64, false);
                    builder.emit(SIRInstruction::Imm(
                        stride_reg,
                        SIRValue::new(idx_entry.stride as u64),
                    ));
                    let stepped_idx = builder.alloc_bit(64, false);
                    builder.emit(SIRInstruction::Binary(
                        stepped_idx,
                        idx_val,
                        BinaryOp::Mul,
                        stride_reg,
                    ));
                    idx_val = stepped_idx;
                }

                if let Some(acc) = total_dynamic {
                    let new_acc = builder.alloc_bit(64, false);
                    builder.emit(SIRInstruction::Binary(new_acc, acc, BinaryOp::Add, idx_val));
                    total_dynamic = Some(new_acc);
                } else {
                    total_dynamic = Some(idx_val);
                }
            }

            if let Some(dynamic_off) = total_dynamic {
                let final_off = builder.alloc_bit(64, false);
                builder.emit(SIRInstruction::Binary(
                    final_off,
                    off_reg,
                    BinaryOp::Add,
                    dynamic_off,
                ));
                builder.emit(SIRInstruction::Load(
                    dest,
                    id.clone(),
                    SIROffset::Dynamic(final_off),
                    width,
                ));
            } else {
                builder.emit(SIRInstruction::Load(
                    dest,
                    id.clone(),
                    SIROffset::Dynamic(off_reg),
                    width,
                ));
            }
        } else {
            builder.emit(SIRInstruction::Load(
                dest,
                id.clone(),
                SIROffset::Static(access.lsb),
                width,
            ));
        }

        dest
    }

    fn build_dynamic_offset<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
        env: Option<&LowerEnv<A>>,
        index: &[crate::logic_tree::comb::SLTIndex],
        access: &BitAccess,
    ) -> RegisterId {
        let off_reg = builder.alloc_bit(64, false);
        builder.emit(SIRInstruction::Imm(
            off_reg,
            SIRValue::new(access.lsb as u64),
        ));

        let mut total_dynamic = None;
        for idx_entry in index {
            let mut idx_val =
                self.lower_inner(builder, idx_entry.node, arena, cache, env, env.is_none());

            if idx_entry.stride > 1 {
                let stride_reg = builder.alloc_bit(64, false);
                builder.emit(SIRInstruction::Imm(
                    stride_reg,
                    SIRValue::new(idx_entry.stride as u64),
                ));
                let stepped_idx = builder.alloc_bit(64, false);
                builder.emit(SIRInstruction::Binary(
                    stepped_idx,
                    idx_val,
                    BinaryOp::Mul,
                    stride_reg,
                ));
                idx_val = stepped_idx;
            }

            if let Some(acc) = total_dynamic {
                let new_acc = builder.alloc_bit(64, false);
                builder.emit(SIRInstruction::Binary(new_acc, acc, BinaryOp::Add, idx_val));
                total_dynamic = Some(new_acc);
            } else {
                total_dynamic = Some(idx_val);
            }
        }

        if let Some(dynamic_off) = total_dynamic {
            let final_off = builder.alloc_bit(64, false);
            builder.emit(SIRInstruction::Binary(
                final_off,
                off_reg,
                BinaryOp::Add,
                dynamic_off,
            ));
            final_off
        } else {
            off_reg
        }
    }

    fn rebuild_override_range<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
        env: &LowerEnv<A>,
        id: &A,
        index: &[crate::logic_tree::comb::SLTIndex],
        access: &BitAccess,
    ) -> Option<RegisterId> {
        let exact = VarAtomBase::new(id.clone(), access.lsb, access.msb);
        if let Some(reg) = env.inputs.get(&exact) {
            return Some(*reg);
        }

        for (target, reg) in &env.inputs {
            if target.id != *id {
                continue;
            }
            if target.access.lsb <= access.lsb && access.msb <= target.access.msb {
                let rel = BitAccess::new(
                    access.lsb - target.access.lsb,
                    access.msb - target.access.lsb,
                );
                return Some(self.slice_reg(builder, *reg, &rel));
            }
        }

        let mut cut_points = vec![access.lsb, access.msb + 1];
        for target in env.inputs.keys() {
            if target.id != *id {
                continue;
            }
            if target.access.msb < access.lsb || access.msb < target.access.lsb {
                continue;
            }
            cut_points.push(target.access.lsb.max(access.lsb));
            cut_points.push((target.access.msb + 1).min(access.msb + 1));
        }
        cut_points.sort_unstable();
        cut_points.dedup();
        if cut_points.len() <= 2 {
            return None;
        }

        let mut part_regs = Vec::new();
        for window in cut_points.windows(2).rev() {
            let part_access = BitAccess::new(window[0], window[1] - 1);
            let mut part_reg = None;
            for (target, reg) in &env.inputs {
                if target.id != *id {
                    continue;
                }
                if target.access.lsb <= part_access.lsb && part_access.msb <= target.access.msb {
                    let rel = BitAccess::new(
                        part_access.lsb - target.access.lsb,
                        part_access.msb - target.access.lsb,
                    );
                    part_reg = Some(self.slice_reg(builder, *reg, &rel));
                    break;
                }
            }
            let reg = part_reg.unwrap_or_else(|| {
                self.lower_input(builder, id, index, &part_access, arena, cache, None)
            });
            part_regs.push(reg);
        }

        if part_regs.len() == 1 {
            part_regs.into_iter().next()
        } else {
            let result = builder.alloc_logic(access.msb - access.lsb + 1);
            builder.emit(SIRInstruction::Concat(result, part_regs));
            Some(result)
        }
    }

    fn lookup_override<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
        env: &LowerEnv<A>,
        id: &A,
        index: &[crate::logic_tree::comb::SLTIndex],
        access: &BitAccess,
    ) -> Option<RegisterId> {
        if !index.is_empty() {
            let dynamic_off =
                self.build_dynamic_offset(builder, arena, cache, Some(env), index, access);
            let mut result = self.lower_input(builder, id, index, access, arena, cache, Some(env));
            let result_width = access.msb - access.lsb + 1;
            for (target, reg) in &env.inputs {
                if target.id != *id {
                    continue;
                }
                let range_lo = target.access.lsb.checked_sub(access.lsb);
                let range_hi = target.access.msb.checked_sub(access.msb);
                let (Some(range_lo), Some(range_hi)) = (range_lo, range_hi) else {
                    continue;
                };
                if range_lo > range_hi {
                    continue;
                }

                let lo_reg = builder.alloc_bit(64, false);
                builder.emit(SIRInstruction::Imm(lo_reg, SIRValue::new(range_lo as u64)));
                let hi_reg = builder.alloc_bit(64, false);
                builder.emit(SIRInstruction::Imm(hi_reg, SIRValue::new(range_hi as u64)));

                let ge_lo = builder.alloc_bit(1, false);
                builder.emit(SIRInstruction::Binary(
                    ge_lo,
                    dynamic_off,
                    BinaryOp::GeU,
                    lo_reg,
                ));
                let le_hi = builder.alloc_bit(1, false);
                builder.emit(SIRInstruction::Binary(
                    le_hi,
                    dynamic_off,
                    BinaryOp::LeU,
                    hi_reg,
                ));
                let in_range = builder.alloc_bit(1, false);
                builder.emit(SIRInstruction::Binary(
                    in_range,
                    ge_lo,
                    BinaryOp::And,
                    le_hi,
                ));

                let rel_off = if range_lo == 0 {
                    dynamic_off
                } else {
                    let rel = builder.alloc_bit(64, false);
                    builder.emit(SIRInstruction::Binary(
                        rel,
                        dynamic_off,
                        BinaryOp::Sub,
                        lo_reg,
                    ));
                    rel
                };

                let shifted = builder.alloc_logic(target.access.msb - target.access.lsb + 1);
                builder.emit(SIRInstruction::Binary(
                    shifted,
                    *reg,
                    BinaryOp::Shr,
                    rel_off,
                ));
                let candidate = self.cast_reg_width(builder, shifted, result_width);
                let merged = builder.alloc_logic(result_width);
                builder.emit(SIRInstruction::Mux(merged, in_range, candidate, result));
                result = merged;
            }
            return Some(result);
        }
        self.rebuild_override_range(builder, arena, cache, env, id, index, access)
    }

    /// Get width (references information from veryl-analyzer)
    fn get_width<A: Hash + Eq + Clone + std::fmt::Debug>(
        &self,
        node: NodeId,
        arena: &SLTNodeArena<A>,
    ) -> usize {
        crate::logic_tree::comb::get_width(node, arena)
    }

    fn get_bound_signed<A: Hash + Eq + Clone + std::fmt::Debug>(
        &self,
        node: NodeId,
        arena: &SLTNodeArena<A>,
    ) -> bool {
        match arena.get(node) {
            SLTNode::Input { signed, .. } => *signed,
            SLTNode::Constant(_, _, _, signed) => *signed,
            SLTNode::Binary(lhs, op, rhs) => match op {
                BinaryOp::Eq
                | BinaryOp::Ne
                | BinaryOp::LtU
                | BinaryOp::LtS
                | BinaryOp::LeU
                | BinaryOp::LeS
                | BinaryOp::GtU
                | BinaryOp::GtS
                | BinaryOp::GeU
                | BinaryOp::GeS
                | BinaryOp::LogicAnd
                | BinaryOp::LogicOr
                | BinaryOp::EqWildcard
                | BinaryOp::NeWildcard => false,
                BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar => self.get_bound_signed(*lhs, arena),
                BinaryOp::Add
                | BinaryOp::Sub
                | BinaryOp::Mul
                | BinaryOp::Div
                | BinaryOp::Rem
                | BinaryOp::And
                | BinaryOp::Or
                | BinaryOp::Xor => {
                    self.get_bound_signed(*lhs, arena) && self.get_bound_signed(*rhs, arena)
                }
            },
            SLTNode::Unary(UnaryOp::Minus, _) => true,
            SLTNode::Unary(_, inner) => self.get_bound_signed(*inner, arena),
            SLTNode::Mux {
                then_expr,
                else_expr,
                ..
            } => {
                self.get_bound_signed(*then_expr, arena) && self.get_bound_signed(*else_expr, arena)
            }
            SLTNode::ForFold { loop_signed, .. } => *loop_signed,
            // Verilog/Veryl bit- and part-select expressions are unsigned even when
            // the source signal is signed.
            SLTNode::Slice { .. } => false,
            SLTNode::Concat(_) => false,
        }
    }

    fn lower_slice_inner<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        expr: NodeId,
        access: &crate::ir::BitAccess,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
        env: Option<&LowerEnv<A>>,
        allow_cache: bool,
    ) -> RegisterId {
        if env.is_none()
            && let SLTNode::Input {
                variable,
                index,
                access: input_access,
                ..
            } = arena.get(expr)
            && !index.is_empty()
            && input_access.lsb <= access.lsb
            && access.msb <= input_access.msb
        {
            return self.lower_input(builder, variable, index, access, arena, cache, env);
        }

        let inner_reg = self.lower_inner(builder, expr, arena, cache, env, allow_cache);
        self.slice_reg(builder, inner_reg, access)
    }

    fn lower_region_slice_inner<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        expr: NodeId,
        access: &crate::ir::BitAccess,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
    ) -> RegisterId {
        if let Some(&full_value) = cache.get(&expr) {
            if access.lsb == 0 && access.msb + 1 == self.get_width(expr, arena) {
                return full_value;
            }
            return self.slice_reg(builder, full_value, access);
        }

        match arena.get(expr) {
            SLTNode::Input {
                variable,
                index,
                access: input_access,
                ..
            } if access.msb <= input_access.msb - input_access.lsb => {
                let composed =
                    BitAccess::new(input_access.lsb + access.lsb, input_access.lsb + access.msb);
                self.lower_input(builder, variable, index, &composed, arena, cache, None)
            }
            SLTNode::Slice {
                expr: inner,
                access: inner_access,
            } if access.msb <= inner_access.msb - inner_access.lsb => {
                let composed =
                    BitAccess::new(inner_access.lsb + access.lsb, inner_access.lsb + access.msb);
                self.lower_region_slice_inner(builder, *inner, &composed, arena, cache)
            }
            SLTNode::Binary(lhs, op @ (BinaryOp::And | BinaryOp::Or | BinaryOp::Xor), rhs)
                if access.msb < self.get_width(*lhs, arena)
                    && access.msb < self.get_width(*rhs, arena) =>
            {
                let lhs_val = self.lower_region_slice_inner(builder, *lhs, access, arena, cache);
                let rhs_val = self.lower_region_slice_inner(builder, *rhs, access, arena, cache);
                let result = builder.alloc_logic(access.msb - access.lsb + 1);
                builder.emit(SIRInstruction::Binary(result, lhs_val, *op, rhs_val));
                result
            }
            SLTNode::Binary(lhs, op @ (BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul), rhs)
                if access.lsb == 0
                    && access.msb < self.get_width(*lhs, arena)
                    && access.msb < self.get_width(*rhs, arena) =>
            {
                let lhs_val = self.lower_region_slice_inner(builder, *lhs, access, arena, cache);
                let rhs_val = self.lower_region_slice_inner(builder, *rhs, access, arena, cache);
                let result = builder.alloc_logic(access.msb + 1);
                builder.emit(SIRInstruction::Binary(result, lhs_val, *op, rhs_val));
                result
            }
            SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } if access.msb < self.get_width(*then_expr, arena)
                && access.msb < self.get_width(*else_expr, arena) =>
            {
                self.lower_region_slice_mux_inner(
                    builder, *cond, *then_expr, *else_expr, access, arena, cache,
                )
            }
            _ => self.lower_slice_inner(builder, expr, access, arena, cache, None, true),
        }
    }

    fn lower_concat_inner<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        parts: &[(NodeId, usize)],
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
        env: Option<&LowerEnv<A>>,
        allow_cache: bool,
    ) -> RegisterId {
        // Fast path: if all parts are constants, fold into a single wide Imm.
        if env.is_none()
            && let Some(reg) = self.try_fold_const_concat(builder, parts, arena)
        {
            return reg;
        }

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

        let reg = builder.alloc_bit(total_width, false);
        builder.emit(SIRInstruction::Imm(
            reg,
            SIRValue::new_four_state(combined_val, combined_mask),
        ));
        Some(reg)
    }

    fn reset_cost_cache<A: Hash + Eq + Clone>(
        &self,
        root: NodeId,
        arena: &SLTNodeArena<A>,
        materialized: &crate::HashMap<NodeId, RegisterId>,
        honor_materialized: bool,
    ) {
        let node_count = arena.len();
        let mut fanout = vec![0usize; node_count];
        let mut initially_materialized = vec![false; node_count];
        let mut visited = crate::HashSet::default();
        let mut work = vec![root];
        while let Some(node) = work.pop() {
            if !visited.insert(node) {
                continue;
            }
            if honor_materialized && materialized.contains_key(&node) {
                initially_materialized[node.0] = true;
                continue;
            }
            for child in Self::node_children(node, arena) {
                fanout[child.0] = fanout[child.0].saturating_add(1);
                work.push(child);
            }
        }
        *self.cost_cache.borrow_mut() = LoweringCostCache {
            tree_costs: vec![None; node_count],
            contains_div_rem: vec![None; node_count],
            fanout,
            initially_materialized,
            owned_costs: vec![None; node_count],
            owned_slice_lower_costs: vec![None; node_count],
            contains_shared_nontrivial: vec![None; node_count],
            is_speculatable_pure: vec![None; node_count],
            #[cfg(test)]
            analysis_node_visits: visited.len(),
        };
        self.cache_insert_log.borrow_mut().clear();
    }

    fn cache_transaction(&self) -> usize {
        self.cache_insert_log.borrow().len()
    }

    #[cfg(test)]
    fn note_analysis_visits(&self, visits: usize) {
        let mut cache = self.cost_cache.borrow_mut();
        cache.analysis_node_visits = cache.analysis_node_visits.saturating_add(visits);
    }

    #[cfg(not(test))]
    #[inline(always)]
    fn note_analysis_visits(&self, _visits: usize) {}

    #[cfg(test)]
    fn analysis_node_visits(&self) -> usize {
        self.cost_cache.borrow().analysis_node_visits
    }

    fn rollback_cache(&self, cache: &mut crate::HashMap<NodeId, RegisterId>, transaction: usize) {
        let mut log = self.cache_insert_log.borrow_mut();
        for node in log.drain(transaction..) {
            cache.remove(&node);
        }
    }

    fn prepare_cost_cache<A: Hash + Eq + Clone>(&self, arena: &SLTNodeArena<A>) {
        let mut cache = self.cost_cache.borrow_mut();
        if cache.tree_costs.len() < arena.len() {
            cache.tree_costs.resize(arena.len(), None);
            cache.contains_div_rem.resize(arena.len(), None);
            cache.fanout.resize(arena.len(), 0);
            cache.initially_materialized.resize(arena.len(), false);
            cache.owned_costs.resize(arena.len(), None);
            cache.owned_slice_lower_costs.resize(arena.len(), None);
            cache.contains_shared_nontrivial.resize(arena.len(), None);
            cache.is_speculatable_pure.resize(arena.len(), None);
        }
    }

    fn node_children<A: Hash + Eq + Clone>(node: NodeId, arena: &SLTNodeArena<A>) -> Vec<NodeId> {
        match arena.get(node) {
            SLTNode::Input { index, .. } => index.iter().map(|entry| entry.node).collect(),
            SLTNode::Constant(..) => Vec::new(),
            SLTNode::Binary(lhs, _, rhs) => vec![*lhs, *rhs],
            SLTNode::Unary(_, inner) => vec![*inner],
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
                children.extend(initials.iter().map(|update| update.expr));
                children.extend(updates.iter().map(|update| update.expr));
                for effect in effects {
                    children.extend(effect.guard);
                    children.extend(effect.args.iter().copied());
                }
                children.push(*continue_cond);
                children
            }
        }
    }

    fn chunks(width: usize) -> u128 {
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
            BinaryOp::Div | BinaryOp::Rem => 12 * chunks.saturating_mul(chunks),
            BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar => 4 * chunks,
            BinaryOp::Eq
            | BinaryOp::Ne
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
    fn intrinsic_node_cost<A: Hash + Eq + Clone + std::fmt::Debug>(
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
            SLTNode::Unary(_, inner) => 2 * Self::chunks(self.get_width(*inner, arena)),
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
        }
    }

    /// Cheap, memoized upper bound used only to avoid building reachability
    /// sets for muxes that cannot possibly pay for a branch.  It may count a
    /// shared descendant more than once; the final decision below never does.
    fn estimated_tree_cost<A: Hash + Eq + Clone + std::fmt::Debug>(
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
        self.cost_cache.borrow_mut().tree_costs[node.0] = Some(cost);
        cost
    }

    fn is_nontrivial_node<A: Hash + Eq + Clone>(node: NodeId, arena: &SLTNodeArena<A>) -> bool {
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
    fn owned_tree_cost<A: Hash + Eq + Clone + std::fmt::Debug>(
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
        self.cost_cache.borrow_mut().owned_costs[node.0] = Some(cost);
        cost
    }

    /// Width-independent lower bound for region-slice lowering.  A Slice node
    /// may compose into its child without emitting an instruction, while every
    /// other non-materialized node emits at least its one-chunk operation.
    fn owned_slice_lower_cost<A: Hash + Eq + Clone + std::fmt::Debug>(
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
                SLTNode::ForFold { updates, .. } => 8 + 2 * updates.len() as u128,
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
        self.cost_cache.borrow_mut().owned_slice_lower_costs[node.0] = Some(cost);
        cost
    }

    fn contains_shared_nontrivial<A: Hash + Eq + Clone>(
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
        self.cost_cache.borrow_mut().contains_shared_nontrivial[node.0] = Some(result);
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
    fn shared_mux_nodes<A: Hash + Eq + Clone>(
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

    fn is_speculatable_pure<A: Hash + Eq + Clone>(
        &self,
        node: NodeId,
        arena: &SLTNodeArena<A>,
    ) -> bool {
        self.prepare_cost_cache(arena);
        if let Some(result) = self.cost_cache.borrow().is_speculatable_pure[node.0] {
            return result;
        }
        self.note_analysis_visits(1);
        // ForFold lowers to runtime loop control, can emit capture effects, and
        // contains Error exits for non-progress.  It is therefore not a pure
        // expression for reverse if-conversion even when its effects vector is
        // empty.  All other SLT nodes lower to read-only/value instructions.
        let result = !matches!(arena.get(node), SLTNode::ForFold { .. })
            && Self::node_children(node, arena)
                .into_iter()
                .all(|child| self.is_speculatable_pure(child, arena));
        self.cost_cache.borrow_mut().is_speculatable_pure[node.0] = Some(result);
        result
    }

    fn contains_div_rem<A: Hash + Eq + Clone + std::fmt::Debug>(
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
                SLTNode::Binary(_, BinaryOp::Div | BinaryOp::Rem, _)
            ) || Self::node_children(node, arena)
                .into_iter()
                .any(|child| self.contains_div_rem(child, arena)));
        self.cost_cache.borrow_mut().contains_div_rem[node.0] = Some(result);
        result
    }

    fn static_true_probability<A: Hash + Eq + Clone>(
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

    fn mux_cfg_is_profitable(
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
        // Branch observes only the value plane, while a four-state Mux merges
        // value and mask planes for X/Z conditions.  No expression shape may
        // bypass this semantic policy.
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
        env: Option<&LowerEnv<A>>,
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
    fn lower_mux_inner<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        cond: NodeId,
        then_expr: NodeId,
        else_expr: NodeId,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
        env: Option<&LowerEnv<A>>,
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
        env: Option<&LowerEnv<A>>,
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

    fn lower_region_slice_mux_inner<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        cond: NodeId,
        then_expr: NodeId,
        else_expr: NodeId,
        access: &BitAccess,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
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
            );
        }

        let cond_reg = self.lower_inner(builder, cond, arena, cache, None, true);
        if let Some(plan) =
            self.mux_slice_cfg_plan(cond, then_expr, else_expr, access, arena, cache)
        {
            self.hoist_shared_mux_nodes(builder, &plan, arena, cache, None, true);
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
            let then_value =
                self.lower_region_slice_inner(builder, then_expr, access, arena, cache);
            builder.seal_block(SIRTerminator::Jump(merge_block, vec![then_value]));
            self.rollback_cache(cache, then_transaction);

            let else_transaction = self.cache_transaction();
            builder.switch_to_block(else_block);
            let else_value =
                self.lower_region_slice_inner(builder, else_expr, access, arena, cache);
            builder.seal_block(SIRTerminator::Jump(merge_block, vec![else_value]));
            self.rollback_cache(cache, else_transaction);

            builder.switch_to_block(merge_block);
            return result;
        }

        let then_value = self.lower_region_slice_inner(builder, then_expr, access, arena, cache);
        let else_value = self.lower_region_slice_inner(builder, else_expr, access, arena, cache);
        let result = builder.alloc_logic(result_width);
        builder.emit(SIRInstruction::Mux(
            result, cond_reg, then_value, else_value,
        ));
        result
    }

    fn slice_reg<A>(
        &self,
        builder: &mut SIRBuilder<A>,
        reg: RegisterId,
        access: &BitAccess,
    ) -> RegisterId {
        let width = access.msb - access.lsb + 1;
        let shift_amt = builder.alloc_bit(64, false);
        builder.emit(SIRInstruction::Imm(
            shift_amt,
            SIRValue::new(access.lsb as u64),
        ));

        let shifted = builder.alloc_logic(width);
        builder.emit(SIRInstruction::Binary(
            shifted,
            reg,
            BinaryOp::Shr,
            shift_amt,
        ));

        let mask_val = (BigUint::from(1u64) << width) - BigUint::from(1u64);
        let mask_reg = builder.alloc_bit(width, false);
        builder.emit(SIRInstruction::Imm(mask_reg, SIRValue::new(mask_val)));

        let dest = builder.alloc_logic(width);
        builder.emit(SIRInstruction::Binary(
            dest,
            shifted,
            BinaryOp::And,
            mask_reg,
        ));
        dest
    }

    fn cast_reg_width<A>(
        &self,
        builder: &mut SIRBuilder<A>,
        reg: RegisterId,
        width: usize,
    ) -> RegisterId {
        self.cast_reg_width_ext(builder, reg, width, false)
    }

    fn cast_reg_width_ext<A>(
        &self,
        builder: &mut SIRBuilder<A>,
        reg: RegisterId,
        width: usize,
        signed: bool,
    ) -> RegisterId {
        let current_width = builder.register(&reg).width();
        if current_width == width {
            return reg;
        }
        if current_width < width {
            let pad_width = width - current_width;
            let pad = if signed {
                let sign = self.slice_reg(
                    builder,
                    reg,
                    &BitAccess::new(current_width - 1, current_width - 1),
                );
                if pad_width == 1 {
                    sign
                } else {
                    let ext = builder.alloc_bit(pad_width, true);
                    builder.emit(SIRInstruction::Concat(
                        ext,
                        std::iter::repeat_n(sign, pad_width).collect(),
                    ));
                    ext
                }
            } else {
                let zero = builder.alloc_bit(pad_width, false);
                builder.emit(SIRInstruction::Imm(zero, SIRValue::new(0u64)));
                zero
            };
            let dest = builder.alloc_bit(width, signed);
            builder.emit(SIRInstruction::Concat(dest, vec![pad, reg]));
            return dest;
        }

        let mask_val = (BigUint::from(1u64) << width) - BigUint::from(1u64);
        let mask_reg = builder.alloc_bit(current_width, false);
        builder.emit(SIRInstruction::Imm(mask_reg, SIRValue::new(mask_val)));
        let masked = builder.alloc_bit(current_width, false);
        builder.emit(SIRInstruction::Binary(masked, reg, BinaryOp::And, mask_reg));
        let sliced = self.slice_reg(builder, masked, &BitAccess::new(0, width - 1));
        let dest = builder.alloc_bit(width, signed);
        builder.emit(SIRInstruction::Unary(
            dest,
            crate::ir::UnaryOp::Ident,
            sliced,
        ));
        dest
    }

    fn lower_bound<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        bound: &SLTLoopBound,
        _canonical_width: usize,
        width: usize,
        signed: bool,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
    ) -> RegisterId {
        match bound {
            SLTLoopBound::Const(v) => {
                let reg = builder.alloc_bit(width, signed);
                builder.emit(SIRInstruction::Imm(reg, SIRValue::new(*v as u64)));
                reg
            }
            SLTLoopBound::Expr(node) => {
                let reg = self.lower_inner(builder, *node, arena, cache, None, true);
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

    fn bound_width(bound: &SLTLoopBound) -> usize {
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
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn lower_for_fold<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
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
        result: &VarAtomBase<A>,
        initials: &[crate::logic_tree::comb::SLTForUpdate<A>],
        updates: &[crate::logic_tree::comb::SLTForUpdate<A>],
        effects: &[crate::logic_tree::comb::SLTForEffect],
        continue_cond: NodeId,
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
        );
        let end_reg = self.lower_bound(
            builder,
            end,
            loop_width,
            compare_width,
            loop_signed,
            arena,
            cache,
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

        let init_counter = if reverse { end_reg } else { start_reg };

        let initial_states: Vec<RegisterId> = initials
            .iter()
            .zip(updates.iter())
            .map(|(init, update)| {
                let reg = self.lower_inner(builder, init.expr, arena, cache, None, true);
                let width = update.target.access.msb - update.target.access.lsb + 1;
                self.cast_reg_width(builder, reg, width)
            })
            .collect();

        let header_counter = builder.alloc_bit(compare_width, loop_signed);
        let header_states: Vec<_> = updates
            .iter()
            .map(|update| {
                let width = update.target.access.msb - update.target.access.lsb + 1;
                builder.alloc_logic(width)
            })
            .collect();
        let body_counter = builder.alloc_bit(compare_width, loop_signed);
        let body_states: Vec<_> = updates
            .iter()
            .map(|update| {
                let width = update.target.access.msb - update.target.access.lsb + 1;
                builder.alloc_logic(width)
            })
            .collect();
        let exit_states: Vec<_> = updates
            .iter()
            .map(|update| {
                let width = update.target.access.msb - update.target.access.lsb + 1;
                builder.alloc_logic(width)
            })
            .collect();

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
            if step == 0 {
                let cmp_op = if loop_signed {
                    if inclusive {
                        BinaryOp::GeS
                    } else {
                        BinaryOp::GtS
                    }
                } else if inclusive {
                    BinaryOp::GeU
                } else {
                    BinaryOp::GtU
                };
                let in_range = builder.alloc_bit(1, false);
                builder.emit(SIRInstruction::Binary(
                    in_range,
                    header_counter,
                    cmp_op,
                    start_reg,
                ));
                let singleton = if inclusive {
                    let eq = builder.alloc_bit(1, false);
                    builder.emit(SIRInstruction::Binary(
                        eq,
                        header_counter,
                        BinaryOp::Eq,
                        start_reg,
                    ));
                    Some(eq)
                } else {
                    None
                };
                let singleton_block = builder.new_block();
                let true_loop_block = builder.new_block();
                let in_range_block = builder.new_block();
                builder.seal_block(SIRTerminator::Branch {
                    cond: in_range,
                    true_block: (in_range_block, vec![]),
                    false_block: (exit_block, header_states.clone()),
                });
                builder.switch_to_block(in_range_block);
                if let Some(singleton) = singleton {
                    builder.seal_block(SIRTerminator::Branch {
                        cond: singleton,
                        true_block: (
                            singleton_block,
                            std::iter::once(header_counter)
                                .chain(header_states.iter().copied())
                                .collect(),
                        ),
                        false_block: (true_loop_block, vec![]),
                    });
                } else {
                    builder.seal_block(SIRTerminator::Jump(true_loop_block, vec![]));
                }
                builder.switch_to_block(true_loop_block);
                builder.seal_block(SIRTerminator::Jump(
                    body_block,
                    std::iter::once(header_counter)
                        .chain(header_states.iter().copied())
                        .collect(),
                ));
                builder.switch_to_block(singleton_block);
                builder.seal_block(SIRTerminator::Jump(
                    body_block,
                    std::iter::once(header_counter)
                        .chain(header_states.iter().copied())
                        .collect(),
                ));
            } else {
                let reverse_width = Self::step_math_width(compare_width, SLTStepOp::Add, step);
                let header_counter_ext =
                    self.cast_reg_width_ext(builder, header_counter, reverse_width, loop_signed);
                let start_ext =
                    self.cast_reg_width_ext(builder, start_reg, reverse_width, loop_signed);
                let reverse_step = builder.alloc_bit(reverse_width, loop_signed);
                builder.emit(SIRInstruction::Imm(
                    reverse_step,
                    SIRValue::new(step as u64),
                ));
                let threshold = builder.alloc_bit(reverse_width, loop_signed);
                builder.emit(SIRInstruction::Binary(
                    threshold,
                    start_ext,
                    BinaryOp::Add,
                    reverse_step,
                ));
                let cond = builder.alloc_bit(1, false);
                builder.emit(SIRInstruction::Binary(
                    cond,
                    header_counter_ext,
                    if loop_signed {
                        BinaryOp::GeS
                    } else {
                        BinaryOp::GeU
                    },
                    if inclusive { start_ext } else { threshold },
                ));
                let body_counter_reg = if inclusive {
                    header_counter
                } else {
                    let next_counter_ext = builder.alloc_bit(reverse_width, loop_signed);
                    builder.emit(SIRInstruction::Binary(
                        next_counter_ext,
                        header_counter_ext,
                        BinaryOp::Sub,
                        reverse_step,
                    ));
                    self.cast_reg_width_ext(builder, next_counter_ext, compare_width, loop_signed)
                };
                builder.seal_block(SIRTerminator::Branch {
                    cond,
                    true_block: (
                        body_block,
                        std::iter::once(body_counter_reg)
                            .chain(header_states.iter().copied())
                            .collect(),
                    ),
                    false_block: (exit_block, header_states.clone()),
                });
            }
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
        let env = LowerEnv { inputs: env_inputs };
        let mut local_cache = crate::HashMap::default();
        self.lower_for_effects(builder, arena, &mut local_cache, &env, effects);
        let next_states: Vec<_> = updates
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
            if step == 0 {
                if inclusive {
                    let error_block = builder.new_block();
                    let terminal = builder.alloc_bit(1, false);
                    builder.emit(SIRInstruction::Binary(
                        terminal,
                        body_counter,
                        BinaryOp::Eq,
                        start_reg,
                    ));
                    builder.seal_block(SIRTerminator::Branch {
                        cond: terminal,
                        true_block: (exit_block, next_states.clone()),
                        false_block: (error_block, vec![]),
                    });
                    builder.switch_to_block(error_block);
                }
                builder.seal_block(SIRTerminator::Error(1));
            } else {
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
                let threshold = builder.alloc_bit(reverse_width, loop_signed);
                builder.emit(SIRInstruction::Binary(
                    threshold,
                    start_math,
                    BinaryOp::Add,
                    reverse_step,
                ));
                let can_continue = builder.alloc_bit(1, false);
                builder.emit(SIRInstruction::Binary(
                    can_continue,
                    current_math,
                    if loop_signed {
                        BinaryOp::GeS
                    } else {
                        BinaryOp::GeU
                    },
                    threshold,
                ));
                let next_counter_ext = builder.alloc_bit(reverse_width, loop_signed);
                builder.emit(SIRInstruction::Binary(
                    next_counter_ext,
                    current_math,
                    BinaryOp::Sub,
                    reverse_step,
                ));
                let next_counter =
                    self.cast_reg_width_ext(builder, next_counter_ext, compare_width, loop_signed);
                builder.seal_block(SIRTerminator::Branch {
                    cond: can_continue,
                    true_block: (
                        header_block,
                        std::iter::once(if inclusive {
                            next_counter
                        } else {
                            body_counter
                        })
                        .chain(next_states.iter().copied())
                        .collect(),
                    ),
                    false_block: (exit_block, next_states.clone()),
                });
            }
        } else {
            if inclusive {
                let terminal = builder.alloc_bit(1, false);
                builder.emit(SIRInstruction::Binary(
                    terminal,
                    body_counter,
                    BinaryOp::Eq,
                    end_reg,
                ));
                let advance_block = builder.new_block();
                builder.seal_block(SIRTerminator::Branch {
                    cond: terminal,
                    true_block: (exit_block, next_states.clone()),
                    false_block: (advance_block, vec![]),
                });
                builder.switch_to_block(advance_block);
            }

            let math_width = Self::step_math_width(compare_width, step_op, step);
            let current_math =
                self.cast_reg_width_ext(builder, body_counter, math_width, loop_signed);
            let step_math = builder.alloc_bit(math_width, loop_signed);
            builder.emit(SIRInstruction::Imm(step_math, SIRValue::new(step as u64)));
            let next_math = builder.alloc_bit(math_width, loop_signed);
            let op = match step_op {
                SLTStepOp::Add => BinaryOp::Add,
                SLTStepOp::Mul => BinaryOp::Mul,
                SLTStepOp::Shl => BinaryOp::Shl,
            };
            builder.emit(SIRInstruction::Binary(
                next_math,
                current_math,
                op,
                step_math,
            ));

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
        let result_idx = updates
            .iter()
            .position(|update| update.target == *result)
            .expect("ForFold result target must be present in updates");
        exit_states[result_idx]
    }

    fn lower_for_effects<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
        env: &LowerEnv<A>,
        effects: &[crate::logic_tree::comb::SLTForEffect],
    ) {
        for effect in effects {
            let emit = |builder: &mut SIRBuilder<A>,
                        this: &Self,
                        cache: &mut crate::HashMap<NodeId, RegisterId>| {
                let args = effect
                    .args
                    .iter()
                    .map(|arg| this.lower_inner(builder, *arg, arena, cache, Some(env), false))
                    .collect();
                builder.emit(SIRInstruction::CombCaptureEvent {
                    site_id: effect.site_id,
                    args,
                    fatal_error_code: effect.fatal_error_code,
                    consume_enabled: false,
                });
            };
            if let Some(guard) = effect.guard {
                let cond = self.lower_inner(builder, guard, arena, cache, Some(env), false);
                let branch_cond = if effect.emit_on_true {
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

impl Drop for SLTToSIRLowerer {
    fn drop(&mut self) {
        let Some(stats) = &self.mux_stats else {
            return;
        };
        let stats = stats.borrow();
        eprintln!(
            "[mux-lower-stats] normal_seen={} slice_seen={} constant_folded={} cfg_cost={} cfg_div_rem={} cfg_slice_cost={} cfg_slice_div_rem={} shared_nodes_hoisted={} kept_four_state={} kept_impure={} kept_dynamic_env={} kept_unprofitable={} kept_deep_shared={} biased_conditions={} owned_cost_sum={} owned_cost_max={} unprofitable_buckets_0_7_15_31_63_127_255_inf={:?}",
            stats.normal_seen,
            stats.slice_seen,
            stats.constant_folded,
            stats.cfg_cost,
            stats.cfg_div_rem,
            stats.cfg_slice_cost,
            stats.cfg_slice_div_rem,
            stats.shared_nodes_hoisted,
            stats.kept_four_state,
            stats.kept_impure,
            stats.kept_dynamic_env,
            stats.kept_unprofitable,
            stats.kept_deep_shared,
            stats.biased_conditions,
            stats.owned_cost_sum,
            stats.owned_cost_max,
            stats.unprofitable_cost_buckets,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{BitAccess, BlockId, ExecutionUnit};
    use crate::logic_tree::comb::SLTNodeArena;

    fn input(arena: &mut SLTNodeArena<u32>, variable: u32, width: usize) -> NodeId {
        arena.alloc(SLTNode::Input {
            variable,
            signed: false,
            index: vec![],
            access: BitAccess::new(0, width - 1),
        })
    }

    fn constant(arena: &mut SLTNodeArena<u32>, value: u64, width: usize) -> NodeId {
        arena.alloc(SLTNode::Constant(value.into(), 0u8.into(), width, false))
    }

    fn operation_chain(
        arena: &mut SLTNodeArena<u32>,
        mut value: NodeId,
        op: BinaryOp,
        operations: usize,
        constant_base: u64,
        width: usize,
    ) -> NodeId {
        for index in 0..operations {
            let rhs = constant(arena, constant_base + index as u64, width);
            value = arena.alloc(SLTNode::Binary(value, op, rhs));
        }
        value
    }

    fn finish_lowering(mut builder: SIRBuilder<u32>) -> ExecutionUnit<u32> {
        builder.seal_block(SIRTerminator::Return);
        let (blocks, register_map, _) = builder.drain();
        let eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        };
        eu.verify_result().unwrap();
        eu
    }

    fn instruction_count(
        eu: &ExecutionUnit<u32>,
        predicate: impl Fn(&SIRInstruction<u32>) -> bool,
    ) -> usize {
        eu.blocks
            .values()
            .flat_map(|block| &block.instructions)
            .filter(|instruction| predicate(instruction))
            .count()
    }

    fn branch_count(eu: &ExecutionUnit<u32>) -> usize {
        eu.blocks
            .values()
            .filter(|block| matches!(block.terminator, SIRTerminator::Branch { .. }))
            .count()
    }

    #[test]
    fn cheap_mux_stays_branchless() {
        let mut arena = SLTNodeArena::new();
        let cond = input(&mut arena, 0, 1);
        let then_expr = input(&mut arena, 1, 8);
        let else_expr = input(&mut arena, 2, 8);
        let mux = arena.alloc(SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        });
        let mut builder = SIRBuilder::new();
        SLTToSIRLowerer::new(false).lower(
            &mut builder,
            mux,
            &arena,
            &mut crate::HashMap::default(),
        );
        let eu = finish_lowering(builder);

        assert_eq!(branch_count(&eu), 0);
        assert_eq!(
            instruction_count(&eu, |inst| matches!(inst, SIRInstruction::Mux(..))),
            1
        );
    }

    #[test]
    fn expected_cost_uses_static_equality_probability() {
        let even = StaticBranchProbability::EVEN;
        let equality = StaticBranchProbability {
            true_weight: 1,
            total_weight: 5,
        };

        // With a 50/50 prior, ten units in the true arm cannot repay the
        // expected branch miss.  When equality is predicted false, 80% of that
        // arm is skipped and the same transformation is profitable.
        assert!(!SLTToSIRLowerer::mux_cfg_is_profitable(10, 0, 64, even));
        assert!(SLTToSIRLowerer::mux_cfg_is_profitable(10, 0, 64, equality));
        assert!(!SLTToSIRLowerer::mux_cfg_is_profitable(
            10,
            0,
            64,
            equality.inverted(),
        ));
    }

    #[test]
    fn wildcard_equality_uses_the_decoder_bias() {
        let mut arena = SLTNodeArena::new();
        let selector = input(&mut arena, 0, 8);
        let opcode = constant(&mut arena, 0x13, 8);
        let eq = arena.alloc(SLTNode::Binary(selector, BinaryOp::EqWildcard, opcode));
        let ne = arena.alloc(SLTNode::Binary(selector, BinaryOp::NeWildcard, opcode));

        let eq_probability = SLTToSIRLowerer::static_true_probability(eq, &arena);
        let ne_probability = SLTToSIRLowerer::static_true_probability(ne, &arena);
        assert_eq!(
            (eq_probability.true_weight, eq_probability.total_weight),
            (1, 5)
        );
        assert_eq!(
            (ne_probability.true_weight, ne_probability.total_weight),
            (4, 5)
        );
    }

    #[test]
    fn expensive_mux_preserves_control_flow_and_verifies() {
        let mut arena = SLTNodeArena::new();
        let cond = input(&mut arena, 0, 1);
        let then_input = input(&mut arena, 1, 64);
        let else_input = input(&mut arena, 2, 64);
        let then_expr = operation_chain(&mut arena, then_input, BinaryOp::Add, 8, 10, 64);
        let else_expr = operation_chain(&mut arena, else_input, BinaryOp::Xor, 12, 100, 64);
        let mux = arena.alloc(SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        });
        let mut builder = SIRBuilder::new();
        SLTToSIRLowerer::new(false).lower(
            &mut builder,
            mux,
            &arena,
            &mut crate::HashMap::default(),
        );
        let eu = finish_lowering(builder);

        assert_eq!(branch_count(&eu), 1);
        assert_eq!(eu.blocks.len(), 4);
        assert_eq!(
            instruction_count(&eu, |inst| matches!(inst, SIRInstruction::Mux(..))),
            0
        );
    }

    #[test]
    fn shared_arm_dag_is_hoisted_once() {
        let mut arena = SLTNodeArena::new();
        let cond = input(&mut arena, 0, 1);
        let source = input(&mut arena, 1, 64);
        let shared = operation_chain(&mut arena, source, BinaryOp::Mul, 3, 3, 64);
        let then_source = input(&mut arena, 2, 64);
        let else_source = input(&mut arena, 3, 64);
        let then_unique = operation_chain(&mut arena, then_source, BinaryOp::Add, 5, 20, 64);
        let else_unique = operation_chain(&mut arena, else_source, BinaryOp::Sub, 5, 40, 64);
        let then_expr = arena.alloc(SLTNode::Binary(shared, BinaryOp::Add, then_unique));
        let else_expr = arena.alloc(SLTNode::Binary(shared, BinaryOp::Sub, else_unique));
        let mux = arena.alloc(SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        });
        let mut builder = SIRBuilder::new();
        SLTToSIRLowerer::new(false).lower(
            &mut builder,
            mux,
            &arena,
            &mut crate::HashMap::default(),
        );
        let eu = finish_lowering(builder);

        assert_eq!(branch_count(&eu), 1);
        assert_eq!(
            instruction_count(&eu, |inst| matches!(
                inst,
                SIRInstruction::Binary(_, _, BinaryOp::Mul, _)
            )),
            3,
        );
        let entry = &eu.blocks[&BlockId(0)];
        assert_eq!(
            entry
                .instructions
                .iter()
                .filter(|inst| matches!(inst, SIRInstruction::Binary(_, _, BinaryOp::Mul, _)))
                .count(),
            3,
        );
    }

    #[test]
    fn nested_cost_directed_muxes_form_valid_ssa() {
        let mut arena = SLTNodeArena::new();
        let outer_cond = input(&mut arena, 0, 1);
        let inner_cond = input(&mut arena, 1, 1);
        let a = input(&mut arena, 2, 64);
        let b = input(&mut arena, 3, 64);
        let c = input(&mut arena, 4, 64);
        let inner_then = operation_chain(&mut arena, a, BinaryOp::Add, 8, 10, 64);
        let inner_else = operation_chain(&mut arena, b, BinaryOp::Sub, 8, 30, 64);
        let inner = arena.alloc(SLTNode::Mux {
            cond: inner_cond,
            then_expr: inner_then,
            else_expr: inner_else,
        });
        let outer_else = operation_chain(&mut arena, c, BinaryOp::Xor, 16, 70, 64);
        let outer = arena.alloc(SLTNode::Mux {
            cond: outer_cond,
            then_expr: inner,
            else_expr: outer_else,
        });
        let mut builder = SIRBuilder::new();
        SLTToSIRLowerer::new(false).lower(
            &mut builder,
            outer,
            &arena,
            &mut crate::HashMap::default(),
        );
        let eu = finish_lowering(builder);

        assert_eq!(branch_count(&eu), 2);
    }

    #[test]
    fn deep_division_forces_cfg_and_casts_merge_width() {
        let mut arena = SLTNodeArena::new();
        let cond = input(&mut arena, 0, 1);
        let narrow = input(&mut arena, 1, 8);
        let numerator = input(&mut arena, 2, 16);
        let denominator = input(&mut arena, 3, 16);
        let quotient = arena.alloc(SLTNode::Binary(numerator, BinaryOp::Div, denominator));
        let one = constant(&mut arena, 1, 16);
        let deep_division = arena.alloc(SLTNode::Binary(quotient, BinaryOp::Add, one));
        let mux = arena.alloc(SLTNode::Mux {
            cond,
            then_expr: narrow,
            else_expr: deep_division,
        });
        let mut builder = SIRBuilder::new();
        SLTToSIRLowerer::new(false).lower(
            &mut builder,
            mux,
            &arena,
            &mut crate::HashMap::default(),
        );
        let eu = finish_lowering(builder);

        assert_eq!(branch_count(&eu), 1);
        let merge = eu
            .blocks
            .values()
            .find(|block| !block.params.is_empty())
            .unwrap();
        assert_eq!(eu.register_map[&merge.params[0]].width(), 16);
    }

    #[test]
    fn four_state_expensive_mux_keeps_xz_select_semantics() {
        let mut arena = SLTNodeArena::new();
        let cond = input(&mut arena, 0, 1);
        let then_input = input(&mut arena, 1, 64);
        let else_input = input(&mut arena, 2, 64);
        let then_expr = operation_chain(&mut arena, then_input, BinaryOp::Add, 10, 10, 64);
        let else_expr = operation_chain(&mut arena, else_input, BinaryOp::Sub, 10, 30, 64);
        let mux = arena.alloc(SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        });
        let mut builder = SIRBuilder::new();
        SLTToSIRLowerer::new(true).lower(&mut builder, mux, &arena, &mut crate::HashMap::default());
        let eu = finish_lowering(builder);

        assert_eq!(branch_count(&eu), 0);
        assert_eq!(
            instruction_count(&eu, |inst| matches!(inst, SIRInstruction::Mux(..))),
            1
        );
    }

    #[test]
    fn for_fold_is_not_a_pure_mux_arm() {
        let mut arena = SLTNodeArena::new();
        let initial = input(&mut arena, 0, 8);
        let update = input(&mut arena, 1, 8);
        let continue_cond = constant(&mut arena, 1, 1);
        let target = VarAtomBase::new(2, 0, 7);
        let fold = arena.alloc(SLTNode::ForFold {
            loop_var: 3,
            loop_width: 8,
            loop_signed: false,
            start: SLTLoopBound::Const(0),
            end: SLTLoopBound::Const(2),
            inclusive: false,
            step: 1,
            step_op: SLTStepOp::Add,
            reverse: false,
            result: target.clone(),
            initials: vec![crate::logic_tree::comb::SLTForUpdate {
                target: target.clone(),
                expr: initial,
            }],
            updates: vec![crate::logic_tree::comb::SLTForUpdate {
                target,
                expr: update,
            }],
            effects: vec![crate::logic_tree::comb::SLTForEffect {
                site_id: 1,
                guard: None,
                emit_on_true: true,
                args: vec![update],
                fatal_error_code: None,
            }],
            continue_cond,
        });

        assert!(!SLTToSIRLowerer::new(false).is_speculatable_pure(fold, &arena));
    }

    #[test]
    fn region_slice_uses_slice_aware_cfg_cost_and_verifies() {
        let mut arena = SLTNodeArena::new();
        let cond = input(&mut arena, 0, 1);
        let then_input = input(&mut arena, 1, 256);
        let else_input = input(&mut arena, 2, 256);
        let then_expr = operation_chain(&mut arena, then_input, BinaryOp::And, 12, 10, 256);
        let else_expr = operation_chain(&mut arena, else_input, BinaryOp::Xor, 12, 100, 256);
        let mux = arena.alloc(SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        });
        let mut builder = SIRBuilder::new();
        SLTToSIRLowerer::new(false).lower_region_slice(
            &mut builder,
            mux,
            BitAccess::new(0, 63),
            &arena,
            &mut crate::HashMap::default(),
        );
        let eu = finish_lowering(builder);

        assert_eq!(branch_count(&eu), 1);
    }

    #[test]
    fn nested_mux_analysis_is_linear_in_dag_size() {
        let mut arena = SLTNodeArena::new();
        let mut value = input(&mut arena, 0, 64);
        for depth in 0..256u32 {
            let cond = input(&mut arena, 1 + depth * 2, 1);
            let arm_input = input(&mut arena, 2 + depth * 2, 64);
            let arm = operation_chain(
                &mut arena,
                arm_input,
                BinaryOp::Add,
                4,
                1_000 + u64::from(depth) * 8,
                64,
            );
            value = arena.alloc(SLTNode::Mux {
                cond,
                then_expr: arm,
                else_expr: value,
            });
        }

        let lowerer = SLTToSIRLowerer::new(false);
        let mut builder = SIRBuilder::new();
        lowerer.lower(&mut builder, value, &arena, &mut crate::HashMap::default());
        let visits = lowerer.analysis_node_visits();
        let node_count = arena.len();
        finish_lowering(builder);

        assert!(
            visits <= node_count * 20,
            "analysis revisited {visits} nodes for a {node_count}-node nested mux DAG",
        );
    }

    #[test]
    fn unrelated_global_cache_does_not_enter_mux_analysis() {
        let mut arena = SLTNodeArena::new();
        let cond = input(&mut arena, 0, 1);
        let then_input = input(&mut arena, 1, 64);
        let else_input = input(&mut arena, 2, 64);
        let then_expr = operation_chain(&mut arena, then_input, BinaryOp::Add, 8, 10, 64);
        let else_expr = operation_chain(&mut arena, else_input, BinaryOp::Sub, 8, 100, 64);
        let mux = arena.alloc(SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        });

        let empty_lowerer = SLTToSIRLowerer::new(false);
        let mut empty_builder = SIRBuilder::new();
        empty_lowerer.lower(
            &mut empty_builder,
            mux,
            &arena,
            &mut crate::HashMap::default(),
        );
        let empty_visits = empty_lowerer.analysis_node_visits();
        finish_lowering(empty_builder);

        let mut large_cache = crate::HashMap::default();
        for index in 0..20_000usize {
            large_cache.insert(NodeId(arena.len() + index), RegisterId(index));
        }
        let cached_lowerer = SLTToSIRLowerer::new(false);
        let mut cached_builder = SIRBuilder::new();
        cached_lowerer.lower(&mut cached_builder, mux, &arena, &mut large_cache);
        let cached_visits = cached_lowerer.analysis_node_visits();
        finish_lowering(cached_builder);

        assert_eq!(cached_visits, empty_visits);
    }

    #[test]
    fn signed_inputs_report_signedness() {
        let mut arena = SLTNodeArena::<u32>::new();
        let node = arena.alloc(SLTNode::Input {
            variable: 0,
            signed: true,
            index: vec![],
            access: BitAccess::new(0, 7),
        });
        let lowerer = SLTToSIRLowerer::new(false);
        assert!(lowerer.get_bound_signed(node, &arena));
    }

    #[test]
    fn unsigned_inputs_report_unsignedness() {
        let mut arena = SLTNodeArena::<u32>::new();
        let node = arena.alloc(SLTNode::Input {
            variable: 0,
            signed: false,
            index: vec![],
            access: BitAccess::new(0, 7),
        });
        let lowerer = SLTToSIRLowerer::new(false);
        assert!(!lowerer.get_bound_signed(node, &arena));
    }

    #[test]
    fn mixed_sign_subtraction_bound_is_unsigned() {
        let mut arena = SLTNodeArena::<u32>::new();
        let lhs = arena.alloc(SLTNode::Constant(1u8.into(), 0u8.into(), 8, false));
        let rhs = arena.alloc(SLTNode::Input {
            variable: 0,
            signed: true,
            index: vec![],
            access: BitAccess::new(0, 7),
        });
        let node = arena.alloc(SLTNode::Binary(lhs, BinaryOp::Sub, rhs));
        let lowerer = SLTToSIRLowerer::new(false);
        assert!(!lowerer.get_bound_signed(node, &arena));
    }

    #[test]
    fn mixed_sign_mux_bound_is_unsigned() {
        let mut arena = SLTNodeArena::<u32>::new();
        let cond = arena.alloc(SLTNode::Constant(1u8.into(), 0u8.into(), 1, false));
        let then_expr = arena.alloc(SLTNode::Input {
            variable: 0,
            signed: true,
            index: vec![],
            access: BitAccess::new(0, 7),
        });
        let else_expr = arena.alloc(SLTNode::Input {
            variable: 1,
            signed: false,
            index: vec![],
            access: BitAccess::new(0, 7),
        });
        let node = arena.alloc(SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        });
        let lowerer = SLTToSIRLowerer::new(false);
        assert!(!lowerer.get_bound_signed(node, &arena));
    }

    #[test]
    fn comparison_bound_is_not_signed() {
        let mut arena = SLTNodeArena::<u32>::new();
        let lhs = arena.alloc(SLTNode::Input {
            variable: 0,
            signed: false,
            index: vec![],
            access: BitAccess::new(0, 7),
        });
        let rhs = arena.alloc(SLTNode::Input {
            variable: 1,
            signed: true,
            index: vec![],
            access: BitAccess::new(0, 7),
        });
        let node = arena.alloc(SLTNode::Binary(lhs, BinaryOp::LtS, rhs));
        let lowerer = SLTToSIRLowerer::new(false);
        assert!(!lowerer.get_bound_signed(node, &arena));
    }

    #[test]
    fn unsigned_target_bound_zero_extends_signed_slice() {
        let mut arena = SLTNodeArena::<u32>::new();
        let inner = arena.alloc(SLTNode::Input {
            variable: 0,
            signed: true,
            index: vec![],
            access: BitAccess::new(0, 15),
        });
        let casted = arena.alloc(SLTNode::Slice {
            expr: inner,
            access: BitAccess::new(0, 7),
        });
        let mut builder = SIRBuilder::<u32>::new();
        let mut cache = crate::HashMap::default();
        let lowerer = SLTToSIRLowerer::new(false);
        let reg = lowerer.lower_bound(
            &mut builder,
            &SLTLoopBound::Expr(casted),
            8,
            9,
            false,
            &arena,
            &mut cache,
        );
        match builder.register(&reg) {
            crate::ir::RegisterType::Bit { signed, .. } => assert!(!signed),
            other => panic!("expected bit register, got {other:?}"),
        }
        assert!(!lowerer.get_bound_signed(casted, &arena));
    }
}
