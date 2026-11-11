//! Specializing logic tree nodes to constants.

use super::*;

#[derive(Clone)]
pub(super) struct SpecializedConstant {
    pub(super) value: BigUint,
    pub(super) mask: BigUint,
    pub(super) width: usize,
    pub(super) signed: bool,
}

pub(super) fn specialize_slt_node(
    node: NodeId,
    loop_value: Option<(VarId, usize)>,
    state_values: Option<&HashMap<VarId, (BitAccess, NodeId)>>,
    arena: &mut SLTNodeArena<VarId>,
    cache: &mut HashMap<NodeId, NodeId>,
) -> Option<NodeId> {
    if let Some(&specialized) = cache.get(&node) {
        return Some(specialized);
    }

    // Arena edges always point to older nodes. Collect the reachable DAG and
    // specialize it in NodeId order instead of recursively walking the tree.
    // Large generated mux/concat trees can be thousands of nodes deep and
    // must not depend on the host thread's stack size.
    let mut pending = vec![node];
    let mut seen = HashSet::default();
    let mut reachable = Vec::new();
    while let Some(current) = pending.pop() {
        if cache.contains_key(&current) || !seen.insert(current) {
            continue;
        }
        match arena.get(current).clone() {
            SLTNode::Input {
                variable, index, ..
            } => {
                let substituted_state =
                    state_values.is_some_and(|values| values.contains_key(&variable));
                let substituted_loop = loop_value.is_some_and(|(loop_var, _)| variable == loop_var);
                if substituted_state {
                    pending.extend(index.into_iter().map(|entry| entry.node));
                } else if !substituted_loop {
                    pending.extend(index.into_iter().map(|entry| entry.node));
                }
            }
            SLTNode::Constant(..) => {}
            SLTNode::Binary(lhs, _, rhs) => {
                pending.push(lhs);
                pending.push(rhs);
            }
            SLTNode::Unary(_, inner) | SLTNode::Capture { expr: inner, .. } => pending.push(inner),
            SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } => {
                pending.push(cond);
                pending.push(then_expr);
                pending.push(else_expr);
            }
            SLTNode::Concat(parts) => {
                pending.extend(parts.into_iter().map(|(part, _)| part));
            }
            SLTNode::Slice { expr, .. } => pending.push(expr),
            SLTNode::ForFold { .. } | SLTNode::ForFoldGroup { .. } => return None,
        }
        reachable.push(current);
    }
    reachable.sort_unstable();

    for node in reachable {
        if cache.contains_key(&node) {
            continue;
        }
        let original_width = get_width(node, arena);
        let specialized = match arena.get(node).clone() {
            SLTNode::Input {
                variable,
                signed,
                index,
                access,
            } => {
                if let Some(&(state_access, state_value)) =
                    state_values.and_then(|values| values.get(&variable))
                {
                    let mut dynamic_offset = 0usize;
                    for entry in &index {
                        let index_node = *cache.get(&entry.node)?;
                        let constant = specialized_constant(index_node, arena)?;
                        if !constant.mask.is_zero() {
                            return None;
                        }
                        dynamic_offset = dynamic_offset
                            .checked_add(constant.value.to_usize()?.checked_mul(entry.stride)?)?;
                    }
                    let width = access.msb.checked_sub(access.lsb)?.checked_add(1)?;
                    if !index.is_empty() {
                        let state_width = state_access
                            .msb
                            .checked_sub(state_access.lsb)?
                            .checked_add(1)?;
                        if width > state_width || access.lsb < state_access.lsb {
                            return None;
                        }
                        let shift = access
                            .lsb
                            .checked_sub(state_access.lsb)?
                            .checked_add(dynamic_offset)?;
                        let shifted = if shift == 0 {
                            state_value
                        } else {
                            let amount = arena
                                .alloc(SLTNode::Constant(
                                    BigUint::from(shift),
                                    BigUint::zero(),
                                    state_width,
                                    false,
                                ))
                                .ok()?;
                            arena
                                .alloc(SLTNode::Binary(state_value, BinaryOp::Shr, amount))
                                .ok()?
                        };
                        if width == state_width {
                            shifted
                        } else {
                            arena
                                .alloc(SLTNode::Slice {
                                    expr: shifted,
                                    access: BitAccess::new(0, width - 1),
                                })
                                .ok()?
                        }
                    } else {
                        if access.lsb < state_access.lsb || access.msb > state_access.msb {
                            return None;
                        }
                        if access == state_access {
                            state_value
                        } else {
                            arena
                                .alloc(SLTNode::Slice {
                                    expr: state_value,
                                    access: BitAccess::new(
                                        access.lsb - state_access.lsb,
                                        access.msb - state_access.lsb,
                                    ),
                                })
                                .ok()?
                        }
                    }
                } else if loop_value.is_some_and(|(loop_var, _)| variable == loop_var) {
                    if !index.is_empty() {
                        return None;
                    }
                    let (_, value) = loop_value?;
                    let width = access.msb.checked_sub(access.lsb)?.checked_add(1)?;
                    let value = (BigUint::from(value) >> access.lsb) & width_mask(width);
                    arena
                        .alloc(SLTNode::Constant(value, BigUint::from(0u8), width, signed))
                        .ok()?
                } else {
                    let index = index
                        .into_iter()
                        .map(|mut entry| {
                            entry.node = *cache.get(&entry.node)?;
                            Some(entry)
                        })
                        .collect::<Option<Vec<_>>>()?;
                    arena
                        .alloc(SLTNode::Input {
                            variable,
                            signed,
                            index,
                            access,
                        })
                        .ok()?
                }
            }
            SLTNode::Constant(value, mask, width, signed) => arena
                .alloc(SLTNode::Constant(value, mask, width, signed))
                .ok()?,
            SLTNode::Binary(lhs, op, rhs) => {
                let lhs = *cache.get(&lhs)?;
                let rhs = *cache.get(&rhs)?;
                if let (Some(lhs_constant), Some(rhs_constant)) = (
                    specialized_constant(lhs, arena),
                    specialized_constant(rhs, arena),
                ) && let Some(result) = specialize_binary_constant(
                    op,
                    &lhs_constant,
                    &rhs_constant,
                    original_width,
                    specialized_binary_result_signed(op, &lhs_constant, &rhs_constant),
                ) {
                    alloc_specialized_constant(result, arena)?
                } else {
                    arena.alloc(SLTNode::Binary(lhs, op, rhs)).ok()?
                }
            }
            SLTNode::Unary(op, inner) => {
                let inner = *cache.get(&inner)?;
                if let Some(inner_constant) = specialized_constant(inner, arena)
                    && let Some(result) = specialize_unary_constant(
                        op,
                        &inner_constant,
                        original_width,
                        specialized_unary_result_signed(op, &inner_constant),
                    )
                {
                    alloc_specialized_constant(result, arena)?
                } else {
                    arena.alloc(SLTNode::Unary(op, inner)).ok()?
                }
            }
            SLTNode::Capture { expr, key } => arena
                .alloc(SLTNode::Capture {
                    expr: *cache.get(&expr)?,
                    key,
                })
                .ok()?,
            SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } => {
                let cond = *cache.get(&cond)?;
                let then_expr = *cache.get(&then_expr)?;
                let else_expr = *cache.get(&else_expr)?;
                if let Some(cond) = specialized_constant(cond, arena)
                    && cond.mask.is_zero()
                {
                    let selected = if cond.value.is_zero() {
                        else_expr
                    } else {
                        then_expr
                    };
                    zero_extend_specialized(selected, original_width, arena)?
                } else {
                    arena
                        .alloc(SLTNode::Mux {
                            cond,
                            then_expr,
                            else_expr,
                        })
                        .ok()?
                }
            }
            SLTNode::Concat(parts) => {
                let parts = parts
                    .into_iter()
                    .map(|(part, width)| Some((*cache.get(&part)?, width)))
                    .collect::<Option<Vec<_>>>()?;
                if let Some(result) = specialize_concat_constant(&parts, arena) {
                    alloc_specialized_constant(result, arena)?
                } else {
                    arena.alloc(SLTNode::Concat(parts)).ok()?
                }
            }
            SLTNode::Slice { expr, access } => {
                let expr = *cache.get(&expr)?;
                if let Some(constant) = specialized_constant(expr, arena) {
                    let width = access.msb.checked_sub(access.lsb)?.checked_add(1)?;
                    alloc_specialized_constant(
                        SpecializedConstant {
                            value: (&constant.value >> access.lsb) & width_mask(width),
                            mask: (&constant.mask >> access.lsb) & width_mask(width),
                            width,
                            signed: false,
                        },
                        arena,
                    )?
                } else if let SLTNode::Input {
                    variable,
                    signed: _,
                    index,
                    access: input_access,
                } = arena.get(expr).clone()
                {
                    let mut offset = 0usize;
                    let mut all_indices_constant = true;
                    for entry in &index {
                        let Some(value) = specialized_constant(entry.node, arena)
                            .filter(|value| value.mask.is_zero())
                            .and_then(|value| value.value.to_usize())
                        else {
                            all_indices_constant = false;
                            break;
                        };
                        offset = offset.checked_add(value.checked_mul(entry.stride)?)?;
                    }
                    if all_indices_constant {
                        let lsb = input_access
                            .lsb
                            .checked_add(offset)?
                            .checked_add(access.lsb)?;
                        let msb = input_access
                            .lsb
                            .checked_add(offset)?
                            .checked_add(access.msb)?;
                        if msb > input_access.msb {
                            return None;
                        }
                        arena
                            .alloc(SLTNode::Input {
                                variable,
                                signed: false,
                                index: Vec::new(),
                                access: BitAccess::new(lsb, msb),
                            })
                            .ok()?
                    } else {
                        arena.alloc(SLTNode::Slice { expr, access }).ok()?
                    }
                } else {
                    arena.alloc(SLTNode::Slice { expr, access }).ok()?
                }
            }
            SLTNode::ForFold { .. } | SLTNode::ForFoldGroup { .. } => return None,
        };
        cache.insert(node, specialized);
    }
    cache.get(&node).copied()
}

