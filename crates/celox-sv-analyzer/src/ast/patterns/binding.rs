//! Native assignment-pattern receiving-shape jobs.
use super::*;
use bound_patterns::{Key, NativeKind};
use celox_sv_syntax::{ExprId, ExprKind};
use std::rc::Rc;
fn mismatch() -> AnalyzerError {
    AnalyzerError::Parse("native assignment pattern disagrees with validated syntax".into())
}
#[derive(Clone)]
enum Shape<'a> {
    Borrowed(&'a VariableDimensions),
    Owned(Rc<VariableDimensions>),
}
impl std::ops::Deref for Shape<'_> {
    type Target = VariableDimensions;
    fn deref(&self) -> &Self::Target {
        match self {
            Self::Borrowed(shape) => shape,
            Self::Owned(shape) => shape,
        }
    }
}
type Pair = ExprId;
struct Selected<'a> {
    value: Pair,
    shape: Shape<'a>,
    default: bool,
    check: bool,
}
/// The type an assignment-pattern type key names (10.9).
fn key_type(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    id: ExprId,
    dims: &PackedDimensions,
) -> Converted<semantic_types::TyId> {
    expression_types::cast_type(tree, session, id, tree.declarations(), dims)
        .ok_or_else(|| unsupported("assignment pattern type key"))
}

/// Whether a member or element of type `ty` matches type key `key`
/// (10.9.1-2, by the matching-types rules of 6.22.1).
fn type_matches(
    session: &mut Session,
    functions: &const_functions::ConstantFunctions,
    dims: &PackedDimensions,
    ty: &Type,
    key: semantic_types::TyId,
) -> bool {
    semantic_types::of_type(&mut session.types, functions, ty, &dims.const_env)
        .is_some_and(|element| session.types.matches(element, key))
}

fn indexed_items(
    kind: NativeKind,
    bindings: Vec<Pair>,
    count: usize,
    index_offset: impl Fn(i128) -> Option<usize>,
    type_fill: Option<Pair>,
) -> Converted<Vec<(Pair, bool)>> {
    match kind {
        NativeKind::List => {
            if bindings.len() != count {
                return Err(unsupported(format!(
                    "assignment pattern with {} items for {count} elements",
                    bindings.len()
                )));
            }
            Ok(bindings.into_iter().map(|value| (value, false)).collect())
        }
        NativeKind::Repeat(times) => {
            let size = times
                .checked_mul(bindings.len())
                .ok_or_else(|| unsupported("assignment pattern repetition size"))?;
            if size != count {
                return Err(unsupported(format!(
                    "assignment pattern with {size} items for {count} elements"
                )));
            }
            Ok(bindings
                .iter()
                .copied()
                .cycle()
                .take(count)
                .map(|value| (value, false))
                .collect())
        }
        NativeKind::Keyed(keys) => {
            let mut slots = vec![None; count];
            let mut default = None;
            for (key, value) in keys.into_iter().zip(bindings) {
                match key {
                    Key::Index(index) => {
                        let slot = index_offset(index)
                            .and_then(|at| slots.get_mut(at))
                            .ok_or_else(|| {
                                unsupported(format!(
                                    "assignment pattern index {index} outside the target"
                                ))
                            })?;
                        *slot = Some((value, false));
                    }
                    Key::Default => default = Some((value, true)),
                    Key::Type(_) => {
                        return Err(unsupported("type key in an integer assignment pattern"));
                    }
                    Key::Member(_) | Key::Name { .. } => {
                        return Err(unsupported("member key in an array assignment pattern"));
                    }
                }
            }
            slots
                .into_iter()
                .enumerate()
                .map(|(at, value)| {
                    // A type key that matches the element type fills the
                    // elements no index key sets, before the default does
                    // (10.9.1).
                    value
                        .or_else(|| type_fill.map(|value| (value, false)))
                        .or(default)
                        .ok_or_else(|| {
                            unsupported(format!(
                                "assignment pattern without a value for element {at}"
                            ))
                        })
                })
                .collect()
        }
    }
}
/// Split the type keys out of a keyed array pattern: the rest of the
/// pattern, and the value of the last type key that matches the element
/// type, which fills the elements no index key sets (10.9.1). A type key
/// that matches no element type at this level sets nothing here; descent
/// into subarrays and structure elements is not supported.
fn extract_type_fill(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    functions: &const_functions::ConstantFunctions,
    kind: NativeKind,
    bindings: Vec<Pair>,
    element: &VariableDimensions,
    dims: &PackedDimensions,
) -> Converted<(NativeKind, Vec<Pair>, Option<Pair>)> {
    let NativeKind::Keyed(keys) = kind else {
        return Ok((kind, bindings, None));
    };
    if !keys.iter().any(|key| matches!(key, Key::Type(_))) {
        return Ok((NativeKind::Keyed(keys), bindings, None));
    }
    let element_type = element.type_id(&mut session.types, functions, &dims.const_env);
    let mut kept = Vec::new();
    let mut kept_bindings = Vec::new();
    let mut fill = None;
    for (key, value) in keys.into_iter().zip(bindings) {
        match key {
            Key::Type(id) => {
                let key = key_type(tree, session, id, dims)?;
                if element_type.is_some_and(|element| session.types.matches(element, key)) {
                    fill = Some(value);
                }
            }
            key => {
                kept.push(key);
                kept_bindings.push(value);
            }
        }
    }
    Ok((NativeKind::Keyed(kept), kept_bindings, fill))
}

