//! Packed structure declaration types and field layout.

use super::*;

#[derive(Clone, Copy)]
pub(in crate::ast) enum AggregateKind {
    Struct,
    HardUnion,
    SoftUnion,
    TaggedUnion,
}

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
    kind: AggregateKind,
    signed: bool,
    owner: Arc<str>,
    fields: Vec<(String, Type, bool)>,
    ranges: Vec<PackedRange>,
    env: &HashMap<String, i128>,
) -> Option<Type> {
    let mut names = HashSet::default();
    let members = fields
        .into_iter()
        .map(|(name, ty, is_void)| {
            if !names.insert(name.clone()) {
                return None;
            }
            Some(PackedMember {
                owner: owner.clone(),
                name,
                offset: 0,
                tag: None,
                is_void,
                r#type: capture_member_type(state, ty, env)?,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    finish_type(state, kind, signed, members, ranges, env)
}
fn finish_type(
    state: &AnalysisState,
    kind: AggregateKind,
    signed: bool,
    mut members: Vec<PackedMember>,
    ranges: Vec<PackedRange>,
    const_env: &HashMap<String, i128>,
) -> Option<Type> {
    let mut width = 0usize;
    // The first declared member occupies the most significant bits.
    for member in members.iter_mut().rev() {
        let member_width = if member.is_void {
            0
        } else {
            expr_type_from_type(state, &member.r#type, const_env)?.width
        };
        if !matches!(kind, AggregateKind::Struct) {
            // IEEE 1800-2023 7.3.1: hard packed members share all bits
            // and must have the same width.
            if matches!(kind, AggregateKind::HardUnion) && width != 0 && width != member_width {
                return None;
            }
            // Soft members are right-justified in the largest member width.
            width = width.max(member_width);
        } else {
            member.offset = width;
            width = width.checked_add(member_width)?;
        }
    }
    let tagged = matches!(kind, AggregateKind::TaggedUnion);
    if tagged {
        let tag_width = (usize::BITS - members.len().checked_sub(1)?.leading_zeros()) as usize;
        for (index, member) in members.iter_mut().enumerate() {
            member.tag = Some(TaggedMember {
                index,
                tag_width,
                payload_width: width,
            });
        }
        width = width.checked_add(tag_width)?;
    }
    let kind = if !tagged
        && members
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
