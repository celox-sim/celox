//! Array locator methods (IEEE 1800-2023 7.12.1) and iterator index
//! querying (7.12.4).
//!
//! A locator call binds to an [`ObjectOp::Locator`] call that the frontend
//! lowers as a loop where the call is evaluated. The `with` expression reads
//! the element and its index through two hidden locals of the enclosing
//! procedural body, the way `foreach` declares its loop variables.
use super::*;
use celox_sv_syntax::{Declarations, ExprId, ExprKind};
use std::sync::Mutex;
use system_functions::{
    ContainerKind, ContainerLayout, HandleProperty, Locator, LocatorKey, LocatorMethod,
    LocatorSource,
};

/// The locals that locator methods declare while a procedural body binds
/// its expressions, which the body then declares (see
/// [`procedural::BodyBuilder`]).
#[derive(Debug, Clone, Default)]
pub(super) struct LocalSink(Arc<Mutex<Sink>>);

#[derive(Debug, Default)]
struct Sink {
    prefix: usize,
    locals: Vec<LocalVariable>,
}

/// A body's sink is its own: binding contexts compare equal regardless.
impl PartialEq for LocalSink {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for LocalSink {}

impl LocalSink {
    pub(super) fn new(prefix: usize) -> Self {
        Self(Arc::new(Mutex::new(Sink {
            prefix,
            ..Sink::default()
        })))
    }

    /// Declare local `source` of the locator call at `site`. The name joins
    /// the local counter of the body when it started, which tells the bodies
    /// of a scope apart, and the site.
    fn declare(
        &self,
        site: &str,
        source: &str,
        r#type: Type,
        tree: &AnalysisContext<'_>,
        dims: &PackedDimensions,
    ) -> String {
        let mut sink = self.0.lock().expect("locator locals");
        let name = format!("{source}@{}.{site}", sink.prefix);
        if sink.locals.iter().any(|local| local.name == name) {
            return name;
        }
        sink.locals.push(LocalVariable {
            name: name.clone(),
            source_name: source.to_string(),
            r#type: crate::ir::Type::from_ast(
                procedural::scoped_type(r#type, &dims.const_env),
                &const_functions::Environment {
                    constants: &*dims.const_env,
                    functions: &tree.state.const_functions,
                },
            ),
        });
        name
    }

    /// The locals declared since the last call.
    pub(super) fn take(&self) -> Vec<LocalVariable> {
        std::mem::take(&mut self.0.lock().expect("locator locals").locals)
    }
}

/// The iterator of an enclosing `with` expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Iterator {
    source: String,
    /// The name of the index querying method (7.12.4).
    method: String,
    /// The local holding the index.
    index: String,
    /// For elements of unpacked structure type kept as records, the
    /// container and the local holding the record of the element.
    record: Option<(Arc<ContainerType>, String)>,
}

/// The array a locator method searches.
enum Array {
    Container(Arc<ContainerType>),
    Fixed {
        left: i64,
        right: i64,
        element: Type,
        layout: ContainerLayout,
    },
}

impl Array {
    fn element(&self) -> &Type {
        match self {
            Self::Container(container) => &container.element,
            Self::Fixed { element, .. } => element,
        }
    }

    fn index(&self) -> Type {
        match self {
            Self::Container(container) => {
                container.key.clone().unwrap_or_else(procedural::int_type)
            }
            Self::Fixed { .. } => procedural::int_type(),
        }
    }

