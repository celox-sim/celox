//! Packed structure declaration types and field layout.

use super::*;

#[derive(Clone, Copy)]
pub(in crate::ast) enum AggregateKind {
    Struct,
    HardUnion,
    SoftUnion,
    TaggedUnion,
    /// A plain unpacked union (7.3), laid out like a soft union but no
    /// integral type.
    UnpackedUnion,
}

/// Freeze member bounds in the declaration environment before generated scopes
/// can shadow their parameter names. Shared by both syntax owners.
pub(in crate::ast) fn capture_member_type(
    functions: &const_functions::ConstantFunctions,
    mut ty: Type,
    env: &ConstEnv,
) -> Option<Type> {
    if !ty.unpacked_ranges.is_empty()
        || ty.unpacked_struct.is_some()
        || matches!(ty.kind, TypeKind::Real | TypeKind::Shortreal)
    {
        return None;
    }
    for range in &mut ty.packed_ranges {
        range.left = ConstExpr::Literal(format_typed_parameter_literal(
            eval_ast_const_expr(functions, &range.left, env)?,
            128,
            true,
        ));
        range.right = ConstExpr::Literal(format_typed_parameter_literal(
            eval_ast_const_expr(functions, &range.right, env)?,
            128,
            true,
        ));
    }
    Some(ty)
}

/// Syntax-independent IEEE 7.2.1/7.3.1 layout kernel; an unpacked union
/// (7.3) shares the soft layout.
pub(in crate::ast) fn native_type(
    tree: &AnalysisContext<'_>,
    kind: AggregateKind,
    signed: bool,
    owner: Arc<str>,
    fields: Vec<(String, Type, bool)>,
    ranges: Vec<PackedRange>,
    env: &ConstEnv,
) -> Option<Type> {
    let functions = &tree.functions;
    let mut names = HashSet::default();
    let members = fields
        .into_iter()
        .map(|(name, ty, is_void)| {
            // A chandle is no packed type (IEEE 1800-2023 6.14).
            if !names.insert(name.clone()) || ty.chandle {
                return None;
            }
            // The members of an unpacked union share one flat storage, so
            // they are integral (7.3): no string, handle, virtual
            // interface or event.
            if matches!(kind, AggregateKind::UnpackedUnion)
                && (matches!(
                    ty.kind,
                    TypeKind::String | TypeKind::Object | TypeKind::Process | TypeKind::Event
                ) || ty.class.is_some()
                    || ty.virtual_interface.is_some()
                    || ty.container.is_some())
            {
                return None;
            }
            let captured = capture_member_type(functions, ty, env)?;
            // An unpacked union is no integral type (7.3), so no packed
            // type holds one as a member (7.2.1). Nor does an unpacked
            // union here: a member of one is not classified as a whole
            // union where it is read.
            if captured.unpacked_union {
                return None;
            }
            Some(PackedMember {
                owner: owner.clone(),
                name,
                offset: 0,
                tag: None,
                is_void,
                r#type: captured,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    finish_type(&tree.functions, kind, signed, members, ranges, env)
}
fn finish_type(
    functions: &const_functions::ConstantFunctions,
    kind: AggregateKind,
    signed: bool,
    mut members: Vec<PackedMember>,
    ranges: Vec<PackedRange>,
    const_env: &ConstEnv,
) -> Option<Type> {
    let mut width = 0usize;
    // The first declared member occupies the most significant bits.
    for member in members.iter_mut().rev() {
        let member_width = if member.is_void {
            0
        } else {
            expr_type_from_type(functions, &member.r#type, const_env)?.width
        };
        if !matches!(kind, AggregateKind::Struct) {
            // IEEE 1800-2023 7.3.1: hard packed members share all bits
            // and must have the same width.
            if matches!(kind, AggregateKind::HardUnion) && width != 0 && width != member_width {
                return None;
            }
            // Soft and unpacked-union members are right-justified in the
            // largest member width (7.3 leaves the representation of an
            // unpacked union to the tool).
            width = width.max(member_width);
        } else {
            member.offset = width;
            width = width.checked_add(member_width)?;
        }
    }
    let unpacked = matches!(kind, AggregateKind::UnpackedUnion);
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
    r#type.unpacked_union = unpacked;
    Some(r#type)
}
