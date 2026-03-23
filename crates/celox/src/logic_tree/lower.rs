use crate::ir::{BinaryOp, RegisterId, SIRBuilder, SIRInstruction, SIROffset, SIRValue, UnaryOp};
use crate::logic_tree::{NodeId, SLTNode, SLTNodeArena};
use num_bigint::BigUint;
use std::hash::Hash;

/// Try to evaluate an SLT node as a compile-time constant.
/// Returns `Some((value, mask))` if the entire subtree is constant, `None` otherwise.
fn try_const_eval<A>(node_id: NodeId, arena: &SLTNodeArena<A>) -> Option<(BigUint, BigUint)> {
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
        if let Some(reg) = cache.get(&node) {
            return *reg;
        }

        let reg = match arena.get(node) {
            // --- Leaf nodes ---
            SLTNode::Input {
                variable: id,
                index,
                access,
            } => {
                let width = access.msb - access.lsb + 1;
                let dest = builder.alloc_logic(width);

                // Compute the cumulative offset for dynamic array/struct access.
                // This combines the base static offset with any dynamic index calculation.
                if !index.is_empty() {
                    // calculate static offset reg for addition
                    let off_reg = builder.alloc_bit(64, false);
                    builder.emit(SIRInstruction::Imm(
                        off_reg,
                        SIRValue::new(access.lsb as u64),
                    ));

                    let mut total_dynamic = None;
                    for idx_entry in index {
                        let mut idx_val = self.lower(builder, idx_entry.node, arena, cache);

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
                            builder.emit(SIRInstruction::Binary(
                                new_acc,
                                acc,
                                BinaryOp::Add,
                                idx_val,
                            ));
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
                        dest
                    } else {
                        // index is present but empty? or some logic error in accumulation
                        // Fallback to static if dynamic calc failed (shouldn't happen with valid index)
                        builder.emit(SIRInstruction::Load(
                            dest,
                            id.clone(),
                            SIROffset::Dynamic(off_reg),
                            width,
                        ));
                        dest
                    }
                } else {
                    // Static access optimization: no need to allocate register for offset
                    builder.emit(SIRInstruction::Load(
                        dest,
                        id.clone(),
                        SIROffset::Static(access.lsb),
                        width,
                    ));
                    dest
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

            // --- Operations ---
            SLTNode::Binary(lhs, op, rhs) => {
                let l = self.lower(builder, *lhs, arena, cache);
                let r = self.lower(builder, *rhs, arena, cache);
                let width = self.get_width(node, arena);
                let dest = builder.alloc_logic(width);
                builder.emit(SIRInstruction::Binary(dest, l, *op, r));
                dest
            }

            SLTNode::Unary(op, inner) => {
                let i = self.lower(builder, *inner, arena, cache);
                let width = self.get_width(node, arena);
                let dest = builder.alloc_logic(width);
                builder.emit(SIRInstruction::Unary(dest, *op, i));
                dest
            }

            // --- Bitwise Manipulation and Composition ---
            SLTNode::Slice { expr, access } => {
                self.lower_slice(builder, *expr, access, arena, cache)
            }

            SLTNode::Concat(parts) => self.lower_concat(builder, parts, arena, cache),

            // --- Structural Control Flow (Mux) ---
            SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } => self.lower_mux(builder, *cond, *then_expr, *else_expr, arena, cache),
        };

        cache.insert(node, reg);
        reg
    }

    /// Get width (references information from veryl-analyzer)
    fn get_width<A: Clone + std::fmt::Debug>(
        &self,
        node: NodeId,
        arena: &SLTNodeArena<A>,
    ) -> usize {
        crate::logic_tree::comb::get_width(node, arena)
    }

    fn lower_slice<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        expr: NodeId,
        access: &crate::ir::BitAccess,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
    ) -> RegisterId {
        let inner_reg = self.lower(builder, expr, arena, cache);
        let width = access.msb - access.lsb + 1;

        // 1. Shift right: Move target LSB to position 0 for easier masking and width management.
        let shift_amt = builder.alloc_bit(64, false);
        builder.emit(SIRInstruction::Imm(
            shift_amt,
            SIRValue::new(access.lsb as u64),
        ));

        let shifted = builder.alloc_logic(width); // Match width after shift
        builder.emit(SIRInstruction::Binary(
            shifted,
            inner_reg,
            BinaryOp::Shr,
            shift_amt,
        ));

        // 2. Clear upper bits: Apply a bitmask to ensure only the requested slice width remains.
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

    fn lower_concat<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        parts: &[(NodeId, usize)],
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
    ) -> RegisterId {
        // Fast path: if all parts are constants, fold into a single wide Imm.
        if let Some(reg) = self.try_fold_const_concat(builder, parts, arena) {
            return reg;
        }

        let mut total_width = 0;
        let mut acc_reg = None;

        // Concatenate parts by shifting them into their respective positions and merging with bitwise OR.
        // Parts are processed from LSB to MSB (reverse order of Concat list).
        for (part_node, part_width) in parts.iter().rev() {
            let part_reg = self.lower(builder, *part_node, arena, cache);

            if let Some(current_acc) = acc_reg {
                let next_width = total_width + part_width;

                // Left shift current part to appropriate position
                let shift_amt = builder.alloc_bit(64, false);
                builder.emit(SIRInstruction::Imm(
                    shift_amt,
                    SIRValue::new(total_width as u64),
                ));

                let shifted = builder.alloc_logic(next_width);
                builder.emit(SIRInstruction::Binary(
                    shifted,
                    part_reg,
                    BinaryOp::Shl,
                    shift_amt,
                ));

                // OR with accumulator
                let next_acc = builder.alloc_logic(next_width);
                builder.emit(SIRInstruction::Binary(
                    next_acc,
                    current_acc,
                    BinaryOp::Or,
                    shifted,
                ));

                acc_reg = Some(next_acc);
                total_width = next_width;
            } else {
                acc_reg = Some(part_reg);
                total_width = *part_width;
            }
        }
        acc_reg.expect("Empty Concat")
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
            combined_val |= (&*val & &width_mask) << total_width;
            combined_mask |= (&*mask & &width_mask) << total_width;
            total_width += *width;
        }

        let reg = builder.alloc_bit(total_width, false);
        builder.emit(SIRInstruction::Imm(
            reg,
            SIRValue::new_four_state(combined_val, combined_mask),
        ));
        Some(reg)
    }

    fn lower_mux<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        cond: NodeId,
        then_expr: NodeId,
        else_expr: NodeId,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
    ) -> RegisterId {
        // Always use select-based (branchless) mux lowering.
        // Branch-based creates 3 blocks per mux, which causes exponential
        // block count growth in deeply nested mux trees (e.g. sorter networks).
        // Select-based evaluates both sides but produces zero extra blocks,
        // keeping Cranelift compilation tractable for large designs.
        self.lower_mux_select(builder, cond, then_expr, else_expr, arena, cache)
    }

    /// Select-based mux lowering: evaluates both branches, then selects.
    /// result = (cond_broadcast & then_val) | (~cond_broadcast & else_val)
    /// When cond is X, Sub(0, X) → all-X mask → AND propagates X → result is X.
    fn lower_mux_select<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        cond: NodeId,
        then_expr: NodeId,
        else_expr: NodeId,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
    ) -> RegisterId {
        let cond_reg = self.lower(builder, cond, arena, cache);
        let then_val = self.lower(builder, then_expr, arena, cache);
        let else_val = self.lower(builder, else_expr, arena, cache);

        let then_width = self.get_width(then_expr, arena);
        let else_width = self.get_width(else_expr, arena);
        let res_width = then_width.max(else_width);

        // Broadcast 1-bit cond to res_width using 0 - cond at res_width:
        //   0 - 1 = 0xFF...F (all ones), 0 - 0 = 0x00...0 (all zeros)
        // We use Binary(Sub) instead of Unary(Minus) because Minus computes
        // at the source register's width (1-bit), giving 0x01 not 0xFF...FF.
        // When cond is X, Sub propagates X through the borrow chain → all-X mask.
        let zero = builder.alloc_logic(res_width);
        builder.emit(SIRInstruction::Imm(zero, SIRValue::new(0u64)));
        let cond_broadcast = builder.alloc_logic(res_width);
        builder.emit(SIRInstruction::Binary(
            cond_broadcast,
            zero,
            BinaryOp::Sub,
            cond_reg,
        ));

        // ~cond_broadcast
        let not_cond = builder.alloc_logic(res_width);
        builder.emit(SIRInstruction::Unary(
            not_cond,
            UnaryOp::BitNot,
            cond_broadcast,
        ));

        // masked_then = cond_broadcast & then_val
        let masked_then = builder.alloc_logic(res_width);
        builder.emit(SIRInstruction::Binary(
            masked_then,
            cond_broadcast,
            BinaryOp::And,
            then_val,
        ));

        // masked_else = ~cond_broadcast & else_val
        let masked_else = builder.alloc_logic(res_width);
        builder.emit(SIRInstruction::Binary(
            masked_else,
            not_cond,
            BinaryOp::And,
            else_val,
        ));

        // result = masked_then | masked_else
        let result = builder.alloc_logic(res_width);
        builder.emit(SIRInstruction::Binary(
            result,
            masked_then,
            BinaryOp::Or,
            masked_else,
        ));

        result
    }
}
