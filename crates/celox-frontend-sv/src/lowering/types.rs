//! Signal types, and the signedness and natural widths of expressions.

use super::*;

pub(super) fn signal_kind_from_port_direction(
    direction: sv::ir::PortDirection,
) -> Result<VariableKind, sv::AnalyzerError> {
    Ok(match direction {
        sv::ir::PortDirection::Input => VariableKind::Input,
        sv::ir::PortDirection::Output => VariableKind::Output,
        sv::ir::PortDirection::Inout => VariableKind::Inout,
        sv::ir::PortDirection::Ref => {
            return Err(sv::AnalyzerError::Unsupported(
                "ref port direction".to_string(),
            ));
        }
        sv::ir::PortDirection::Unspecified => VariableKind::Variable,
    })
}

pub(super) struct SvSignalType {
    pub(super) float: Option<celox_sir::FloatPrecision>,
    pub(super) value_kind: celox_design::ValueKind,
    pub(super) width: usize,
    pub(super) signed: bool,
    pub(super) is_4state: bool,
    /// Flat structure storage is selected only through analyzer-checked member windows.
    pub(super) is_unpacked_struct: bool,
    pub(super) packed_ranges: Vec<(i128, i128)>,
    pub(super) array_dims: Vec<usize>,
    pub(super) unpacked_ranges: Vec<(i128, i128)>,
    pub(super) type_kind: PortTypeKind,
}

