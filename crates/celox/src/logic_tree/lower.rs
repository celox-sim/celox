use crate::ir::{
    BinaryOp, BitAccess, RegisterId, SIRBuilder, SIRInstruction, SIROffset, SIRTerminator,
    SIRValue, VarAtomBase,
};
use crate::logic_tree::{NodeId, SLTLoopBound, SLTNode, SLTNodeArena, comb::SLTStepOp};
use num_bigint::BigUint;
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
                BinaryOp::Sub => {
                    if lv >= rv {
                        &lv - &rv
                    } else {
                        return None;
                    }
                }
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

pub struct SLTToSIRLowerer;

struct LowerEnv<A: Hash + Eq + Clone> {
    inputs: crate::HashMap<VarAtomBase<A>, RegisterId>,
}

impl SLTToSIRLowerer {
    pub fn new(_four_state: bool) -> Self {
        Self
    }

    /// Recursively expand SLT nodes into SIR instructions
    pub fn lower<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        node: NodeId,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
    ) -> RegisterId {
        self.lower_inner(builder, node, arena, cache, None, true)
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
                *continue_cond,
            ),
        };

        if allow_cache {
            cache.insert(node, reg);
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
        let inner_reg = self.lower_inner(builder, expr, arena, cache, env, allow_cache);
        self.slice_reg(builder, inner_reg, access)
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
            .map(|(node, _)| self.lower_inner(builder, *node, arena, cache, env, allow_cache))
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

    /// Select-based mux lowering: evaluates both branches, then selects.
    /// result = (cond_broadcast & then_val) | (~cond_broadcast & else_val)
    /// When cond is X, Sub(0, X) → all-X mask → AND propagates X → result is X.
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
        let cond_reg = self.lower_inner(builder, cond, arena, cache, env, allow_cache);
        let then_val = self.lower_inner(builder, then_expr, arena, cache, env, allow_cache);
        let else_val = self.lower_inner(builder, else_expr, arena, cache, env, allow_cache);

        let then_width = self.get_width(then_expr, arena);
        let else_width = self.get_width(else_expr, arena);
        let res_width = then_width.max(else_width);

        // Use Mux instruction: preserves Z in 4-state, branchless select in 2-state.
        // Backends handle value and mask selection independently.
        let result = builder.alloc_logic(res_width);
        builder.emit(SIRInstruction::Mux(result, cond_reg, then_val, else_val));

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
        canonical_width: usize,
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
                let canonical = if signed {
                    self.cast_reg_width_ext(builder, reg, canonical_width, signed)
                } else {
                    reg
                };
                self.cast_reg_width_ext(builder, canonical, width, signed)
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
        env_inputs.insert(
            VarAtomBase::new(loop_var.clone(), 0, loop_width - 1),
            loop_value_trunc,
        );
        for (update, state_reg) in updates.iter().zip(body_states.iter().copied()) {
            env_inputs.insert(update.target.clone(), state_reg);
        }
        let env = LowerEnv { inputs: env_inputs };
        let mut local_cache = crate::HashMap::default();
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
}
