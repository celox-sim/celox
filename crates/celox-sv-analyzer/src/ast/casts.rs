//! Cast target resolution and constant literal conversion.

use super::*;

fn cast_target_type(
    casting_type: &sv_parser::CastingType,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Option<ExprType> {
    if let Some(r#type) = integer_atom_expr_type(RefNode::CastingType(casting_type)) {
        return Some(r#type);
    }
    match casting_type {
        sv_parser::CastingType::SimpleType(simple_type) => {
            let sv_parser::SimpleType::PsTypeIdentifier(identifier) = simple_type.as_ref() else {
                return None;
            };
            let name = reference_name(RefNode::PsTypeIdentifier(identifier), syntax_tree)?;
            if let Some(r#type) = type_aliases.get(&name) {
                expr_type_from_type(r#type, const_env)
            } else {
                Some(ExprType {
                    width: usize::try_from(*const_env.get(&name)?)
                        .ok()
                        .filter(|width| *width > 0)?,
                    signed: false,
                })
            }
        }
        sv_parser::CastingType::ConstantPrimary(primary) => {
            if let Some(r#type) =
                size_system_function_expr_type(primary, syntax_tree, const_env, type_aliases)
            {
                return (r#type.width > 0).then_some(r#type);
            }
            let target = const_expr_from_ref_node(RefNode::ConstantPrimary(primary), syntax_tree)
                .ok()
                .flatten()?;
            if let ConstExpr::Ident(name) = &target {
                if let Some(r#type) = type_aliases.get(name) {
                    return expr_type_from_type(r#type, const_env);
                }
            }
            let width = eval_ast_const_expr(&target, const_env)?;
            Some(ExprType {
                width: usize::try_from(width).ok().filter(|width| *width > 0)?,
                signed: false,
            })
        }
        _ => None,
    }
}

pub(super) fn expr_type_from_type(
    r#type: &Type,
    const_env: &HashMap<String, i128>,
) -> Option<ExprType> {
    if !r#type.unpacked_ranges().is_empty() {
        return None;
    }
    let width = r#type
        .packed_ranges()
        .iter()
        .try_fold(1usize, |width, range| {
            let left = eval_ast_const_expr(range.left(), const_env)?;
            let right = eval_ast_const_expr(range.right(), const_env)?;
            width.checked_mul(usize::try_from(left.abs_diff(right)).ok()?.checked_add(1)?)
        })?;
    Some(ExprType {
        width: width.max(1),
        signed: r#type.is_signed(),
    })
}

pub(super) fn cast_is_supported(
    cast: &sv_parser::Cast,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> bool {
    cast_zero_type(cast, syntax_tree, const_env, type_aliases).is_some()
        || matches!(cast.nodes.0, sv_parser::CastingType::Signing(_))
        || cast_target_type(&cast.nodes.0, syntax_tree, const_env, type_aliases).is_some()
}

/// Lower a runtime cast of `expr`.
///
/// A size cast `N'(x)` and `signed'(x)` / `unsigned'(x)` keep or set the
/// operand's own signedness, while a type cast `T'(x)` yields `T`'s signedness.
/// In every case the operand is extended according to its own signedness, as in
/// an assignment to a variable of the target type. Casts to two-state types
/// turn unknown bits into zero.
pub(super) fn runtime_cast_expr(
    cast: &sv_parser::Cast,
    expr: Expr,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Converted<Expr> {
    let const_env = &packed_dimensions.const_env;
    let type_aliases = &packed_dimensions.type_aliases;
    if let Some(r#type) = cast_zero_type(cast, syntax_tree, const_env, type_aliases) {
        return Ok(Expr::Resize {
            expr: Box::new(expr),
            width: r#type.width,
            signed: r#type.signed,
        });
    }
    if let sv_parser::CastingType::Signing(signing) = &cast.nodes.0 {
        let width = expr_static_width(&expr, packed_dimensions)
            .ok_or_else(|| unsupported("signedness cast of an operand without a static width"))?;
        return Ok(Expr::Resize {
            expr: Box::new(expr),
            width,
            signed: matches!(**signing, sv_parser::Signing::Signed(_)),
        });
    }
    let target = cast_target_type(&cast.nodes.0, syntax_tree, const_env, type_aliases)
        .ok_or_else(|| unsupported("cast target type"))?;
    let operand_signed = expr_signedness_with_return_types(
        &expr,
        &packed_dimensions.expression_signedness,
        &packed_dimensions.functions,
        &packed_dimensions.function_return_types,
    )
    .or_else(|| {
        // A genvar or an untyped constant is an `int`.
        let Expr::Ident(name) = &expr else {
            return None;
        };
        const_env.contains_key(name).then(|| {
            parameter_types_from_const_env(const_env)
                .get(name)
                .is_none_or(|r#type| r#type.signed)
        })
    })
    .ok_or_else(|| unsupported("cast of an operand whose signedness is unknown"))?;
    let expr = if cast_target_is_two_state(&cast.nodes.0, syntax_tree, const_env, type_aliases) {
        Expr::Unary {
            op: UnaryOp::ToTwoState,
            expr: Box::new(expr),
        }
    } else {
        expr
    };
    let resized = Expr::Resize {
        expr: Box::new(expr),
        width: target.width,
        signed: operand_signed,
    };
    if casting_type_is_numeric_size(&cast.nodes.0, syntax_tree, const_env, type_aliases)
        || target.signed == operand_signed
    {
        Ok(resized)
    } else {
        Ok(Expr::Resize {
            expr: Box::new(resized),
            width: target.width,
            signed: target.signed,
        })
    }
}

pub(super) fn constant_cast_is_supported(
    cast: &sv_parser::ConstantCast,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> bool {
    constant_cast_const_expr(cast, syntax_tree, const_env, type_aliases).is_some()
}

pub(super) fn cast_zero_type(
    cast: &sv_parser::Cast,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Option<ExprType> {
    let ConstExpr::Literal(literal) = const_expr_from_expr(&cast.nodes.2.nodes.1, syntax_tree)
        .ok()
        .flatten()?
    else {
        return None;
    };
    let literal = typecheck::parse_integral_literal(&literal)?;
    if literal.value != 0u8.into() || literal.mask != 0u8.into() {
        return None;
    }
    let target_type = cast_target_type(&cast.nodes.0, syntax_tree, const_env, type_aliases)?;
    let signed =
        if casting_type_is_numeric_size(&cast.nodes.0, syntax_tree, const_env, type_aliases) {
            literal.signed
        } else {
            target_type.signed
        };
    Some(ExprType {
        signed,
        ..target_type
    })
}

pub(super) fn constant_cast_const_expr(
    cast: &sv_parser::ConstantCast,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Option<ConstExpr> {
    let operand = const_expr_from_ref_node_with_env(
        RefNode::ConstantExpression(&cast.nodes.2.nodes.1),
        syntax_tree,
        const_env,
        type_aliases,
    )
    .ok()
    .flatten()?;
    cast_constant_operand(operand, &cast.nodes.0, syntax_tree, const_env, type_aliases)
}

pub(super) fn runtime_constant_cast_const_expr(
    cast: &sv_parser::Cast,
    syntax_tree: &SyntaxTree,
    dimensions: &PackedDimensions,
) -> Option<ConstExpr> {
    let operand = indexed_select_base(
        RefNode::Expression(&cast.nodes.2.nodes.1),
        syntax_tree,
        dimensions,
    )
    .ok()?;
    cast_constant_operand(
        operand,
        &cast.nodes.0,
        syntax_tree,
        &dimensions.const_env,
        &dimensions.type_aliases,
    )
}

pub(super) fn cast_constant_operand(
    operand: ConstExpr,
    casting_type: &sv_parser::CastingType,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Option<ConstExpr> {
    let parameter_types = parameter_types_from_const_env(const_env);
    let operand_type = infer_const_expr_type(&operand, &parameter_types)?;
    let literal = if let ConstExpr::Literal(literal) = &operand {
        typecheck::parse_integral_literal(literal)?
    } else {
        let ir_operand: crate::ir::ConstExpr = operand.clone().into();
        let ir_parameter_types = parameter_types
            .iter()
            .map(|(name, r#type)| (name.clone(), (r#type.width, r#type.signed)))
            .collect();
        typecheck::eval_const_integral_literal_with_types(
            &ir_operand,
            const_env,
            &ir_parameter_types,
        )
        .or_else(|| {
            let operand_value = eval_ast_const_expr(&operand, const_env)?;
            let operand_literal = format_typed_parameter_literal(
                operand_value,
                operand_type.width,
                operand_type.signed,
            );
            typecheck::parse_integral_literal(&operand_literal)
        })?
    };
    let target_type = cast_target_type(casting_type, syntax_tree, const_env, type_aliases)?;
    // A numeric size cast keeps the source expression's signedness; a type
    // cast takes the target type's signedness.
    let signed = if casting_type_is_numeric_size(casting_type, syntax_tree, const_env, type_aliases)
    {
        literal.signed
    } else {
        target_type.signed
    };
    let resized = match &operand {
        ConstExpr::Literal(value) => {
            resize_unbased_fill_literal_for_cast(value, target_type.width, signed).unwrap_or_else(
                || resize_integral_literal_for_cast(literal, target_type.width, signed),
            )
        }
        _ => resize_integral_literal_for_cast(literal, target_type.width, signed),
    };
    let resized = if cast_target_is_two_state(casting_type, syntax_tree, const_env, type_aliases) {
        two_state_integral_literal(&resized, target_type.width, signed)?
    } else {
        resized
    };
    Some(ConstExpr::Literal(resized))
}

fn cast_target_is_two_state(
    casting_type: &sv_parser::CastingType,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> bool {
    if type_from_ref_node_with_env(
        RefNode::CastingType(casting_type),
        syntax_tree,
        const_env,
        type_aliases,
    )
    .is_some_and(|r#type| r#type.kind() == TypeKind::Bit)
    {
        return true;
    }
    match casting_type {
        sv_parser::CastingType::SimpleType(simple_type) => {
            let sv_parser::SimpleType::PsTypeIdentifier(identifier) = simple_type.as_ref() else {
                return false;
            };
            reference_name(RefNode::PsTypeIdentifier(identifier), syntax_tree)
                .and_then(|name| type_aliases.get(&name))
                .is_some_and(|r#type| r#type.kind() == TypeKind::Bit)
        }
        sv_parser::CastingType::ConstantPrimary(primary) => {
            let Ok(Some(ConstExpr::Ident(name))) =
                const_expr_from_ref_node(RefNode::ConstantPrimary(primary), syntax_tree)
            else {
                return false;
            };
            type_aliases
                .get(&name)
                .is_some_and(|r#type| r#type.kind() == TypeKind::Bit)
        }
        _ => false,
    }
}

fn two_state_integral_literal(value: &str, width: usize, signed: bool) -> Option<String> {
    let mut literal = typecheck::parse_integral_literal(value)?;
    let keep = (num_bigint::BigUint::from(1usize) << width) - num_bigint::BigUint::from(1usize);
    literal.value &= &keep ^ &literal.mask;
    literal.mask = num_bigint::BigUint::default();
    Some(resize_integral_literal_for_cast(literal, width, signed))
}

fn casting_type_is_numeric_size(
    casting_type: &sv_parser::CastingType,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> bool {
    match casting_type {
        sv_parser::CastingType::ConstantPrimary(primary) => {
            let Ok(Some(ConstExpr::Ident(name))) =
                const_expr_from_ref_node(RefNode::ConstantPrimary(primary), syntax_tree)
            else {
                return true;
            };
            !type_aliases.contains_key(&name)
        }
        sv_parser::CastingType::SimpleType(simple_type) => {
            let sv_parser::SimpleType::PsTypeIdentifier(identifier) = simple_type.as_ref() else {
                return false;
            };
            let Some(name) = reference_name(RefNode::PsTypeIdentifier(identifier), syntax_tree)
            else {
                return false;
            };
            !type_aliases.contains_key(&name) && const_env.contains_key(&name)
        }
        _ => false,
    }
}

pub(super) fn resize_unbased_fill_literal_for_cast(
    value: &str,
    width: usize,
    signed: bool,
) -> Option<String> {
    let normalized = value.trim().to_ascii_lowercase();
    let mut chars = normalized.chars();
    (chars.next()? == '\'' && chars.clone().count() == 1).then_some(())?;
    let fill = chars.next()?;
    matches!(fill, '0' | '1' | 'x' | 'z' | '?').then_some(())?;
    let signing = if signed { "s" } else { "" };
    Some(format!(
        "{width}'{signing}b{}",
        fill.to_string().repeat(width)
    ))
}

pub(super) fn resize_integral_literal_for_cast(
    literal: typecheck::IntegralLiteral,
    width: usize,
    signed: bool,
) -> String {
    let mut value = literal.value;
    let mut mask = literal.mask;
    if literal.signed && literal.width > 0 && literal.width < width {
        let extension = ((num_bigint::BigUint::from(1usize) << (width - literal.width))
            - num_bigint::BigUint::from(1usize))
            << literal.width;
        if value.bit((literal.width - 1) as u64) {
            value |= &extension;
        }
        if mask.bit((literal.width - 1) as u64) {
            mask |= extension;
        }
    }
    let keep = (num_bigint::BigUint::from(1usize) << width) - num_bigint::BigUint::from(1usize);
    value &= &keep;
    mask &= keep;
    let signing = if signed { "s" } else { "" };
    if mask == num_bigint::BigUint::default() {
        return format!("{width}'{signing}d{value}");
    }
    let bits = (0..width)
        .rev()
        .map(|bit| {
            if mask.bit(bit as u64) {
                if value.bit(bit as u64) { 'x' } else { 'z' }
            } else if value.bit(bit as u64) {
                '1'
            } else {
                '0'
            }
        })
        .collect::<String>();
    format!("{width}'{signing}b{bits}")
}