pub(super) fn specialized_constant(
    node: NodeId,
    arena: &SLTNodeArena<VarId>,
) -> Option<SpecializedConstant> {
    let SLTNode::Constant(value, mask, width, signed) = arena.get(node) else {
        return None;
    };
    Some(SpecializedConstant {
        value: value.clone(),
        mask: mask.clone(),
        width: *width,
        signed: *signed,
    })
}

pub(super) fn specialized_binary_result_signed(
    op: BinaryOp,
    lhs: &SpecializedConstant,
    rhs: &SpecializedConstant,
) -> bool {
    match op {
        BinaryOp::Float { .. } => false,
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
        | BinaryOp::LogicAnd
        | BinaryOp::LogicOr
        | BinaryOp::DivU
        | BinaryOp::RemU => false,
        BinaryOp::DivS | BinaryOp::RemS => true,
        BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar => lhs.signed,
        BinaryOp::Add
        | BinaryOp::Sub
        | BinaryOp::Mul
        | BinaryOp::And
        | BinaryOp::Or
        | BinaryOp::Xor => lhs.signed && rhs.signed,
    }
}

pub(super) fn specialized_unary_result_signed(op: UnaryOp, inner: &SpecializedConstant) -> bool {
    match op {
        UnaryOp::Float { op, .. } => matches!(op, celox_design::FloatUnaryOp::ToSigned { .. }),
        UnaryOp::Ident | UnaryOp::ToTwoState | UnaryOp::Minus | UnaryOp::BitNot => inner.signed,
        UnaryOp::LogicNot
        | UnaryOp::And
        | UnaryOp::Or
        | UnaryOp::Xor
        | UnaryOp::PopCount
        | UnaryOp::CountLeadingZeros
        | UnaryOp::CountTrailingZeros
        | UnaryOp::ToPlanes
        | UnaryOp::FromPlanes => false,
    }
}