    /// The layout of a queue of the elements or of the indices.
    fn result(&self, method: LocatorMethod) -> ContainerLayout {
        let queue = ContainerKind::Queue { capacity: 0 };
        let int = ContainerLayout {
            kind: queue,
            width: 32,
            signed: true,
            two_state: true,
            handle: None,
        };
        match self {
            Self::Container(container) if method.returns_index() => match container.layout.kind {
                ContainerKind::Associative {
                    key_width,
                    signed,
                    string,
                    object,
                } => ContainerLayout {
                    kind: queue,
                    width: key_width,
                    signed,
                    two_state: string
                        || object
                        || container
                            .key
                            .as_ref()
                            .is_some_and(|key| key.kind() == TypeKind::Bit),
                    handle: if string {
                        Some(HandleProperty::String)
                    } else if object {
                        Some(HandleProperty::Object)
                    } else {
                        None
                    },
                },
                _ => int,
            },
            Self::Container(container) => ContainerLayout {
                kind: queue,
                ..container.layout
            },
            Self::Fixed { .. } if method.returns_index() => int,
            Self::Fixed { layout, .. } => *layout,
        }
    }
}

/// A locator method call, before its `with` expression is bound.
struct Call {
    method: LocatorMethod,
    base: ExprId,
    array: Array,
    iterator: String,
    index_method: String,
    predicate: Option<ExprId>,
}

fn parse(
    id: ExprId,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    dims: &PackedDimensions,
) -> Converted<Option<Call>> {
    let (method_call, predicate) = match &native.expression(id).kind {
        ExprKind::ArrayMethodWith {
            expression,
            predicate,
        } => (*expression, Some(*predicate)),
        ExprKind::Call { .. } | ExprKind::Member { .. } => (id, None),
        _ => return Ok(None),
    };
    let (member, arguments) = match &native.expression(method_call).kind {
        ExprKind::Call {
            expression,
            arguments,
        } => (*expression, arguments.as_slice()),
        _ => (method_call, &[][..]),
    };
    let ExprKind::Member {
        expression: base,
        separator,
        name,
    } = &native.expression(member).kind
    else {
        return Ok(None);
    };
    let Some(method) = LocatorMethod::from_name(&name.spelling) else {
        return Ok(None);
    };
    if separator != "." {
        return Ok(None);
    }
    let Some(array) = array(*base, native, tree, dims)? else {
        return Ok(None);
    };
    // `a.min()` lists one empty argument.
    let arguments = match arguments {
        [argument] if argument.name.is_none() && argument.value.is_none() => &[],
        arguments => arguments,
    };
    let mut names = Vec::new();
    for argument in arguments {
        if argument.name.is_some() {
            return Err(unsupported("named array locator method argument"));
        }
        names.push(match argument.value {
            None => None,
            Some(value) => match &native.expression(value).kind {
                ExprKind::Name(name) => Some(instances::normalize_identifier(&name.spelling)),
                _ => return Err(unsupported("array locator method iterator argument")),
            },
        });
    }
    if names.len() > 2 {
        return Err(unsupported("array locator method arguments"));
    }
    if predicate.is_none() && !names.is_empty() {
        return Err(AnalyzerError::Parse(format!(
            "`{}` names an iterator without a with clause (IEEE 1800-2023 7.12)",
            name.spelling
        )));
    }
    if predicate.is_none() && method.finds() {
        return Err(AnalyzerError::Parse(format!(
            "`{}` requires a with clause (IEEE 1800-2023 7.12.1)",
            name.spelling
        )));
    }
    let mut names = names.into_iter();
    Ok(Some(Call {
        method,
        base: *base,
        array,
        iterator: names.next().flatten().unwrap_or_else(|| "item".into()),
        index_method: names.next().flatten().unwrap_or_else(|| "index".into()),
        predicate,
    }))
}

