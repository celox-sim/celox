//! Real values use IEEE payload bits in storage and explicit floating operations.
use super::*;
use celox_sir::{FloatBinaryOp as FB, FloatPrecision as FP, FloatUnaryOp as FU};

type Value = (NodeId, HashSet<VarAtomBase<SourceVarId>>);

pub(super) fn precision(
    expr: &sv::ir::Expr,
    variables: &HashMap<SourceVarId, SvVariable>,
    names: &HashMap<String, SourceVarId>,
) -> Option<FP> {
    use sv::ir::{Expr as E, SystemFunction as F};
    match expr {
        E::Ident(name) => variables.get(names.get(name)?).and_then(|v| v.float),
        E::Literal(text)
            if text.contains(['.', 'e', 'E']) && text.replace('_', "").parse::<f64>().is_ok() =>
        {
            Some(FP::F64)
        }
        E::Call {
            callee: sv::ir::Callee::System(F::Bitstoreal | F::Itor | F::Realtime),
            ..
        } => Some(FP::F64),
        E::Call {
            callee: sv::ir::Callee::System(F::Bitstoshortreal),
            ..
        } => Some(FP::F32),
        E::Unary {
            op: sv::ir::UnaryOp::Plus | sv::ir::UnaryOp::Minus | sv::ir::UnaryOp::ToTwoState,
            expr,
        } => precision(expr, variables, names),
        E::Mux {
            then_expr,
            else_expr,
            ..
        } => match (
            precision(then_expr, variables, names),
            precision(else_expr, variables, names),
        ) {
            (Some(FP::F64), _) | (_, Some(FP::F64)) => Some(FP::F64),
            (Some(FP::F32), _) | (_, Some(FP::F32)) => Some(FP::F32),
            _ => None,
        },
        E::Binary { left, op, right } if binary(*op).is_some_and(|(_, comparison)| !comparison) => {
            match (
                precision(left, variables, names),
                precision(right, variables, names),
            ) {
                (Some(FP::F64), _) | (_, Some(FP::F64)) => Some(FP::F64),
                (Some(FP::F32), _) | (_, Some(FP::F32)) => Some(FP::F32),
                _ => None,
            }
        }
        _ => None,
    }
}

fn binary(op: sv::ir::BinaryOp) -> Option<(FB, bool)> {
    use sv::ir::BinaryOp as B;
    Some(match op {
        B::Add => (FB::Add, false),
        B::Sub => (FB::Sub, false),
        B::Mul => (FB::Mul, false),
        B::Div => (FB::Div, false),
        B::Pow => (FB::Pow, false),
        B::Eq => (FB::Eq, true),
        B::Ne => (FB::Ne, true),
        B::Lt => (FB::Lt, true),
        B::Le => (FB::Le, true),
        B::Gt => (FB::Gt, true),
        B::Ge => (FB::Ge, true),
        _ => return None,
    })
}

pub(super) fn handles(
    expr: &sv::ir::Expr,
    variables: &HashMap<SourceVarId, SvVariable>,
    names: &HashMap<String, SourceVarId>,
) -> bool {
    use sv::ir::{Expr as E, SystemFunction as F};
    precision(expr, variables, names).is_some()
        || match expr {
            E::Binary { left, right, .. } => {
                precision(left, variables, names).is_some()
                    || precision(right, variables, names).is_some()
            }
            E::Unary { expr, .. } | E::Resize { expr, .. } => {
                precision(expr, variables, names).is_some()
            }
            E::Call {
                callee: sv::ir::Callee::System(F::Rtoi | F::Realtobits | F::Shortrealtobits),
                ..
            } => true,
            _ => false,
        }
}