pub(super) fn zero_extend_specialized(
    node: NodeId,
    target_width: usize,
    arena: &mut SLTNodeArena<VarId>,
) -> Option<NodeId> {
    let width = get_width(node, arena);
    if width == target_width {
        return Some(node);
    }
    if width == 0 || width > target_width {
        return None;
    }
    if let Some(mut constant) = specialized_constant(node, arena) {
        constant.width = target_width;
        constant.signed = false;
        return alloc_specialized_constant(constant, arena);
    }
    let padding_width = target_width.checked_sub(width)?;
    let padding = arena
        .alloc(SLTNode::Constant(
            BigUint::from(0u8),
            BigUint::from(0u8),
            padding_width,
            false,
        ))
        .ok()?;
    arena
        .alloc(SLTNode::Concat(vec![
            (padding, padding_width),
            (node, width),
        ]))
        .ok()
}

pub(super) fn alloc_specialized_constant(
    constant: SpecializedConstant,
    arena: &mut SLTNodeArena<VarId>,
) -> Option<NodeId> {
    arena
        .alloc(SLTNode::Constant(
            constant.value & width_mask(constant.width),
            constant.mask & width_mask(constant.width),
            constant.width,
            constant.signed,
        ))
        .ok()
}

pub(super) fn specialize_concat_constant(
    parts: &[(NodeId, usize)],
    arena: &SLTNodeArena<VarId>,
) -> Option<SpecializedConstant> {
    let mut value = BigUint::from(0u8);
    let mut mask = BigUint::from(0u8);
    let mut total_width = 0usize;
    for &(part, width) in parts {
        let part = specialized_constant(part, arena)?;
        if part.width != width {
            return None;
        }
        value = (value << width) | (part.value & width_mask(width));
        mask = (mask << width) | (part.mask & width_mask(width));
        total_width = total_width.checked_add(width)?;
    }
    Some(SpecializedConstant {
        value,
        mask,
        width: total_width,
        signed: false,
    })
}

