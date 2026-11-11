//! SLT helpers: the four-state value of a constant tree, the signedness of
//! a node, and the truth of a condition.

use super::*;

/// The value of a constant SLT tree, when every bit is known.
pub(in crate::lowering) fn slt_const<A: std::hash::Hash + Eq + Clone>(
    arena: &SLTNodeArena<A>,
    consts: &mut ConstCache,
    node: NodeId,
) -> Option<(BigUint, usize)> {
    let (value, mask, width) = slt_const4(arena, consts, node)?;
    mask.is_zero().then_some((value, width))
}

/// A four-state constant: `(payload, mask, width)`. An unknown bit has its
/// mask bit set; its payload bit is 1 for X and 0 for Z.
pub(in crate::lowering) type Const4 = (BigUint, BigUint, usize);

pub(super) fn mask_of(width: usize) -> BigUint {
    (BigUint::from(1u8) << width) - BigUint::from(1u8)
}

/// The [`slt_const4`] values of the nodes of one arena. Interned nodes never
/// change, so a value stays valid while the arena grows; a cache must not be
/// shared between arenas.
#[derive(Default)]
pub(in crate::lowering) struct ConstCache(HashMap<NodeId, Option<Const4>>);

/// The four-state value of a constant SLT tree (IEEE 1800-2023 11.4 for the
/// treatment of unknown bits).
///
/// Symbolic execution builds deep DAGs (one level per unrolled loop
/// iteration, for example), so the tree is evaluated with an explicit stack,
/// and each node is evaluated once per `consts`.
pub(in crate::lowering) fn slt_const4<A: std::hash::Hash + Eq + Clone>(
    arena: &SLTNodeArena<A>,
    consts: &mut ConstCache,
    node: NodeId,
) -> Option<Const4> {
    let values = &mut consts.0;
    let mut stack = vec![node];
    while let Some(&top) = stack.last() {
        if values.contains_key(&top) {
            stack.pop();
            continue;
        }
        // Evaluate `top` from the operand values known so far. The first
        // operand it needs that is not yet evaluated is pushed, and `top` is
        // evaluated again once that operand is known.
        let mut missing = None;
        let mut child = |id: NodeId| match values.get(&id) {
            Some(value) => value.clone(),
            None => {
                missing.get_or_insert(id);
                None
            }
        };
        let value = slt_const4_node(arena, top, &mut child);
        match missing {
            Some(operand) => stack.push(operand),
            None => {
                values.insert(top, value);
                stack.pop();
            }
        }
    }
    values[&node].clone()
}