pub(super) fn unpacked_range_bounds(
    range: &sv::ir::UnpackedRange,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> Option<(i128, i128)> {
    // User constant functions belong to the analyzer session, which has
    // ended by lowering. Retain its evaluated bounds as a fallback while
    // keeping the symbolic declaration contract available to other consumers.
    sv::typecheck::eval_const_expr_with_types(range.left(), constants, parameter_types)
        .zip(sv::typecheck::eval_const_expr_with_types(
            range.right(),
            constants,
            parameter_types,
        ))
        .or_else(|| range.resolved_bounds())
}

pub(super) fn signal_type_from_sv(
    typ: &sv::ir::Type,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> Result<SvSignalType, sv::AnalyzerError> {
    // The bounds of the packed dimensions, as the analysis resolved them:
    // a range may name constants of a scope, such as a package, that
    // `constants` does not hold. A range that neither resolves is an error,
    // not a dimension to leave out.
    let packed_ranges: Vec<(i128, i128)> = match typ.packed_bounds() {
        Some(bounds) => bounds.to_vec(),
        None => typ
            .packed_ranges()
            .iter()
            .map(|range| {
                let bound = |expr| {
                    sv::typecheck::eval_const_expr_with_types(expr, constants, parameter_types)
                };
                bound(range.left())
                    .zip(bound(range.right()))
                    .ok_or_else(|| {
                        sv::AnalyzerError::Unsupported(format!(
                            "packed dimension whose bounds {:?} and {:?} are not constant here",
                            range.left(),
                            range.right(),
                        ))
                    })
            })
            .collect::<Result<_, _>>()?,
    };
    let packed_width = packed_ranges
        .iter()
        .try_fold(1usize, |acc, &(left, right)| {
            acc.checked_mul(usize::try_from(left.abs_diff(right)).ok()?.checked_add(1)?)
        })
        .ok_or_else(|| sv::AnalyzerError::Unsupported("packed width overflow".to_string()))?
        .max(1);
    let array_dims = typ
        .unpacked_ranges()
        .iter()
        .map(|range| {
            let (left, right) = unpacked_range_bounds(range, constants, parameter_types)?;
            usize::try_from(left.abs_diff(right))
                .ok()
                .and_then(|width| width.checked_add(1))
        })
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| {
            sv::AnalyzerError::Unsupported("unresolved unpacked array dimension".to_string())
        })?;
    let unpacked_ranges = typ
        .unpacked_ranges()
        .iter()
        .filter_map(|range| unpacked_range_bounds(range, constants, parameter_types))
        .collect();
    let element_count = array_dims
        .iter()
        .copied()
        .try_fold(1usize, usize::checked_mul)
        .ok_or_else(|| sv::AnalyzerError::Unsupported("signal width overflow".to_string()))?;
    let width = packed_width
        .checked_mul(element_count)
        .ok_or_else(|| sv::AnalyzerError::Unsupported("signal width overflow".to_string()))?;
    let signed = typ.is_signed();
    let float = match typ.kind() {
        sv::ir::TypeKind::Real => Some(celox_sir::FloatPrecision::F64),
        sv::ir::TypeKind::Shortreal => Some(celox_sir::FloatPrecision::F32),
        _ => None,
    };
    let is_4state = float.is_none()
        && !matches!(
            typ.kind(),
            sv::ir::TypeKind::Bit
                | sv::ir::TypeKind::String
                | sv::ir::TypeKind::Object
                | sv::ir::TypeKind::Process
                | sv::ir::TypeKind::Event
        );
    let type_kind = match typ.kind() {
        sv::ir::TypeKind::Bit => PortTypeKind::Bit,
        sv::ir::TypeKind::Real => PortTypeKind::Real,
        sv::ir::TypeKind::Shortreal => PortTypeKind::ShortReal,
        sv::ir::TypeKind::String => PortTypeKind::String,
        sv::ir::TypeKind::Logic | sv::ir::TypeKind::Reg | sv::ir::TypeKind::Implicit => {
            PortTypeKind::Logic
        }
        sv::ir::TypeKind::Object | sv::ir::TypeKind::Process | sv::ir::TypeKind::Event => {
            PortTypeKind::Other
        }
    };
    let value_kind = match typ.kind() {
        sv::ir::TypeKind::String => celox_design::ValueKind::StrRef,
        sv::ir::TypeKind::Object => celox_design::ValueKind::ObjRef,
        _ => celox_design::ValueKind::Bits,
    };
    Ok(SvSignalType {
        float,
        value_kind,
        width,
        signed,
        is_4state,
        is_unpacked_struct: typ.structure_identity().is_some(),
        packed_ranges,
        array_dims,
        unpacked_ranges,
        type_kind,
    })
}

pub(super) fn sv_glue_expr_is_signed(
    expr: &sv::ir::Expr,
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> bool {
    match expr {
        sv::ir::Expr::Ident(name) => name_to_id
            .get(name)
            .and_then(|id| variables.get(id))
            .map(|variable| variable.signed)
            .or_else(|| parameter_types.get(name).map(|(_, signed)| *signed))
            .unwrap_or(false),
        sv::ir::Expr::Literal(literal) => {
            sv::typecheck::parse_integral_literal(literal).is_some_and(|literal| literal.signed)
        }
        sv::ir::Expr::Resize { signed, .. } => *signed,
        sv::ir::Expr::Select { signed, .. } => *signed,
        sv::ir::Expr::Inside { .. } => false,
        sv::ir::Expr::Call { callee, .. } => callee.result_type().is_some_and(|(_, signed)| signed),
        sv::ir::Expr::Concat(_) | sv::ir::Expr::RepeatConcat { .. } => false,
        sv::ir::Expr::Unary { op, expr } => {
            matches!(
                op,
                sv::ir::UnaryOp::Plus
                    | sv::ir::UnaryOp::Minus
                    | sv::ir::UnaryOp::BitNot
                    | sv::ir::UnaryOp::ToTwoState
            ) && sv_glue_expr_is_signed(expr, variables, name_to_id, parameter_types)
        }
        sv::ir::Expr::Binary { left, op, right } => match op {
            sv::ir::BinaryOp::Pow
            | sv::ir::BinaryOp::Shl
            | sv::ir::BinaryOp::Shr
            | sv::ir::BinaryOp::Sar => {
                sv_glue_expr_is_signed(left, variables, name_to_id, parameter_types)
            }
            sv::ir::BinaryOp::Add
            | sv::ir::BinaryOp::Sub
            | sv::ir::BinaryOp::Mul
            | sv::ir::BinaryOp::Div
            | sv::ir::BinaryOp::Mod
            | sv::ir::BinaryOp::BitAnd
            | sv::ir::BinaryOp::BitOr
            | sv::ir::BinaryOp::BitXor => {
                sv_glue_expr_is_signed(left, variables, name_to_id, parameter_types)
                    && sv_glue_expr_is_signed(right, variables, name_to_id, parameter_types)
            }
            _ => false,
        },
        sv::ir::Expr::Mux {
            then_expr,
            else_expr,
            ..
        } => {
            sv_glue_expr_is_signed(then_expr, variables, name_to_id, parameter_types)
                && sv_glue_expr_is_signed(else_expr, variables, name_to_id, parameter_types)
        }
    }
}

pub(super) fn sv_expr_is_signed_with_parameters(
    expr: &sv::ir::Expr,
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> bool {
    match expr {
        sv::ir::Expr::Ident(name) => name_to_id
            .get(name)
            .and_then(|id| variables.get(id))
            .map_or_else(
                || parameter_types.get(name).is_some_and(|(_, signed)| *signed),
                |variable| variable.signed,
            ),
        sv::ir::Expr::Literal(literal) => {
            sv::typecheck::parse_integral_literal(literal).is_some_and(|literal| literal.signed)
        }
        sv::ir::Expr::Resize { signed, .. } => *signed,
        sv::ir::Expr::Select { signed, .. } => *signed,
        sv::ir::Expr::Inside { .. } => false,
        sv::ir::Expr::Call { callee, .. } => callee.result_type().is_some_and(|(_, signed)| signed),
        sv::ir::Expr::Concat(_) | sv::ir::Expr::RepeatConcat { .. } => false,
        sv::ir::Expr::Unary { op, expr } => {
            matches!(
                op,
                sv::ir::UnaryOp::Plus
                    | sv::ir::UnaryOp::Minus
                    | sv::ir::UnaryOp::BitNot
                    | sv::ir::UnaryOp::ToTwoState
            ) && sv_expr_is_signed_with_parameters(expr, variables, name_to_id, parameter_types)
        }
        sv::ir::Expr::Binary { left, op, right } => match op {
            sv::ir::BinaryOp::Pow
            | sv::ir::BinaryOp::Shl
            | sv::ir::BinaryOp::Shr
            | sv::ir::BinaryOp::Sar => {
                sv_expr_is_signed_with_parameters(left, variables, name_to_id, parameter_types)
            }
            sv::ir::BinaryOp::Add
            | sv::ir::BinaryOp::Sub
            | sv::ir::BinaryOp::Mul
            | sv::ir::BinaryOp::Div
            | sv::ir::BinaryOp::Mod
            | sv::ir::BinaryOp::BitAnd
            | sv::ir::BinaryOp::BitOr
            | sv::ir::BinaryOp::BitXor => {
                sv_expr_is_signed_with_parameters(left, variables, name_to_id, parameter_types)
                    && sv_expr_is_signed_with_parameters(
                        right,
                        variables,
                        name_to_id,
                        parameter_types,
                    )
            }
            _ => false,
        },
        // The division-by-zero guard takes the type of the division it
        // guards; its unknown arm is internal.
        sv::ir::Expr::Mux {
            then_expr,
            else_expr,
            ..
        } if matches!(
            &**then_expr,
            sv::ir::Expr::Literal(literal) if literal == sv::DIV_ZERO_UNKNOWN_LITERAL
        ) =>
        {
            sv_expr_is_signed_with_parameters(else_expr, variables, name_to_id, parameter_types)
        }
        sv::ir::Expr::Mux {
            then_expr,
            else_expr,
            ..
        } => {
            sv_expr_is_signed_with_parameters(then_expr, variables, name_to_id, parameter_types)
                && sv_expr_is_signed_with_parameters(
                    else_expr,
                    variables,
                    name_to_id,
                    parameter_types,
                )
        }
    }
}

pub(super) fn unbased_fill_literal(literal: &str) -> Option<char> {
    let normalized = literal.trim().to_ascii_lowercase();
    let mut chars = normalized.chars();
    (chars.next()? == '\'' && chars.clone().count() == 1).then_some(chars.next()?)
}

pub(super) fn expr_unbased_fill_literal(expr: &sv::ir::Expr) -> Option<char> {
    match expr {
        sv::ir::Expr::Literal(literal) => unbased_fill_literal(literal),
        _ => None,
    }
}

pub(super) fn expr_is_unknown_literal(expr: &sv::ir::Expr) -> bool {
    let sv::ir::Expr::Literal(literal) = expr else {
        return false;
    };
    sv::typecheck::parse_integral_literal(literal)
        .is_some_and(|literal| literal.mask != BigUint::default())
}

fn unbased_fill_value(fill: char, width: usize) -> Option<(BigUint, BigUint)> {
    let all_ones = if width == 0 {
        BigUint::default()
    } else {
        (BigUint::from(1u8) << width) - BigUint::from(1u8)
    };
    match fill {
        '0' => Some((BigUint::default(), BigUint::default())),
        '1' => Some((all_ones, BigUint::default())),
        'x' => Some((all_ones.clone(), all_ones)),
        'z' | '?' => Some((BigUint::default(), all_ones)),
        _ => None,
    }
}

pub(super) fn lower_unbased_fill_literal_slt<A: std::hash::Hash + Eq + Clone>(
    arena: &mut SLTNodeArena<A>,
    fill: char,
    width: usize,
) -> Option<celox_slt::NodeId> {
    let (value, mask) = unbased_fill_value(fill, width)?;
    arena
        .alloc(SLTNode::Constant(value, mask, width, false))
        .ok()
}

pub(super) fn sv_expr_natural_width(
    expr: &sv::ir::Expr,
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> Option<usize> {
    match expr {
        sv::ir::Expr::Ident(name) => name_to_id
            .get(name)
            .and_then(|id| variables.get(id))
            .map_or_else(
                || {
                    constants
                        .contains_key(name)
                        .then(|| parameter_types.get(name).map_or(32, |(width, _)| *width))
                },
                |var| Some(var.width),
            ),
        sv::ir::Expr::Literal(literal) => Some(
            unbased_fill_literal(literal)
                .map(|_| 1)
                .unwrap_or(sv::typecheck::parse_integral_literal(literal)?.width),
        ),
        sv::ir::Expr::Select { expr, msb, lsb, .. } => {
            if let Some((_, _, access)) = dynamic_array_element_subselection(
                expr,
                msb,
                lsb,
                variables,
                name_to_id,
                constants,
                parameter_types,
            ) {
                return Some(access.msb - access.lsb + 1);
            }
            if let Some(width) =
                runtime_select_width(msb, lsb, name_to_id, constants, parameter_types)
            {
                return Some(width);
            }
            let msb = sv::typecheck::eval_const_expr_with_types(msb, constants, parameter_types)?;
            let lsb = sv::typecheck::eval_const_expr_with_types(lsb, constants, parameter_types)?;
            usize::try_from(msb.abs_diff(lsb)).ok()?.checked_add(1)
        }
        sv::ir::Expr::Resize { width, .. } => Some(*width),
        sv::ir::Expr::Unary { op, expr } => matches!(
            op,
            sv::ir::UnaryOp::LogicNot
                | sv::ir::UnaryOp::RedAnd
                | sv::ir::UnaryOp::RedOr
                | sv::ir::UnaryOp::RedXor
        )
        .then_some(1)
        .or_else(|| sv_expr_natural_width(expr, variables, name_to_id, constants, parameter_types)),
        sv::ir::Expr::Binary { left, op, right } => {
            if matches!(
                op,
                sv::ir::BinaryOp::LogicAnd
                    | sv::ir::BinaryOp::LogicOr
                    | sv::ir::BinaryOp::Eq
                    | sv::ir::BinaryOp::Ne
                    | sv::ir::BinaryOp::EqCase
                    | sv::ir::BinaryOp::NeCase
                    | sv::ir::BinaryOp::EqWildcard
                    | sv::ir::BinaryOp::NeWildcard
                    | sv::ir::BinaryOp::Lt
                    | sv::ir::BinaryOp::Le
                    | sv::ir::BinaryOp::Gt
                    | sv::ir::BinaryOp::Ge
            ) {
                Some(1)
            } else if matches!(
                op,
                sv::ir::BinaryOp::Shl
                    | sv::ir::BinaryOp::Shr
                    | sv::ir::BinaryOp::Sar
                    | sv::ir::BinaryOp::Pow
            ) {
                // Shift counts and exponents are self-determined (Table 11-21).
                sv_expr_natural_width(left, variables, name_to_id, constants, parameter_types)
            } else {
                Some(
                    sv_expr_natural_width(left, variables, name_to_id, constants, parameter_types)?
                        .max(sv_expr_natural_width(
                            right,
                            variables,
                            name_to_id,
                            constants,
                            parameter_types,
                        )?),
                )
            }
        }
        sv::ir::Expr::Concat(parts) => parts.iter().try_fold(0usize, |width, part| {
            width.checked_add(sv_expr_natural_width(
                part,
                variables,
                name_to_id,
                constants,
                parameter_types,
            )?)
        }),
        sv::ir::Expr::RepeatConcat { count, parts } => {
            let count = usize::try_from(sv::typecheck::eval_const_expr_with_types(
                count,
                constants,
                parameter_types,
            )?)
            .ok()?;
            let parts_width = parts.iter().try_fold(0usize, |width, part| {
                width.checked_add(sv_expr_natural_width(
                    part,
                    variables,
                    name_to_id,
                    constants,
                    parameter_types,
                )?)
            })?;
            count.checked_mul(parts_width)
        }
        sv::ir::Expr::Mux {
            then_expr,
            else_expr,
            ..
        } => Some(
            sv_expr_natural_width(then_expr, variables, name_to_id, constants, parameter_types)?
                .max(sv_expr_natural_width(
                    else_expr,
                    variables,
                    name_to_id,
                    constants,
                    parameter_types,
                )?),
        ),
        sv::ir::Expr::Inside { .. } => Some(1),
        sv::ir::Expr::Call { callee, .. } => callee.result_type().map(|(width, _)| width),
    }
}

pub(super) fn sv_comparison_operand_width(
    left: &sv::ir::Expr,
    right: &sv::ir::Expr,
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> Option<usize> {
    Some(
        sv_expr_natural_width(left, variables, name_to_id, constants, parameter_types)?.max(
            sv_expr_natural_width(right, variables, name_to_id, constants, parameter_types)?,
        ),
    )
}
