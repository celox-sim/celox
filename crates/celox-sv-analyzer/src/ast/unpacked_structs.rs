//! Unpacked structures (IEEE 1800-2023 7.2).
//!
//! An unpacked structure is not storage of its own. A variable of such a type
//! is a set of *leaf* variables, one for each member that is not itself an
//! unpacked structure, named by its member path (`s.a.b`). Each leaf keeps the
//! type of its member, so a `string` or class handle member is an ordinary
//! string or handle variable.
//!
//! A value of the whole structure is the concatenation of its leaves, the
//! first declared member in the most significant bits. Such a flat value only
//! lives inside one evaluation, so the handles it carries need no root of
//! their own.

use super::*;
use celox_sv_syntax::{ExprId, ExprKind};

/// The members of an unpacked structure type.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(in crate::ast) struct UnpackedStruct {
    /// Nominal identity (IEEE 1800-2023 6.22.1): the declaration.
    owner: Arc<str>,
    members: Vec<Member>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(in crate::ast) struct Member {
    pub(in crate::ast) name: String,
    pub(in crate::ast) r#type: Type,
    /// Bits of the member in a flat value.
    pub(in crate::ast) width: usize,
}

/// A member that is not an unpacked structure, with its place in a flat value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::ast) struct Leaf {
    /// Member names from the structure down to the leaf.
    pub(in crate::ast) path: Vec<String>,
    pub(in crate::ast) r#type: Type,
    /// Least significant bit in a flat value.
    pub(in crate::ast) offset: usize,
    pub(in crate::ast) width: usize,
}

impl Leaf {
    /// The name of this leaf of the variable `base`.
    pub(in crate::ast) fn name(&self, base: &str) -> String {
        leaf_name(base, &self.path)
    }
}

/// The leaf or member variable `path` of `base`.
pub(in crate::ast) fn leaf_name(base: &str, path: &[String]) -> String {
    let mut name = base.to_owned();
    for part in path {
        name.push('.');
        name.push_str(part);
    }
    name
}

impl UnpackedStruct {
    pub(in crate::ast) fn same_type(&self, other: &Self) -> bool {
        self.owner == other.owner
    }

    pub(in crate::ast) fn members(&self) -> &[Member] {
        &self.members
    }

    pub(in crate::ast) fn member(&self, name: &str) -> Option<&Member> {
        self.members.iter().find(|member| member.name == name)
    }

    /// Bits of a flat value.
    pub(in crate::ast) fn width(&self) -> usize {
        self.members.iter().map(|member| member.width).sum()
    }

    /// The leaves in declaration order.
    pub(in crate::ast) fn leaves(&self) -> Vec<Leaf> {
        let mut leaves = Vec::new();
        let mut offset = self.width();
        // Depth-first in declaration order: members are pushed in reverse.
        fn push<'m>(
            stack: &mut Vec<(Vec<String>, &'m Member)>,
            path: &[String],
            members: &'m [Member],
        ) {
            for member in members.iter().rev() {
                let mut member_path = path.to_vec();
                member_path.push(member.name.clone());
                stack.push((member_path, member));
            }
        }
        let mut stack = Vec::new();
        push(&mut stack, &[], &self.members);
        while let Some((path, member)) = stack.pop() {
            if let Some(inner) = member.r#type.unpacked_struct() {
                push(&mut stack, &path, &inner.members);
                continue;
            }
            offset -= member.width;
            leaves.push(Leaf {
                path,
                r#type: member.r#type.clone(),
                offset,
                width: member.width,
            });
        }
        leaves
    }
}