/// The four-state value of `node`, given the values of its operands.
pub(super) fn slt_const4_node<A: std::hash::Hash + Eq + Clone>(
    arena: &SLTNodeArena<A>,
    node: NodeId,
    child: &mut dyn FnMut(NodeId) -> Option<Const4>,
) -> Option<Const4> {
    let width = celox_slt::get_width(node, arena);
    let all = mask_of(width);
    let unknown_all = || (all.clone(), all.clone(), width);
    let bit1 = |value: Option<bool>| -> Const4 {
        match value {
            Some(value) => (BigUint::from(u8::from(value)), BigUint::zero(), 1),
            None => (BigUint::from(1u8), BigUint::from(1u8), 1),
        }
    };
    let signed_value = |value: &BigUint, width: usize| -> num_bigint::BigInt {
        if width > 0 && value.bit(width as u64 - 1) {
            num_bigint::BigInt::from(value.clone()) - (num_bigint::BigInt::from(1u8) << width)
        } else {
            num_bigint::BigInt::from(value.clone())
        }
    };
    // The truth of a value: Some(true) for a known one bit, Some(false) when
    // every bit is a known zero, None otherwise.
    let truth = |(value, mask, width): &Const4| -> Option<bool> {
        let known = mask_of(*width) ^ (mask & mask_of(*width));
        if !(value & &known).is_zero() {
            Some(true)
        } else if (mask & mask_of(*width)).is_zero() {
            Some(false)
        } else {
            None
        }
    };
    let result: Const4 = match arena.get(node) {
        SLTNode::Constant(value, mask, width, _) => {
            (value & mask_of(*width), mask & mask_of(*width), *width)
        }
        SLTNode::Unary(op, inner) => {
            let (value, mask, inner_width) = child(*inner)?;
            let inner_all = mask_of(inner_width);
            match op {
                UnaryOp::Float { .. } | UnaryOp::ToPlanes | UnaryOp::FromPlanes => return None,
                UnaryOp::Ident => (value, mask, width),
                UnaryOp::ToTwoState => (
                    (&value & (&inner_all ^ &mask)) & &all,
                    BigUint::zero(),
                    width,
                ),
                UnaryOp::BitNot => {
                    // Unknown bits stay unknown (as X).
                    ((&all ^ &value) | &mask, mask, width)
                }
                UnaryOp::Minus => {
                    if !mask.is_zero() {
                        return Some(unknown_all());
                    }
                    (
                        (&all + BigUint::from(1u8) - (&value & &all)) & &all,
                        BigUint::zero(),
                        width,
                    )
                }
                UnaryOp::LogicNot => bit1(truth(&(value, mask, inner_width)).map(|value| !value)),
                UnaryOp::And => {
                    let known_zero = &inner_all ^ (&value | &mask);
                    if !known_zero.is_zero() {
                        bit1(Some(false))
                    } else if !mask.is_zero() {
                        bit1(None)
                    } else {
                        bit1(Some(true))
                    }
                }
                UnaryOp::Or => bit1(truth(&(value, mask, inner_width))),
                UnaryOp::Xor => {
                    if !mask.is_zero() {
                        bit1(None)
                    } else {
                        bit1(Some(value.count_ones() % 2 == 1))
                    }
                }
                UnaryOp::PopCount | UnaryOp::CountLeadingZeros | UnaryOp::CountTrailingZeros => {
                    if !mask.is_zero() {
                        return Some(unknown_all());
                    }
                    let result = match op {
                        UnaryOp::PopCount => BigUint::from(value.count_ones()),
                        UnaryOp::CountLeadingZeros => {
                            BigUint::from(inner_width.saturating_sub(value.bits() as usize))
                        }
                        _ => BigUint::from(
                            value
                                .trailing_zeros()
                                .map_or(inner_width, |zeros| (zeros as usize).min(inner_width)),
                        ),
                    };
                    (result & &all, BigUint::zero(), width)
                }
            }
        }
        SLTNode::Binary(left, op, right) => {
            // A logical operator that its left operand decides does not
            // evaluate its right operand (IEEE 1800-2023 11.4.7), which need
            // not be constant.
            if matches!(op, BinaryOp::LogicAnd | BinaryOp::LogicOr) {
                let decides = matches!(op, BinaryOp::LogicOr);
                if child(*left).and_then(|left| truth(&left)) == Some(decides) {
                    return Some((BigUint::from(u8::from(decides)), BigUint::zero(), width));
                }
            }
            let (lv, lm, lw) = child(*left)?;
            let (rv, rm, rw) = child(*right)?;
            let both_signed = node_is_signed(arena, *left) && node_is_signed(arena, *right);
            let compare_width = lw.max(rw);
            let extend = |value: &BigUint,
                          mask: &BigUint,
                          from: usize,
                          signed: bool|
             -> (BigUint, BigUint) {
                if signed && from > 0 && (value.bit(from as u64 - 1) || mask.bit(from as u64 - 1)) {
                    let fill = mask_of(compare_width) ^ mask_of(from);
                    let value = if value.bit(from as u64 - 1) {
                        value | &fill
                    } else {
                        value.clone()
                    };
                    let mask = if mask.bit(from as u64 - 1) {
                        mask | &fill
                    } else {
                        mask.clone()
                    };
                    (value, mask)
                } else {
                    (value.clone(), mask.clone())
                }
            };
            let any_unknown = !lm.is_zero() || !rm.is_zero();
            match op {
                BinaryOp::And => {
                    let zero = (mask_of(lw) ^ (&lv | &lm)) | (mask_of(rw) ^ (&rv | &rm));
                    let one = (&lv & (mask_of(lw) ^ &lm)) & (&rv & (mask_of(rw) ^ &rm));
                    let unknown = &all ^ ((&zero | &one) & &all);
                    ((&one | &unknown) & &all, unknown & &all, width)
                }
                BinaryOp::Or => {
                    let one = (&lv & (mask_of(lw) ^ &lm)) | (&rv & (mask_of(rw) ^ &rm));
                    let zero = (mask_of(lw) ^ (&lv | &lm)) & (mask_of(rw) ^ (&rv | &rm));
                    let unknown = &all ^ ((&zero | &one) & &all);
                    ((&one | &unknown) & &all, unknown & &all, width)
                }
                BinaryOp::Xor => {
                    let unknown = (&lm | &rm) & &all;
                    (((&lv ^ &rv) | &unknown) & &all, unknown, width)
                }
                BinaryOp::EqCase | BinaryOp::NeCase => {
                    let (lv, lm) = extend(&lv, &lm, lw, both_signed);
                    let (rv, rm) = extend(&rv, &rm, rw, both_signed);
                    let equal = lv == rv && lm == rm;
                    bit1(Some(equal == (*op == BinaryOp::EqCase)))
                }
                BinaryOp::Eq | BinaryOp::Ne | BinaryOp::EqWildcard | BinaryOp::NeWildcard => {
                    let (lv, lm) = extend(&lv, &lm, lw, both_signed);
                    let (rv, rm) = extend(&rv, &rm, rw, both_signed);
                    let wildcard = matches!(op, BinaryOp::EqWildcard | BinaryOp::NeWildcard);
                    let care = if wildcard {
                        mask_of(compare_width) ^ &rm
                    } else {
                        mask_of(compare_width)
                    };
                    let unknown = if wildcard {
                        &lm & &care
                    } else {
                        (&lm | &rm) & &care
                    };
                    let known = &care ^ &unknown;
                    let differs = !(((&lv ^ &rv) & &known).is_zero());
                    let equal = if differs {
                        Some(false)
                    } else if !unknown.is_zero() {
                        None
                    } else {
                        Some(true)
                    };
                    let positive = matches!(op, BinaryOp::Eq | BinaryOp::EqWildcard);
                    bit1(equal.map(|equal| equal == positive))
                }
                BinaryOp::LogicAnd => {
                    let left = truth(&(lv, lm, lw));
                    let right = truth(&(rv, rm, rw));
                    bit1(match (left, right) {
                        (Some(false), _) | (_, Some(false)) => Some(false),
                        (Some(true), Some(true)) => Some(true),
                        _ => None,
                    })
                }
                BinaryOp::LogicOr => {
                    let left = truth(&(lv, lm, lw));
                    let right = truth(&(rv, rm, rw));
                    bit1(match (left, right) {
                        (Some(true), _) | (_, Some(true)) => Some(true),
                        (Some(false), Some(false)) => Some(false),
                        _ => None,
                    })
                }
                BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar if rm.is_zero() => {
                    // Unknown bits of the shifted operand move with it; only an
                    // unknown amount makes the whole result unknown.
                    let shift = rv.to_usize().unwrap_or(usize::MAX);
                    match op {
                        BinaryOp::Shl => {
                            if shift >= width {
                                (BigUint::zero(), BigUint::zero(), width)
                            } else {
                                ((&lv << shift) & &all, (&lm << shift) & &all, width)
                            }
                        }
                        BinaryOp::Shr => {
                            if shift >= lw {
                                (BigUint::zero(), BigUint::zero(), width)
                            } else {
                                ((&lv >> shift) & &all, (&lm >> shift) & &all, width)
                            }
                        }
                        _ => {
                            let shift = shift.min(lw);
                            let fill = mask_of(lw) ^ mask_of(lw - shift);
                            let sign_value = lw > 0 && lv.bit(lw as u64 - 1);
                            let sign_mask = lw > 0 && lm.bit(lw as u64 - 1);
                            let mut value = if shift >= lw {
                                BigUint::zero()
                            } else {
                                &lv >> shift
                            };
                            let mut mask = if shift >= lw {
                                BigUint::zero()
                            } else {
                                &lm >> shift
                            };
                            if sign_value {
                                value |= &fill;
                            }
                            if sign_mask {
                                mask |= &fill;
                            }
                            (value & &all, mask & &all, width)
                        }
                    }
                }
                _ if any_unknown => {
                    if matches!(
                        op,
                        BinaryOp::LtU
                            | BinaryOp::LtS
                            | BinaryOp::LeU
                            | BinaryOp::LeS
                            | BinaryOp::GtU
                            | BinaryOp::GtS
                            | BinaryOp::GeU
                            | BinaryOp::GeS
                    ) {
                        bit1(None)
                    } else {
                        unknown_all()
                    }
                }
                _ => {
                    let extend2 = |value: &BigUint, from: usize, signed: bool| -> BigUint {
                        extend(value, &BigUint::zero(), from, signed).0
                    };
                    let compare = |signed: bool| -> std::cmp::Ordering {
                        let l = extend2(&lv, lw, signed);
                        let r = extend2(&rv, rw, signed);
                        if signed {
                            signed_value(&l, compare_width).cmp(&signed_value(&r, compare_width))
                        } else {
                            l.cmp(&r)
                        }
                    };
                    let value = match op {
                        BinaryOp::Add => (&lv + &rv) & &all,
                        BinaryOp::Sub => {
                            ((&all + BigUint::from(1u8) + (&lv & &all)) - (&rv & &all)) & &all
                        }
                        BinaryOp::Mul => (&lv * &rv) & &all,
                        BinaryOp::DivU | BinaryOp::RemU => {
                            if rv.is_zero() {
                                return Some(unknown_all());
                            }
                            if *op == BinaryOp::DivU {
                                &lv / &rv
                            } else {
                                &lv % &rv
                            }
                        }
                        BinaryOp::DivS | BinaryOp::RemS => {
                            let l = signed_value(&extend2(&lv, lw, true), compare_width);
                            let r = signed_value(&extend2(&rv, rw, true), compare_width);
                            if r.is_zero() {
                                return Some(unknown_all());
                            }
                            let value = if *op == BinaryOp::DivS { l / r } else { l % r };
                            let modulus = num_bigint::BigInt::from(1u8) << width;
                            (((value % &modulus) + &modulus) % &modulus).to_biguint()?
                        }
                        BinaryOp::Shl => {
                            let shift = rv.to_usize().unwrap_or(usize::MAX);
                            if shift >= width {
                                BigUint::zero()
                            } else {
                                (&lv << shift) & &all
                            }
                        }
                        BinaryOp::Shr => {
                            let shift = rv.to_usize().unwrap_or(usize::MAX);
                            if shift >= lw {
                                BigUint::zero()
                            } else {
                                &lv >> shift
                            }
                        }
                        BinaryOp::Sar => {
                            let shift = rv.to_usize().unwrap_or(usize::MAX).min(lw);
                            let negative = lw > 0 && lv.bit(lw as u64 - 1);
                            let shifted = if shift >= lw {
                                BigUint::zero()
                            } else {
                                &lv >> shift
                            };
                            if negative {
                                (shifted | (mask_of(lw) ^ mask_of(lw - shift))) & &all
                            } else {
                                shifted
                            }
                        }
                        BinaryOp::LtU => BigUint::from(u8::from(compare(false).is_lt())),
                        BinaryOp::LtS => BigUint::from(u8::from(compare(true).is_lt())),
                        BinaryOp::LeU => BigUint::from(u8::from(compare(false).is_le())),
                        BinaryOp::LeS => BigUint::from(u8::from(compare(true).is_le())),
                        BinaryOp::GtU => BigUint::from(u8::from(compare(false).is_gt())),
                        BinaryOp::GtS => BigUint::from(u8::from(compare(true).is_gt())),
                        BinaryOp::GeU => BigUint::from(u8::from(compare(false).is_ge())),
                        BinaryOp::GeS => BigUint::from(u8::from(compare(true).is_ge())),
                        _ => return None,
                    };
                    (value & &all, BigUint::zero(), width)
                }
            }
        }
        SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        } => {
            let merge_arms = |child: &mut dyn FnMut(NodeId) -> Option<Const4>| -> Option<Const4> {
                let (tv, tm, _) = child(*then_expr)?;
                let (ev, em, _) = child(*else_expr)?;
                // Bits equal in both arms (an unknown bit too) keep their
                // value; the others become X.
                let differ = ((&tv ^ &ev) | (&tm ^ &em)) & &all;
                let keep = &all ^ &differ;
                Some((
                    ((&tv & &keep) | &differ) & &all,
                    ((&tm & &keep) | &differ) & &all,
                    width,
                ))
            };
            match child(*cond) {
                Some(condition) => match truth(&condition) {
                    Some(true) => child(*then_expr)?,
                    Some(false) => child(*else_expr)?,
                    None => merge_arms(child)?,
                },
                // A run-time condition selecting between equal constants.
                None => {
                    let (tv, tm, _) = child(*then_expr)?;
                    let (ev, em, _) = child(*else_expr)?;
                    if tv != ev || tm != em {
                        return None;
                    }
                    (tv, tm, width)
                }
            }
        }
        SLTNode::Concat(parts) => {
            let mut value = BigUint::zero();
            let mut mask = BigUint::zero();
            for (part, part_width) in parts {
                let (part_value, part_mask, _) = child(*part)?;
                value = (value << *part_width) | (part_value & mask_of(*part_width));
                mask = (mask << *part_width) | (part_mask & mask_of(*part_width));
            }
            (value, mask, width)
        }
        SLTNode::Slice { expr, access } => {
            let (value, mask, _) = child(*expr)?;
            let slice = mask_of(access.msb - access.lsb + 1);
            (
                (value >> access.lsb) & &slice,
                (mask >> access.lsb) & &slice,
                width,
            )
        }
        SLTNode::Capture { expr, .. } => child(*expr)?,
        _ => return None,
    };
    Some((result.0 & mask_of(width), result.1 & mask_of(width), width))
}

