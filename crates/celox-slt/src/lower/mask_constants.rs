//! Exact constants used by projected partial-write masks. Unsupported
//! operations remain opaque; no runtime inputs or fold bindings are evaluated.

use super::*;

impl SLTToSIRLowerer {
    fn mask_constant<A: Hash + Eq + Clone + std::fmt::Debug>(
        &self,
        root: NodeId,
        arena: &SLTNodeArena<A>,
    ) -> Option<(BigUint, BigUint)> {
        self.prepare_cost_cache(arena);
        let mut memo = self.cost_cache.borrow_mut();
        let mut work = vec![(root, false)];
        while let Some((node, expanded)) = work.pop() {
            if memo.mask_constants[node.0].is_some() {
                continue;
            }
            let supported = matches!(
                arena.get(node),
                SLTNode::Constant(..)
                    | SLTNode::Unary(UnaryOp::Ident | UnaryOp::BitNot, _)
                    | SLTNode::Binary(
                        _,
                        BinaryOp::Shl
                            | BinaryOp::Shr
                            | BinaryOp::And
                            | BinaryOp::Or
                            | BinaryOp::Xor
                            | BinaryOp::Add
                            | BinaryOp::Sub
                            | BinaryOp::Mul,
                        _
                    )
                    | SLTNode::Slice { .. }
                    | SLTNode::Concat(_)
            );
            memo.touch(node);
            if !supported {
                memo.mask_constants[node.0] = Some(None);
                continue;
            }
            if !expanded {
                work.push((node, true));
                for child in Self::node_children(node, arena).into_iter().rev() {
                    if memo.mask_constants[child.0].is_none() {
                        work.push((child, false));
                    }
                }
                continue;
            }
            let get = |child: NodeId| memo.mask_constants[child.0].as_ref().unwrap().clone();
            let width = self.get_width(node, arena);
            let all = (BigUint::from(1u8) << width) - 1u8;
            let result = (|| {
                let (value, mask) = match arena.get(node) {
                    SLTNode::Constant(value, mask, ..) => (value.clone(), mask.clone()),
                    SLTNode::Unary(op, inner) => {
                        let (value, mask) = get(*inner)?;
                        match op {
                            UnaryOp::Ident => (value, mask),
                            // Bitwise operations normalize unknown Z to X.
                            UnaryOp::BitNot => ((&all ^ value) | &mask, mask),
                            _ => unreachable!(),
                        }
                    }
                    SLTNode::Binary(lhs, op, rhs) => {
                        let (left, left_mask) = get(*lhs)?;
                        let (right, right_mask) = get(*rhs)?;
                        if !right_mask.is_zero() {
                            return None;
                        }
                        if matches!(op, BinaryOp::Shl | BinaryOp::Shr) {
                            // Compare before converting/allocating: a wide or
                            // negative-as-unsigned shift must not allocate a
                            // buffer proportional to the shift amount.
                            if right >= BigUint::from(width) {
                                (BigUint::zero(), BigUint::zero())
                            } else {
                                let shift = right.to_usize()?;
                                if matches!(op, BinaryOp::Shl) {
                                    (left << shift, left_mask << shift)
                                } else {
                                    (left >> shift, left_mask >> shift)
                                }
                            }
                        } else {
                            // A narrow operand whose sign bit is known zero
                            // has the same signed and unsigned extension.
                            // Other mixed-width cases stay in normal lowering.
                            let extension_is_unambiguous = |child: NodeId, value: &BigUint| {
                                let source_width = self.get_width(child, arena);
                                source_width >= width || value.bits() < source_width as u64
                            };
                            if !extension_is_unambiguous(*lhs, &left)
                                || !extension_is_unambiguous(*rhs, &right)
                            {
                                return None;
                            }
                            // Arithmetic/bitwise propagation of unknowns is
                            // unnecessary for the known mask shapes. Refuse
                            // them instead of inventing a constant result.
                            if !left_mask.is_zero() {
                                return None;
                            }
                            let left = left & &all;
                            let right = right & &all;
                            let value = match op {
                                BinaryOp::And => left & right,
                                BinaryOp::Or => left | right,
                                BinaryOp::Xor => left ^ right,
                                BinaryOp::Add => left + right,
                                BinaryOp::Sub => (left + (BigUint::from(1u8) << width)) - right,
                                BinaryOp::Mul => left * right,
                                _ => unreachable!(),
                            };
                            (value, BigUint::zero())
                        }
                    }
                    SLTNode::Slice { expr, access } => {
                        if access.msb >= self.get_width(*expr, arena) {
                            return None;
                        }
                        let (value, mask) = get(*expr)?;
                        (value >> access.lsb, mask >> access.lsb)
                    }
                    SLTNode::Concat(parts) => {
                        let (mut value, mut mask) = (BigUint::zero(), BigUint::zero());
                        for (part, bits) in parts {
                            let (part_value, part_mask) = get(*part)?;
                            let part_all = (BigUint::from(1u8) << bits) - 1u8;
                            value = (value << bits) | (part_value & &part_all);
                            mask = (mask << bits) | (part_mask & part_all);
                        }
                        (value, mask)
                    }
                    _ => unreachable!(),
                };
                Some((value & &all, mask & &all))
            })();
            memo.mask_constants[node.0] = Some(result);
        }
        memo.mask_constants[root.0].as_ref().unwrap().clone()
    }

    pub(super) fn projected_mask_constant<A: Hash + Eq + Clone + std::fmt::Debug>(
        &self,
        node: NodeId,
        access: BitAccess,
        arena: &SLTNodeArena<A>,
    ) -> Option<(BigUint, BigUint)> {
        // Keep extension behavior in the ordinary lowering path.
        if access.msb >= self.get_width(node, arena) {
            return None;
        }
        let (value, mask) = self.mask_constant(node, arena)?;
        let all = (BigUint::from(1u8) << (access.msb - access.lsb + 1)) - 1u8;
        Some(((value >> access.lsb) & &all, (mask >> access.lsb) & all))
    }
}
