//! Binding of dynamically sized unpacked value containers (clause 7).
use super::*;
use celox_sv_syntax::{Dimension, DimensionKind, ExprId, ExprKind};
use system_functions::{ContainerKind, ContainerLayout, ContainerOp, HandleProperty};

pub(super) fn with_dimensions(
    base: Type,
    dims: &[Dimension],
    tree: &AnalysisContext<'_>,
    context: &PackedDimensions,
) -> Converted<Type> {
    let Some(first) = dims.first() else {
        return Ok(base);
    };
    let mut index_type = None;
    let kind = match first.kind {
        DimensionKind::Unsized => Some(ContainerKind::Dynamic),
        DimensionKind::Queue(bound) => {
            let capacity = match bound {
                None => 0,
                Some(id) => {
                    let bound = constant_binding::bind(id, tree, context)?
                        .and_then(|v| eval_ast_const_expr(&tree.state, &v, &context.const_env))
                        .and_then(|v| u32::try_from(v).ok())
                        .filter(|v| *v > 0 && *v < u32::MAX)
                        .ok_or_else(|| {
                            unsupported("queue bound must be a positive integer below 2^32-1")
                        })?;
                    bound + 1
                }
            };
            Some(ContainerKind::Queue { capacity })
        }
        DimensionKind::Index(id) => {
            let key_type = match &tree.declarations().expression(id).kind {
                ExprKind::Type(id) => type_binding::bind(*id, tree, context)?,
                ExprKind::Name(name) => {
                    let name = instances::normalize_identifier(&name.spelling);
                    match context.type_aliases.get(&name) {
                        Some(ty) => Some(ty.clone()),
                        // A class: the index is an object handle (IEEE
                        // 1800-2023 7.8.5).
                        None if context.get(&name).is_none() => classes::resolve(tree, &name)
                            .and_then(|class| classes::handle_type(tree, &class)),
                        None => None,
                    }
                }
                _ => None,
            };
            match key_type {
                None => None,
                Some(ty) => {
                    index_type = Some(ty.clone());
                    let string = ty.kind() == TypeKind::String;
                    let object = ty.kind() == TypeKind::Object && ty.container.is_none();
                    let shape = casts::expr_type_from_type(&tree.state, &ty, &context.const_env)
                        .ok_or_else(|| unsupported("associative index type"))?;
                    if !string
                        && !object
                        && (!matches!(ty.kind(), TypeKind::Bit | TypeKind::Logic | TypeKind::Reg)
                            || shape.width > 64
                            || !ty.unpacked_ranges.is_empty())
                    {
                        return Err(unsupported(
                            "associative indices require strings, class handles or integral types up to 64 bits",
                        ));
                    }
                    Some(ContainerKind::Associative {
                        key_width: shape.width as u32,
                        signed: shape.signed && !object,
                        string,
                        object,
                    })
                }
            }
        }
        DimensionKind::Wildcard => return Err(unsupported("wildcard associative index type")),
        _ => None,
    };
    let Some(kind) = kind else {
        return Ok(types::type_with_unpacked_ranges(
            base,
            constant_binding::bind_unpacked_ranges(dims, tree, context)?,
        ));
    };
    // `T a[x][y]`: each element of `a` is a container `T [y]` (IEEE
    // 1800-2023 7.4.5, 7.8), held by its handle like any container value.
    let base = if dims.len() > 1 {
        with_dimensions(base, &dims[1..], tree, context)?
    } else {
        base
    };
    if base.container.is_none() && dims.len() > 1 || !base.unpacked_ranges.is_empty() {
        return Err(unsupported(
            "dynamic container of fixed-size unpacked arrays",
        ));
    }
    if matches!(base.kind(), TypeKind::Real | TypeKind::Shortreal) {
        return Err(unsupported("real dynamic container elements"));
    }
    let record = match base.unpacked_struct() {
        Some(aggregate) => unpacked_structs::record(&tree.state, aggregate, &context.const_env)?,
        None => None,
    };
    let shape = casts::expr_type_from_type(&tree.state, &base, &context.const_env)
        .ok_or_else(|| unsupported("dynamic container element type"))?;
    if record.is_none() && (shape.width == 0 || shape.width > 64) {
        return Err(unsupported("dynamic container elements wider than 64 bits"));
    }
    let handle = match base.kind() {
        // An element in a record, or a nested container, is a handle.
        _ if record.is_some() || base.container.is_some() => Some(HandleProperty::Object),
        TypeKind::String => Some(HandleProperty::String),
        TypeKind::Object => Some(HandleProperty::Object),
        _ => None,
    };
    let layout = ContainerLayout {
        kind,
        width: if record.is_some() {
            classes::HANDLE_WIDTH as u32
        } else {
            shape.width as u32
        },
        signed: shape.signed && record.is_none(),
        two_state: matches!(base.kind(), TypeKind::Bit | TypeKind::Process) || handle.is_some(),
        handle,
    };
    let mut ty = Type::new(TypeKind::Object);
    ty.packed_ranges.push(PackedRange::new(
        ConstExpr::Literal("31".into()),
        ConstExpr::Literal("0".into()),
    ));
    ty.container = Some(Arc::new(ContainerType {
        key: index_type,
        layout,
        element: base,
        record,
    }));
    Ok(ty)
}