/// Whether the lowering treats the value of `node` as signed when it extends it.
///
/// A node is signed when every operand that determines its signedness is;
/// the operands are visited with an explicit stack because symbolic
/// execution builds deep DAGs.
pub(in crate::lowering) fn node_is_signed<A: std::hash::Hash + Eq + Clone>(
    arena: &SLTNodeArena<A>,
    node: NodeId,
) -> bool {
    let mut visited = HashSet::default();
    let mut stack = vec![node];
    while let Some(node) = stack.pop() {
        if !visited.insert(node) {
            continue;
        }
        let signed = match arena.get(node) {
            SLTNode::Input { signed, .. } => *signed,
            SLTNode::Constant(_, _, _, signed) => *signed,
            SLTNode::Binary(left, op, right) => match op {
                BinaryOp::DivS | BinaryOp::RemS => true,
                BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar => {
                    stack.push(*left);
                    true
                }
                BinaryOp::Add
                | BinaryOp::Sub
                | BinaryOp::Mul
                | BinaryOp::And
                | BinaryOp::Or
                | BinaryOp::Xor => {
                    stack.extend([*right, *left]);
                    true
                }
                _ => false,
            },
            SLTNode::Unary(
                UnaryOp::Ident | UnaryOp::ToTwoState | UnaryOp::Minus | UnaryOp::BitNot,
                inner,
            )
            | SLTNode::Capture { expr: inner, .. } => {
                stack.push(*inner);
                true
            }
            SLTNode::Mux {
                then_expr,
                else_expr,
                ..
            } => {
                stack.extend([*else_expr, *then_expr]);
                true
            }
            SLTNode::ForFold {
                loop_signed,
                result: celox_slt::SLTForFoldResult::State(_),
                ..
            } => *loop_signed,
            _ => false,
        };
        if !signed {
            return false;
        }
    }
    true
}