/// The array that `base` names, if it is an unpacked array whose elements a
/// locator method can return.
fn array(
    base: ExprId,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    dims: &PackedDimensions,
) -> Converted<Option<Array>> {
    if let Some(container) = containers::native_type(base, native, tree, dims)? {
        if matches!(container.layout.kind, ContainerKind::Associative { .. })
            && container.key.is_none()
        {
            return Err(unsupported("array locator method on a wildcard index"));
        }
        return Ok(Some(Array::Container(container)));
    }
    let ExprKind::Name(name) = &native
        .expression(expression_binding::ungroup(base, native))
        .kind
    else {
        return Ok(None);
    };
    let Some(shape) = dims.get(&instances::normalize_identifier(&name.spelling)) else {
        return Ok(None);
    };
    if shape.container.is_some() || shape.unpacked.is_empty() {
        return Ok(None);
    }
    if shape.unpacked.len() != 1 {
        return Err(unsupported(
            "array locator method on a multidimensional unpacked array",
        ));
    }
    if shape.real {
        return Err(unsupported("array locator method on real elements"));
    }
    let bound = |expr: &ConstExpr| {
        eval_ast_const_expr(&tree.state, expr, &dims.const_env)
            // The index local is an `int` (7.12.4).
            .and_then(|value| i32::try_from(value).ok())
            .map(i64::from)
            .ok_or_else(|| unsupported("array locator method bound outside int"))
    };
    let left = bound(&shape.unpacked[0].left)?;
    let right = bound(&shape.unpacked[0].right)?;
    let element = if shape.string {
        type_binding::builtin_type("string", None).expect("string is a builtin type")
    } else if let Some(class) = &shape.class {
        classes::handle(class)
    } else {
        let mut ty = Type::new(if shape.is_2state {
            TypeKind::Bit
        } else {
            TypeKind::Logic
        });
        ty.is_signed = shape.signed;
        ty.packed_ranges = shape
            .packed
            .iter()
            .map(|dimension| PackedRange::new(dimension.left.clone(), dimension.right.clone()))
            .collect();
        ty.members = shape.members.clone();
        ty.is_enum = shape.enum_domain.is_some();
        ty.enum_domain = shape.enum_domain.clone();
        ty.packed_enum = shape.packed_enum;
        ty.signed_element_depth = shape.signed_element_depth;
        ty
    };
    let width = casts::expr_type_from_type(&tree.state, &element, &dims.const_env)
        .map(|shape| shape.width)
        .filter(|width| (1..=64).contains(width))
        .ok_or_else(|| unsupported("array locator method on elements wider than 64 bits"))?;
    let handle = if shape.string {
        Some(HandleProperty::String)
    } else if shape.class.is_some() {
        Some(HandleProperty::Object)
    } else {
        None
    };
    let layout = ContainerLayout {
        kind: ContainerKind::Queue { capacity: 0 },
        width: width as u32,
        signed: shape.signed,
        two_state: shape.is_2state || handle.is_some(),
        handle,
    };
    Ok(Some(Array::Fixed {
        left,
        right,
        element,
        layout,
    }))
}

/// The queue type that locator call `id` returns, if it is one.
pub(super) fn result_type(
    id: ExprId,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    dims: &PackedDimensions,
) -> Converted<Option<Arc<ContainerType>>> {
    let Some(call) = parse(id, native, tree, dims)? else {
        return Ok(None);
    };
    let record = match &call.array {
        Array::Container(container) if !call.method.returns_index() => container.record.clone(),
        _ => None,
    };
    Ok(Some(Arc::new(ContainerType {
        key: None,
        layout: call.array.result(call.method),
        element: if call.method.returns_index() {
            call.array.index()
        } else {
            call.array.element().clone()
        },
        record,
    })))
}