pub(super) fn convert<A: std::hash::Hash + Eq + Clone>(
    arena: &mut SLTNodeArena<A>,
    node: NodeId,
    from: Option<FP>,
    to: Option<FP>,
    width: usize,
    signed: bool,
    round: bool,
) -> Option<NodeId> {
    let node = if from.is_none() && to.is_some() {
        arena
            .alloc(SLTNode::Unary(UnaryOp::ToTwoState, node))
            .ok()?
    } else {
        node
    };
    let (precision, op) = match (from, to) {
        (Some(a), Some(b)) if a == b => return Some(node),
        (Some(a), Some(b)) => (a, FU::Convert { to: b }),
        (None, Some(p)) => {
            let width = celox_slt::get_width(node, arena);
            (
                p,
                if signed {
                    FU::FromSigned { width }
                } else {
                    FU::FromUnsigned { width }
                },
            )
        }
        (Some(p), None) => (
            p,
            if signed {
                FU::ToSigned { width, round }
            } else {
                FU::ToUnsigned { width, round }
            },
        ),
        (None, None) => return Some(node),
    };
    arena
        .alloc(SLTNode::Unary(UnaryOp::Float { op, precision }, node))
        .ok()
}

pub(super) fn lower(
    expr: &sv::ir::Expr,
    variables: &HashMap<SourceVarId, SvVariable>,
    names: &HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    types: &HashMap<String, (usize, bool)>,
    arena: &mut SLTNodeArena<SourceVarId>,
) -> Option<Value> {
    use sv::ir::{Expr as E, SystemFunction as F};
    let recur = |expr: &E, arena: &mut SLTNodeArena<SourceVarId>| {
        lower_expr_with_context(expr, variables, names, constants, types, arena, None, None)
    };
    let operand = |expr: &E, to: FP, arena: &mut SLTNodeArena<SourceVarId>| -> Option<Value> {
        let (node, sources) = recur(expr, arena)?;
        let signed = sv_expr_is_signed_with_parameters(expr, variables, names, types);
        Some((
            convert(
                arena,
                node,
                precision(expr, variables, names),
                Some(to),
                0,
                signed,
                true,
            )?,
            sources,
        ))
    };
    match expr {
        E::Ident(name) => {
            let id = *names.get(name)?;
            let var = variables.get(&id)?;
            let node = arena
                .alloc(SLTNode::Input {
                    variable: id,
                    signed: false,
                    index: Vec::new(),
                    access: BitAccess::new(0, var.width - 1),
                })
                .ok()?;
            Some((
                node,
                [VarAtomBase::new(id, 0, var.width - 1)]
                    .into_iter()
                    .collect(),
            ))
        }
        E::Literal(text) => {
            let bits = text.replace('_', "").parse::<f64>().ok()?.to_bits();
            Some((
                arena
                    .alloc(SLTNode::Constant(
                        BigUint::from(bits),
                        BigUint::default(),
                        64,
                        false,
                    ))
                    .ok()?,
                HashSet::default(),
            ))
        }
        E::Binary { left, op, right } => {
            if matches!(op, sv::ir::BinaryOp::LogicAnd | sv::ir::BinaryOp::LogicOr) {
                let (mut a, mut sources) = recur(left, arena)?;
                let (mut b, other) = recur(right, arena)?;
                sources.extend(other);
                if let Some(p) = precision(left, variables, names) {
                    a = arena
                        .alloc(SLTNode::Unary(
                            UnaryOp::Float {
                                op: FU::ToBool,
                                precision: p,
                            },
                            a,
                        ))
                        .ok()?;
                }
                if let Some(p) = precision(right, variables, names) {
                    b = arena
                        .alloc(SLTNode::Unary(
                            UnaryOp::Float {
                                op: FU::ToBool,
                                precision: p,
                            },
                            b,
                        ))
                        .ok()?;
                }
                let op = if *op == sv::ir::BinaryOp::LogicAnd {
                    BinaryOp::LogicAnd
                } else {
                    BinaryOp::LogicOr
                };
                return Some((arena.alloc(SLTNode::Binary(a, op, b)).ok()?, sources));
            }
            let (op, _) = binary(*op)?;
            let p = match (
                precision(left, variables, names),
                precision(right, variables, names),
            ) {
                (Some(FP::F64), _) | (_, Some(FP::F64)) => FP::F64,
                _ => FP::F32,
            };
            let (left, mut sources) = operand(left, p, arena)?;
            let (right, other) = operand(right, p, arena)?;
            sources.extend(other);
            Some((
                arena
                    .alloc(SLTNode::Binary(
                        left,
                        BinaryOp::Float { op, precision: p },
                        right,
                    ))
                    .ok()?,
                sources,
            ))
        }
        E::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            let p = precision(expr, variables, names)?;
            let (condition_node, mut sources) = recur(condition, arena)?;
            let condition_node = if let Some(p) = precision(condition, variables, names) {
                arena
                    .alloc(SLTNode::Unary(
                        UnaryOp::Float {
                            op: FU::ToBool,
                            precision: p,
                        },
                        condition_node,
                    ))
                    .ok()?
            } else {
                condition_node
            };
            let (then_expr, other) = operand(then_expr, p, arena)?;
            sources.extend(other);
            let (else_expr, other) = operand(else_expr, p, arena)?;
            sources.extend(other);
            Some((
                arena
                    .alloc(SLTNode::Mux {
                        cond: condition_node,
                        then_expr,
                        else_expr,
                    })
                    .ok()?,
                sources,
            ))
        }
        E::Resize {
            expr,
            width,
            signed,
        } => {
            let p = precision(expr, variables, names)?;
            let (node, sources) = recur(expr, arena)?;
            Some((
                convert(arena, node, Some(p), None, *width, *signed, true)?,
                sources,
            ))
        }
        E::Unary { op, expr } => {
            let p = precision(expr, variables, names)?;
            let (node, sources) = recur(expr, arena)?;
            let node = match op {
                sv::ir::UnaryOp::Plus | sv::ir::UnaryOp::ToTwoState => node,
                sv::ir::UnaryOp::Minus => arena
                    .alloc(SLTNode::Unary(
                        UnaryOp::Float {
                            op: FU::Neg,
                            precision: p,
                        },
                        node,
                    ))
                    .ok()?,
                sv::ir::UnaryOp::LogicNot => {
                    let truth = arena
                        .alloc(SLTNode::Unary(
                            UnaryOp::Float {
                                op: FU::ToBool,
                                precision: p,
                            },
                            node,
                        ))
                        .ok()?;
                    arena.alloc(SLTNode::Unary(UnaryOp::LogicNot, truth)).ok()?
                }
                _ => return None,
            };
            Some((node, sources))
        }
        E::Call {
            callee: sv::ir::Callee::System(function),
            args,
            ..
        } => {
            let arg = args.first()?.as_ref()?;
            match function {
                F::Bitstoreal | F::Bitstoshortreal => {
                    let (node, sources) = recur(arg, arena)?;
                    let width = if *function == F::Bitstoreal { 64 } else { 32 };
                    Some((
                        coerce_node_width(arena, node, Some(width), false).ok()?,
                        sources,
                    ))
                }
                F::Realtobits | F::Shortrealtobits => operand(
                    arg,
                    if *function == F::Realtobits {
                        FP::F64
                    } else {
                        FP::F32
                    },
                    arena,
                ),
                F::Itor => {
                    let (node, sources) = recur(arg, arena)?;
                    let node = if let Some(p) = precision(arg, variables, names) {
                        convert(arena, node, Some(p), None, 32, true, true)?
                    } else {
                        coerce_node_width(
                            arena,
                            node,
                            Some(32),
                            sv_expr_is_signed_with_parameters(arg, variables, names, types),
                        )
                        .ok()?
                    };
                    Some((
                        convert(arena, node, None, Some(FP::F64), 32, true, true)?,
                        sources,
                    ))
                }
                F::Rtoi => {
                    let p = precision(arg, variables, names).unwrap_or(FP::F64);
                    let (node, sources) = operand(arg, p, arena)?;
                    Some((
                        convert(arena, node, Some(p), None, 32, true, false)?,
                        sources,
                    ))
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// Preserve floating zero semantics in conditions, including negative zero.
pub(super) fn condition(
    expr: &sv::ir::Expr,
    variables: &HashMap<SourceVarId, SvVariable>,
    names: &HashMap<String, SourceVarId>,
) -> sv::ir::Expr {
    if precision(expr, variables, names).is_none() {
        return expr.clone();
    }
    sv::ir::Expr::Unary {
        op: sv::ir::UnaryOp::LogicNot,
        expr: Box::new(sv::ir::Expr::Unary {
            op: sv::ir::UnaryOp::LogicNot,
            expr: Box::new(expr.clone()),
        }),
    }
}