/// The boolean constant `node` holds, if it is one.
pub(in crate::lowering) fn slt_bool<A: std::hash::Hash + Eq + Clone>(
    arena: &SLTNodeArena<A>,
    consts: &mut ConstCache,
    node: NodeId,
) -> Option<bool> {
    slt_const(arena, consts, node).map(|(value, _)| !value.is_zero())
}

pub(in crate::lowering) fn slt_constant<A: std::hash::Hash + Eq + Clone>(
    arena: &mut SLTNodeArena<A>,
    value: BigUint,
    width: usize,
    signed: bool,
) -> Result<NodeId, sv::AnalyzerError> {
    arena
        .alloc(SLTNode::Constant(value, BigUint::default(), width, signed))
        .map_err(slt_error)
}

pub(in crate::lowering) fn slt_unknown<A: std::hash::Hash + Eq + Clone>(
    arena: &mut SLTNodeArena<A>,
    width: usize,
) -> Result<NodeId, sv::AnalyzerError> {
    let mask = (BigUint::from(1u8) << width) - BigUint::from(1u8);
    arena
        .alloc(SLTNode::Constant(mask.clone(), mask, width, false))
        .map_err(slt_error)
}

pub(in crate::lowering) fn slt_error(error: impl std::fmt::Display) -> sv::AnalyzerError {
    unsupported(format!("procedural lowering: {error}"))
}

