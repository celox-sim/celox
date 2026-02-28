use crate::ir::{BinaryOp, RegisterId, SIRBuilder, SIRInstruction, SIROffset, SIRValue, UnaryOp};
use crate::logic_tree::{NodeId, SLTNode, SLTNodeArena};
use malachite_bigint::BigUint;
use std::hash::Hash;

pub struct SLTToSIRLowerer {
    pub four_state: bool,
}

impl SLTToSIRLowerer {
    pub fn new(four_state: bool) -> Self {
        Self { four_state }
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

            SLTNode::Constant(val, width, _signed) => {
                let reg = builder.alloc_bit(*width, false);
                builder.emit(SIRInstruction::Imm(reg, SIRValue::new(val.clone())));
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

    fn lower_mux<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        cond: NodeId,
        then_expr: NodeId,
        else_expr: NodeId,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
    ) -> RegisterId {
        if self.four_state {
            // 4-state mode: Evaluate both branches and use AND/OR select pattern.
            // This ensures X condition correctly propagates to the output.
            // result = (cond_broadcast & then_val) | (~cond_broadcast & else_val)
            self.lower_mux_select(builder, cond, then_expr, else_expr, arena, cache)
        } else {
            // 2-state mode: Use branch-based CFG for performance (lazy evaluation).
            self.lower_mux_branch(builder, cond, then_expr, else_expr, arena, cache)
        }
    }

    /// Branch-based mux lowering (2-state): evaluates only the selected branch.
    fn lower_mux_branch<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        cond: NodeId,
        then_expr: NodeId,
        else_expr: NodeId,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
    ) -> RegisterId {
        // 1. Evaluate condition expression
        let cond_reg = self.lower(builder, cond, arena, cache);

        // 2. Create blocks
        let then_bb = builder.new_block();
        let else_bb = builder.new_block();
        let merge_bb = builder.new_block();

        // 3. Conditional evaluation: Jump to either the 'then' or 'else' block based on the condition.
        // A merge block is used to reconcile the results (using block parameters as a Phi-equivalent).
        let then_width = self.get_width(then_expr, arena);
        let else_width = self.get_width(else_expr, arena);
        let res_width = then_width.max(else_width);

        let res_reg = builder.alloc_logic(res_width);

        // Set parameters for merge block
        builder.set_block_params(merge_bb, vec![res_reg]);

        // 4. Issue conditional branch instruction
        builder.seal_block(crate::ir::SIRTerminator::Branch {
            cond: cond_reg,
            true_block: (then_bb, vec![]),
            false_block: (else_bb, vec![]),
        });

        // --- Then Path ---
        builder.switch_to_block(then_bb);
        let mut then_cache = cache.clone();
        let then_val = self.lower(builder, then_expr, arena, &mut then_cache);
        builder.seal_block(crate::ir::SIRTerminator::Jump(merge_bb, vec![then_val]));

        // --- Else Path ---
        builder.switch_to_block(else_bb);
        let mut else_cache = cache.clone();
        let else_val = self.lower(builder, else_expr, arena, &mut else_cache);
        builder.seal_block(crate::ir::SIRTerminator::Jump(merge_bb, vec![else_val]));

        // --- Switch to merge point ---
        builder.switch_to_block(merge_bb);

        // Return register ID pointing to merged value
        res_reg
    }

    /// Select-based mux lowering (4-state): evaluates both branches, then selects.
    /// result = (cond_broadcast & then_val) | (~cond_broadcast & else_val)
    /// When cond is X, Minus(X) → all-X mask → AND propagates X → result is X.
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

        // Broadcast 1-bit cond to res_width using Minus:
        //   -1 = 0xFF...F (all ones), -0 = 0x00...0 (all zeros)
        let cond_broadcast = builder.alloc_logic(res_width);
        builder.emit(SIRInstruction::Unary(
            cond_broadcast,
            UnaryOp::Minus,
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
