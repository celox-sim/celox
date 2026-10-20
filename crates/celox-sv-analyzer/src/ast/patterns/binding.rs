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
fn indexed_items(
    kind: NativeKind,
    bindings: Vec<Pair>,
    count: usize,
    index_offset: impl Fn(i128) -> Option<usize>,
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
                    Key::Member(_) => {
                        return Err(unsupported("member key in an array assignment pattern"));
                    }
                }
            }
            slots
                .into_iter()
                .enumerate()
                .map(|(at, value)| {
                    value.or(default).ok_or_else(|| {
                        unsupported(format!(
                            "assignment pattern without a value for element {at}"
                        ))
                    })
                })
                .collect()
        }
    }
}
fn select<'a>(
    state: &AnalysisState,
    kind: NativeKind,
    bindings: Vec<Pair>,
    shape: Shape<'a>,
    dims: &PackedDimensions,
) -> Converted<(Vec<Selected<'a>>, bool, bool)> {
    if let Some((dim, rest)) = shape.unpacked.split_first() {
        let (left, right, count) = dimension_bounds(state, &dim.left, &dim.right, dims)?;
        let sub = Shape::Owned(Rc::new(VariableDimensions {
            real: false,
            unpacked: rest.to_vec(),
            ..(*shape).clone()
        }));
        let items = indexed_items(kind, bindings, count, |index| {
            (left.min(right)..=left.max(right))
                .contains(&index)
                .then(|| usize::try_from(index.abs_diff(left)).ok())
                .flatten()
        })?;
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
    if !shape.members.is_empty() {
        let members = &shape.members;
        if members.len() > 1 && members.iter().all(|member| member.offset() == 0) {
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
                bindings
            }
            NativeKind::Keyed(keys) => {
                let mut named = Vec::new();
                let mut default = None;
                for (key, value) in keys.into_iter().zip(bindings) {
                    match key {
                        Key::Member(name) => named.push((name, value)),
                        Key::Default => default = Some(value),
                        Key::Index(_) => {
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
                members
                    .iter()
                    .map(|member| {
                        named
                            .iter()
                            .find(|(name, _)| name == member.name())
                            .map(|(_, value)| *value)
                            .or(default)
                            .ok_or_else(|| {
                                unsupported(format!(
                                    "assignment pattern without a value for member `{}`",
                                    member.name()
                                ))
                            })
                    })
                    .collect::<Converted<Vec<_>>>()?
            }
            _ => return Err(unsupported("structure assignment pattern form")),
        };
        return Ok((
            members
                .iter()
                .zip(values)
                .map(|(member, value)| Selected {
                    value,
                    shape: Shape::Owned(Rc::new(procedural::dimensions_from_type(member.r#type()))),
                    default: false,
                    check: false,
                })
                .collect(),
            false,
            false,
        ));
    }
    if let Some((dim, rest)) = shape.packed.split_first()
        && !rest.is_empty()
    {
        let (left, right, count) = dimension_bounds(state, &dim.left, &dim.right, dims)?;
        let sub = Shape::Owned(Rc::new(VariableDimensions {
            randomizable: true,
            real: false,
            packed: rest.to_vec(),
            unpacked: Vec::new(),
            signed: shape.signed_element_depth == Some(1),
            is_2state: shape.is_2state,
            members: Vec::new(),
            signed_element_depth: shape
                .signed_element_depth
                .and_then(|depth| depth.checked_sub(1))
                .filter(|depth| *depth > 0),
        }));
        let items = indexed_items(kind, bindings, count, |index| {
            (left.min(right)..=left.max(right))
                .contains(&index)
                .then(|| usize::try_from(index.abs_diff(left)).ok())
                .flatten()
        })?;
        return Ok((
            items
                .into_iter()
                .map(|(value, _)| Selected {
                    value,
                    shape: sub.clone(),
                    default: false,
                    check: false,
                })
                .collect(),
            false,
            true,
        ));
    }
    let width = shape_width(state, &shape, dims)
        .ok_or_else(|| unsupported("assignment pattern target width"))?;
    let bit = Shape::Owned(Rc::new(VariableDimensions {
        randomizable: true,
        real: false,
        packed: Vec::new(),
        unpacked: Vec::new(),
        signed: false,
        is_2state: shape.is_2state,
        members: Vec::new(),
        signed_element_depth: None,
    }));
    let items = indexed_items(kind, bindings, width, |index| {
        usize::try_from(index)
            .ok()
            .filter(|index| *index < width)
            .map(|index| width - 1 - index)
    })?;
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
    dims: &PackedDimensions,
) -> Converted<bound_patterns::Value> {
    let shape =
        typed_shape(root, tree, dims)?.ok_or_else(|| unsupported("assignment pattern type"))?;
    let value = bind_native(root, tree.declarations(), tree, dims, Some(&shape))?;
    super::constant_pattern_value(&tree.state, value, &shape, dims)
}
pub(in crate::ast) fn expression(
    root: ExprId,
    shape: &VariableDimensions,
    tree: &AnalysisContext<'_>,
    dims: &PackedDimensions,
) -> Converted<Option<Expr>> {
    bind_native(root, tree.declarations(), tree, dims, Some(shape)).map(Some)
}
pub(in crate::ast) fn bind_native(
    root: ExprId,
    native: &celox_sv_syntax::Declarations,
    tree: &AnalysisContext<'_>,
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
    let shape = owned
        .as_ref()
        .or(shape)
        .ok_or_else(|| unsupported("assignment pattern without target shape"))?;
    let mut jobs = vec![Job::Pattern(root, Shape::Borrowed(shape))];
    let mut values = Vec::new();
    while let Some(job) = jobs.pop() {
        match job {
            Job::Pattern(id, shape) => {
                let plan = bound_patterns::prepare_native(id, tree, dims, Some(&shape))?;
                let (items, reverse, keep) =
                    select(&tree.state, plan.kind, plan.bindings, shape, dims)?;
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
                    && native_rank(id, native, dims) < shape.unpacked.len()
                {
                    let (_, _, count) = dimension_bounds(&tree.state, &dim.left, &dim.right, dims)?;
                    let sub = Shape::Owned(Rc::new(VariableDimensions {
                        real: false,
                        unpacked: rest.to_vec(),
                        ..(*shape).clone()
                    }));
                    jobs.push(Job::Broadcast(count));
                    jobs.push(Job::Value(Selected {
                        value: id,
                        shape: sub,
                        default: true,
                        check: false,
                    }));
                    continue;
                }
                if check {
                    check_native_array(&tree.state, id, native, &shape, dims)?;
                }
                if pattern {
                    let typed = typed_shape(id, tree, dims)?;
                    jobs.push(Job::Pattern(
                        id,
                        typed
                            .map(|shape| Shape::Owned(Rc::new(shape)))
                            .unwrap_or(shape),
                    ));
                } else {
                    let value = expression_binding::bind_value(id, native, tree, dims, None)?;
                    let width = shape_width(&tree.state, &shape, dims)
                        .ok_or_else(|| unsupported("assignment pattern element width"))?;
                    let signed =
                        expr_signedness(&value, &dims.expression_signedness, &dims.functions)
                            .unwrap_or(false);
                    values.push(Expr::Resize {
                        expr: Box::new(value),
                        width,
                        signed,
                    });
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
fn native_rank(
    mut id: ExprId,
    native: &celox_sv_syntax::Declarations,
    dims: &PackedDimensions,
) -> usize {
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
                return dims
                    .get(&instances::normalize_identifier(&name.spelling))
                    .map_or(0, |shape| shape.unpacked.len().saturating_sub(indices));
            }
            _ => return 0,
        }
    }
}
fn check_native_array(
    state: &AnalysisState,
    id: ExprId,
    native: &celox_sv_syntax::Declarations,
    shape: &VariableDimensions,
    dims: &PackedDimensions,
) -> Converted<()> {
    array_compatibility::check_native_unpacked_assignment(
        state,
        id,
        native,
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
            &HashMap::default(),
            &HashMap::from_iter([("T".into(), Type::new(TypeKind::Logic))]),
        );
        let result = bind_native(root, tree.declarations(), tree, &dims, None).map(Some);
        drop(context);
        drop(original);
        assert!(matches!(
            result.unwrap().unwrap(),
            Expr::Resize { width: 1, .. }
        ));
    }
    #[test]
    fn deeply_nested_native_scalar_patterns_bind_on_the_default_stack() {
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
        let dims =
            PackedDimensions::new(HashMap::default(), &HashMap::default(), &HashMap::default());
        let result = expression(root, &shape, tree, &dims);
        let mut ty = Type::new(TypeKind::Logic);
        ty.packed_ranges = vec![
            PackedRange::new(
                ConstExpr::Literal("0".into()),
                ConstExpr::Literal("0".into())
            );
            500
        ];
        let ranked = expression(root, &procedural::dimensions_from_type(&ty), tree, &dims);
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