/// The truth of a procedural condition: a known nonzero bit (IEEE 1800-2023
/// 12.4). Unknown bits count as false.
pub(in crate::lowering) fn slt_truth<A: std::hash::Hash + Eq + Clone>(
    arena: &mut SLTNodeArena<A>,
    condition: NodeId,
) -> Result<NodeId, sv::AnalyzerError> {
    if let SLTNode::Constant(value, mask, width, _) = arena.get(condition) {
        let width_mask = (BigUint::from(1u8) << *width) - BigUint::from(1u8);
        let known = &width_mask ^ (mask & &width_mask);
        let truth = !(value & known).is_zero();
        return slt_constant(arena, BigUint::from(u8::from(truth)), 1, false);
    }
    if celox_slt::get_width(condition, arena) == 1
        && let SLTNode::Unary(UnaryOp::ToTwoState, _) = arena.get(condition)
    {
        return Ok(condition);
    }
    let any = arena
        .alloc(SLTNode::Unary(UnaryOp::Or, condition))
        .map_err(slt_error)?;
    arena
        .alloc(SLTNode::Unary(UnaryOp::ToTwoState, any))
        .map_err(slt_error)
}

/// Whether the truth of a condition is ambiguous: no bit is one and some
/// bit is unknown (IEEE 1800-2023 11.4.11). A condition with a known one is
/// true even when other bits are unknown.
pub(in crate::lowering) fn slt_truth_unknown<A: std::hash::Hash + Eq + Clone>(
    arena: &mut SLTNodeArena<A>,
    consts: &mut ConstCache,
    condition: NodeId,
) -> Result<NodeId, sv::AnalyzerError> {
    let any = arena
        .alloc(SLTNode::Unary(UnaryOp::Or, condition))
        .map_err(slt_error)?;
    let known = arena
        .alloc(SLTNode::Unary(UnaryOp::ToTwoState, any))
        .map_err(slt_error)?;
    let is_known = arena
        .alloc(SLTNode::Binary(any, BinaryOp::EqCase, known))
        .map_err(slt_error)?;
    slt_not(arena, consts, is_known)
}

