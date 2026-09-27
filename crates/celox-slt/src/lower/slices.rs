//! Slice lowering and register width conversion.

use super::*;

impl SLTToSIRLowerer {
    pub(super) fn lower_slice_inner<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        expr: NodeId,
        access: &BitAccess,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
        env: Option<&LowerEnv<'_, A>>,
        allow_cache: bool,
    ) -> RegisterId {
        self.lower_region_slice_inner(builder, expr, access, arena, cache, env, allow_cache)
    }

    pub(super) fn lower_region_slice_inner<
        A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display,
    >(
        &self,
        builder: &mut SIRBuilder<A>,
        expr: NodeId,
        access: &BitAccess,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
        env: Option<&LowerEnv<'_, A>>,
        allow_cache: bool,
    ) -> RegisterId {
        // A cached value is a snapshot at the point where the scheduler first
        // lowered this node. Preserve that snapshot instead of introducing a
        // later memory read which could cross an intervening Store.
        if allow_cache && let Some(&full_value) = cache.get(&expr) {
            if access.lsb == 0 && access.msb + 1 == self.get_width(expr, arena) {
                return full_value;
            }
            return self.slice_reg(builder, full_value, access);
        }

        // Eliminate an annihilated expression before consulting the shared
        // projection cache. Neither operand may be visited: it can contain a
        // dead wide division or a loop-carried Input which would otherwise be
        // rebuilt as a dynamic read from the current state version.
        if let SLTNode::Binary(lhs, BinaryOp::And, rhs) = arena.get(expr)
            && (slt_const_u64(*lhs, arena) == Some(0) || slt_const_u64(*rhs, arena) == Some(0))
        {
            let width = access.msb - access.lsb + 1;
            let result = builder.alloc_bit(width, false);
            builder.emit(SIRInstruction::Imm(result, SIRValue::new(0u8)));
            return result;
        }

        // Cache the projection rather than materializing the full expression.
        // Repeated references in a read-modify-write DAG then stay linear while
        // wide arithmetic continues to operate on only the requested region.
        let cache_projection = allow_cache
            && env.is_none()
            && self
                .cost_cache
                .borrow()
                .fanout
                .get(expr.0)
                .is_some_and(|fanout| *fanout > 1)
            && Self::is_nontrivial_node(expr, arena);
        let projection_key = (expr, *access);
        if cache_projection
            && let Some(&projected) = self.region_slice_cache.borrow().get(&projection_key)
        {
            return projected;
        }

        let result = match arena.get(expr) {
            SLTNode::Input {
                variable,
                index,
                access: input_access,
                ..
            } if access.msb <= input_access.msb - input_access.lsb => {
                let composed =
                    BitAccess::new(input_access.lsb + access.lsb, input_access.lsb + access.msb);
                if let Some(env) = env
                    && let Some(reg) = self.lookup_override(
                        builder, expr, arena, cache, env, variable, index, &composed,
                    )
                {
                    return reg;
                }
                self.lower_input_for_node(
                    builder, expr, variable, index, &composed, arena, cache, env,
                )
            }
            SLTNode::Slice {
                expr: inner,
                access: inner_access,
            } if access.msb <= inner_access.msb - inner_access.lsb => {
                let composed =
                    BitAccess::new(inner_access.lsb + access.lsb, inner_access.lsb + access.msb);
                self.lower_region_slice_inner(
                    builder,
                    *inner,
                    &composed,
                    arena,
                    cache,
                    env,
                    allow_cache,
                )
            }
            SLTNode::Binary(lhs, op @ (BinaryOp::And | BinaryOp::Or | BinaryOp::Xor), rhs)
                if access.msb < self.get_width(*lhs, arena)
                    && access.msb < self.get_width(*rhs, arena) =>
            {
                let lhs_val = self.lower_region_slice_inner(
                    builder,
                    *lhs,
                    access,
                    arena,
                    cache,
                    env,
                    allow_cache,
                );
                let rhs_val = self.lower_region_slice_inner(
                    builder,
                    *rhs,
                    access,
                    arena,
                    cache,
                    env,
                    allow_cache,
                );
                let result = builder.alloc_logic(access.msb - access.lsb + 1);
                builder.emit(SIRInstruction::Binary(result, lhs_val, *op, rhs_val));
                result
            }
            SLTNode::Binary(lhs, op @ (BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul), rhs)
                if access.lsb == 0
                    && access.msb < self.get_width(*lhs, arena)
                    && access.msb < self.get_width(*rhs, arena) =>
            {
                let lhs_val = self.lower_region_slice_inner(
                    builder,
                    *lhs,
                    access,
                    arena,
                    cache,
                    env,
                    allow_cache,
                );
                let rhs_val = self.lower_region_slice_inner(
                    builder,
                    *rhs,
                    access,
                    arena,
                    cache,
                    env,
                    allow_cache,
                );
                let result = builder.alloc_logic(access.msb + 1);
                builder.emit(SIRInstruction::Binary(result, lhs_val, *op, rhs_val));
                result
            }
            SLTNode::Unary(
                op @ (UnaryOp::Ident | UnaryOp::ToTwoState | UnaryOp::BitNot),
                inner,
            ) if access.msb < self.get_width(*inner, arena) => {
                let input = self.lower_region_slice_inner(
                    builder,
                    *inner,
                    access,
                    arena,
                    cache,
                    env,
                    allow_cache,
                );
                let width = access.msb - access.lsb + 1;
                let result = if matches!(op, UnaryOp::ToTwoState) {
                    builder.alloc_bit(width, self.get_bound_signed(expr, arena))
                } else {
                    builder.alloc_logic(width)
                };
                builder.emit(SIRInstruction::Unary(result, *op, input));
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
                    builder,
                    *cond,
                    *then_expr,
                    *else_expr,
                    access,
                    arena,
                    cache,
                    env,
                    allow_cache,
                )
            }
            _ => {
                let inner = self.lower_inner(builder, expr, arena, cache, env, allow_cache);
                self.slice_reg(builder, inner, access)
            }
        };

        if cache_projection {
            let previous = self
                .region_slice_cache
                .borrow_mut()
                .insert(projection_key, result);
            debug_assert!(previous.is_none());
            if previous.is_none() {
                self.region_slice_cache_insert_log
                    .borrow_mut()
                    .push(projection_key);
            }
        }
        result
    }

    pub(super) fn slice_reg<A>(
        &self,
        builder: &mut SIRBuilder<A>,
        reg: RegisterId,
        access: &BitAccess,
    ) -> RegisterId {
        let width = access.msb - access.lsb + 1;
        if self.four_state {
            // A bit-select transports Z. Lowering it to bitwise AND would
            // incorrectly turn every retained Z into X (IEEE 1800 11.4.8).
            let dest = builder.alloc_logic(width);
            builder.emit(SIRInstruction::Slice(dest, reg, access.lsb, width));
            return dest;
        }
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

    pub(super) fn cast_reg_width<A>(
        &self,
        builder: &mut SIRBuilder<A>,
        reg: RegisterId,
        width: usize,
    ) -> RegisterId {
        self.cast_reg_width_ext(builder, reg, width, false)
    }

    pub(super) fn cast_reg_width_ext<A>(
        &self,
        builder: &mut SIRBuilder<A>,
        reg: RegisterId,
        width: usize,
        signed: bool,
    ) -> RegisterId {
        let source_type = builder.register(&reg).clone();
        let current_width = source_type.width();
        let alloc_like_source = |builder: &mut SIRBuilder<A>, width, signed| match &source_type {
            RegisterType::Logic { .. } => builder.alloc_logic(width),
            RegisterType::Bit { .. } => builder.alloc_bit(width, signed),
        };
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
                    let ext = alloc_like_source(builder, pad_width, true);
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
            let dest = alloc_like_source(builder, width, signed);
            builder.emit(SIRInstruction::Concat(dest, vec![pad, reg]));
            return dest;
        }

        let mask_val = (BigUint::from(1u64) << width) - BigUint::from(1u64);
        let mask_reg = builder.alloc_bit(current_width, false);
        builder.emit(SIRInstruction::Imm(mask_reg, SIRValue::new(mask_val)));
        let masked = alloc_like_source(builder, current_width, signed);
        builder.emit(SIRInstruction::Binary(masked, reg, BinaryOp::And, mask_reg));
        let sliced = self.slice_reg(builder, masked, &BitAccess::new(0, width - 1));
        let dest = alloc_like_source(builder, width, signed);
        builder.emit(SIRInstruction::Unary(dest, UnaryOp::Ident, sliced));
        dest
    }
}