fn select<'a>(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    functions: &const_functions::ConstantFunctions,
    kind: NativeKind,
    bindings: Vec<Pair>,
    shape: Shape<'a>,
    dims: &PackedDimensions,
) -> Converted<(Vec<Selected<'a>>, bool, bool)> {
    if let Some(aggregate) = shape.unpacked_struct.clone()
        && shape.unpacked.is_empty()
    {
        return Ok((
            unpacked_struct_items(tree, session, functions, &aggregate, kind, bindings, dims)?,
            false,
            true,
        ));
    }
    if let Some((dim, rest)) = shape.unpacked.split_first() {
        let (left, right, count) = dimension_bounds(functions, &dim.left, &dim.right, dims)?;
        // Selecting an unpacked dimension keeps the declaration of its
        // element type, including the nominal structure identity and the
        // class of a handle element.
        let sub = Shape::Owned(Rc::new(VariableDimensions::new(VariableShape {
            container: None,
            real: false,
            unpacked: rest.to_vec(),
            ..(*shape).clone_shape()
        })));
        let (kind, bindings, fill) =
            extract_type_fill(tree, session, functions, kind, bindings, &sub, dims)?;
        let items = indexed_items(
            kind,
            bindings,
            count,
            |index| {
                (left.min(right)..=left.max(right))
                    .contains(&index)
                    .then(|| usize::try_from(index.abs_diff(left)).ok())
                    .flatten()
            },
            fill,
        )?;
        return Ok((
            items
                .into_iter()
                .map(|(value, default)| Selected {
                    value,
                    shape: sub.clone(),
                    default,
                    check: !default,
                })
                .collect(),
            true,
            false,
        ));
    }
    if let Some((dim, rest)) = shape.packed.split_first()
        && !rest.is_empty()
    {
        let (left, right, count) = dimension_bounds(functions, &dim.left, &dim.right, dims)?;
        let sub = Shape::Owned(Rc::new(VariableDimensions::new(VariableShape {
            declared: type_names::Declared::default(),
            container: None,
            chandle: false,
            randomizable: true,
            enum_domain: None,
            packed_enum: false,
            real: false,
            string: false,
            class: None,
            virtual_interface: None,
            packed: rest.to_vec(),
            unpacked: Vec::new(),
            signed: shape.signed_element_depth == Some(1),
            is_2state: shape.is_2state,
            members: shape.members.clone(),
            signed_element_depth: shape
                .signed_element_depth
                .and_then(|depth| depth.checked_sub(1))
                .filter(|depth| *depth > 0),
            unpacked_struct: None,
        })));
        let (kind, bindings, fill) =
            extract_type_fill(tree, session, functions, kind, bindings, &sub, dims)?;
        let items = indexed_items(
            kind,
            bindings,
            count,
            |index| {
                (left.min(right)..=left.max(right))
                    .contains(&index)
                    .then(|| usize::try_from(index.abs_diff(left)).ok())
                    .flatten()
            },
            fill,
        )?;
        return Ok((
            items
                .into_iter()
                .map(|(value, default)| Selected {
                    value,
                    shape: sub.clone(),
                    default,
                    check: false,
                })
                .collect(),
            false,
            true,
        ));
    }
    if !shape.members.is_empty() {
        let members = &shape.members;
        if members.iter().any(packed_structs::PackedMember::is_tagged)
            || (members.len() > 1 && members.iter().all(|member| member.offset() == 0))
        {
            return Err(unsupported("assignment pattern for a packed union"));
        }
        let values = match kind {
            NativeKind::List => {
                if bindings.len() != members.len() {
                    return Err(unsupported(format!(
                        "assignment pattern with {} items for {} structure members",
                        bindings.len(),
                        members.len()
                    )));
                }
                bindings.into_iter().map(|value| (value, false)).collect()
            }
            NativeKind::Keyed(keys) => {
                let mut named = Vec::new();
                let mut typed = Vec::new();
                let mut default = None;
                for (key, value) in keys.into_iter().zip(bindings) {
                    match key {
                        Key::Member(name) => named.push((name, value)),
                        Key::Type(id) => {
                            typed.push((key_type(tree, session, id, dims)?, value));
                        }
                        Key::Default => default = Some(value),
                        Key::Index(_) | Key::Name { .. } => {
                            return Err(unsupported("structure assignment pattern form"));
                        }
                    }
                }
                if let Some((name, _)) = named
                    .iter()
                    .find(|(name, _)| !members.iter().any(|member| member.name() == name))
                {
                    return Err(unsupported(format!(
                        "assignment pattern key `{name}` that is not a structure member"
                    )));
                }
                // A member key sets its member; a type key sets each
                // member of a matching type that no member key set, the
                // last of a type winning; the default sets the rest
                // (10.9.2).
                let mut values = Vec::new();
                for member in members {
                    let value = named
                        .iter()
                        .find(|(name, _)| name == member.name())
                        .map(|(_, value)| (*value, false))
                        .or_else(|| {
                            typed
                                .iter()
                                .rev()
                                .find(|(key, _)| {
                                    type_matches(session, functions, dims, member.r#type(), *key)
                                })
                                .map(|(_, value)| (*value, false))
                        })
                        .or(default.map(|value| (value, true)))
                        .ok_or_else(|| {
                            unsupported(format!(
                                "assignment pattern without a value for member `{}`",
                                member.name()
                            ))
                        })?;
                    values.push(value);
                }
                values
            }
            _ => return Err(unsupported("structure assignment pattern form")),
        };
        return Ok((
            members
                .iter()
                .zip(values)
                .map(|(member, (value, default))| Selected {
                    value,
                    shape: Shape::Owned(Rc::new(procedural::dimensions_from_type(member.r#type()))),
                    default,
                    check: false,
                })
                .collect(),
            false,
            false,
        ));
    }
    let width = shape_width(functions, &shape, dims)
        .ok_or_else(|| unsupported("assignment pattern target width"))?;
    let bit = Shape::Owned(Rc::new(VariableDimensions::new(VariableShape {
        declared: type_names::Declared::default(),
        container: None,
        chandle: false,
        randomizable: true,
        enum_domain: None,
        packed_enum: false,
        real: false,
        string: false,
        class: None,
        virtual_interface: None,
        packed: Vec::new(),
        unpacked: Vec::new(),
        signed: false,
        is_2state: shape.is_2state,
        members: Vec::new(),
        signed_element_depth: None,
        unpacked_struct: None,
    })));
    let items = indexed_items(
        kind,
        bindings,
        width,
        |index| {
            usize::try_from(index)
                .ok()
                .filter(|index| *index < width)
                .map(|index| width - 1 - index)
        },
        None,
    )?;
    Ok((
        items
            .into_iter()
            .map(|(value, _)| Selected {
                value,
                shape: bit.clone(),
                default: false,
                check: false,
            })
            .collect(),
        false,
        false,
    ))
}
/// Resolve a native typed target into existing semantic shape metadata.
pub(in crate::ast) fn typed_shape(
    root: ExprId,
    tree: &AnalysisContext<'_>,
    dims: &PackedDimensions,
) -> Converted<Option<VariableDimensions>> {
    let native = tree.declarations();
    let ExprKind::Pattern { target, .. } = &native.expression(root).kind else {
        return Err(mismatch());
    };
    let Some(target) = target else {
        return Ok(None);
    };
    let ty = if let Some((name, dimensions)) = type_binding::argument_alias(*target, native) {
        if !dimensions.is_empty() {
            return Err(unsupported("assignment pattern type"));
        }
        dims.type_aliases
            .get(&name)
            .cloned()
            .ok_or_else(|| unsupported(format!("assignment pattern type `{name}`")))?
    } else if let ExprKind::Type(id) = native.expression(*target).kind {
        let data = native.data_type(id);
        let celox_sv_syntax::DataTypeKind::Builtin(name) = &data.kind else {
            return Err(unsupported("assignment pattern type"));
        };
        if !data.packed_dimensions.is_empty()
            || !matches!(
                name.as_str(),
                "byte" | "shortint" | "int" | "longint" | "integer" | "time"
            )
        {
            return Err(unsupported("assignment pattern type"));
        }
        type_binding::builtin_type(name, data.signed).ok_or_else(mismatch)?
    } else {
        return Err(unsupported("assignment pattern type"));
    };
    Ok(Some(procedural::dimensions_from_type(&ty)))
}
pub(in crate::ast) fn constant_native(
    root: ExprId,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dims: &PackedDimensions,
) -> Converted<bound_patterns::Value> {
    let shape =
        typed_shape(root, tree, dims)?.ok_or_else(|| unsupported("assignment pattern type"))?;
    let value = bind_native(root, tree.declarations(), tree, session, dims, Some(&shape))?;
    super::constant_pattern_value(tree, session, value, &shape, dims)
}
pub(in crate::ast) fn expression(
    root: ExprId,
    shape: &VariableDimensions,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dims: &PackedDimensions,
) -> Converted<Option<Expr>> {
    bind_native(root, tree.declarations(), tree, session, dims, Some(shape)).map(Some)
}
pub(in crate::ast) fn bind_native(
    root: ExprId,
    native: &celox_sv_syntax::Declarations,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dims: &PackedDimensions,
    shape: Option<&VariableDimensions>,
) -> Converted<Expr> {
    enum Job<'a> {
        Pattern(ExprId, Shape<'a>),
        Value(Selected<'a>),
        Finish {
            count: usize,
            reverse: bool,
            keep: bool,
        },
        Broadcast(usize),
    }
    let owned = typed_shape(root, tree, dims)?;
    let shape = owned.as_ref().or(shape).ok_or_else(|| {
        // The pattern itself tells which of a design's many it is.
        let mut text = native.token_text(native.expression(root).tokens);
        if let Some((end, _)) = text.char_indices().nth(80) {
            text.truncate(end);
            text.push_str("...");
        }
        unsupported(format!("assignment pattern without target shape: `{text}`"))
    })?;
    let mut jobs = vec![Job::Pattern(root, Shape::Borrowed(shape))];
    let mut values = Vec::new();
    while let Some(job) = jobs.pop() {
        match job {
            Job::Pattern(id, shape) => {
                let plan = bound_patterns::prepare_native(id, tree, session, dims, Some(&shape))?;
                let (items, reverse, keep) = select(
                    tree,
                    session,
                    &tree.functions,
                    plan.kind,
                    plan.bindings,
                    shape,
                    dims,
                )?;
                jobs.push(Job::Finish {
                    count: items.len(),
                    reverse,
                    keep,
                });
                jobs.extend(items.into_iter().rev().map(Job::Value));
            }
            Job::Value(Selected {
                value: id,
                shape,
                default,
                check,
            }) => {
                let mut id = id;
                while let ExprKind::Group(inner) = native.expression(id).kind {
                    id = inner;
                }
                let pattern = matches!(native.expression(id).kind, ExprKind::Pattern { .. });
                if default
                    && let Some((dim, rest)) = shape.unpacked.split_first()
                    && !pattern
                    && native_rank(id, native, tree, session, dims)? < shape.unpacked.len()
                {
                    let (_, _, count) =
                        dimension_bounds(&tree.functions, &dim.left, &dim.right, dims)?;
                    let sub = Shape::Owned(Rc::new(VariableDimensions::new(VariableShape {
                        container: None,
                        real: false,
                        unpacked: rest.to_vec(),
                        ..(*shape).clone_shape()
                    })));
                    jobs.push(Job::Broadcast(count));
                    jobs.push(Job::Value(Selected {
                        value: id,
                        shape: sub,
                        default: true,
                        check: false,
                    }));
                    continue;
                }
                // A default that is not broadcast to the elements is an
                // array of the selected one's type, checked as an item is
                // (IEEE 1800-2023 10.9.1, 7.6).
                if check || (default && !pattern && !shape.unpacked.is_empty()) {
                    check_native_array(tree, session, id, &shape, dims)?;
                }
                // An item of an unpacked level is assigned to a subarray, so
                // a value that is no array is not assignment compatible with
                // it (10.9.1, 7.6).
                if check && !pattern && !shape.unpacked.is_empty() {
                    check_subarray_item(tree, session, id, &shape, dims)?;
                }
                // A typed pattern of a member must name the member's
                // structure type (6.22.3).
                if pattern
                    && shape.unpacked.is_empty()
                    && let Some(structure) =
                        unpacked_structs::shape_struct(&shape, tree, session, dims)
                {
                    unpacked_structs::check_value(structure, id, native, tree, session, dims)?;
                }
                if let Some(aggregate) = shape.unpacked_struct.clone()
                    && shape.unpacked.is_empty()
                    && !pattern
                {
                    // A default that is not a structure of the member's type
                    // applies to each of its members (IEEE 1800-2023 10.9.2).
                    if default
                        && !unpacked_structs::value_struct(id, native, tree, session, dims)
                            .zip(unpacked_structs::shape_struct(&shape, tree, session, dims))
                            .is_some_and(|(value, target)| session.types.matches(value, target))
                    {
                        let members = aggregate.members();
                        jobs.push(Job::Finish {
                            count: members.len(),
                            reverse: false,
                            keep: true,
                        });
                        jobs.extend(members.iter().rev().map(|member| {
                            Job::Value(Selected {
                                value: id,
                                shape: Shape::Owned(Rc::new(procedural::dimensions_from_type(
                                    &member.r#type,
                                ))),
                                default: true,
                                check: false,
                            })
                        }));
                        continue;
                    }
                    let structure = shape
                        .type_id(&mut session.types, &tree.functions, &dims.const_env)
                        .ok_or_else(|| unsupported("unpacked structure member type"))?;
                    unpacked_structs::check_value(structure, id, native, tree, session, dims)?;
                    values.push(expression_binding::bind_value(
                        id, native, tree, session, dims, None,
                    )?);
                    continue;
                }
                // An item of an enumerated element is of its type (6.19.3).
                if !pattern
                    && shape.unpacked.is_empty()
                    && shape.container.is_none()
                    && let Some(enumeration) = shape
                        .type_id(&mut session.types, &tree.functions, &dims.const_env)
                        .filter(|ty| session.types.is_enum(*ty))
                {
                    assignments::check_assign(
                        tree,
                        session,
                        id,
                        native,
                        dims,
                        enumeration,
                        || "assignment pattern item".into(),
                    )?;
                }
                // A handle is checked where it is one element; a value
                // given to a subarray is an array, checked as one above.
                let class = expression_binding::objects::shape_class(&shape, tree, session, dims);
                if let Some(class) = class.as_deref()
                    && !pattern
                    && shape.unpacked.is_empty()
                {
                    if let Some(call) = expression_binding::object_constructor(
                        tree, session, id, native, dims, class,
                    )? {
                        values.push(call);
                        continue;
                    }
                    expression_binding::check_handle_assignment(
                        tree,
                        session,
                        id,
                        native,
                        dims,
                        class,
                        || "assignment pattern member".into(),
                    )?;
                }
                let scalar_string = shape.string && shape.unpacked.is_empty();
                if (scalar_string || class.is_some()) && !pattern {
                    let value =
                        expression_binding::bind_value(id, native, tree, session, dims, None)?;
                    values.push(if scalar_string {
                        expression_binding::string_operand(value, dims)
                    } else {
                        value
                    });
                    continue;
                }
                if pattern {
                    // A chandle has no elements a pattern could give (6.14).
                    if shape.chandle && shape.unpacked.is_empty() {
                        expression_binding::check_chandle_assignment(
                            true, id, tree, native, dims, session,
                        )?;
                    }
                    let typed = typed_shape(id, tree, dims)?;
                    jobs.push(Job::Pattern(
                        id,
                        typed
                            .map(|shape| Shape::Owned(Rc::new(shape)))
                            .unwrap_or(shape),
                    ));
                } else {
                    let chandle = shape.chandle && shape.unpacked.is_empty();
                    expression_binding::check_chandle_assignment(
                        chandle, id, tree, native, dims, session,
                    )?;
                    let value = if chandle {
                        expression_binding::bind_chandle_value(
                            id, native, tree, session, dims, None,
                        )?
                    } else {
                        expression_binding::bind_value(id, native, tree, session, dims, None)?
                    };
                    let type_dims = dimensions::query_context_for_operand(id, tree, session, dims)?;
                    let signed =
                        dimensions::expression_signedness(&value, &type_dims, &type_dims.const_env)
                            .ok_or_else(|| unsupported("assignment pattern operand signedness"))?;
                    let descend_default = default
                        && (shape.unpacked.is_empty()
                            || native_rank(id, native, tree, session, dims)?
                                < shape.unpacked.len());
                    let value_type =
                        bound_patterns::native_value_type(id, tree, session, &type_dims)?;
                    values.push(assignment_value(
                        tree,
                        session,
                        &value,
                        signed,
                        value_type.as_ref(),
                        &shape,
                        dims,
                        descend_default,
                    )?);
                }
            }
            Job::Finish {
                count,
                reverse,
                keep,
            } => {
                let start = values.len().checked_sub(count).ok_or_else(mismatch)?;
                let mut parts = values.split_off(start);
                if reverse {
                    parts.reverse();
                }
                values.push(if !keep && parts.len() == 1 {
                    parts.pop().unwrap()
                } else {
                    Expr::Concat(parts)
                });
            }
            Job::Broadcast(count) => {
                let value = values.pop().ok_or_else(mismatch)?;
                values.push(if count == 1 {
                    value
                } else {
                    Expr::Concat(vec![value; count])
                });
            }
        }
    }
    if values.len() != 1 {
        return Err(mismatch());
    }
    values.pop().ok_or_else(mismatch)
}
/// The member values of an assignment pattern for an unpacked structure
/// (IEEE 1800-2023 10.9.2), first declared first.
fn unpacked_struct_items<'a>(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    functions: &const_functions::ConstantFunctions,
    aggregate: &unpacked_structs::UnpackedStruct,
    kind: NativeKind,
    bindings: Vec<Pair>,
    dims: &PackedDimensions,
) -> Converted<Vec<Selected<'a>>> {
    let members = aggregate.members();
    let values = match kind {
        NativeKind::List => {
            if bindings.len() != members.len() {
                return Err(unsupported(format!(
                    "assignment pattern with {} items for {} structure members",
                    bindings.len(),
                    members.len()
                )));
            }
            bindings.into_iter().map(|value| (value, false)).collect()
        }
        NativeKind::Repeat(times) => indexed_items(
            NativeKind::Repeat(times),
            bindings,
            members.len(),
            |_| None,
            None,
        )?,
        NativeKind::Keyed(keys) => {
            let mut named = Vec::new();
            let mut typed = Vec::new();
            let mut default = None;
            for (key, value) in keys.into_iter().zip(bindings) {
                match key {
                    Key::Member(name) if aggregate.member(&name).is_some() => {
                        named.push((name, value))
                    }
                    Key::Member(name) => {
                        return Err(unsupported(format!(
                            "assignment pattern key `{name}` that is not a structure member"
                        )));
                    }
                    Key::Type(id) => {
                        typed.push((key_type(tree, session, id, dims)?, value));
                    }
                    Key::Default => default = Some(value),
                    Key::Index(_) | Key::Name { .. } => {
                        return Err(unsupported("structure assignment pattern form"));
                    }
                }
            }
            // Member, then type, then default keys, as for a packed
            // structure (10.9.2).
            let mut values = Vec::new();
            for member in members {
                let value = named
                    .iter()
                    .find(|(name, _)| *name == member.name)
                    .map(|(_, value)| (*value, false))
                    .or_else(|| {
                        typed
                            .iter()
                            .rev()
                            .find(|(key, _)| {
                                type_matches(session, functions, dims, &member.r#type, *key)
                            })
                            .map(|(_, value)| (*value, false))
                    })
                    .or(default.map(|value| (value, true)))
                    .ok_or_else(|| {
                        unsupported(format!(
                            "assignment pattern without a value for member `{}`",
                            member.name
                        ))
                    })?;
                values.push(value);
            }
            values
        }
    };
    Ok(members
        .iter()
        .zip(values)
        .map(|(member, (value, default))| Selected {
            value,
            shape: Shape::Owned(Rc::new(procedural::dimensions_from_type(&member.r#type))),
            default,
            check: false,
        })
        .collect())
}
fn native_rank(
    mut id: ExprId,
    native: &celox_sv_syntax::Declarations,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dims: &PackedDimensions,
) -> Converted<usize> {
    let root = id;
    let mut indices = 0;
    loop {
        match &native.expression(id).kind {
            ExprKind::Group(inner) => id = *inner,
            ExprKind::Select {
                expression,
                dimension,
            } if matches!(dimension.kind, celox_sv_syntax::DimensionKind::Index(_)) => {
                indices += 1;
                id = *expression;
            }
            ExprKind::Name(name) => {
                return Ok(dims
                    .get(&instances::normalize_identifier(&name.spelling))
                    .map_or(0, |shape| shape.unpacked.len().saturating_sub(indices)));
            }
            _ => {
                // Members and subarrays retain their resolved unpacked shape.
                // An array-valued default is copied as a whole value rather
                // than broadcast into scalar elements (10.9.2).
                if let Some(shape) = dimensions::operand_shape(root, tree, session, dims)? {
                    return Ok(shape.unpacked.len());
                }
                // Any other expression, such as a call returning an array,
                // has the unpacked dimensions of its own type.
                let Ok(mut ty) = expression_types::type_of(tree, session, root, native, dims)
                else {
                    return Ok(0);
                };
                let mut rank = 0;
                while let semantic_types::TyData::Array {
                    kind: semantic_types::ArrayKind::Fixed(_),
                    element,
                } = session.types.data(ty)
                {
                    rank += 1;
                    ty = *element;
                }
                return Ok(rank);
            }
        }
    }
}
fn check_subarray_item(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    id: ExprId,
    shape: &VariableDimensions,
    dims: &PackedDimensions,
) -> Converted<()> {
    let Some(item) = dimensions::operand_shape(id, tree, session, dims)? else {
        return Ok(());
    };
    if !item.unpacked.is_empty() || item.container.is_some() {
        return Ok(());
    }
    let Some(target) = shape.type_id(&mut session.types, &tree.functions, &dims.const_env) else {
        return Ok(());
    };
    Err(AnalyzerError::NotUnpackedArray {
        context: "assignment pattern item".into(),
        target: session.types.spell(target),
    })
}
fn check_native_array(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    id: ExprId,
    shape: &VariableDimensions,
    dims: &PackedDimensions,
) -> Converted<()> {
    array_compatibility::check_native_unpacked_assignment(
        tree,
        session,
        id,
        shape,
        || "assignment pattern item".into(),
        dims,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    struct Original(Option<SyntaxTree>);
    impl Drop for Original {
        fn drop(&mut self) {
            if let Some(tree) = self.0.take() {
                std::thread::Builder::new()
                    .stack_size(128 * 1024 * 1024)
                    .spawn(move || drop(tree))
                    .unwrap()
                    .join()
                    .unwrap();
            }
        }
    }
    #[test]
    fn deeply_nested_native_typed_patterns_share_the_job_stack() {
        let session = &mut Session::default();
        let tree = std::thread::Builder::new()
            .stack_size(128 * 1024 * 1024)
            .spawn(|| {
                let source = format!(
                    "module M(output logic y);typedef logic T;assign y={}1{};endmodule",
                    "T'{".repeat(500),
                    "}".repeat(500)
                );
                crate::source_tree::parse_source(&source, Path::new("native-deep-typed-pattern.sv"))
            })
            .unwrap()
            .join()
            .unwrap()
            .unwrap();
        let original = Original(Some(tree));
        let context = AnalysisContext::unspecialized(original.0.as_ref().unwrap());
        let tree = &context;
        let root = tree.declarations().assign_statements[0].assignments[0].right;
        let dims = PackedDimensions::new(
            HashMap::default(),
            &ConstEnv::default(),
            &TypeAliases::from_iter([("T".into(), Type::new(TypeKind::Logic))]),
        );
        let result = bind_native(root, tree.declarations(), tree, session, &dims, None).map(Some);
        drop(context);
        drop(original);
        assert!(matches!(
            result.unwrap().unwrap(),
            Expr::Resize { width: 1, .. }
        ));
    }
    #[test]
    fn deeply_nested_native_scalar_patterns_bind_on_the_default_stack() {
        let session = &mut Session::default();
        let tree = std::thread::Builder::new()
            .stack_size(128 * 1024 * 1024)
            .spawn(|| {
                let source = format!(
                    "module M(output logic y);assign y={}1{};endmodule",
                    "'{".repeat(500),
                    "}".repeat(500)
                );
                crate::source_tree::parse_source(
                    &source,
                    Path::new("native-deep-vector-pattern.sv"),
                )
            })
            .unwrap()
            .join()
            .unwrap()
            .unwrap();
        let original = Original(Some(tree));
        let context = AnalysisContext::unspecialized(original.0.as_ref().unwrap());
        let tree = &context;
        let root = tree.declarations().assign_statements[0].assignments[0].right;
        let shape = procedural::dimensions_from_type(&Type::new(TypeKind::Logic));
        let dims = PackedDimensions::new(
            HashMap::default(),
            &ConstEnv::default(),
            &TypeAliases::default(),
        );
        let result = expression(root, &shape, tree, session, &dims);
        let mut ty = Type::new(TypeKind::Logic);
        ty.packed_ranges = vec![
            PackedRange::new(
                ConstExpr::Literal("0".into()),
                ConstExpr::Literal("0".into())
            );
            500
        ];
        let ranked = expression(
            root,
            &procedural::dimensions_from_type(&ty),
            tree,
            session,
            &dims,
        );
        drop(context);
        drop(original);
        assert!(matches!(
            result.unwrap().unwrap(),
            Expr::Resize { width: 1, .. }
        ));
        let ranked = ranked.unwrap().unwrap();
        let mut at = &ranked;
        let mut depth = 0;
        while let Expr::Concat(parts) = at {
            assert_eq!(parts.len(), 1);
            depth += 1;
            at = &parts[0];
        }
        assert_eq!(depth, 499);
        assert!(matches!(at, Expr::Resize { width: 1, .. }));
        drop(ranked);
    }
}