/// Whether a value is not logically false: some bit is one or unknown. The
/// second operand of `&&` is skipped only when the first is false
/// (IEEE 1800-2023 11.4.7); an ambiguous first operand still evaluates it.
pub(in crate::lowering) fn slt_not_false<A: std::hash::Hash + Eq + Clone>(
    arena: &mut SLTNodeArena<A>,
    value: NodeId,
) -> Result<NodeId, sv::AnalyzerError> {
    let width = celox_slt::get_width(value, arena);
    let zero = slt_constant(arena, BigUint::zero(), width, false)?;
    arena
        .alloc(SLTNode::Binary(value, BinaryOp::NeCase, zero))
        .map_err(slt_error)
}

pub(in crate::lowering) fn slt_not<A: std::hash::Hash + Eq + Clone>(
    arena: &mut SLTNodeArena<A>,
    consts: &mut ConstCache,
    condition: NodeId,
) -> Result<NodeId, sv::AnalyzerError> {
    if let Some(value) = slt_bool(arena, consts, condition) {
        return slt_constant(arena, BigUint::from(u8::from(!value)), 1, false);
    }
    arena
        .alloc(SLTNode::Unary(UnaryOp::LogicNot, condition))
        .map_err(slt_error)
}

pub(in crate::lowering) fn slt_and<A: std::hash::Hash + Eq + Clone>(
    arena: &mut SLTNodeArena<A>,
    consts: &mut ConstCache,
    left: NodeId,
    right: NodeId,
) -> Result<NodeId, sv::AnalyzerError> {
    match (
        slt_bool(arena, consts, left),
        slt_bool(arena, consts, right),
    ) {
        (Some(false), _) | (_, Some(false)) => slt_constant(arena, BigUint::zero(), 1, false),
        (Some(true), _) => Ok(right),
        (_, Some(true)) => Ok(left),
        _ => arena
            .alloc(SLTNode::Binary(left, BinaryOp::LogicAnd, right))
            .map_err(slt_error),
    }
}

pub(in crate::lowering) fn slt_or<A: std::hash::Hash + Eq + Clone>(
    arena: &mut SLTNodeArena<A>,
    consts: &mut ConstCache,
    left: NodeId,
    right: NodeId,
) -> Result<NodeId, sv::AnalyzerError> {
    match (
        slt_bool(arena, consts, left),
        slt_bool(arena, consts, right),
    ) {
        (Some(true), _) | (_, Some(true)) => slt_constant(arena, BigUint::from(1u8), 1, false),
        (Some(false), _) => Ok(right),
        (_, Some(false)) => Ok(left),
        _ => arena
            .alloc(SLTNode::Binary(left, BinaryOp::LogicOr, right))
            .map_err(slt_error),
    }
}
