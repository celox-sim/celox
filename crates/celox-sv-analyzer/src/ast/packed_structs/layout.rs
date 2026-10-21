//! Packed structure declaration types and field layout.

use super::*;

/// Freeze member bounds in the declaration environment before generated scopes
/// can shadow their parameter names. Shared by both syntax owners.
pub(in crate::ast) fn capture_member_type(
    state: &AnalysisState,
    mut ty: Type,
    env: &HashMap<String, i128>,
) -> Option<Type> {
    if !ty.unpacked_ranges.is_empty() || matches!(ty.kind, TypeKind::Real | TypeKind::Shortreal) {
        return None;
    }
    for range in &mut ty.packed_ranges {
        range.left = ConstExpr::Literal(format_typed_parameter_literal(
            eval_ast_const_expr(state, &range.left, env)?,
            128,
            true,
        ));
        range.right = ConstExpr::Literal(format_typed_parameter_literal(
            eval_ast_const_expr(state, &range.right, env)?,
            128,
            true,
        ));
    }
    Some(ty)
}

/// Syntax-independent IEEE7.2.1/7.3.1 layout kernel.
pub(in crate::ast) fn native_type(
    state: &AnalysisState,
    union: bool,
    signed: bool,
    fields: Vec<(String, Type)>,
    ranges: Vec<PackedRange>,
    env: &HashMap<String, i128>,
) -> Option<Type> {
    let mut names = HashSet::default();
    let members = fields
        .into_iter()
        .map(|(name, ty)| {
            if !names.insert(name.clone()) {
                return None;
            }
            Some(PackedMember {
                name,
                offset: 0,
                r#type: capture_member_type(state, ty, env)?,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    finish_type(state, union, signed, members, ranges, env)
}
fn finish_type(
    state: &AnalysisState,
    union: bool,
    signed: bool,
    mut members: Vec<PackedMember>,
    ranges: Vec<PackedRange>,
    const_env: &HashMap<String, i128>,
) -> Option<Type> {
    let mut width = 0usize;
    // The first declared member occupies the most significant bits.
    for member in members.iter_mut().rev() {
        let member_width = expr_type_from_type(state, &member.r#type, const_env)?.width;
        if union {
            // IEEE 1800-2023 7.3.1: hard packed members share all bits
            // and must have the same width.
            if width != 0 && width != member_width {
                return None;
            }
            width = member_width;
        } else {
            member.offset = width;
            width = width.checked_add(member_width)?;
        }
    }
    let kind = if members
        .iter()
        .all(|member| member.r#type.kind == TypeKind::Bit)
    {
        TypeKind::Bit
    } else {
        TypeKind::Logic
    };
    let mut r#type = Type::new(kind);
    r#type.is_signed = signed;
    r#type.packed_ranges = ranges;
    r#type.packed_ranges.push(PackedRange::new(
        ConstExpr::Literal(width.checked_sub(1)?.to_string()),
        ConstExpr::Literal("0".into()),
    ));
    r#type.members = members;
    Some(r#type)
}