/// The type of an unpacked structure with `fields`, or `None` when a member
/// type has no supported unpacked-structure representation.
pub(in crate::ast) fn native_type(
    state: &AnalysisState,
    owner: Arc<str>,
    fields: Vec<(String, Type)>,
    env: &HashMap<String, i128>,
) -> Option<Type> {
    let members = fields
        .into_iter()
        .map(|(name, ty)| {
            if !ty.unpacked_ranges.is_empty()
                || ty.container.is_some()
                || matches!(ty.kind, TypeKind::Real | TypeKind::Shortreal)
            {
                return None;
            }
            let ty = match ty.unpacked_struct() {
                Some(_) => ty,
                None => packed_structs::capture_member_type(state, ty, env)?,
            };
            let width = match ty.unpacked_struct() {
                Some(inner) => inner.width(),
                None => casts::expr_type_from_type(state, &ty, env)?.width,
            };
            Some(Member {
                name,
                r#type: ty,
                width,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    let aggregate = UnpackedStruct { owner, members };
    let four_state = aggregate.leaves().iter().any(|leaf| {
        matches!(
            leaf.r#type.kind,
            TypeKind::Logic | TypeKind::Reg | TypeKind::Implicit
        )
    });
    let width = aggregate.width();
    let mut r#type = Type::new(if four_state {
        TypeKind::Logic
    } else {
        TypeKind::Bit
    });
    r#type.packed_ranges.push(PackedRange::new(
        ConstExpr::Literal(width.checked_sub(1)?.to_string()),
        ConstExpr::Literal("0".into()),
    ));
    r#type.unpacked_struct = Some(Arc::new(aggregate));
    Some(r#type)
}

/// The flat value of the variable or member `name` of unpacked structure
/// type: the concatenation of its leaf variables.
pub(in crate::ast) fn whole(name: &str, dims: &PackedDimensions) -> Option<Expr> {
    let aggregate = dims.get(name)?.unpacked_struct.clone()?;
    Some(Expr::Concat(
        aggregate
            .leaves()
            .iter()
            .map(|leaf| Expr::Ident(leaf.name(name)))
            .collect(),
    ))
}

/// The leaf variables that an assignment to the whole variable or member
/// `name` of unpacked structure type writes, first declared first.
pub(in crate::ast) fn targets(name: &str, dims: &PackedDimensions) -> Option<Vec<LValue>> {
    let aggregate = dims.get(name)?.unpacked_struct.clone()?;
    Some(
        aggregate
            .leaves()
            .iter()
            .map(|leaf| LValue::Ident(leaf.name(name)))
            .collect(),
    )
}

/// Join the leading names of a member path that select within a variable of
/// unpacked structure type: they name one of its member variables.
pub(in crate::ast) fn collapse(parts: &mut Vec<String>, dims: &PackedDimensions) {
    if parts.len() < 2
        || !dims
            .get(&parts[0])
            .is_some_and(|shape| shape.unpacked_struct.is_some())
    {
        return;
    }
    let mut name = parts[0].clone();
    let mut used = 1;
    for part in &parts[1..] {
        let candidate = format!("{name}.{part}");
        if dims.get(&candidate).is_none() {
            break;
        }
        name = candidate;
        used += 1;
    }
    parts.splice(..used, [name]);
}

/// Every variable and member of unpacked structure type in a variable
/// `base` of type `aggregate`, outermost first, with its type.
pub(in crate::ast) fn aggregates(base: &str, r#type: &Type) -> Vec<(String, Type)> {
    let mut found = Vec::new();
    let mut jobs = vec![(base.to_owned(), r#type.clone())];
    while let Some((name, ty)) = jobs.pop() {
        if let Some(aggregate) = ty.unpacked_struct() {
            for member in aggregate.members.iter().rev() {
                jobs.push((format!("{name}.{}", member.name), member.r#type.clone()));
            }
            found.push((name, ty));
        }
    }
    found
}

/// The unpacked structure type of native expression `id`, judged before it
/// is bound, or `None` when it is not a value of such a type.
pub(in crate::ast) fn native_struct(
    id: ExprId,
    native: &celox_sv_syntax::Declarations,
    tree: &AnalysisContext<'_>,
    dims: &PackedDimensions,
) -> Converted<Option<Arc<UnpackedStruct>>> {
    let id = expression_binding::ungroup(id, native);
    Ok(match &native.expression(id).kind {
        ExprKind::Name(_) | ExprKind::Member { .. } => match member_path(id, native)
            .map(|mut parts| {
                collapse(&mut parts, dims);
                parts
            })
            .filter(|parts| parts.len() == 1)
            .and_then(|parts| dims.get(&parts[0])?.unpacked_struct.clone())
        {
            Some(aggregate) => Some(aggregate),
            None if let Some((ty, _)) = containers::element_member(id, native, tree, dims)? => {
                ty.unpacked_struct().cloned()
            }
            // A class property of unpacked structure type.
            None => expression_binding::object_aggregate_property(tree, id, native, dims)?
                .and_then(|(aggregate, _, _)| aggregate.r#type.unpacked_struct().cloned()),
        },
        ExprKind::Conditional {
            then_expression,
            else_expression,
            ..
        } => {
            let then_type = native_struct(*then_expression, native, tree, dims)?;
            let else_type = native_struct(*else_expression, native, tree, dims)?;
            then_type.filter(|then_type| {
                else_type
                    .as_ref()
                    .is_some_and(|else_type| then_type.same_type(else_type))
            })
        }
        // A typed assignment pattern, `T'{...}`.
        ExprKind::Pattern {
            target: Some(_), ..
        } if std::ptr::eq(native, tree.declarations()) => {
            patterns::typed_shape(id, tree, dims)?.and_then(|shape| shape.unpacked_struct)
        }
        _ if containers::native_struct_element(id, native, tree, dims)?.is_some() => {
            containers::native_struct_element(id, native, tree, dims)?
        }
        // A method of the scope class hides a module function of its name,
        // as call binding resolves it.
        ExprKind::Call { expression, .. } => {
            match expression_binding::object_method_result(tree, *expression, native, dims)? {
                Some(ty) => ty.unpacked_struct().cloned(),
                None => expression_binding::callable_name(&tree.state, *expression, native)
                    .and_then(|name| dims.functions.get(&name))
                    .and_then(|function| function.return_type.as_ref())
                    .and_then(|ty| ty.unpacked_struct().cloned()),
            }
        }
        _ => None,
    })
}

/// The names of a native `a.b.c` path, or `None` for another expression.
fn member_path(mut id: ExprId, native: &celox_sv_syntax::Declarations) -> Option<Vec<String>> {
    let mut parts = Vec::new();
    loop {
        match &native.expression(id).kind {
            ExprKind::Name(name) => {
                parts.push(instances::normalize_identifier(&name.spelling));
                break;
            }
            ExprKind::Member {
                expression,
                separator,
                name,
            } if separator == "." => {
                parts.push(instances::normalize_identifier(&name.spelling));
                id = *expression;
            }
            ExprKind::Group(inner) => id = *inner,
            _ => return None,
        }
    }
    parts.reverse();
    Some(parts)
}

/// The variable that native member path `id` names when it selects within a
/// variable of unpacked structure type.
pub(in crate::ast) fn native_member(
    id: ExprId,
    native: &celox_sv_syntax::Declarations,
    dims: &PackedDimensions,
) -> Option<String> {
    let mut parts = member_path(id, native)?;
    if parts.len() < 2 {
        return None;
    }
    collapse(&mut parts, dims);
    (parts.len() == 1).then(|| parts.pop().expect("one part"))
}

/// Check that `right` may be assigned to the variable or member `name` of
/// unpacked structure type: an assignment pattern or a value of the same
/// type (IEEE 1800-2023 6.22.3).
pub(in crate::ast) fn check_assignment(
    name: &str,
    right: ExprId,
    tree: &AnalysisContext<'_>,
    dims: &PackedDimensions,
) -> Converted<()> {
    let target = dims
        .get(name)
        .and_then(|shape| shape.unpacked_struct.clone())
        .ok_or_else(|| unsupported("unpacked structure target"))?;
    check_value(&target, right, tree.declarations(), tree, dims)
}

/// Check that `value` is an assignment pattern or a value of type `target`.
pub(in crate::ast) fn check_value(
    target: &UnpackedStruct,
    value: ExprId,
    native: &celox_sv_syntax::Declarations,
    tree: &AnalysisContext<'_>,
    dims: &PackedDimensions,
) -> Converted<()> {
    let value = expression_binding::ungroup(value, native);
    // An untyped pattern takes the target's type; a typed one, `T'{...}`,
    // must name it.
    if matches!(
        native.expression(value).kind,
        ExprKind::Pattern { target: None, .. }
    ) {
        return Ok(());
    }
    if let ExprKind::Conditional {
        then_expression,
        else_expression,
        ..
    } = &native.expression(value).kind
    {
        check_value(target, *then_expression, native, tree, dims)?;
        return check_value(target, *else_expression, native, tree, dims);
    }
    match native_struct(value, native, tree, dims)? {
        Some(actual) if actual.same_type(target) => Ok(()),
        _ => Err(unsupported(
            "assignment to an unpacked structure of a value that is not of its type",
        )),
    }
}

/// Reject an operator applied to a whole unpacked structure value: only the
/// equality operators, the conditional operator and assignment take one
/// (IEEE 1800-2023 7.2, 11.2.2).
pub(in crate::ast) fn check_operands(
    id: ExprId,
    native: &celox_sv_syntax::Declarations,
    tree: &AnalysisContext<'_>,
    dims: &PackedDimensions,
) -> Converted<()> {
    let operands: Vec<ExprId> = match &native.expression(id).kind {
        ExprKind::Unary { expression, .. } => vec![*expression],
        ExprKind::Binary {
            left,
            operator,
            right,
        } => {
            if matches!(operator.as_str(), "==" | "!=" | "===" | "!==") {
                let left_type = native_struct(*left, native, tree, dims)?;
                let right_type = native_struct(*right, native, tree, dims)?;
                return match (left_type, right_type) {
                    (None, None) => Ok(()),
                    (Some(left), Some(right)) if left.same_type(&right) => Ok(()),
                    _ => Err(unsupported(format!(
                        "operator `{operator}` comparing an unpacked structure with a value of another type"
                    ))),
                };
            }
            vec![*left, *right]
        }
        ExprKind::Concatenation(items) => items.clone(),
        ExprKind::Replication { expressions, .. } => expressions.clone(),
        ExprKind::Cast { expression, .. } => vec![*expression],
        ExprKind::Select { expression, .. } => vec![*expression],
        _ => return Ok(()),
    };
    for operand in operands {
        if native_struct(operand, native, tree, dims)?.is_some() {
            return Err(unsupported(
                "operator, select or cast applied to an unpacked structure",
            ));
        }
    }
    Ok(())
}

/// The leaf values of a flat value `value` of type `aggregate` when it is
/// made of one part per member (a pattern or a whole variable), with nested
/// structure members split in turn; `None` otherwise.
pub(in crate::ast) fn leaf_parts(aggregate: &UnpackedStruct, value: &Expr) -> Option<Vec<Expr>> {
    let Expr::Concat(parts) = value else {
        return None;
    };
    // A pattern has one part per member; descend into the parts of nested
    // structure members, so a string leaf gets its string value.
    if parts.len() == aggregate.members.len() {
        let mut leaves = Vec::new();
        for (member, part) in aggregate.members.iter().zip(parts) {
            match member.r#type.unpacked_struct() {
                Some(inner) => match leaf_parts(inner, part) {
                    Some(inner_parts) => leaves.extend(inner_parts),
                    // A whole variable member with one leaf is that leaf.
                    None if inner.leaves().len() == 1 => leaves.push(part.clone()),
                    None => return None,
                },
                None => leaves.push(part.clone()),
            }
        }
        return Some(leaves);
    }
    // A whole variable is one part per leaf.
    (parts.len() == aggregate.leaves().len()).then(|| parts.clone())
}

/// The record that holds a container element of type `aggregate`, or `None`
/// when the element is kept as its flat value: at most 64 bits, with no
/// handle member and members that are all 2-state or all 4-state, so the
/// container's default element is the structure's default.
pub(in crate::ast) fn record(
    state: &AnalysisState,
    aggregate: &UnpackedStruct,
    env: &HashMap<String, i128>,
) -> Converted<Option<crate::system_functions::Record>> {
    let leaves = aggregate.leaves();
    let two_state = |leaf: &Leaf| {
        matches!(
            leaf.r#type.kind,
            TypeKind::Bit | TypeKind::String | TypeKind::Object | TypeKind::Process
        )
    };
    let handles = leaves
        .iter()
        .any(|leaf| matches!(leaf.r#type.kind, TypeKind::String | TypeKind::Object));
    let uniform = leaves.iter().all(two_state) || !leaves.iter().any(two_state);
    if aggregate.width() <= 64 && !handles && uniform {
        return Ok(None);
    }
    let mut size = 0;
    let mut fields = Vec::new();
    for leaf in &leaves {
        let mut field = classes::field(state, &leaf.r#type, env, size)?;
        field.nullable = true;
        size = field.offset + field.storage();
        fields.push(field);
    }
    Ok(Some(crate::system_functions::Record {
        size: size.max(1),
        fields,
    }))
}

/// The member at `path` of `aggregate`, with its type and the leaves it
/// covers.
pub(in crate::ast) fn member_leaves(
    aggregate: &UnpackedStruct,
    path: &[String],
) -> Option<(Type, Vec<Leaf>)> {
    let mut ty = None::<&Type>;
    let mut current = aggregate;
    for (index, name) in path.iter().enumerate() {
        let member = current.member(name)?;
        ty = Some(&member.r#type);
        if index + 1 < path.len() {
            current = member.r#type.unpacked_struct()?;
        }
    }
    let ty = ty?.clone();
    let leaves = aggregate
        .leaves()
        .into_iter()
        .filter(|leaf| leaf.path.starts_with(path))
        .collect();
    Some((ty, leaves))
}