pub(super) fn specialize_binary_constant(
    op: BinaryOp,
    lhs: &SpecializedConstant,
    rhs: &SpecializedConstant,
    result_width: usize,
    result_signed: bool,
) -> Option<SpecializedConstant> {
    if !lhs.mask.is_zero() || !rhs.mask.is_zero() || result_width == 0 {
        return None;
    }
    let mask = width_mask(result_width);
    let shift = rhs.value.to_usize();
    let bool_value = |value: bool| BigUint::from(u8::from(value));
    let comparison_width = lhs.width.max(rhs.width);
    let comparison_signed = matches!(
        op,
        BinaryOp::LtS | BinaryOp::LeS | BinaryOp::GtS | BinaryOp::GeS
    ) || matches!(
        op,
        BinaryOp::Eq
            | BinaryOp::Ne
            | BinaryOp::EqCase
            | BinaryOp::NeCase
            | BinaryOp::EqWildcard
            | BinaryOp::NeWildcard
    ) && lhs.signed
        && rhs.signed;
    let extend_comparison = |operand: &SpecializedConstant| {
        if comparison_signed {
            signed_to_bits(
                bits_to_signed(&operand.value, operand.width),
                comparison_width,
            )
        } else {
            Some(&operand.value & width_mask(comparison_width))
        }
    };
    let comparison_lhs = extend_comparison(lhs)?;
    let comparison_rhs = extend_comparison(rhs)?;
    let value = match op {
        BinaryOp::Float { op, precision } => BigUint::from(celox_design::eval_float_binary(
            op,
            precision,
            lhs.value.to_u64()?,
            rhs.value.to_u64()?,
        )),
        BinaryOp::Add => (&lhs.value + &rhs.value) & &mask,
        BinaryOp::Sub => {
            let modulus = BigUint::from(1u8) << result_width;
            (&modulus + &lhs.value - (&rhs.value & &mask)) & &mask
        }
        BinaryOp::Mul => (&lhs.value * &rhs.value) & &mask,
        BinaryOp::DivU => {
            if rhs.value.is_zero() {
                return None;
            }
            (&lhs.value / &rhs.value) & &mask
        }
        BinaryOp::RemU => {
            if rhs.value.is_zero() {
                return None;
            }
            (&lhs.value % &rhs.value) & &mask
        }
        BinaryOp::DivS => {
            let divisor = bits_to_signed(&rhs.value, rhs.width);
            if divisor.is_zero() {
                return None;
            }
            signed_to_bits(
                bits_to_signed(&lhs.value, lhs.width) / divisor,
                result_width,
            )?
        }
        BinaryOp::RemS => {
            let divisor = bits_to_signed(&rhs.value, rhs.width);
            if divisor.is_zero() {
                return None;
            }
            signed_to_bits(
                bits_to_signed(&lhs.value, lhs.width) % divisor,
                result_width,
            )?
        }
        BinaryOp::And => (&lhs.value & &rhs.value) & &mask,
        BinaryOp::Or => (&lhs.value | &rhs.value) & &mask,
        BinaryOp::Xor => (&lhs.value ^ &rhs.value) & &mask,
        BinaryOp::Shl => match shift {
            Some(shift) if shift < result_width => (&lhs.value << shift) & &mask,
            Some(_) => BigUint::from(0u8),
            None => return None,
        },
        BinaryOp::Shr => match shift {
            Some(shift) if shift < lhs.width => (&lhs.value >> shift) & &mask,
            Some(_) => BigUint::from(0u8),
            None => return None,
        },
        BinaryOp::Sar => {
            let shift = shift?;
            signed_to_bits(bits_to_signed(&lhs.value, lhs.width) >> shift, result_width)?
        }
        BinaryOp::Eq | BinaryOp::EqCase | BinaryOp::EqWildcard => {
            bool_value(comparison_lhs == comparison_rhs)
        }
        BinaryOp::Ne | BinaryOp::NeCase | BinaryOp::NeWildcard => {
            bool_value(comparison_lhs != comparison_rhs)
        }
        BinaryOp::LtU => bool_value(comparison_lhs < comparison_rhs),
        BinaryOp::LeU => bool_value(comparison_lhs <= comparison_rhs),
        BinaryOp::GtU => bool_value(comparison_lhs > comparison_rhs),
        BinaryOp::GeU => bool_value(comparison_lhs >= comparison_rhs),
        BinaryOp::LtS => bool_value(
            bits_to_signed(&comparison_lhs, comparison_width)
                < bits_to_signed(&comparison_rhs, comparison_width),
        ),
        BinaryOp::LeS => bool_value(
            bits_to_signed(&comparison_lhs, comparison_width)
                <= bits_to_signed(&comparison_rhs, comparison_width),
        ),
        BinaryOp::GtS => bool_value(
            bits_to_signed(&comparison_lhs, comparison_width)
                > bits_to_signed(&comparison_rhs, comparison_width),
        ),
        BinaryOp::GeS => bool_value(
            bits_to_signed(&comparison_lhs, comparison_width)
                >= bits_to_signed(&comparison_rhs, comparison_width),
        ),
        BinaryOp::LogicAnd => bool_value(!lhs.value.is_zero() && !rhs.value.is_zero()),
        BinaryOp::LogicOr => bool_value(!lhs.value.is_zero() || !rhs.value.is_zero()),
    };
    Some(SpecializedConstant {
        value: value & mask,
        mask: BigUint::from(0u8),
        width: result_width,
        signed: !matches!(
            op,
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
                | BinaryOp::LogicAnd
                | BinaryOp::LogicOr
        ) && result_signed,
    })
}