/// Bind locator call `id`, if it is one.
pub(super) fn bind(
    id: ExprId,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    dims: &PackedDimensions,
) -> Converted<Option<Expr>> {
    let Some(call) = parse(id, native, tree, dims)? else {
        return Ok(None);
    };
    let sink = dims
        .locator_locals
        .as_ref()
        .ok_or_else(|| unsupported("array locator method outside a procedural body"))?;
    let element = call.array.element().clone();
    // An element kept as a record is read through its record handle, as
    // `q[i].m` reads it.
    let record = match &call.array {
        Array::Container(container) if container.record.is_some() => Some(container.clone()),
        _ => None,
    };
    // The tokens of the call tell it apart from the calls in its arguments.
    let tokens = native.expression(id).tokens;
    let site = format!("{}.{}", tokens.start, tokens.end);
    let local_type = if record.is_some() {
        let mut handle = Type::new(TypeKind::Object);
        handle.packed_ranges.push(PackedRange::new(
            ConstExpr::Literal("31".into()),
            ConstExpr::Literal("0".into()),
        ));
        handle
    } else {
        element.clone()
    };
    // The iterator and the index method may have the same name.
    let iterator = sink.declare(
        &format!("{site}.item"),
        &call.iterator,
        local_type,
        tree,
        dims,
    );
    let index = sink.declare(
        &format!("{site}.index"),
        &call.index_method,
        call.array.index(),
        tree,
        dims,
    );
    let (predicate, key) = match call.predicate {
        Some(predicate) => {
            // The iterator is visible in the with expression only (7.12).
            let mut scope = dims.clone();
            let shape = procedural::dimensions_from_type(&element);
            for key in
                parameters::binding_keys(&call.iterator, &scope.const_env, shape.packed.len())
            {
                scope.const_env.remove(&key);
            }
            scope.parameter_values.remove(&call.iterator);
            if record.is_some() {
                scope.remove(&call.iterator);
                scope.expression_signedness.remove(&call.iterator);
            } else {
                scope.insert(call.iterator.clone(), shape);
                scope
                    .expression_signedness
                    .insert(call.iterator.clone(), element.is_signed());
            }
            Arc::make_mut(&mut scope.locator_iterators).push(Iterator {
                source: call.iterator.clone(),
                method: call.index_method.clone(),
                index: index.clone(),
                record: record
                    .clone()
                    .map(|container| (container, iterator.clone())),
            });
            // `item.index` reads the index local by its own name.
            let index_type = call.array.index();
            scope.insert(index.clone(), procedural::dimensions_from_type(&index_type));
            scope
                .expression_signedness
                .insert(index.clone(), index_type.is_signed());
            let mut value = expression_binding::value(predicate, native, tree, &scope)?;
            let key = if call.method.finds() {
                None
            } else if expression_binding::string_value(&value, &scope) {
                Some(LocatorKey::String)
            } else {
                if expression_binding::real_value(&value, &scope) {
                    return Err(unsupported("array locator method on real values"));
                }
                let r#type = dimensions::bound_bits_type(&tree.state, &value, &scope)
                    .filter(|r#type| (1..=64).contains(&r#type.width))
                    .ok_or_else(|| {
                        unsupported("array locator with expression wider than 64 bits")
                    })?;
                value = Expr::Resize {
                    expr: Box::new(value),
                    width: r#type.width,
                    signed: r#type.signed,
                };
                Some(LocatorKey::Integral {
                    width: r#type.width as u32,
                    signed: r#type.signed,
                })
            };
            procedural::rename_expr_idents(&mut value, &mut |name| {
                if *name == call.iterator {
                    *name = iterator.clone();
                }
                None
            });
            (value, key)
        }
        None => {
            let key = match &call.array {
                _ if element.kind() == TypeKind::String => LocatorKey::String,
                _ if element.class().is_some()
                    || element.unpacked_struct().is_some()
                    || element.container.is_some() =>
                {
                    return Err(unsupported(
                        "array locator method comparing nonintegral elements requires a with expression",
                    ));
                }
                Array::Container(container) => LocatorKey::Integral {
                    width: container.layout.width,
                    signed: container.layout.signed,
                },
                Array::Fixed { layout, .. } => LocatorKey::Integral {
                    width: layout.width,
                    signed: layout.signed,
                },
            };
            (Expr::Ident(iterator.clone()), Some(key))
        }
    };
    if matches!(call.method, LocatorMethod::Min | LocatorMethod::Max) && key.is_none() {
        return Err(AnalyzerError::Parse("invalid array locator call".into()));
    }
    let (source, array) = match &call.array {
        Array::Container(container) => (
            LocatorSource::Container(container.layout),
            expression_binding::value(call.base, native, tree, dims)?,
        ),
        Array::Fixed { left, right, .. } => {
            let ExprKind::Name(name) = &native
                .expression(expression_binding::ungroup(call.base, native))
                .kind
            else {
                return Err(AnalyzerError::Parse("invalid array locator call".into()));
            };
            let name = instances::normalize_identifier(&name.spelling);
            let lvalue = selects::lvalue_from_indices(
                &tree.state,
                name,
                vec![ConstExpr::Ident(index.clone())],
                dims,
            )?;
            (
                LocatorSource::Fixed {
                    left: *left,
                    right: *right,
                },
                statements::expr_from_lvalue(&tree.state, &lvalue, dims),
            )
        }
    };
    let locator = Locator {
        method: call.method,
        source,
        result: call.array.result(call.method),
        key,
    };
    Ok(Some(classes::object_call(
        ObjectOp::Locator(locator),
        vec![array, predicate, Expr::Ident(iterator), Expr::Ident(index)],
    )))
}

/// The container and the record handle of the iterator that `id` names when
/// the elements it visits are kept as records, for member reads such as
/// `item.m` (see `containers::element_member`).
pub(super) fn record_iterator(
    id: ExprId,
    native: &Declarations,
    dims: &PackedDimensions,
) -> Option<(Arc<ContainerType>, Expr)> {
    let ExprKind::Name(name) = &native.expression(id).kind else {
        return None;
    };
    let name = instances::normalize_identifier(&name.spelling);
    let iterator = dims
        .locator_iterators
        .iter()
        .rev()
        .find(|iterator| iterator.source == name)?;
    let (container, handle) = iterator.record.as_ref()?;
    Some((container.clone(), Expr::Ident(handle.clone())))
}

/// A query of the index of the iterator of an enclosing `with` expression,
/// such as `item.index` (IEEE 1800-2023 7.12.4).
pub(super) fn index(
    id: ExprId,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    dims: &PackedDimensions,
) -> Converted<Option<Expr>> {
    if dims.locator_iterators.is_empty() {
        return Ok(None);
    }
    // The whole value of an element kept as a record.
    if let Some((container, handle)) = record_iterator(id, native, dims) {
        let record = container.record.clone().expect("a record element");
        return Ok(Some(classes::object_call(
            ObjectOp::Unbox(record),
            vec![handle],
        )));
    }
    let (member, arguments) = match &native.expression(id).kind {
        ExprKind::Call {
            expression,
            arguments,
        } => (*expression, arguments.as_slice()),
        ExprKind::Member { .. } => (id, &[][..]),
        _ => return Ok(None),
    };
    let ExprKind::Member {
        expression: base,
        separator,
        name,
    } = &native.expression(member).kind
    else {
        return Ok(None);
    };
    let ExprKind::Name(receiver) = &native.expression(*base).kind else {
        return Ok(None);
    };
    let receiver = instances::normalize_identifier(&receiver.spelling);
    // An inner iterator hides an outer one of the same name.
    let Some(iterator) = dims
        .locator_iterators
        .iter()
        .rev()
        .find(|iterator| iterator.source == receiver)
    else {
        return Ok(None);
    };
    if separator != "." || name.spelling != iterator.method {
        return Ok(None);
    }
    let arguments = match arguments {
        [argument] if argument.name.is_none() && argument.value.is_none() => &[],
        arguments => arguments,
    };
    match arguments {
        [] => {}
        [argument] if argument.name.is_none() => {
            let dimension = match argument.value {
                Some(value) => constant_binding::bind(value, tree, dims)?
                    .and_then(|value| eval_ast_const_expr(&tree.state, &value, &dims.const_env)),
                None => None,
            };
            if dimension != Some(1) {
                return Err(unsupported(
                    "iterator index query of a dimension other than the first",
                ));
            }
        }
        _ => return Err(unsupported("iterator index query arguments")),
    }
    Ok(Some(Expr::Ident(iterator.index.clone())))
}