pub(super) fn native_type(
    id: ExprId,
    native: &celox_sv_syntax::Declarations,
    tree: &AnalysisContext<'_>,
    dims: &PackedDimensions,
) -> Converted<Option<Arc<ContainerType>>> {
    if let ExprKind::Name(name) = &native
        .expression(expression_binding::ungroup(id, native))
        .kind
        && let Some(shape) = dims.get(&instances::normalize_identifier(&name.spelling))
    {
        return Ok(shape.container.clone());
    }
    if let Some((property, _)) = expression_binding::object_property(tree, id, native, dims)? {
        return Ok(property.r#type.container);
    }
    if let Some(name) = expression_binding::object_static_property(tree, id, native, dims)?
        && let Some(shape) = dims.get(&name)
    {
        return Ok(shape.container.clone());
    }
    // An element popped from a queue of containers.
    let callee = match &native
        .expression(expression_binding::ungroup(id, native))
        .kind
    {
        ExprKind::Call { expression, .. } => Some(*expression),
        ExprKind::Member { .. } => Some(expression_binding::ungroup(id, native)),
        _ => None,
    };
    if let Some(callee) = callee
        && let ExprKind::Member {
            expression, name, ..
        } = &native.expression(callee).kind
        && matches!(name.spelling.as_str(), "pop_front" | "pop_back")
        && let Some(outer) = native_type(*expression, native, tree, dims)?
        && let Some(inner) = outer.element.container.clone()
    {
        return Ok(Some(inner));
    }
    // An element of a container of containers, `m[a]` of `m[a][b]`.
    if let ExprKind::Select {
        expression,
        dimension,
    } = &native
        .expression(expression_binding::ungroup(id, native))
        .kind
        && matches!(
            dimension.kind,
            DimensionKind::Index(_) | DimensionKind::Queue(None)
        )
        && let Some(outer) = native_type(*expression, native, tree, dims)?
    {
        return Ok(outer.element.container.clone());
    }
    Ok(None)
}

pub(super) fn call(op: ContainerOp, layout: ContainerLayout, args: Vec<Expr>) -> Expr {
    classes::object_call(ObjectOp::Container { op, layout }, args)
}

/// Container assignment is value assignment, whereas object elements are handles.
pub(super) fn assignment(
    root: ExprId,
    tree: &AnalysisContext<'_>,
    dims: &PackedDimensions,
    target: &ContainerType,
) -> Converted<Expr> {
    let native = tree.declarations();
    let root = expression_binding::ungroup(root, native);
    let (construct, initializer) = match &native.expression(root).kind {
        ExprKind::Call {
            expression,
            arguments,
        } if arguments.len() == 1 && arguments[0].name.is_none() => {
            (*expression, arguments[0].value)
        }
        _ => (root, None),
    };
    if let ExprKind::Select {
        expression,
        dimension,
    } = &native.expression(construct).kind
        && matches!(&native.expression(*expression).kind, ExprKind::Name(n) if n.spelling == "new")
    {
        if target.layout.kind != ContainerKind::Dynamic {
            return Err(unsupported("new[] requires a dynamic array"));
        }
        let DimensionKind::Index(size) = dimension.kind else {
            return Err(unsupported("dynamic array size"));
        };
        let size = expression_binding::value(size, native, tree, dims)?;
        let init = match initializer {
            Some(id) => {
                check_compatible(id, native, tree, dims, target)?;
                expression_binding::value(id, native, tree, dims)?
            }
            None => Expr::Literal("0".into()),
        };
        return Ok(call(ContainerOp::New, target.layout, vec![size, init]));
    }
    let items = match &native.expression(root).kind {
        ExprKind::Concatenation(items) => Some(items.clone()),
        ExprKind::Pattern {
            target: None,
            items,
        } if items.iter().all(|item| item.key.is_none()) => {
            Some(items.iter().map(|item| item.value).collect())
        }
        _ => None,
    };
    if let Some(items) = items {
        if matches!(target.layout.kind, ContainerKind::Associative { .. }) {
            return Err(unsupported("associative array assignment pattern"));
        }
        let values = items
            .into_iter()
            .map(|id| element_value(id, tree, dims, target))
            .collect::<Converted<Vec<_>>>()?;
        return Ok(call(ContainerOp::Literal, target.layout, values));
    }
    check_compatible(root, native, tree, dims, target)?;
    Ok(call(
        ContainerOp::Convert,
        target.layout,
        vec![expression_binding::value(root, native, tree, dims)?],
    ))
}
fn check_compatible(
    id: ExprId,
    native: &celox_sv_syntax::Declarations,
    tree: &AnalysisContext<'_>,
    dims: &PackedDimensions,
    target: &ContainerType,
) -> Converted<()> {
    let Some(source) = native_type(id, native, tree, dims)? else {
        return Err(unsupported(
            "container assignment requires a compatible container",
        ));
    };
    if source.key != target.key
        || source.element != target.element
        || match (source.layout.kind, target.layout.kind) {
            (ContainerKind::Associative { .. }, _) | (_, ContainerKind::Associative { .. }) => {
                source.layout.kind != target.layout.kind
            }
            _ => false,
        }
    {
        return Err(unsupported("incompatible dynamic container assignment"));
    }
    Ok(())
}

fn element_value(
    id: ExprId,
    tree: &AnalysisContext<'_>,
    dims: &PackedDimensions,
    target: &ContainerType,
) -> Converted<Expr> {
    let native = tree.declarations();
    // An element that is itself a container takes a container value, as an
    // assignment to a container variable does.
    if let Some(inner) = target.element.container.as_deref() {
        return assignment(id, tree, dims, inner);
    }
    if native_type(id, native, tree, dims)?.is_some() {
        return Err(unsupported("container used as an element"));
    }
    if let Some(class) = target.element.class.as_deref() {
        if let Some(call) = expression_binding::object_constructor(tree, id, native, dims, class)? {
            return Ok(call);
        }
        expression_binding::check_handle_assignment(tree, id, native, dims, class, || {
            "container element assignment".into()
        })?;
    }
    if let Some(aggregate) = target.element.unpacked_struct() {
        unpacked_structs::check_value(aggregate, id, native, tree, dims)?;
    }
    let shape = procedural::dimensions_from_type(&target.element);
    let value = expression_binding::bind_value(id, native, tree, dims, Some(&shape))?;
    if let Some(record) = &target.record {
        return Ok(classes::object_call(
            ObjectOp::Box(record.clone()),
            vec![value],
        ));
    }
    Ok(if target.layout.handle == Some(HandleProperty::String) {
        expression_binding::string_operand(value, dims)
    } else {
        value
    })
}
fn index_value(
    id: ExprId,
    base: ExprId,
    tree: &AnalysisContext<'_>,
    dims: &PackedDimensions,
    target: &ContainerType,
) -> Converted<Expr> {
    let native = tree.declarations();
    // A class index takes handles of its class or a derived one.
    if let Some(class) = target.key.as_ref().and_then(|key| key.class()).filter(|_| {
        matches!(
            target.layout.kind,
            ContainerKind::Associative { object: true, .. }
        )
    }) {
        expression_binding::check_handle_assignment(tree, id, native, dims, class, || {
            "associative array index".into()
        })?;
    }
    if matches!(
        target.layout.kind,
        ContainerKind::Associative { string: true, .. }
    ) && !expression_binding::native_string(tree, id, native, dims)
        && !matches!(&native.expression(expression_binding::ungroup(id, native)).kind, ExprKind::Literal(text) if text.starts_with('"'))
    {
        return Err(unsupported(
            "string associative index requires a string value",
        ));
    }
    let value = match &native
        .expression(expression_binding::ungroup(id, native))
        .kind
    {
        ExprKind::Name(name)
            if name.spelling == "$"
                && matches!(target.layout.kind, ContainerKind::Queue { .. }) =>
        {
            Expr::Binary {
                left: Box::new(call(
                    ContainerOp::Size,
                    target.layout,
                    vec![expression_binding::value(base, native, tree, dims)?],
                )),
                op: BinaryOp::Sub,
                right: Box::new(Expr::Literal("1".into())),
            }
        }
        ExprKind::Binary {
            left,
            operator,
            right,
        } if operator == "+" || operator == "-" => Expr::Binary {
            left: Box::new(index_value(*left, base, tree, dims, target)?),
            op: if operator == "+" {
                BinaryOp::Add
            } else {
                BinaryOp::Sub
            },
            right: Box::new(index_value(*right, base, tree, dims, target)?),
        },
        _ => expression_binding::value(id, native, tree, dims)?,
    };
    Ok(
        if matches!(
            target.layout.kind,
            ContainerKind::Associative { string: true, .. }
        ) {
            expression_binding::string_operand(value, dims)
        } else {
            value
        },
    )
}

pub(super) fn value(
    id: ExprId,
    native: &celox_sv_syntax::Declarations,
    tree: &AnalysisContext<'_>,
    dims: &PackedDimensions,
    statement: bool,
) -> Converted<Option<Expr>> {
    if let Some(query) = query(id, native, tree, dims)? {
        return Ok(Some(query));
    }
    match &native.expression(id).kind {
        ExprKind::Binary {
            left,
            operator,
            right,
        } if native_type(*left, native, tree, dims)?.is_some()
            || native_type(*right, native, tree, dims)?.is_some() =>
        {
            let Some(target) = native_type(*left, native, tree, dims)? else {
                return Err(unsupported("container comparison requires two containers"));
            };
            check_compatible(*right, native, tree, dims, &target)?;
            let source = native_type(*right, native, tree, dims)?.expect("checked compatibility");
            if std::mem::discriminant(&source.layout.kind)
                != std::mem::discriminant(&target.layout.kind)
            {
                return Err(unsupported(
                    "container comparison requires equivalent array kinds",
                ));
            }

            if target.record.is_some() {
                return Err(unsupported(
                    "comparison of dynamic containers of unpacked structures",
                ));
            }
            // The elements are inner containers' handles, not their values.
            if target.element.container.is_some() {
                return Err(unsupported("comparison of nested dynamic containers"));
            }
            let op = match operator.as_str() {
                "==" | "!=" => ContainerOp::Equal,
                "===" | "!==" => ContainerOp::CaseEqual,
                _ => return Err(unsupported("integral operator on a dynamic container")),
            };
            let value = call(
                op,
                target.layout,
                vec![
                    expression_binding::value(*left, native, tree, dims)?,
                    expression_binding::value(*right, native, tree, dims)?,
                ],
            );
            Ok(Some(if matches!(operator.as_str(), "!=" | "!==") {
                Expr::Unary {
                    op: UnaryOp::LogicNot,
                    expr: Box::new(value),
                }
            } else {
                value
            }))
        }
        ExprKind::Select {
            expression,
            dimension,
        } => {
            let Some(target) = native_type(*expression, native, tree, dims)? else {
                return Ok(None);
            };
            let index = match dimension.kind {
                DimensionKind::Index(index) => Some(index),
                DimensionKind::Queue(None)
                    if matches!(target.layout.kind, ContainerKind::Queue { .. }) =>
                {
                    None
                }
                _ => return Err(unsupported("dynamic container slice")),
            };
            Ok(Some(element_read(
                &target,
                call(
                    ContainerOp::Get,
                    target.layout,
                    vec![
                        expression_binding::value(*expression, native, tree, dims)?,
                        match index {
                            Some(index) => index_value(index, *expression, tree, dims, &target)?,
                            None => Expr::Binary {
                                left: Box::new(call(
                                    ContainerOp::Size,
                                    target.layout,
                                    vec![expression_binding::value(
                                        *expression,
                                        native,
                                        tree,
                                        dims,
                                    )?],
                                )),
                                op: BinaryOp::Sub,
                                right: Box::new(Expr::Literal("1".into())),
                            },
                        },
                    ],
                ),
            )))
        }
        ExprKind::Call { .. } | ExprKind::Member { .. } => {
            // No-argument built-in methods also permit the bare member form,
            // e.g. q.size, q.pop_front and map.delete (clauses 7.9 and 7.10).
            let (expression, arguments) = match &native.expression(id).kind {
                ExprKind::Call {
                    expression,
                    arguments,
                } => (*expression, arguments.as_slice()),
                _ => (id, &[][..]),
            };
            let ExprKind::Member {
                expression: base,
                separator,
                name,
            } = &native.expression(expression).kind
            else {
                return Ok(None);
            };
            let Some(target) = native_type(*base, native, tree, dims)? else {
                return Ok(None);
            };
            if separator != "." {
                return Err(unsupported("container scoped call"));
            }
            let (op, count) = match name.spelling.as_str() {
                "reverse" | "sort" | "rsort"
                    if !matches!(target.layout.kind, ContainerKind::Associative { .. }) =>
                {
                    let op = match name.spelling.as_str() {
                        "reverse" => ContainerOp::Reverse,
                        "sort" => ContainerOp::Sort,
                        _ => ContainerOp::Rsort,
                    };
                    if op != ContainerOp::Reverse && target.element.unpacked_struct().is_some() {
                        return Err(unsupported(
                            "sorting unpacked structures requires a with expression",
                        ));
                    }
                    if op != ContainerOp::Reverse
                        && target.layout.handle == Some(HandleProperty::Object)
                    {
                        return Err(unsupported(
                            "sorting class handles requires a with expression",
                        ));
                    }
                    (op, 0)
                }
                "size" | "num"
                    if name.spelling == "size"
                        || matches!(target.layout.kind, ContainerKind::Associative { .. }) =>
                {
                    (ContainerOp::Size, 0)
                }
                "delete" if arguments.is_empty() => (ContainerOp::Clear, 0),
                "delete" if target.layout.kind != ContainerKind::Dynamic => {
                    (ContainerOp::Delete, 1)
                }
                "insert" if matches!(target.layout.kind, ContainerKind::Queue { .. }) => {
                    (ContainerOp::Insert, 2)
                }
                "push_front" if matches!(target.layout.kind, ContainerKind::Queue { .. }) => {
                    (ContainerOp::PushFront, 1)
                }
                "push_back" if matches!(target.layout.kind, ContainerKind::Queue { .. }) => {
                    (ContainerOp::PushBack, 1)
                }
                "pop_front" if matches!(target.layout.kind, ContainerKind::Queue { .. }) => {
                    (ContainerOp::PopFront, 0)
                }
                "pop_back" if matches!(target.layout.kind, ContainerKind::Queue { .. }) => {
                    (ContainerOp::PopBack, 0)
                }
                "exists" if matches!(target.layout.kind, ContainerKind::Associative { .. }) => {
                    (ContainerOp::Exists, 1)
                }
                "first" if matches!(target.layout.kind, ContainerKind::Associative { .. }) => {
                    (ContainerOp::First, 1)
                }
                "last" if matches!(target.layout.kind, ContainerKind::Associative { .. }) => {
                    (ContainerOp::Last, 1)
                }
                "next" if matches!(target.layout.kind, ContainerKind::Associative { .. }) => {
                    (ContainerOp::Next, 1)
                }
                "prev" if matches!(target.layout.kind, ContainerKind::Associative { .. }) => {
                    (ContainerOp::Prev, 1)
                }
                _ => return Err(unsupported(format!("container method `{}`", name.spelling))),
            };
            if op.mutates()
                && !matches!(op, ContainerOp::PopFront | ContainerOp::PopBack)
                && !statement
            {
                return Err(unsupported("void container method used as a value"));
            }
            if arguments.len() != count
                || arguments
                    .iter()
                    .any(|arg| arg.name.is_some() || arg.value.is_none())
            {
                return Err(unsupported(format!(
                    "arguments of container method `{}`",
                    name.spelling
                )));
            }
            if dims.type_query
                && matches!(op, ContainerOp::PopFront | ContainerOp::PopBack)
                && target.layout.handle.is_some()
            {
                return Err(unsupported("type queries on nonintegral queue pop results"));
            }
            let mut args = vec![expression_binding::value(*base, native, tree, dims)?];
            for (index, arg) in arguments.iter().enumerate() {
                let id = arg.value.unwrap();
                if matches!(
                    op,
                    ContainerOp::First | ContainerOp::Last | ContainerOp::Next | ContainerOp::Prev
                ) {
                    let ExprKind::Name(name) = &native.expression(id).kind else {
                        return Err(unsupported(
                            "associative traversal requires an index variable",
                        ));
                    };
                    let shape = dims
                        .get(&instances::normalize_identifier(&name.spelling))
                        .ok_or_else(|| unsupported("associative traversal index variable"))?;
                    // The index variable of a class index is passed by
                    // reference, so it has the index's class (IEEE 1800-2023
                    // 7.9.4).
                    let key_class = target.key.as_ref().and_then(|key| key.class());
                    if let ContainerKind::Associative { string, object, .. } = target.layout.kind
                        && (shape.string != string
                            || shape.container.is_some()
                            || !shape.unpacked.is_empty()
                            || object && shape.class.as_deref() != key_class)
                    {
                        return Err(unsupported(
                            "incompatible associative traversal index variable",
                        ));
                    }
                    args.push(expression_binding::value(id, native, tree, dims)?);
                } else if matches!(op, ContainerOp::PushFront | ContainerOp::PushBack)
                    || op == ContainerOp::Insert && index == 1
                {
                    args.push(element_value(id, tree, dims, &target)?);
                } else {
                    args.push(index_value(id, *base, tree, dims, &target)?);
                }
            }
            let value = call(op, target.layout, args);
            Ok(Some(
                if matches!(op, ContainerOp::PopFront | ContainerOp::PopBack) {
                    element_read(&target, value)
                } else {
                    value
                },
            ))
        }
        _ => Ok(None),
    }
}

fn query(
    id: ExprId,
    native: &celox_sv_syntax::Declarations,
    tree: &AnalysisContext<'_>,
    dims: &PackedDimensions,
) -> Converted<Option<Expr>> {
    let ExprKind::Call {
        expression,
        arguments,
    } = &native.expression(id).kind
    else {
        return Ok(None);
    };
    let ExprKind::Name(name) = &native.expression(*expression).kind else {
        return Ok(None);
    };
    if !matches!(
        name.spelling.as_str(),
        "$bits"
            | "$size"
            | "$left"
            | "$right"
            | "$low"
            | "$high"
            | "$increment"
            | "$dimensions"
            | "$unpacked_dimensions"
    ) {
        return Ok(None);
    }
    let Some(base) = arguments.first().and_then(|a| a.value) else {
        return Ok(None);
    };
    let Some(target) = native_type(base, native, tree, dims)? else {
        return Ok(None);
    };
    if arguments
        .iter()
        .any(|arg| arg.name.is_some() || arg.value.is_none())
    {
        return Err(unsupported("dynamic array query arguments"));
    }
    if matches!(
        name.spelling.as_str(),
        "$dimensions" | "$unpacked_dimensions"
    ) {
        if arguments.len() != 1 {
            return Err(unsupported("dynamic array query arguments"));
        }
        // A container of containers has one unpacked dimension per level.
        let mut innermost = target.clone();
        let mut unpacked = 1;
        while let Some(inner) = innermost.element.container.clone() {
            innermost = inner;
            unpacked += 1;
        }
        return Ok(Some(Expr::Literal(
            if name.spelling == "$unpacked_dimensions" {
                unpacked
            } else {
                unpacked
                    + if innermost.layout.handle.is_some() {
                        0
                    } else {
                        innermost.element.packed_ranges.len()
                    }
            }
            .to_string(),
        )));
    }
    if matches!(
        target.layout.kind,
        ContainerKind::Associative { string: true, .. }
    ) {
        return Err(unsupported("array query on a string associative dimension"));
    }
    if name.spelling == "$bits" {
        return Err(unsupported("$bits of a dynamic container"));
    }
    if arguments.len() > 2 {
        return Err(unsupported("dynamic array query arguments"));
    }
    let dimension = match arguments.get(1).and_then(|a| a.value) {
        None => 1,
        Some(id) => constant_binding::bind(id, tree, dims)?
            .and_then(|v| eval_ast_const_expr(&tree.state, &v, &dims.const_env))
            .ok_or_else(|| unsupported("nonconstant dynamic array query dimension"))?,
    };
    if dimension != 1 {
        return Err(unsupported(
            "query of a dynamic container element dimension",
        ));
    }
    if matches!(target.layout.kind, ContainerKind::Associative { .. }) && name.spelling != "$size" {
        return Err(unsupported("associative bound query"));
    }
    let size = || -> Converted<Expr> {
        Ok(call(
            ContainerOp::Size,
            target.layout,
            vec![expression_binding::value(base, native, tree, dims)?],
        ))
    };
    Ok(Some(match name.spelling.as_str() {
        "$size" => size()?,
        "$left" | "$low" => Expr::Literal("32'sd0".into()),
        "$increment" => Expr::Literal("32'shffffffff".into()),
        _ => Expr::Binary {
            left: Box::new(size()?),
            op: BinaryOp::Sub,
            right: Box::new(Expr::Literal("1".into())),
        },
    }))
}

pub(super) fn set(
    left: ExprId,
    right: ExprId,
    op: &str,
    nonblocking: bool,
    tree: &AnalysisContext<'_>,
    dims: &PackedDimensions,
) -> Converted<Option<Stmt>> {
    let native = tree.declarations();
    let ExprKind::Select {
        expression,
        dimension,
    } = &native.expression(left).kind
    else {
        return Ok(None);
    };
    let Some(target) = native_type(*expression, native, tree, dims)? else {
        return Ok(None);
    };
    if op != "=" || nonblocking {
        return Err(unsupported(
            "compound or nonblocking dynamic element assignment",
        ));
    }
    let index = match dimension.kind {
        DimensionKind::Index(index) => index_value(index, *expression, tree, dims, &target)?,
        DimensionKind::Queue(None) if matches!(target.layout.kind, ContainerKind::Queue { .. }) => {
            Expr::Binary {
                left: Box::new(call(
                    ContainerOp::Size,
                    target.layout,
                    vec![expression_binding::value(*expression, native, tree, dims)?],
                )),
                op: BinaryOp::Sub,
                right: Box::new(Expr::Literal("1".into())),
            }
        }
        _ => return Err(unsupported("dynamic element slice assignment")),
    };
    Ok(Some(Stmt::Eval(call(
        ContainerOp::Set,
        target.layout,
        vec![
            expression_binding::value(*expression, native, tree, dims)?,
            index,
            element_value(right, tree, dims, &target)?,
        ],
    ))))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn class_containers_preserve_access_and_compatibility_checks() {
        for (source, expected) in [
            (
                "module Top; class C; local int q[$]; endclass C c; initial begin c=new; c.q.push_back(1); end endmodule",
                "local",
            ),
            (
                "module Top; class C; protected int q[$]; endclass C c; int v; initial begin c=new; v=$size(c.q); end endmodule",
                "protected",
            ),
            (
                "module Top; class C; const int q[$]; endclass C c; initial c=new; endmodule",
                "const dynamic",
            ),
            (
                "module Top; class C; int q[$]; byte a[]; endclass C c; initial begin c=new; c.q=c.a; end endmodule",
                "incompatible",
            ),
            (
                "module Top; class C; int q[$]; endclass C c; initial begin c=new; c.q[0]<=1; end endmodule",
                "nonblocking",
            ),
            (
                "module Top; class C; int q[2]; endclass C c; initial c=new; endmodule",
                "unpacked dimensions",
            ),
        ] {
            let error = crate::analyze_source(source, Path::new("class-container-rejection.sv"))
                .unwrap_err()
                .to_string();
            assert!(error.contains(expected), "{source}: {error}");
        }
    }

    #[test]
    fn unsupported_container_forms_fail_before_lowering() {
        for (source, expected) in [
            ("module Top; bit[127:0] a[]; endmodule", "wider than 64"),
            (
                "module Top; int a[][2]; endmodule",
                "dynamic container of fixed-size unpacked arrays",
            ),
            (
                "module Top; int a[][], b[][]; bit x; initial x = a == b; endmodule",
                "comparison of nested dynamic containers",
            ),
            (
                "module Top; class A; endclass class B; endclass int m[A]; B b; initial begin b = new; m[b] = 1; end endmodule",
                "associative array index",
            ),
            (
                "module Top; class A; endclass class B; endclass int m[A]; B k; initial void'(m.first(k)); endmodule",
                "incompatible associative traversal index variable",
            ),
            ("module Top; int a[*]; endmodule", "wildcard"),
            (
                "module Top; int a[string]; initial a[123] = 7; endmodule",
                "requires a string",
            ),
            (
                "module Top; int q[$], value; initial value = q.push_back(7); endmodule",
                "void container",
            ),
            (
                "module Top; int a[], q[$]; bit value; initial value = a == q; endmodule",
                "equivalent array kinds",
            ),
            (
                "module Top; int a[]; initial a[0] <= 7; endmodule",
                "nonblocking",
            ),
            (
                "module Top; int a[]; initial a[0] += 7; endmodule",
                "compound",
            ),
        ] {
            let tree = crate::source_tree::parse_source(source, Path::new("rejected_container.sv"))
                .unwrap();
            let error = Source::from_syntax(&tree).unwrap_err().to_string();
            assert!(error.contains(expected), "{source}: {error}");
        }
    }

    #[test]
    fn typedef_container_metadata_survives_discovery() {
        let tree = crate::source_tree::parse_source(
            "module Top; typedef bit[7:0] queue_t[$]; queue_t q; endmodule",
            Path::new("container.sv"),
        )
        .unwrap();
        let source = Source::from_syntax(&tree).unwrap();
        let q = source.modules()[0]
            .signals()
            .iter()
            .find(|s| s.name() == "q")
            .unwrap();
        assert!(q.r#type().container.is_some(), "{:?}", q.r#type());
        let tree = crate::source_tree::parse_source("module Top; bit[7:0] a[] = new[2]; typedef bit[7:0] queue_t[$]; queue_t q; initial begin automatic bit[7:0] b[] = new[3]; q.push_back(9); a[1]=7; b[2]=8; if(a.size()==2 && b.size()==3 && q[0]==9)begin end end endmodule", Path::new("container.sv")).unwrap();
        Source::from_syntax(&tree).unwrap();
    }
}

#[cfg(test)]
mod method_call_tests {
    use super::*;

    #[test]
    fn ordering_methods_reject_unsupported_receivers_arguments_and_void_values() {
        for statement in [
            "map.sort;",
            "map.rsort();",
            "map.reverse;",
            "objects.sort();",
            "objects.rsort;",
            "q.sort(1);",
            "q.reverse(.item(1));",
            "q.rsort(item);",
            "value = q.sort;",
            "value = a.reverse();",
            "value = $bits(q.rsort());",
            "value = $dimensions(a.sort);",
        ] {
            let source = format!(
                "module Top; int q[$], a[], map[int], value;
                 class Cell; int n; endclass Cell objects[$];
                 initial begin {statement} end endmodule"
            );
            let result = crate::analyze_source(&source, Path::new("ordering-rejection.sv"));
            assert!(
                matches!(result, Err(AnalyzerError::Unsupported(_))),
                "{statement}: {result:?}"
            );
        }
    }

    #[test]
    fn ordering_with_clauses_are_not_silently_ignored() {
        for call in [
            "a.sort with (-item)",
            "a.rsort(x) with (x)",
            "a.reverse with (item)",
        ] {
            let source = format!("module Top; int a[]; initial {call}; endmodule");
            assert!(
                crate::analyze_source(&source, Path::new("ordering-with.sv")).is_err(),
                "{call}"
            );
        }
    }

    #[test]
    fn bare_methods_keep_argument_and_void_result_checks() {
        for statement in [
            "q.push_back;",
            "q.push_front;",
            "q.insert;",
            "map.exists;",
            "map.first;",
            "map.last;",
            "map.next;",
            "map.prev;",
            "value = q.delete;",
            "value = a.delete;",
            "value = map.delete;",
            "value = q.num;",
            "value = $bits(strings.pop_front);",
            "value = $dimensions(strings.pop_back());",
            "value = $bits(objects.pop_front());",
            "value = $dimensions(objects.pop_back);",
        ] {
            let source = format!(
                "module Top; int q[$], a[], map[int], value; string strings[$];
                 class Cell; int n; endclass Cell objects[$];
                 initial begin {statement} end endmodule"
            );
            let result = crate::analyze_source(&source, Path::new("bare-container-method.sv"));
            assert!(
                matches!(result, Err(AnalyzerError::Unsupported(_))),
                "{statement}: {result:?}"
            );
        }
    }
}

/// An element that `get` reads, as a whole value: the flat structure value
/// of a record element.
fn element_read(target: &ContainerType, get: Expr) -> Expr {
    match &target.record {
        Some(record) => classes::object_call(ObjectOp::Unbox(record.clone()), vec![get]),
        None => get,
    }
}

/// The container and the element read of native expression `id` when it
/// reads an element of unpacked structure type (`q[i]`, `q.pop_front()`),
/// before a record element is unboxed.
fn struct_element(
    id: ExprId,
    native: &celox_sv_syntax::Declarations,
    tree: &AnalysisContext<'_>,
    dims: &PackedDimensions,
) -> Converted<Option<(Arc<ContainerType>, Expr)>> {
    let id = expression_binding::ungroup(id, native);
    let base = match &native.expression(id).kind {
        ExprKind::Select { expression, .. } => *expression,
        ExprKind::Call { .. } | ExprKind::Member { .. } => {
            let callee = match &native.expression(id).kind {
                ExprKind::Call { expression, .. } => *expression,
                _ => id,
            };
            match &native.expression(callee).kind {
                ExprKind::Member {
                    expression, name, ..
                } if matches!(name.spelling.as_str(), "pop_front" | "pop_back") => *expression,
                _ => return Ok(None),
            }
        }
        _ => return Ok(None),
    };
    let Some(target) = native_type(base, native, tree, dims)? else {
        return Ok(None);
    };
    if target.element.unpacked_struct().is_none() {
        return Ok(None);
    }
    let Some(value) = value(id, native, tree, dims, false)? else {
        return Ok(None);
    };
    let raw = match value {
        Expr::Call {
            callee: Callee::Object(op),
            mut args,
            ..
        } if matches!(*op, ObjectOp::Unbox(_)) => args
            .pop()
            .flatten()
            .ok_or_else(|| unsupported("structure record read"))?,
        value => value,
    };
    Ok(Some((target, raw)))
}

/// The unpacked structure type of native expression `id` when it reads a
/// container element of such a type.
pub(super) fn native_struct_element(
    id: ExprId,
    native: &celox_sv_syntax::Declarations,
    tree: &AnalysisContext<'_>,
    dims: &PackedDimensions,
) -> Converted<Option<Arc<unpacked_structs::UnpackedStruct>>> {
    let id = expression_binding::ungroup(id, native);
    let base = match &native.expression(id).kind {
        ExprKind::Select { expression, .. } => *expression,
        ExprKind::Call { expression, .. } => match &native.expression(*expression).kind {
            ExprKind::Member {
                expression, name, ..
            } if matches!(name.spelling.as_str(), "pop_front" | "pop_back") => *expression,
            _ => return Ok(None),
        },
        ExprKind::Member {
            expression, name, ..
        } if matches!(name.spelling.as_str(), "pop_front" | "pop_back") => *expression,
        _ => return Ok(None),
    };
    Ok(native_type(base, native, tree, dims)?
        .and_then(|target| target.element.unpacked_struct().cloned()))
}

/// A member `e.m` (or `e.m.n`) of a container element `e` of unpacked
/// structure type: its type and its value.
pub(super) fn element_member(
    id: ExprId,
    native: &celox_sv_syntax::Declarations,
    tree: &AnalysisContext<'_>,
    dims: &PackedDimensions,
) -> Converted<Option<(Type, Expr)>> {
    let mut names = Vec::new();
    let mut current = expression_binding::ungroup(id, native);
    let element = loop {
        let ExprKind::Member {
            expression,
            separator,
            name,
        } = &native.expression(current).kind
        else {
            return Ok(None);
        };
        if separator != "." {
            return Ok(None);
        }
        names.push(instances::normalize_identifier(&name.spelling));
        current = expression_binding::ungroup(*expression, native);
        if native_struct_element(current, native, tree, dims)?.is_some() {
            break current;
        }
    };
    names.reverse();
    let Some((target, raw)) = struct_element(element, native, tree, dims)? else {
        return Ok(None);
    };
    let aggregate = target
        .element
        .unpacked_struct()
        .expect("a structure element")
        .clone();
    // A path into a packed structure member selects its bits; a path
    // through a class handle member continues as a property access.
    let mut packed = Vec::new();
    let (member_type, leaves) = loop {
        if let Some(found) = unpacked_structs::member_leaves(&aggregate, &names) {
            break found;
        }
        let Some(name) = names.pop() else {
            return Ok(None);
        };
        packed.insert(0, name);
    };
    if !packed.is_empty() && member_type.members.is_empty() {
        return Ok(None);
    }
    let leaves_in_order = aggregate.leaves();
    let parts = leaves
        .iter()
        .map(|leaf| {
            let index = leaves_in_order
                .iter()
                .position(|candidate| candidate.path == leaf.path)
                .expect("a leaf of the structure");
            match &target.record {
                Some(record) => {
                    classes::object_call(ObjectOp::Load(record.fields[index]), vec![raw.clone()])
                }
                None => Expr::Select {
                    expr: Box::new(raw.clone()),
                    msb: ConstExpr::Literal((leaf.offset + leaf.width - 1).to_string()),
                    lsb: ConstExpr::Literal(leaf.offset.to_string()),
                    signed: leaf.r#type.is_signed,
                },
            }
        })
        .collect::<Vec<_>>();
    // Each leaf is read from the element, which a pop or any other call
    // with an effect (a subroutine, `$urandom`) would change.
    if parts.len() > 1 && inlining::effectful(&raw) {
        return Err(unsupported(
            "structure member with several members of an element read with a side effect",
        ));
    }
    let value = if member_type.unpacked_struct().is_some() {
        Expr::Concat(parts)
    } else {
        parts.into_iter().next().expect("one leaf")
    };
    let mut member_type = member_type;
    let mut value = value;
    for name in packed {
        let Some(member) = member_type
            .members
            .iter()
            .find(|member| member.name() == name)
            .cloned()
        else {
            return Err(unsupported(format!("structure member `{name}`")));
        };
        if member.is_tagged() {
            return Err(unsupported("tagged union member of a container element"));
        }
        let width = casts::expr_type_from_type(&tree.state, member.r#type(), &dims.const_env)
            .ok_or_else(|| unsupported("structure member width"))?
            .width;
        value = Expr::Select {
            expr: Box::new(value),
            msb: ConstExpr::Literal((member.offset() + width - 1).to_string()),
            lsb: ConstExpr::Literal(member.offset().to_string()),
            signed: member.r#type().is_signed(),
        };
        member_type = member.r#type().clone();
    }
    Ok(Some((member_type, value)))
}