pub(super) fn specialize_unary_constant(
    op: UnaryOp,
    inner: &SpecializedConstant,
    result_width: usize,
    result_signed: bool,
) -> Option<SpecializedConstant> {
    if result_width == 0 {
        return None;
    }
    let result_mask = width_mask(result_width);
    if !inner.mask.is_zero() {
        if matches!(op, UnaryOp::ToTwoState) {
            let unknown = &inner.mask & &result_mask;
            return Some(SpecializedConstant {
                value: (&inner.value & &result_mask) & (&result_mask ^ unknown),
                mask: BigUint::from(0u8),
                width: result_width,
                signed: result_signed,
            });
        }
        if matches!(op, UnaryOp::LogicNot | UnaryOp::Or) {
            let inner_width_mask = width_mask(inner.width);
            let unknown = &inner.mask & &inner_width_mask;
            let known = &inner_width_mask ^ &unknown;
            let definite_ones = (&inner.value & &inner_width_mask) & known;
            let has_definite_one = !definite_ones.is_zero();
            let has_unknown = !unknown.is_zero();
            let (value, mask) = match op {
                UnaryOp::LogicNot if has_definite_one => (0u8, 0u8),
                UnaryOp::Or if has_definite_one => (1u8, 0u8),
                _ if has_unknown => (1u8, 1u8),
                UnaryOp::LogicNot => (1u8, 0u8),
                UnaryOp::Or => (0u8, 0u8),
                _ => unreachable!(),
            };
            return Some(SpecializedConstant {
                value: BigUint::from(value) & &result_mask,
                mask: BigUint::from(mask) & &result_mask,
                width: result_width,
                signed: false,
            });
        }
        return None;
    }
    let value = match op {
        UnaryOp::ToPlanes | UnaryOp::FromPlanes => return None,
        UnaryOp::Float { op, precision } => BigUint::from(celox_design::eval_float_unary(
            op,
            precision,
            inner.value.to_u64()?,
        )),
        UnaryOp::Ident | UnaryOp::ToTwoState => inner.value.clone(),
        UnaryOp::Minus => {
            let modulus = BigUint::from(1u8) << result_width;
            (&modulus - (&inner.value & &result_mask)) & &result_mask
        }
        UnaryOp::BitNot => &result_mask ^ (&inner.value & &result_mask),
        UnaryOp::LogicNot => BigUint::from(u8::from(inner.value.is_zero())),
        UnaryOp::And => BigUint::from(u8::from(
            (&inner.value & width_mask(inner.width)) == width_mask(inner.width),
        )),
        UnaryOp::Or => BigUint::from(u8::from(!inner.value.is_zero())),
        UnaryOp::Xor => BigUint::from(
            inner
                .value
                .iter_u64_digits()
                .map(u64::count_ones)
                .sum::<u32>()
                & 1,
        ),
        UnaryOp::PopCount => BigUint::from(
            inner
                .value
                .iter_u64_digits()
                .map(u64::count_ones)
                .sum::<u32>(),
        ),
        UnaryOp::CountLeadingZeros => {
            BigUint::from(inner.width.saturating_sub(inner.value.bits() as usize))
        }
        UnaryOp::CountTrailingZeros => {
            let zeros = inner
                .value
                .iter_u64_digits()
                .enumerate()
                .find_map(|(index, digit)| {
                    (digit != 0)
                        .then_some(index * u64::BITS as usize + digit.trailing_zeros() as usize)
                })
                .unwrap_or(inner.width)
                .min(inner.width);
            BigUint::from(zeros)
        }
    };
    Some(SpecializedConstant {
        value: value & result_mask,
        mask: BigUint::from(0u8),
        width: result_width,
        signed: matches!(
            op,
            UnaryOp::Ident | UnaryOp::ToTwoState | UnaryOp::Minus | UnaryOp::BitNot
        ) && result_signed,
    })
}

pub(super) fn width_mask(width: usize) -> BigUint {
    if width == 0 {
        BigUint::from(0u8)
    } else {
        (BigUint::from(1u8) << width) - BigUint::from(1u8)
    }
}

pub(super) fn bits_to_signed(value: &BigUint, width: usize) -> BigInt {
    let value = value & width_mask(width);
    if width != 0 && ((&value >> (width - 1)) & BigUint::from(1u8)) == BigUint::from(1u8) {
        BigInt::from_biguint(Sign::Plus, value)
            - BigInt::from_biguint(Sign::Plus, BigUint::from(1u8) << width)
    } else {
        BigInt::from_biguint(Sign::Plus, value)
    }
}

pub(super) fn signed_to_bits(value: BigInt, width: usize) -> Option<BigUint> {
    let modulus = BigInt::from_biguint(Sign::Plus, BigUint::from(1u8) << width);
    let normalized = ((value % &modulus) + &modulus) % &modulus;
    normalized.to_biguint()
}
