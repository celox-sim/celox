//! Native expression jobs and shared semantic IR kernels.
use super::*;
use std::rc::Rc;

pub(in crate::ast) fn ungroup(mut id: ExprId, native: &Declarations) -> ExprId {
    while let ExprKind::Group(inner) = native.expression(id).kind {
        id = inner;
    }
    id
}

pub(in crate::ast) fn call_name(
    state: &AnalysisState,
    id: ExprId,
    native: &Declarations,
) -> Option<String> {
    match &native.expression(ungroup(id, native)).kind {
        ExprKind::Name(name) => Some(instances::normalize_identifier(&name.spelling)),
        ExprKind::Member {
            expression,
            separator,
            name,
        } if separator == "::" => Some(scope::qualified_name(
            state,
            &call_name(state, *expression, native)?,
            &instances::normalize_identifier(&name.spelling),
        )),
        _ => None,
    }
}

pub(in crate::ast) fn callable_name(
    state: &AnalysisState,
    id: ExprId,
    native: &Declarations,
) -> Option<String> {
    if let Some(name) = call_name(state, id, native) {
        return (name != "randomize").then_some(name);
    }
    let ExprKind::Member {
        expression,
        separator,
        name,
    } = &native.expression(id).kind
    else {
        return None;
    };
    if separator != "." {
        return None;
    }
    let handle = call_name(state, *expression, native)?;
    scope::imported(state)
        .interface_members
        .get(&format!(
            "{handle}.{}",
            instances::normalize_identifier(&name.spelling)
        ))
        .cloned()
}

/// Resolve interface selections with the constant environment of the call site.
pub(super) fn bound_callable_name(
    tree: &AnalysisContext<'_>,
    id: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
) -> Converted<Option<String>> {
    if let Some(name) = callable_name(&tree.state, id, native) {
        return Ok(Some(name));
    }
    if let Some((name, _, objects::Receiver::Static)) =
        objects::method(tree, id, native, dimensions)?
    {
        return Ok(Some(name));
    }
    let ExprKind::Member {
        expression,
        separator,
        name,
    } = &native.expression(ungroup(id, native)).kind
    else {
        return Ok(None);
    };
    if separator != "." {
        return Ok(None);
    }
    let handle = interface_binding::actual_handle_key(
        *expression,
        tree,
        &dimensions.const_env,
        &dimensions.type_aliases,
    )?;
    Ok(scope::imported(&tree.state)
        .interface_members
        .get(&format!(
            "{handle}.{}",
            instances::normalize_identifier(&name.spelling)
        ))
        .cloned())
}

pub(in crate::ast) fn arguments(
    name: &str,
    args: &[celox_sv_syntax::Argument],
    dimensions: &PackedDimensions,
) -> Converted<(Vec<Option<ExprId>>, Vec<usize>)> {
    let args = if args
        .first()
        .is_some_and(|a| a.name.is_none() && a.value.is_none())
        && args.len() > 1
        && args[1..].iter().all(|a| a.name.is_some())
    {
        &args[1..]
    } else {
        args
    };
    let (positional, named) = split_arguments(name, args)?;
    let signatures = expressions::call_signatures(name, dimensions);
    let (values, order) =
        expressions::bind_normalized_call_arguments(name, positional, named, &signatures)?;
    expressions::check_query_omissions(name, &values, dimensions)?;
    Ok((values, order))
}

/// The actuals `args` of a call of `name` pass to each of `formals`, by
/// position or by name, and the order they are evaluated in, see
/// [`expressions::bind_to_formals`].
pub(in crate::ast) fn formal_arguments(
    name: &str,
    args: &[celox_sv_syntax::Argument],
    formals: &[Option<String>],
) -> Converted<(Vec<Option<ExprId>>, Vec<usize>)> {
    // `f()` lists one empty actual, which a subroutine without formals
    // does not take.
    let args = match args {
        [arg] if formals.is_empty() && arg.name.is_none() && arg.value.is_none() => &[],
        args => args,
    };
    let (positional, named) = split_arguments(name, args)?;
    let formals = formals
        .iter()
        .map(|formal| formal.as_deref().unwrap_or_default())
        .collect::<Vec<_>>();
    expressions::bind_to_formals(name, positional, named, &formals)
}

/// The positional actuals, which come first, and the named ones with their
/// normalized names (IEEE 1800-2023 13.5.4).
fn split_arguments(
    name: &str,
    args: &[celox_sv_syntax::Argument],
) -> Converted<(Vec<Option<ExprId>>, Vec<(String, Option<ExprId>)>)> {
    let mut positional = Vec::new();
    let mut named = Vec::new();
    for arg in args {
        if let Some(key) = &arg.name {
            named.push((instances::normalize_identifier(&key.spelling), arg.value));
        } else if named.is_empty() {
            positional.push(arg.value);
        } else {
            return Err(AnalyzerError::InvalidCallArguments {
                subroutine: name.to_string(),
                detail: "a positional actual after a named one (IEEE 1800-2023 13.5.4)".into(),
            });
        }
    }
    Ok((positional, named))
}

pub(in crate::ast) fn cast_target_native(
    target: ExprId,
    operand: ExprId,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    dimensions: &PackedDimensions,
) -> Converted<casts::RuntimeCastTarget> {
    let target = ungroup(target, native);
    if let ExprKind::Type(id) = native.expression(target).kind
        && matches!(&native.data_type(id).kind, celox_sv_syntax::DataTypeKind::Builtin(name) if name == "string")
    {
        return Ok(casts::RuntimeCastTarget::String);
    }
    if let ExprKind::Type(id) = native.expression(target).kind
        && let Some(ty) = type_binding::binding::bind_in(id, native, tree, dimensions)?
        && matches!(ty.kind(), TypeKind::Real | TypeKind::Shortreal)
    {
        return Ok(casts::RuntimeCastTarget::Real {
            short: ty.kind() == TypeKind::Shortreal,
        });
    }
    if let ExprKind::Name(name) = &native.expression(target).kind
        && matches!(name.spelling.as_str(), "signed" | "unsigned")
    {
        return Ok(casts::RuntimeCastTarget::Signing(name.spelling == "signed"));
    }
    let named = call_name(&tree.state, target, native);
    let (ty, numeric, two_state) = if let Some(ty) = named
        .as_ref()
        .and_then(|name| dimensions.type_aliases.get(name))
    {
        (
            casts::expr_type_from_type(&tree.state, ty, &dimensions.const_env)
                .ok_or_else(|| unsupported("cast target type"))?,
            false,
            ty.kind() == TypeKind::Bit,
        )
    } else if let ExprKind::Type(id) = native.expression(target).kind {
        match &native.data_type(id).kind {
            celox_sv_syntax::DataTypeKind::Builtin(name)
                if matches!(
                    name.as_str(),
                    "byte" | "shortint" | "int" | "longint" | "integer" | "time"
                ) => {}
            celox_sv_syntax::DataTypeKind::Named(_) => {}
            _ => return Err(unsupported("cast target type")),
        }
        let ty = type_binding::binding::bind_in(id, native, tree, dimensions)?
            .ok_or_else(|| unsupported("cast target type"))?;
        (
            casts::expr_type_from_type(&tree.state, &ty, &dimensions.const_env)
                .ok_or_else(|| unsupported("cast target type"))?,
            false,
            ty.kind() == TypeKind::Bit,
        )
    } else {
        let value = super::bind_value(target, native, tree, dimensions, None)?;
        let value = expr_to_const(&tree.state, value)
            .and_then(|value| eval_ast_const_expr(&tree.state, &value, &dimensions.const_env))
            .and_then(|value| usize::try_from(value).ok())
            .filter(|width| *width > 0)
            .ok_or_else(|| unsupported("cast target type"))?;
        (
            ExprType {
                width: value,
                signed: false,
            },
            true,
            false,
        )
    };
    if let ExprKind::Literal(text) = &native.expression(ungroup(operand, native)).kind
        && let Some(literal) = typecheck::parse_integral_literal(text)
        && literal.value == 0u8.into()
        && literal.mask == 0u8.into()
    {
        return Ok(casts::RuntimeCastTarget::Zero(ExprType {
            signed: if numeric { literal.signed } else { ty.signed },
            ..ty
        }));
    }
    Ok(casts::RuntimeCastTarget::Integral {
        target: ty,
        numeric_size: numeric,
        two_state,
    })
}

enum Finish {
    ScopeRandomize(Vec<Option<Vec<i128>>>),
    Tagged(packed_structs::Construction),
    Unary(String),
    Binary(String),
    Mux,
    Bits,
    Concat,
    Call {
        name: String,
        args: Vec<Option<ExprId>>,
        order: Vec<usize>,
        system: Option<SystemFunction>,
    },
    Cast(casts::RuntimeCastTarget),
    Repeat,
    Inside(Vec<bool>),
    /// A string operation on the bound children, see [`strings`].
    StringCall(StringOp),
    /// An enumerated type method; the children are the receiver, then the
    /// count when one is given.
    EnumMethod {
        method: enums::Method,
        domain: std::sync::Arc<crate::ast::enums::EnumDomain>,
        count: bool,
    },
    /// `$sformatf` of the bound children, whose native expressions are `ids`.
    Format {
        ticks_per_unit: u64,
        ids: Vec<ExprId>,
    },
    /// A read of property `property` of the object handle child.
    ObjectLoad(Box<crate::ast::classes::Property>),
    /// A read of a member through the virtual interface child.
    VirtualMember(Box<(crate::ast::virtual_interfaces::VirtualInterface, String)>),
    MethodCall(Box<MethodCall>),
}

/// A call of method subroutine `name` with `formals` formals; the first
/// child is the object handle when `receiver` is one, then come the given
/// actuals in the order of the formals.
struct MethodCall {
    name: String,
    /// Whether each formal has an actual.
    given: Vec<bool>,
    /// The formals in the order their actuals are evaluated.
    order: Vec<usize>,
    formals: usize,
    receiver: objects::Receiver,
    selection: Option<Expr>,
}

enum Job {
    Enter(
        ExprId,
        Option<Rc<VariableDimensions>>,
        Option<Rc<PackedDimensions>>,
    ),
    Finish(Finish, usize, Option<Rc<PackedDimensions>>),
    /// A streaming concatenation operand of a bit-stream cast to an integral
    /// type (IEEE 1800-2023 11.4.14).
    Stream(
        ExprId,
        Option<streams::Receiver>,
        Option<Rc<PackedDimensions>>,
    ),
}

/// Whether the call copies actual `position` of `name` into its formal and
/// never back: an input, not a reference (IEEE 1800-2023 13.5.1).
pub(super) fn copied_in(dimensions: &PackedDimensions, name: &str, position: usize) -> bool {
    dimensions
        .subroutine_directions
        .get(name)
        .and_then(|directions| directions.get(position))
        .is_some_and(|direction| *direction == crate::procedural::ParamDirection::Input)
}

/// Check the actual of an output or inout container formal, which the call
/// copies back as a handle.
fn check_written_container(
    name: &str,
    position: usize,
    actual: ExprId,
    tree: &AnalysisContext<'_>,
    dimensions: &PackedDimensions,
) -> Converted<()> {
    use crate::procedural::ParamDirection;
    let direction = dimensions
        .subroutine_directions
        .get(name)
        .and_then(|directions| directions.get(position))
        .copied();
    if let Some(direction @ (ParamDirection::Output | ParamDirection::Inout)) = direction
        && let Some(container) = dimensions
            .subroutine_param_shapes
            .get(name)
            .and_then(|shapes| shapes.get(position))
            .and_then(|shape| shape.container.as_deref())
    {
        containers::check_written_actual(
            actual,
            tree,
            dimensions,
            container,
            direction == ParamDirection::Inout,
        )?;
    }
    Ok(())
}

pub(in crate::ast) fn bind_value(
    root: ExprId,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    dimensions: &PackedDimensions,
    target: Option<&VariableDimensions>,
) -> Converted<Expr> {
    let mut jobs = vec![Job::Enter(root, target.cloned().map(Rc::new), None)];
    let mut values = Vec::new();
    while let Some(job) = jobs.pop() {
        match job {
            Job::Enter(id, target, scope) => {
                let dimensions = scope.as_deref().unwrap_or(dimensions);
                if let Some(value) = dimensions.effect_values.get(&id) {
                    values.push(value.clone());
                    continue;
                }
                unpacked_structs::check_operands(id, native, tree, dimensions)?;
                // An argument, result or other value received as an unpacked
                // structure is a pattern or a value of its type (6.22.3).
                if let Some(aggregate) = target
                    .as_deref()
                    .filter(|shape| shape.unpacked.is_empty())
                    .and_then(|shape| shape.unpacked_struct.as_deref())
                {
                    unpacked_structs::check_value(aggregate, id, native, tree, dimensions)?;
                } else if target
                    .as_deref()
                    .is_some_and(|shape| shape.unpacked.is_empty() && shape.container.is_none())
                    && unpacked_structs::native_struct(id, native, tree, dimensions)?.is_some()
                {
                    return Err(unsupported(
                        "unpacked structure value received by a destination that is not of its type",
                    ));
                }
                if let Some(ty) = target.as_deref().and_then(|shape| {
                    shape
                        .virtual_interface
                        .as_deref()
                        .filter(|_| shape.unpacked.is_empty() && shape.container.is_none())
                }) {
                    virtual_interfaces::check_assignment(tree, id, native, dimensions, ty)?;
                }
                if let Some(ty) = target.as_deref().and_then(|shape| {
                    shape
                        .virtual_interface
                        .as_deref()
                        .filter(|_| !shape.unpacked.is_empty() && shape.container.is_none())
                }) {
                    virtual_interfaces::check_array_assignment(id, native, dimensions, ty)?;
                }
                if let Some(shape) = target.as_deref()
                    && shape
                        .members
                        .first()
                        .is_some_and(packed_structs::PackedMember::is_tagged)
                    && !matches!(
                        native.expression(id).kind,
                        ExprKind::Group(_) | ExprKind::Conditional { .. } | ExprKind::Tagged { .. }
                    )
                {
                    let actual = if let ExprKind::Cast { target, .. } = native.expression(id).kind {
                        cast_shape(target, native, tree, dimensions)?
                            .and_then(|shape| shape.pattern_type(&tree.state, dimensions))
                    } else {
                        bound_patterns::native_value_type(id, tree, dimensions)?
                    };
                    if !actual.is_some_and(|ty| {
                        ty.members
                            .first()
                            .is_some_and(|member| member.same_owner(&shape.members[0]))
                            && ty.packed_ranges.len() == 1
                            && ty.unpacked_ranges.is_empty()
                    }) {
                        return Err(unsupported(
                            "assignment to a tagged union requires the same union type or a tagged expression",
                        ));
                    }
                }
                // A container formal passed by value receives a copy of a
                // container value, as an assignment does (13.5.1, 7.6).
                if let Some(container) = target
                    .as_deref()
                    .filter(|shape| shape.unpacked.is_empty())
                    .and_then(|shape| shape.container.as_deref())
                {
                    if containers::dynamic_array_new(id, native).is_some() {
                        return Err(AnalyzerError::Parse(
                            "`new[]` is not an argument; it only initializes or is assigned \
                             to a dynamic array (IEEE 1800-2023 7.5.1)"
                                .into(),
                        ));
                    }
                    values.push(containers::assignment(id, tree, dimensions, container)?);
                    continue;
                }
                if let Some(value) = locators::index(id, native, tree, dimensions)? {
                    values.push(value);
                    continue;
                }
                if let Some(value) = locators::bind(id, native, tree, dimensions)? {
                    values.push(value);
                    continue;
                }
                let enum_method = match &native.expression(id).kind {
                    ExprKind::Call {
                        expression,
                        arguments,
                    } => enums::method(tree, *expression, native, dimensions)?
                        .map(|method| (method, arguments.as_slice())),
                    ExprKind::Member { .. } => {
                        enums::method(tree, id, native, dimensions)?.map(|method| (method, &[][..]))
                    }
                    _ => None,
                };
                if let Some(((method, receiver, domain), arguments)) = enum_method {
                    // `v.next()` lists one empty argument.
                    let arguments = match arguments {
                        [argument] if argument.name.is_none() && argument.value.is_none() => &[],
                        arguments => arguments,
                    };
                    let counts = arguments
                        .iter()
                        .map(|argument| argument.value.filter(|_| argument.name.is_none()))
                        .collect::<Option<Vec<_>>>()
                        .filter(|counts| counts.len() <= method.formals())
                        .ok_or_else(|| {
                            unsupported(format!(
                                "arguments of enumerated type method `{}`",
                                native.token_text(native.expression(id).tokens).trim()
                            ))
                        })?;
                    let mut children = vec![receiver];
                    children.extend(&counts);
                    jobs.push(Job::Finish(
                        Finish::EnumMethod {
                            method,
                            domain,
                            count: !counts.is_empty(),
                        },
                        children.len(),
                        scope.clone(),
                    ));
                    jobs.extend(
                        children
                            .into_iter()
                            .rev()
                            .map(|id| Job::Enter(id, None, scope.clone())),
                    );
                    continue;
                }
                if let Some(call) =
                    super::super::processes::value(tree, id, native, dimensions, false)?
                {
                    values.push(call);
                    continue;
                }
                if let Some(call) =
                    super::super::semaphores::value(tree, id, native, dimensions, false)?
                {
                    values.push(call);
                    continue;
                }
                // `e.triggered` (IEEE 1800-2023 15.5.3).
                if let Some(event) = super::super::events::triggered(tree, id, native, dimensions)?
                {
                    let event = super::value(event, native, tree, dimensions)?;
                    values.push(super::super::events::call(
                        celox_design::ProcessOp::Triggered,
                        vec![event],
                    ));
                    continue;
                }
                if let Some((value, _)) = array_reductions::bind(id, native, tree, dimensions)? {
                    values.push(value);
                    continue;
                }
                if let Some(value) = containers::value(id, native, tree, dimensions, false)? {
                    values.push(value);
                    continue;
                }
                if let Some(call) =
                    objects::constraint_mode_call(tree, id, native, dimensions, false)?
                {
                    values.push(call);
                    continue;
                }
                if let Some(call) = objects::rand_mode_call(tree, id, native, dimensions, false)? {
                    values.push(call);
                    continue;
                }
                if timescales::is_query(id, native) {
                    values.push(Expr::Literal(parameters::format_typed_parameter_literal(
                        i128::from(timescales::query(id, tree)?),
                        32,
                        true,
                    )));
                    continue;
                }
                let time_call = match &native.expression(id).kind {
                    ExprKind::Name(name)
                        if matches!(name.spelling.as_str(), "$time" | "$stime" | "$realtime") =>
                    {
                        Some((name.spelling.clone(), &[][..]))
                    }
                    ExprKind::Call {
                        expression,
                        arguments,
                    } => call_name(&tree.state, *expression, native)
                        .filter(|name| matches!(name.as_str(), "$time" | "$stime" | "$realtime"))
                        .map(|name| (name, arguments.as_slice())),
                    _ => None,
                };
                if let Some((name, arguments)) = time_call {
                    let presence = arguments
                        .iter()
                        .map(|argument| {
                            if argument.value.is_some() {
                                system_functions::Arg::Given
                            } else {
                                system_functions::Arg::Omitted
                            }
                        })
                        .collect::<Vec<_>>();
                    let function = system_functions::check_function_call(
                        &name,
                        (!arguments.iter().any(|argument| argument.name.is_some()))
                            .then_some(presence.as_slice()),
                    )?;
                    values.push(Expr::Call {
                        callee: Callee::System(function),
                        args: vec![Some(Expr::Literal(format_typed_parameter_literal(
                            i128::from(timescales::time_divisor(id, tree)?),
                            64,
                            false,
                        )))],
                        arg_order: Vec::new(),
                    });
                    continue;
                }
                let (finish, children) = match &native.expression(id).kind {
                    ExprKind::Group(inner) => {
                        jobs.push(Job::Enter(*inner, target, scope.clone()));
                        continue;
                    }
                    ExprKind::Literal(text) if text == "null" => {
                        // A handle of no object (IEEE 1800-2023 8.4).
                        values.push(Expr::Literal("32'h0".into()));
                        continue;
                    }
                    ExprKind::Literal(text) => {
                        if let Some(packed) = packed_string_literal(text) {
                            values.push(Expr::Literal(packed));
                        } else {
                            if typecheck::integral_literal_type_metadata(text).is_none()
                                && !(text.contains(['.', 'e', 'E'])
                                    && text.replace('_', "").parse::<f64>().is_ok())
                            {
                                return Err(unsupported("primary literal"));
                            }
                            values.push(Expr::Literal(text.clone()));
                        }
                        continue;
                    }
                    ExprKind::Name(name) if !name.spelling.starts_with('$') => {
                        let name = instances::normalize_identifier(&name.spelling);
                        if name == "null" {
                            values.push(Expr::Literal("32'h0".into()));
                            continue;
                        }
                        if matches!(name.as_str(), "this" | "super")
                            && dimensions.get(&name).is_none()
                        {
                            return Err(unsupported(format!("`{name}`")));
                        }
                        if let Some(value) = unpacked_structs::whole(&name, dimensions) {
                            values.push(value);
                            continue;
                        }
                        if let Some((aggregate, class, _)) =
                            objects::aggregate_property(tree, id, native, dimensions)?
                        {
                            values.push(objects::aggregate_load(
                                &aggregate,
                                &class,
                                &objects::this(),
                            )?);
                            continue;
                        }
                        if let Some(value) = objects::unqualified_property(tree, dimensions, &name)?
                        {
                            values.push(value);
                            continue;
                        }
                        if let Some(value) = virtual_interfaces::identity(tree, &name, dimensions)?
                        {
                            values.push(value);
                            continue;
                        }
                        values.push(expressions::identifier_expression(
                            &tree.state,
                            name,
                            dimensions,
                        )?);
                        continue;
                    }
                    ExprKind::Unary {
                        operator,
                        expression,
                        postfix: false,
                    } if !matches!(operator.as_str(), "++" | "--") => {
                        (Finish::Unary(operator.clone()), vec![(*expression, None)])
                    }
                    ExprKind::Binary {
                        left,
                        operator,
                        right,
                    } if operator != "inside" => {
                        if matches!(operator.as_str(), "==" | "!=" | "===" | "!==") {
                            virtual_interfaces::check_equality(
                                tree, *left, *right, native, dimensions,
                            )?;
                        }
                        (
                            Finish::Binary(operator.clone()),
                            vec![(*left, None), (*right, None)],
                        )
                    }
                    ExprKind::Conditional {
                        condition,
                        then_expression,
                        else_expression,
                    } => (
                        Finish::Mux,
                        vec![
                            (*condition, None),
                            (*then_expression, target.clone()),
                            (*else_expression, target.clone()),
                        ],
                    ),
                    ExprKind::Concatenation(parts) if !parts.is_empty() => {
                        (Finish::Concat, parts.iter().map(|id| (*id, None)).collect())
                    }
                    ExprKind::Replication { count, expressions } if !expressions.is_empty() => {
                        let mut children = vec![(*count, None)];
                        children.extend(expressions.iter().map(|id| (*id, None)));
                        (Finish::Repeat, children)
                    }
                    ExprKind::Cast { target, expression }
                        if streams::is_stream(*expression, native) =>
                    {
                        // A cast to a type that is not integral receives the
                        // stream as an assignment does.
                        let ty = match call_name(&tree.state, *target, native) {
                            Some(name) => dimensions.type_aliases.get(&name).cloned(),
                            None => match native.expression(ungroup(*target, native)).kind {
                                ExprKind::Type(ty) => {
                                    type_binding::binding::bind_in(ty, native, tree, dimensions)?
                                }
                                _ => None,
                            },
                        };
                        if let Some(shape) = ty
                            .as_ref()
                            .map(procedural::dimensions_from_type)
                            .filter(|shape| {
                                shape.container.is_some()
                                    || shape.string
                                    || !shape.unpacked.is_empty()
                            })
                        {
                            let receiver = streams::receiver(&shape, &tree.state, dimensions)?;
                            values.push(streams::pack(
                                *expression,
                                Some(receiver),
                                native,
                                tree,
                                dimensions,
                            )?);
                            continue;
                        }
                        let cast =
                            cast_target_native(*target, *expression, native, tree, dimensions)?;
                        let receiver = match &cast {
                            casts::RuntimeCastTarget::String => {
                                values.push(streams::pack(
                                    *expression,
                                    Some(streams::Receiver::String),
                                    native,
                                    tree,
                                    dimensions,
                                )?);
                                continue;
                            }
                            casts::RuntimeCastTarget::Integral { target, .. }
                            | casts::RuntimeCastTarget::Zero(target) => {
                                Some(streams::Receiver::Cast(target.width))
                            }
                            _ => None,
                        };
                        jobs.push(Job::Finish(Finish::Cast(cast), 1, scope.clone()));
                        jobs.push(Job::Stream(*expression, receiver, scope.clone()));
                        continue;
                    }
                    ExprKind::Cast { target, expression } => (
                        Finish::Cast(cast_target_native(
                            *target,
                            *expression,
                            native,
                            tree,
                            dimensions,
                        )?),
                        vec![(
                            *expression,
                            cast_shape(*target, native, tree, dimensions)?.map(Rc::new),
                        )],
                    ),
                    ExprKind::Call {
                        expression,
                        arguments: args,
                    } if objects::method(tree, *expression, native, dimensions)?.is_some() => {
                        let (name, formals, receiver) =
                            objects::method(tree, *expression, native, dimensions)?
                                .expect("checked by the guard");
                        let selection = name
                            .ends_with("::randomize")
                            .then(|| {
                                objects::random_selection(tree, dimensions, &name, args, native)
                            })
                            .transpose()?;
                        if name.ends_with("::srandom")
                            && (args.len() != 1 || args[0].name.is_some())
                        {
                            return Err(unsupported(format!("arguments of method `{name}`")));
                        }
                        let (actuals, order) = match selection {
                            Some(_) => (Vec::new(), Vec::new()),
                            None => formal_arguments(
                                name.strip_suffix("$virtual").unwrap_or(&name),
                                args,
                                &formals,
                            )?,
                        };
                        let mut children = Vec::new();
                        if let objects::Receiver::Handle(handle) = receiver {
                            children.push((handle, None));
                        }
                        // The shapes of the formals, after `this` of a method
                        // that is not static, receive assignment patterns.
                        let first = usize::from(receiver != objects::Receiver::Static);
                        let shapes = dimensions.subroutine_param_shapes.get(&name);
                        for (position, value) in actuals.iter().enumerate() {
                            let Some(value) = *value else { continue };
                            references::check_actual(
                                &name,
                                first + position,
                                value,
                                native,
                                tree,
                                dimensions,
                            )?;
                            check_written_container(
                                &name,
                                first + position,
                                value,
                                tree,
                                dimensions,
                            )?;
                            let shape = shapes.and_then(|shapes| shapes.get(first + position));
                            let copied_in = copied_in(dimensions, &name, first + position);
                            // The formal is copied out to the actual.
                            if let Some(formal) = shape
                                .filter(|shape| !copied_in && shape.unpacked.is_empty())
                                .and_then(|shape| shape.virtual_interface.as_deref())
                            {
                                virtual_interfaces::check_copy_out(
                                    tree, value, native, dimensions, formal,
                                )?;
                            }
                            let shape = shape
                                .filter(|shape| {
                                    shape.unpacked_struct.is_some()
                                        || (shape.container.is_some()
                                            || shape.virtual_interface.is_some())
                                            && copied_in
                                })
                                .cloned()
                                .map(Rc::new);
                            children.push((value, shape));
                        }
                        (
                            Finish::MethodCall(Box::new(MethodCall {
                                name,
                                given: actuals.iter().map(Option::is_some).collect(),
                                order,
                                formals: formals.len(),
                                receiver,
                                selection,
                            })),
                            children,
                        )
                    }
                    ExprKind::Call {
                        expression,
                        arguments: args,
                    } if strings::method(tree, *expression, native, dimensions).is_some() => {
                        let (op, base) = strings::method(tree, *expression, native, dimensions)
                            .expect("checked by the guard");
                        if op.updates_receiver() {
                            return Err(unsupported(format!(
                                "string method `{op}` used as a value"
                            )));
                        }
                        let (_, count) = StringOp::method(&op.to_string()).expect("a method name");
                        let values = args
                            .iter()
                            .map(|arg| arg.value.filter(|_| arg.name.is_none()))
                            .collect::<Option<Vec<_>>>()
                            .filter(|values| values.len() == count)
                            .ok_or_else(|| {
                                unsupported(format!("arguments of string method `{op}`"))
                            })?;
                        (
                            Finish::StringCall(op),
                            std::iter::once(base)
                                .chain(values)
                                .map(|id| (id, None))
                                .collect(),
                        )
                    }
                    ExprKind::Call {
                        expression,
                        arguments: args,
                    } => {
                        if matches!(
                            call_name(&tree.state, *expression, native).as_deref(),
                            Some("randomize" | "std::randomize")
                        ) {
                            if args
                                .iter()
                                .any(|arg| arg.name.is_some() || arg.value.is_none())
                            {
                                return Err(unsupported(
                                    "scope randomize requires positional variable identifiers",
                                ));
                            }
                            let actuals =
                                args.iter().filter_map(|arg| arg.value).collect::<Vec<_>>();
                            let mut domains = Vec::with_capacity(actuals.len());
                            for &actual in &actuals {
                                let ExprKind::Name(name) = &native.expression(actual).kind else {
                                    return Err(unsupported(
                                        "scope randomize requires whole packed variable identifiers",
                                    ));
                                };
                                let name = instances::normalize_identifier(&name.spelling);
                                let shape = dimensions
                                    .get(&name)
                                    .ok_or_else(|| unsupported("scope randomize variable"))?;
                                if !shape.unpacked.is_empty()
                                    || !shape.randomizable
                                    || shape.real
                                    || shape.string
                                    || shape.class.is_some()
                                {
                                    return Err(unsupported(
                                        "scope randomize requires a packed variable with a supported enum domain",
                                    ));
                                }
                                domains
                                    .push(shape.enum_domain.as_ref().map(|domain| domain.values()));
                                if !expressions::query_actual_is_writable(
                                    &Expr::Ident(name),
                                    dimensions,
                                ) {
                                    return Err(unsupported(
                                        "scope randomize argument is not writable",
                                    ));
                                }
                            }
                            jobs.push(Job::Finish(
                                Finish::ScopeRandomize(domains),
                                actuals.len(),
                                scope.clone(),
                            ));
                            jobs.extend(
                                actuals
                                    .into_iter()
                                    .rev()
                                    .map(|id| Job::Enter(id, None, scope.clone())),
                            );
                            continue;
                        }
                        let name = bound_callable_name(tree, *expression, native, dimensions)?
                            .ok_or_else(|| unsupported("method or randomize call"))?;
                        let cast = match name.as_str() {
                            "$cast" => objects::cast(tree, args, native, dimensions)?,
                            _ => None,
                        };
                        if let Some(cast) = cast {
                            let objects::Cast::Call {
                                name,
                                destination,
                                source,
                            } = cast
                            else {
                                values.push(Expr::Literal("0".into()));
                                continue;
                            };
                            (
                                Finish::Call {
                                    name,
                                    args: vec![Some(destination), Some(source)],
                                    order: Vec::new(),
                                    system: None,
                                },
                                vec![(destination, None), (source, None)],
                            )
                        } else if name == "$sformatf" {
                            let actuals = format_arguments(&name, args)?;
                            (
                                Finish::Format {
                                    ticks_per_unit: timescales::time_divisor(id, tree)?,
                                    ids: actuals.clone(),
                                },
                                actuals.into_iter().map(|id| (id, None)).collect(),
                            )
                        } else if name.starts_with('$') && !name.contains("::") {
                            let presence = args
                                .iter()
                                .map(|arg| {
                                    if arg.value.is_some() {
                                        system_functions::Arg::Given
                                    } else {
                                        system_functions::Arg::Omitted
                                    }
                                })
                                .collect::<Vec<_>>();
                            let function = system_functions::check_function_call(
                                &name,
                                (!args.iter().any(|arg| arg.name.is_some()))
                                    .then_some(presence.as_slice()),
                            )?;
                            if function == SystemFunction::Typename {
                                let argument = args[0]
                                    .value
                                    .ok_or_else(|| unsupported(format!("operand of `{name}`")))?;
                                values.push(type_names::query_value(
                                    argument,
                                    tree,
                                    scope.as_deref().unwrap_or(dimensions),
                                )?);
                                continue;
                            }
                            if function == SystemFunction::Bits
                                && let Some(argument) = args.first().and_then(|a| a.value)
                            {
                                let argument = ungroup(argument, native);
                                let ordinary = matches!(
                                    native.expression(argument).kind,
                                    ExprKind::Unary { .. }
                                        | ExprKind::Binary { .. }
                                        | ExprKind::Conditional { .. }
                                        | ExprKind::Concatenation(_)
                                        | ExprKind::Replication { .. }
                                        | ExprKind::Cast { .. }
                                        | ExprKind::Call { .. }
                                );
                                if ordinary {
                                    let context = match &scope {
                                        Some(scope) => scope.clone(),
                                        None => Rc::new(dimensions::query_context_for_operand(
                                            argument, tree, dimensions,
                                        )?),
                                    };
                                    if scope.is_none() {
                                        dimensions::validate_data_operand(
                                            argument, function, tree, &context,
                                        )?;
                                    }
                                    jobs.push(Job::Finish(Finish::Bits, 1, Some(context.clone())));
                                    jobs.push(Job::Enter(argument, None, Some(context)));
                                    continue;
                                }
                            }
                            if matches!(
                                function,
                                SystemFunction::Bits
                                    | SystemFunction::Size
                                    | SystemFunction::Dimensions
                                    | SystemFunction::UnpackedDimensions
                                    | SystemFunction::ArrayQuery(_)
                            ) {
                                values.push(
                                    dimensions::query_value(id, tree, dimensions)?.ok_or_else(
                                        || unsupported(format!("operand of `{name}`")),
                                    )?,
                                );
                                continue;
                            }
                            if function == SystemFunction::Isunbounded {
                                let arg = args
                                    .first()
                                    .and_then(|arg| arg.value)
                                    .and_then(|id| call_name(&tree.state, id, native));
                                let Some(arg) = arg.filter(|name| {
                                    dimensions
                                        .const_env
                                        .contains_key(&parameters::unbounded_parameter_marker(name))
                                }) else {
                                    return Err(AnalyzerError::InvalidSystemTfCall {
                                        name,
                                        detail: "argument must name a value parameter".into(),
                                    });
                                };
                                values.push(Expr::Literal(format!(
                                    "1'b{}",
                                    usize::from(
                                        dimensions
                                            .const_env
                                            .get(&parameters::unbounded_parameter_marker(&arg))
                                            == Some(&1)
                                    )
                                )));
                                continue;
                            }
                            if args.iter().filter_map(|arg| arg.value).any(|id| {
                                matches!(
                                    native.expression(ungroup(id, native)).kind,
                                    ExprKind::Type(_)
                                )
                            }) {
                                return Err(unsupported(format!("operand of `{name}`")));
                            }
                            if function.is_effectful() && args.iter().filter_map(|arg| arg.value).any(|id| {
                                matches!(&native.expression(ungroup(id, native)).kind, ExprKind::Literal(text)
                                    if typecheck::integral_literal_type_metadata(text).is_none() && packed_string_literal(text).is_none())
                            }) {
                                return Err(unsupported(format!("integral argument of `{name}`")));
                            }
                            let actuals = args.iter().map(|arg| arg.value).collect::<Vec<_>>();
                            for (position, id) in actuals.iter().enumerate() {
                                if function.writes_argument(position)
                                    && let Some(id) = id
                                {
                                    check_tagged_copyout(*id, None, tree, dimensions)?;
                                }
                            }
                            (
                                Finish::Call {
                                    name,
                                    args: actuals.clone(),
                                    order: Vec::new(),
                                    system: Some(function),
                                },
                                actuals.into_iter().flatten().map(|id| (id, None)).collect(),
                            )
                        } else {
                            let (actuals, order) = arguments(&name, args, dimensions)?;
                            for (position, id) in actuals.iter().enumerate() {
                                if let Some(id) = id {
                                    references::check_actual(
                                        &name, position, *id, native, tree, dimensions,
                                    )?;
                                    check_written_container(
                                        &name, position, *id, tree, dimensions,
                                    )?;
                                }
                                if let Some(id) = id
                                    && let Some(shape) = dimensions
                                        .subroutine_param_shapes
                                        .get(&name)
                                        .and_then(|shapes| shapes.get(position))
                                {
                                    if dimensions
                                        .subroutine_directions
                                        .get(&name)
                                        .and_then(|directions| directions.get(position))
                                        .is_some_and(|direction| direction.is_written())
                                    {
                                        check_tagged_copyout(*id, Some(shape), tree, dimensions)?;
                                        // The formal is copied out to the actual.
                                        if let Some(formal) = shape
                                            .virtual_interface
                                            .as_deref()
                                            .filter(|_| shape.unpacked.is_empty())
                                        {
                                            virtual_interfaces::check_copy_out(
                                                tree, *id, native, dimensions, formal,
                                            )?;
                                        }
                                    }
                                    array_compatibility::check_native_unpacked_assignment(
                                        &tree.state,
                                        *id,
                                        native,
                                        shape,
                                        || format!("argument {} of `{name}`", position + 1),
                                        dimensions,
                                    )?;
                                }
                            }
                            let children = actuals
                                .iter()
                                .enumerate()
                                .filter_map(|(position, id)| {
                                    id.map(|id| {
                                        let output = dimensions
                                            .subroutine_directions
                                            .get(&name)
                                            .and_then(|directions| directions.get(position))
                                            .is_some_and(|direction| {
                                                matches!(direction, ParamDirection::Output)
                                            });
                                        (
                                            id,
                                            dimensions
                                                .subroutine_param_shapes
                                                .get(&name)
                                                .and_then(|shapes| shapes.get(position))
                                                .filter(|shape| {
                                                    shape.container.is_none()
                                                        || copied_in(dimensions, &name, position)
                                                })
                                                .cloned()
                                                // An output actual is not a value
                                                // assigned to the formal.
                                                .map(|mut shape| {
                                                    if output {
                                                        shape.virtual_interface = None;
                                                    }
                                                    shape
                                                })
                                                .map(Rc::new),
                                        )
                                    })
                                })
                                .collect();
                            (
                                Finish::Call {
                                    name,
                                    args: actuals,
                                    order,
                                    system: None,
                                },
                                children,
                            )
                        }
                    }
                    ExprKind::Name(name) if name.spelling.starts_with('$') => {
                        let function =
                            system_functions::check_function_call(&name.spelling, Some(&[]))?;
                        (
                            Finish::Call {
                                name: name.spelling.clone(),
                                args: Vec::new(),
                                order: Vec::new(),
                                system: Some(function),
                            },
                            Vec::new(),
                        )
                    }
                    ExprKind::Select {
                        expression,
                        dimension,
                    } if strings::native_string(tree, *expression, native, dimensions) => {
                        let celox_sv_syntax::DimensionKind::Index(index) = dimension.kind else {
                            return Err(unsupported("part-select of a string"));
                        };
                        (
                            Finish::StringCall(StringOp::Getc),
                            vec![(*expression, None), (index, None)],
                        )
                    }
                    ExprKind::Member { .. }
                        if objects::class_parameter(tree, id, native, dimensions)?.is_some() =>
                    {
                        values.push(
                            objects::class_parameter(tree, id, native, dimensions)?
                                .expect("checked by the guard"),
                        );
                        continue;
                    }
                    ExprKind::Member { .. }
                        if virtual_interfaces::native_member(tree, id, native, dimensions)
                            .is_some() =>
                    {
                        let (handle, ty, member) =
                            virtual_interfaces::native_member(tree, id, native, dimensions)
                                .expect("checked by the guard");
                        (
                            Finish::VirtualMember(Box::new((ty.as_ref().clone(), member))),
                            vec![(handle, None)],
                        )
                    }
                    ExprKind::Member { .. }
                        if containers::element_member(id, native, tree, dimensions)?.is_some() =>
                    {
                        let (_, value) = containers::element_member(id, native, tree, dimensions)?
                            .expect("checked by the guard");
                        values.push(value);
                        continue;
                    }
                    ExprKind::Member { .. }
                        if objects::aggregate_property(tree, id, native, dimensions)?.is_some() =>
                    {
                        let (aggregate, class, receiver) =
                            objects::aggregate_property(tree, id, native, dimensions)?
                                .expect("checked by the guard");
                        let handle = objects::receiver(tree, receiver, native, dimensions)?;
                        values.push(objects::aggregate_load(&aggregate, &class, &handle)?);
                        continue;
                    }
                    ExprKind::Member { .. }
                        if objects::property(tree, id, native, dimensions)?.is_some() =>
                    {
                        let (property, receiver) = objects::property(tree, id, native, dimensions)?
                            .expect("checked by the guard");
                        match receiver {
                            Some(receiver) => (
                                Finish::ObjectLoad(Box::new(property)),
                                vec![(receiver, None)],
                            ),
                            None => {
                                values.push(objects::load(&property, objects::this()));
                                continue;
                            }
                        }
                    }
                    ExprKind::Member { .. } | ExprKind::Select { .. }
                        if virtual_interfaces::native_identity(tree, id, dimensions).is_some() =>
                    {
                        values.push(
                            virtual_interfaces::native_identity(tree, id, dimensions)
                                .expect("checked by the guard"),
                        );
                        continue;
                    }
                    ExprKind::Select { .. }
                        if let Some((handle, ty, member)) =
                            virtual_interfaces::native_selected_member(
                                tree, id, native, dimensions,
                            ) =>
                    {
                        let selected = selected_value(id, native, tree, dimensions)?;
                        let handle = bind_value(handle, native, tree, dimensions, None)?;
                        values.push(virtual_interfaces::selected_read(
                            &tree.state,
                            handle,
                            &ty,
                            &member,
                            selected,
                            dimensions,
                        )?);
                        continue;
                    }
                    ExprKind::Member { .. } | ExprKind::Select { .. } => {
                        values.push(selected_value(id, native, tree, dimensions)?);
                        continue;
                    }
                    ExprKind::Pattern { .. } => {
                        values.push(patterns::bind_native(
                            id,
                            native,
                            tree,
                            dimensions,
                            target.as_deref(),
                        )?);
                        continue;
                    }
                    ExprKind::Binary {
                        left,
                        operator,
                        right,
                    } if operator == "inside" => {
                        let ExprKind::Set(items) = &native.expression(*right).kind else {
                            return Err(unsupported("inside expression"));
                        };
                        let mut children = vec![(*left, None)];
                        let mut ranges = Vec::new();
                        for item in items {
                            match item.kind {
                                DimensionKind::Index(id) => {
                                    children.push((id, None));
                                    ranges.push(false);
                                }
                                DimensionKind::Range { left, right } => {
                                    children.extend([(left, None), (right, None)]);
                                    ranges.push(true);
                                }
                                _ => return Err(unsupported("inside range")),
                            }
                        }
                        (Finish::Inside(ranges), children)
                    }
                    ExprKind::Unary { .. } => {
                        return Err(unsupported("increment or decrement expression"));
                    }
                    ExprKind::Assignment { .. } => {
                        return Err(unsupported("assignment used as an expression"));
                    }
                    ExprKind::Tagged { member, value } => {
                        let shape = target.as_deref().ok_or_else(|| {
                            unsupported("tagged union expression without a type context")
                        })?;
                        let plan = packed_structs::construction_member(
                            shape,
                            &instances::normalize_identifier(&member.spelling),
                        )?;
                        if value.is_some() != plan.shape.is_some() {
                            return Err(unsupported(
                                "tagged member payload does not match void/non-void member",
                            ));
                        }
                        let children = value
                            .iter()
                            .map(|id| (*id, plan.shape.clone().map(Rc::new)))
                            .collect();
                        (Finish::Tagged(plan), children)
                    }
                    // A stream is the source of an assignment to a target of
                    // the shape the context gives (IEEE 1800-2023 11.4.14).
                    ExprKind::Streaming { .. } => {
                        let Some(shape) = target.as_deref() else {
                            return Err(AnalyzerError::InvalidStream(
                                "a streaming concatenation operand shall first be cast to a bit-stream type".into(),
                            ));
                        };
                        let receiver = streams::receiver(shape, &tree.state, dimensions)?;
                        values.push(streams::pack(id, Some(receiver), native, tree, dimensions)?);
                        continue;
                    }
                    ExprKind::Concatenation(_) => {
                        return Err(unsupported("empty unpacked array concatenation"));
                    }
                    ExprKind::RandomizeWith {
                        expression,
                        constraints,
                    } => {
                        values.push(objects::randomize_with(
                            tree,
                            *expression,
                            constraints.as_deref(),
                            native,
                            dimensions,
                        )?);
                        continue;
                    }
                    ExprKind::ArrayMethodWith { .. } => {
                        return Err(unsupported("array method with expression"));
                    }
                    ExprKind::MinTypMax { .. } => return Err(unsupported("mintypmax expression")),
                    _ => return Err(unsupported("primary expression")),
                };
                jobs.push(Job::Finish(finish, children.len(), scope.clone()));
                jobs.extend(
                    children
                        .into_iter()
                        .rev()
                        .map(|(id, shape)| Job::Enter(id, shape, scope.clone())),
                );
            }
            Job::Stream(id, receiver, scope) => {
                let dimensions = scope.as_deref().unwrap_or(dimensions);
                values.push(streams::pack(id, receiver, native, tree, dimensions)?);
            }
            Job::Finish(finish, count, scope) => {
                let dimensions = scope.as_deref().unwrap_or(dimensions);
                let start = values.len().checked_sub(count).ok_or_else(disagreement)?;
                let mut operands = values.split_off(start);
                let value = match finish {
                    Finish::Tagged(plan) => {
                        packed_structs::construct(&tree.state, plan, operands.pop(), dimensions)?
                    }
                    Finish::Unary(op) => {
                        let operand = operands.pop().ok_or_else(disagreement)?;
                        if strings::string_value(&operand, dimensions) {
                            return Err(unsupported(format!(
                                "operator `{op}` on a string operand"
                            )));
                        }
                        if real_value(&operand, dimensions)
                            && !matches!(op.as_str(), "+" | "-" | "!")
                        {
                            return Err(unsupported("operator on a real operand"));
                        }
                        constants::unary_expr_from_text(&op, operand)
                            .ok_or_else(|| unsupported(format!("unary operator `{op}`")))?
                    }
                    Finish::Binary(op) => {
                        let right = operands.pop().ok_or_else(disagreement)?;
                        let left = operands.pop().ok_or_else(disagreement)?;
                        if strings::string_value(&left, dimensions)
                            || strings::string_value(&right, dimensions)
                        {
                            values.push(strings::binary(&op, left, right, dimensions)?);
                            continue;
                        }
                        if (real_value(&left, dimensions) || real_value(&right, dimensions))
                            && !matches!(
                                op.as_str(),
                                "+" | "-"
                                    | "*"
                                    | "/"
                                    | "**"
                                    | "=="
                                    | "!="
                                    | "<"
                                    | "<="
                                    | ">"
                                    | ">="
                                    | "&&"
                                    | "||"
                                    | "->"
                                    | "<->"
                            )
                        {
                            return Err(unsupported("operator on a real operand"));
                        }
                        let implication = op == "->";
                        let equivalence = op == "<->";
                        let left = if implication || equivalence {
                            Expr::Unary {
                                op: UnaryOp::LogicNot,
                                expr: Box::new(left),
                            }
                        } else {
                            left
                        };
                        // Each truth-value conversion evaluates its operand once (11.4.7).
                        let right = if equivalence {
                            Expr::Unary {
                                op: UnaryOp::LogicNot,
                                expr: Box::new(right),
                            }
                        } else {
                            right
                        };
                        let xnor = matches!(op.as_str(), "~^" | "^~");
                        let value = Expr::Binary {
                            left: Box::new(left),
                            op: if equivalence {
                                BinaryOp::Eq
                            } else if implication {
                                BinaryOp::LogicOr
                            } else if xnor {
                                BinaryOp::BitXor
                            } else {
                                constants::binary_op_from_text(&op)
                                    .ok_or_else(|| unsupported(format!("binary operator `{op}`")))?
                            },
                            right: Box::new(right),
                        };
                        if xnor {
                            Expr::Unary {
                                op: UnaryOp::BitNot,
                                expr: Box::new(value),
                            }
                        } else {
                            value
                        }
                    }
                    Finish::Bits => {
                        let operand = operands.pop().ok_or_else(disagreement)?;
                        let ty = dimensions::bound_bits_type(&tree.state, &operand, dimensions)
                            .ok_or_else(|| unsupported("operand of `$bits`"))?;
                        Expr::Literal(ty.width.to_string())
                    }
                    Finish::Mux => {
                        let mut else_expr = operands.pop().ok_or_else(disagreement)?;
                        let mut then_expr = operands.pop().ok_or_else(disagreement)?;
                        let condition = operands.pop().ok_or_else(disagreement)?;
                        if strings::string_value(&then_expr, dimensions)
                            || strings::string_value(&else_expr, dimensions)
                        {
                            then_expr = strings::to_string(then_expr, dimensions);
                            else_expr = strings::to_string(else_expr, dimensions);
                        }
                        Expr::Mux {
                            condition: Box::new(condition),
                            then_expr: Box::new(then_expr),
                            else_expr: Box::new(else_expr),
                        }
                    }
                    Finish::Concat
                        if operands
                            .iter()
                            .any(|operand| strings::string_value(operand, dimensions)) =>
                    {
                        strings::call(StringOp::Concat, operands)
                    }
                    Finish::Concat => Expr::Concat(operands),
                    Finish::EnumMethod {
                        method,
                        domain,
                        count,
                    } => {
                        let count = count.then(|| operands.pop()).flatten();
                        enums::call(
                            &tree.state,
                            method,
                            &domain,
                            operands.pop(),
                            count,
                            dimensions,
                        )?
                    }
                    Finish::StringCall(op) => {
                        strings::call(op, strings::operands(op, operands, dimensions))
                    }
                    Finish::Format {
                        ticks_per_unit,
                        ids,
                    } => format_patterns::format(
                        ticks_per_unit,
                        &ids,
                        operands,
                        tree,
                        native,
                        dimensions,
                    )?,
                    Finish::ScopeRandomize(domains) => {
                        // Preserve the original call when no enum is present.
                        // Every scalar operation succeeds and returns signed int 1;
                        // bitwise AND preserves that type and evaluates all operands.
                        if domains.iter().all(Option::is_none) {
                            Expr::Call {
                                callee: Callee::System(SystemFunction::ScopeRandomize),
                                args: operands.into_iter().map(Some).collect(),
                                arg_order: Vec::new(),
                            }
                        } else {
                            let mut calls =
                                operands.into_iter().zip(domains).map(|(operand, domain)| {
                                    let mut args = vec![Some(operand)];
                                    let function = if let Some(values) = domain {
                                        args.extend(values.into_iter().map(|value| {
                                            Some(Expr::Literal(format_typed_parameter_literal(
                                                value,
                                                64,
                                                value < 0,
                                            )))
                                        }));
                                        SystemFunction::ScopeRandomizeEnum
                                    } else {
                                        SystemFunction::ScopeRandomize
                                    };
                                    Expr::Call {
                                        callee: Callee::System(function),
                                        args,
                                        arg_order: Vec::new(),
                                    }
                                });
                            let first = calls.next().ok_or_else(disagreement)?;
                            calls.fold(first, |left, right| Expr::Binary {
                                left: Box::new(left),
                                op: BinaryOp::BitAnd,
                                right: Box::new(right),
                            })
                        }
                    }
                    Finish::ObjectLoad(property) => {
                        let handle = operands.pop().ok_or_else(disagreement)?;
                        objects::load(&property, handle)
                    }
                    Finish::VirtualMember(access) => {
                        let (ty, member) = *access;
                        let handle = operands.pop().ok_or_else(disagreement)?;
                        virtual_interfaces::read(&tree.state, handle, &ty, &member, dimensions, Ok)?
                    }
                    Finish::MethodCall(call) => {
                        let MethodCall {
                            name,
                            given,
                            order,
                            formals,
                            receiver,
                            selection,
                        } = *call;
                        let mut args: Vec<Option<Expr>> = Vec::with_capacity(formals + 1);
                        let mut operands = operands.into_iter();
                        if receiver != objects::Receiver::Static {
                            args.push(Some(match receiver {
                                objects::Receiver::This => objects::this(),
                                _ => operands.next().ok_or_else(disagreement)?,
                            }));
                        }
                        let handles = args.len();
                        for given in given {
                            args.push(if given {
                                Some(operands.next().ok_or_else(disagreement)?)
                            } else {
                                None
                            });
                        }
                        args.extend(selection.map(Some));
                        // Omitted arguments take their defaults (13.5.3).
                        args.resize(formals + handles, None);
                        // The receiver is evaluated before the actuals.
                        let arg_order = if order.iter().enumerate().all(|(at, index)| at == *index)
                        {
                            Vec::new()
                        } else {
                            (0..handles)
                                .chain(order.into_iter().map(|index| index + handles))
                                .collect()
                        };
                        Expr::Call {
                            callee: Callee::Subroutine(name),
                            args,
                            arg_order,
                        }
                    }
                    Finish::Cast(target) => {
                        let operand = remove_real_division_guards(
                            guard_zero_divisions(
                                &tree.state,
                                operands.pop().ok_or_else(disagreement)?,
                            ),
                            dimensions,
                        );
                        if dimensions.constant_indexed_base {
                            let operand =
                                selects::indexed_expression_value(&tree.state, operand, dimensions)
                                    .ok_or_else(|| unsupported("constant cast operand"))?;
                            const_expr_to_expr(
                                casts::cast_constant_target(
                                    &tree.state,
                                    operand,
                                    target,
                                    &dimensions.const_env,
                                )
                                .ok_or_else(|| unsupported("constant cast operand"))?,
                            )
                        } else {
                            casts::runtime_cast_target_expr(
                                &tree.state,
                                target,
                                operand,
                                dimensions,
                            )?
                        }
                    }
                    Finish::Repeat
                        if operands[1..]
                            .iter()
                            .any(|operand| strings::string_value(operand, dimensions)) =>
                    {
                        // A string replication count need not be constant
                        // (11.4.12.2).
                        let count = operands.remove(0);
                        let string = if operands.len() == 1 {
                            strings::to_string(operands.remove(0), dimensions)
                        } else {
                            strings::call(StringOp::Concat, operands)
                        };
                        strings::call(StringOp::Replicate, vec![string, count])
                    }
                    Finish::Repeat => {
                        let count = operands.remove(0);
                        let count = expr_to_const(&tree.state, count.clone())
                            .or_else(|| expr_to_index_const(&tree.state, count, dimensions))
                            .ok_or_else(|| unsupported("replication count"))?;
                        Expr::RepeatConcat {
                            count,
                            parts: operands,
                        }
                    }
                    Finish::Inside(ranges) => {
                        let mut operands = operands.into_iter();
                        let operand = operands.next().ok_or_else(disagreement)?;
                        let mut items = Vec::new();
                        for range in ranges {
                            let low = operands.next().ok_or_else(disagreement)?;
                            items.push(if range {
                                InsideItem::Range {
                                    low,
                                    high: operands.next().ok_or_else(disagreement)?,
                                }
                            } else {
                                InsideItem::Value(low)
                            });
                        }
                        Expr::Inside {
                            expr: Box::new(operand),
                            items,
                        }
                    }
                    Finish::Call {
                        name,
                        args,
                        order,
                        system,
                    } => {
                        if let Some(function) = system {
                            if function == SystemFunction::Fread {
                                let mut operands = operands.into_iter();
                                let mut values = Vec::new();
                                for (position, id) in args.into_iter().enumerate() {
                                    let value = if id.is_some() {
                                        Some(operands.next().ok_or_else(disagreement)?)
                                    } else {
                                        None
                                    };
                                    if position < 2 && value.is_none() {
                                        return Err(unsupported(
                                            "omitted $fread destination or descriptor",
                                        ));
                                    }
                                    if let Some(value) = &value {
                                        if position == 0 {
                                            if !expressions::query_actual_is_writable(
                                                value, dimensions,
                                            ) {
                                                return Err(unsupported(
                                                    "writable $fread destination",
                                                ));
                                            }
                                        } else if expr_static_width(&tree.state, value, dimensions)
                                            .is_none()
                                        {
                                            return Err(unsupported("integral $fread argument"));
                                        }
                                    }
                                    values.push(value);
                                }
                                Expr::Call {
                                    callee: Callee::System(function),
                                    args: values,
                                    arg_order: Vec::new(),
                                }
                            } else if matches!(
                                function,
                                SystemFunction::Urandom
                                    | SystemFunction::Random
                                    | SystemFunction::DistUniform
                                    | SystemFunction::DistNormal
                                    | SystemFunction::DistExponential
                                    | SystemFunction::DistPoisson
                                    | SystemFunction::DistChiSquare
                                    | SystemFunction::DistT
                                    | SystemFunction::DistErlang
                                    | SystemFunction::TestPlusargs
                                    | SystemFunction::ValuePlusargs
                                    | SystemFunction::Fopen
                                    | SystemFunction::Ftell
                                    | SystemFunction::Fseek
                                    | SystemFunction::Rewind
                                    | SystemFunction::Fgetc
                                    | SystemFunction::Ungetc
                                    | SystemFunction::Feof
                                    | SystemFunction::Sscanf
                                    | SystemFunction::Fscanf
                                    | SystemFunction::UrandomRange
                                    | SystemFunction::ScopeRandomize
                            ) {
                                for (position, arg) in operands.iter().enumerate() {
                                    let real_output = matches!(
                                        function,
                                        SystemFunction::Sscanf | SystemFunction::Fscanf
                                    ) && position >= 2
                                        && real_value(arg, dimensions);
                                    if (!real_output
                                        && expr_static_width(&tree.state, arg, dimensions)
                                            .is_none())
                                        || args
                                            .get(position)
                                            .copied()
                                            .flatten()
                                            .and_then(|id| {
                                                array_compatibility::native_unpacked_shape(
                                                    id, native, dimensions,
                                                )
                                            })
                                            .is_some()
                                    {
                                        return Err(unsupported(format!(
                                            "integral argument of `{name}`"
                                        )));
                                    }
                                    if function.writes_argument(position)
                                        && !expressions::query_actual_is_writable(arg, dimensions)
                                    {
                                        return Err(AnalyzerError::InvalidSystemTfCall {
                                            name: name.clone(),
                                            detail: if matches!(
                                                function,
                                                SystemFunction::Sscanf | SystemFunction::Fscanf
                                            ) {
                                                "output argument must be a writable integral, real or shortreal variable".into()
                                            } else {
                                                "output/inout argument must be an integral variable"
                                                    .into()
                                            },
                                        });
                                    }
                                }
                                Expr::Call {
                                    callee: Callee::System(function),
                                    args: operands.into_iter().map(Some).collect(),
                                    arg_order: Vec::new(),
                                }
                            } else {
                                let mut operands = operands;
                                // An omitted number of ticks of `$past` is
                                // one (IEEE 1800-2023 16.9.3); the gate stays
                                // the third argument.
                                if function == SystemFunction::Past
                                    && args.get(1).is_some_and(Option::is_none)
                                {
                                    operands.insert(1, Expr::Literal("1".into()));
                                }
                                expressions::numeric_system_function(
                                    &tree.state,
                                    function,
                                    operands,
                                    dimensions,
                                )?
                            }
                        } else {
                            let mut operands = operands.into_iter();
                            let mut values = Vec::new();
                            for (position, id) in args.into_iter().enumerate() {
                                let value = if id.is_some() {
                                    Some(operands.next().ok_or_else(disagreement)?)
                                } else {
                                    None
                                };
                                if let Some(value) = &value {
                                    expressions::validate_call_argument(
                                        &name, position, value, dimensions,
                                    )?;
                                }
                                values.push(value);
                            }
                            Expr::Call {
                                callee: Callee::Subroutine(name),
                                args: values,
                                arg_order: order,
                            }
                        }
                    }
                };
                values.push(value);
            }
        }
    }
    if values.len() != 1 {
        return Err(disagreement());
    }
    Ok(remove_real_division_guards(
        guard_zero_divisions(&tree.state, values.pop().unwrap()),
        dimensions,
    ))
}

fn index_value(
    id: ExprId,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    dimensions: &PackedDimensions,
) -> Converted<ConstExpr> {
    let value = bind_value(id, native, tree, dimensions, None)?;
    if real_value(&value, dimensions) {
        return Err(unsupported("real-valued select index"));
    }
    let value = expand_expr_calls(
        value,
        &dimensions.functions,
        &dimensions.expression_signedness,
        0,
        true,
    );
    if let Some(folded) = selects::indexed_expression_value(&tree.state, value.clone(), dimensions)
        && (matches!(folded, ConstExpr::Literal(_))
            || eval_ast_const_expr(&tree.state, &folded, &dimensions.const_env).is_some())
    {
        if matches!(&folded, ConstExpr::Literal(text) if real_value(&Expr::Literal(text.clone()), dimensions))
        {
            return Err(unsupported("real-valued select index"));
        }
        return Ok(folded);
    }
    (if dimensions.constant_indexed_base {
        selects::indexed_expression_value(&tree.state, value, dimensions)
    } else {
        expr_to_index_const(&tree.state, value, dimensions)
    })
    .ok_or_else(|| unsupported("index expression"))
}

fn read_index_value(
    id: ExprId,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    dimensions: &PackedDimensions,
) -> Converted<ConstExpr> {
    let value = index_value(id, native, tree, dimensions)?;
    if !dimensions.constant_indexed_base
        && matches!(
            native.expression(ungroup(id, native)).kind,
            ExprKind::Conditional { .. }
        )
        && eval_ast_const_expr(&tree.state, &value, &dimensions.const_env).is_none()
        && !inlining::calls_subroutine(&bind_value(id, native, tree, dimensions, None)?)
    {
        return Err(unsupported(format!(
            "select index `{}`",
            native.token_text(native.expression(id).tokens).trim()
        )));
    }
    Ok(value)
}

pub(super) fn path(
    mut root: ExprId,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    dimensions: &PackedDimensions,
) -> Converted<(Vec<String>, Option<String>)> {
    let authored = root;
    // A member through a virtual interface is selected in its template,
    // which the caller applies to the member of each instance.
    if let Some((_, ty, member)) = virtual_interfaces::native_member(tree, root, native, dimensions)
    {
        virtual_interfaces::member(&tree.state, &ty, &member)?;
        return Ok((
            vec![virtual_interfaces::template(&ty.interface, &member)],
            None,
        ));
    }
    // A static class property is a variable of the module.
    if let Some(variable) = objects::static_property(tree, root, native, dimensions)? {
        return Ok((vec![variable], None));
    }
    let mut parts = Vec::new();
    let mut qualified = None;
    loop {
        match &native.expression(root).kind {
            ExprKind::Name(name) => {
                parts.push(instances::normalize_identifier(&name.spelling));
                break;
            }
            ExprKind::Member {
                expression,
                separator,
                name,
            } => {
                parts.push(instances::normalize_identifier(&name.spelling));
                if separator == "::" {
                    qualified = Some(parts.len());
                }
                root = *expression;
            }
            ExprKind::Select {
                expression,
                dimension,
            } if matches!(dimension.kind, DimensionKind::Index(_)) => {
                let DimensionKind::Index(index) = dimension.kind else {
                    unreachable!()
                };
                let value = index_value(index, native, tree, dimensions)?;
                let value = eval_ast_const_expr(&tree.state, &value, &dimensions.const_env)
                    .ok_or_else(|| unsupported("hierarchical index"))?;
                let (mut prefix, base) = path(*expression, native, tree, dimensions)?;
                let last = prefix.last_mut().ok_or_else(disagreement)?;
                last.push_str(&format!("[{value}]"));
                prefix.extend(parts.into_iter().rev());
                if base.is_none()
                    && let Some(name) =
                        interface_binding::unindexed_parameter(&prefix, tree, dimensions, authored)
                {
                    return Ok((vec![name], None));
                }
                return Ok((prefix, base));
            }
            _ => return Err(unsupported("member receiver")),
        }
    }
    parts.reverse();
    if qualified.is_none() {
        unpacked_structs::collapse(&mut parts, dimensions);
    }
    if qualified.is_none()
        && let Some(name) =
            interface_binding::unindexed_parameter(&parts, tree, dimensions, authored)
    {
        return Ok((vec![name], None));
    }

    if let Some(length) = qualified {
        if parts.len() == length + 1 {
            let base = parts[..2].join("::");
            parts.drain(..1);
            qualified = None;
            let _ = qualified;
            return Ok((parts, Some(base)));
        }
    }
    Ok((parts, None))
}

fn selected_value(
    root: ExprId,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    dimensions: &PackedDimensions,
) -> Converted<Expr> {
    let mut base = root;
    let mut suffix = Vec::new();
    while let ExprKind::Select {
        expression,
        dimension,
    } = &native.expression(base).kind
    {
        suffix.push(&dimension.kind);
        base = *expression;
    }
    suffix.reverse();
    if let ExprKind::Name(name) = &native.expression(base).kind
        && dimensions
            .get(&instances::normalize_identifier(&name.spelling))
            .is_some_and(|ty| ty.real && suffix.len() > ty.unpacked.len())
    {
        return Err(unsupported("bit-select or part-select of a real variable"));
    }
    // A select of a member of a container element reads the member first.
    let element_member = if suffix.is_empty() {
        None
    } else {
        containers::element_member(base, native, tree, dimensions)?
    };
    if element_member.is_some()
        || matches!(
            native.expression(base).kind,
            ExprKind::Concatenation(_) | ExprKind::Replication { .. }
        )
    {
        let expr = match element_member {
            Some((ty, value)) => {
                // The select indices are offsets from bit 0, which only a
                // `[N-1:0]` range declares.
                let zero_based = match ty.packed_ranges() {
                    [range] => {
                        eval_ast_const_expr(&tree.state, range.right(), &dimensions.const_env)
                            == Some(0)
                            && eval_ast_const_expr(&tree.state, range.left(), &dimensions.const_env)
                                .is_some_and(|left| left >= 0)
                    }
                    _ => false,
                };
                if !zero_based || ty.unpacked_struct().is_some() || !ty.members.is_empty() {
                    return Err(unsupported(
                        "select of a container element member that is not declared [N-1:0]",
                    ));
                }
                value
            }
            None => bind_value(base, native, tree, dimensions, None)?,
        };
        if suffix.len() != 1 {
            return Err(unsupported("concatenation select"));
        }
        let selection = match suffix[0] {
            DimensionKind::Range { left, right } => {
                selects::AggregateSelection::Range(selects::ParameterRange::Bounds(
                    read_index_value(*left, native, tree, dimensions)?,
                    read_index_value(*right, native, tree, dimensions)?,
                ))
            }
            DimensionKind::Index(id) => {
                selects::AggregateSelection::Index(read_index_value(*id, native, tree, dimensions)?)
            }
            DimensionKind::Indexed {
                base,
                operator,
                width,
            } => selects::AggregateSelection::Range(selects::ParameterRange::Indexed {
                base: read_index_value(*base, native, tree, dimensions)?,
                width: read_index_value(*width, native, tree, dimensions)?,
                plus: operator == "+:",
            }),
            _ => return Err(unsupported("concatenation select")),
        };
        return selects::aggregate_selection_value(&tree.state, expr, selection, dimensions);
    }
    let (parts, qualified) = path(base, native, tree, dimensions)?;
    if dimensions.constant_indexed_base && parts.len() == 1 {
        let name = qualified.clone().unwrap_or_else(|| parts[0].clone());
        let mut indices = Vec::new();
        let mut range = None;
        let mut ranged = false;
        for dimension in &suffix {
            if ranged {
                return Err(unsupported("select following a part-select"));
            }
            ranged = !matches!(dimension, DimensionKind::Index(_));
            match dimension {
                DimensionKind::Index(id) => {
                    indices.push(read_index_value(*id, native, tree, dimensions)?)
                }
                DimensionKind::Range { left, right } => {
                    range = Some(selects::ParameterRange::Bounds(
                        read_index_value(*left, native, tree, dimensions)?,
                        read_index_value(*right, native, tree, dimensions)?,
                    ))
                }
                DimensionKind::Indexed {
                    base,
                    operator,
                    width,
                } => {
                    range = Some(selects::ParameterRange::Indexed {
                        base: read_index_value(*base, native, tree, dimensions)?,
                        width: read_index_value(*width, native, tree, dimensions)?,
                        plus: operator == "+:",
                    })
                }
                _ => return Err(unsupported("select dimension")),
            }
        }
        return if let Some(range) = range {
            selects::indexed_parameter_range(&tree.state, name, indices, range, dimensions)
        } else {
            selects::indexed_parameter_indices(&tree.state, name, indices, dimensions)
        };
    }
    for dimension in &suffix {
        if let DimensionKind::Index(id) = dimension
            && matches!(
                native.expression(ungroup(*id, native)).kind,
                ExprKind::Conditional { .. }
            )
        {
            read_index_value(*id, native, tree, dimensions)?;
        }
    }
    let value = expr_from_lvalue(
        &tree.state,
        &selected_lvalue(root, native, tree, dimensions, false)?,
        dimensions,
    );
    if let Expr::Ident(name) = &value
        && let Some(value) = unpacked_structs::whole(name, dimensions)
    {
        return Ok(value);
    }
    let guards = member_context(root, native, tree, dimensions)?
        .map(|(guards, _)| guards)
        .unwrap_or_default();
    packed_structs::checked_value(guards, value, &tree.state, dimensions)
}

pub(in crate::ast) fn selected_lvalue(
    root: ExprId,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    dimensions: &PackedDimensions,
    net: bool,
) -> Converted<LValue> {
    let mut base = root;
    let mut suffix = Vec::new();
    while let ExprKind::Select {
        expression,
        dimension,
    } = &native.expression(base).kind
    {
        suffix.push(&dimension.kind);
        base = *expression;
    }
    suffix.reverse();
    let (parts, qualified) = path(base, native, tree, dimensions)?;
    if suffix.is_empty() && parts.len() > 1 {
        return packed_structs::whole_member(&tree.state, parts, qualified, dimensions);
    }
    let finish = |name: String, context: &PackedDimensions, strict: bool| -> Converted<LValue> {
        if context.get(&name).is_some_and(|ty| {
            ty.members
                .first()
                .is_some_and(packed_structs::PackedMember::is_tagged)
                && suffix.len() >= ty.unpacked.len() + ty.packed.len()
        }) {
            return Err(unsupported("bit-select or part-select of a tagged union"));
        }
        if context
            .get(&name)
            .is_some_and(|ty| ty.real && suffix.len() > ty.unpacked.len())
        {
            return Err(unsupported("bit-select or part-select of a real variable"));
        }
        let mut indices = Vec::new();
        let mut range = None;
        let mut ranged = false;
        for dimension in &suffix {
            if ranged {
                return Err(unsupported("select following a part-select"));
            }
            ranged = !matches!(dimension, DimensionKind::Index(_));
            match dimension {
                DimensionKind::Index(id) => indices.push(selects::self_determined_index(
                    &tree.state,
                    index_value(*id, native, tree, context)?,
                    context,
                )),
                DimensionKind::Range { left, right } => {
                    range = Some((
                        index_value(*left, native, tree, context)?,
                        index_value(*right, native, tree, context)?,
                        false,
                    ))
                }
                DimensionKind::Indexed {
                    base,
                    operator,
                    width,
                } => {
                    let start = index_value(*base, native, tree, context)?;
                    let width = index_value(*width, native, tree, context)?;
                    let ((msb, lsb), dynamic) = if net {
                        let width = eval_ast_const_expr(&tree.state, &width, &context.const_env)
                            .ok_or_else(|| {
                                unsupported("indexed part-select width that is not constant")
                            })?;
                        (
                            selects::indexed_select_bounds_values(
                                &tree.state,
                                start,
                                operator == "+:",
                                width,
                                (Some(&name), indices.len()),
                                context,
                            )?,
                            false,
                        )
                    } else {
                        selects::indexed_range_values(
                            &tree.state,
                            &name,
                            start,
                            width,
                            operator == "+:",
                            indices.len(),
                            context,
                        )?
                    };
                    range = Some((msb, lsb, dynamic));
                }
                _ => return Err(unsupported("select dimension")),
            }
        }
        if strict
            && !selects::constant_packed_indices_in_range(&tree.state, &name, &indices, context)
        {
            return Err(unsupported(format!(
                "constant select of `{name}` outside its declared range"
            )));
        }
        if let Some((msb, lsb, dynamic)) = range {
            selects::lvalue_from_range(&tree.state, name, indices, msb, lsb, dynamic, context)
        } else {
            selects::lvalue_from_indices(&tree.state, name, indices, context)
        }
    };
    let lvalue = if parts.len() > 1 {
        packed_structs::select_member(&tree.state, parts, qualified, dimensions, finish)?
    } else {
        finish(
            qualified.unwrap_or_else(|| parts[0].clone()),
            dimensions,
            false,
        )?
    };
    Ok(lvalue)
}

/// The actuals of `$sformatf` (IEEE 1800-2023 21.3.3): a format followed by
/// any values, all given and positional.
pub(in crate::ast) fn format_arguments(
    name: &str,
    args: &[celox_sv_syntax::Argument],
) -> Converted<Vec<ExprId>> {
    if args.iter().any(|arg| arg.name.is_some()) {
        return Err(unsupported(format!("named arguments of `{name}`")));
    }
    let actuals = args
        .iter()
        .enumerate()
        .map(|(position, arg)| {
            arg.value.ok_or_else(|| AnalyzerError::InvalidSystemTfCall {
                name: name.to_string(),
                detail: format!("argument {} is omitted", position + 1),
            })
        })
        .collect::<Converted<Vec<_>>>()?;
    if actuals.is_empty() {
        return Err(AnalyzerError::InvalidSystemTfCall {
            name: name.to_string(),
            detail: "expected a format argument".into(),
        });
    }
    Ok(actuals)
}

/// String literals in integral contexts are unsigned packed arrays of bytes
/// (IEEE 1800-2023 5.9). Keep the semantic IR integral, including escapes.
fn packed_string_literal(text: &str) -> Option<String> {
    match format_patterns::bytes_literal(&string_literal_bytes(text)?) {
        Expr::Literal(literal) => Some(literal),
        _ => unreachable!("a packed literal"),
    }
}

/// The bytes of string literal `text`, written with its quotes, after its
/// escapes (IEEE 1800-2023 5.9.1).
pub(in crate::ast) fn string_literal_bytes(text: &str) -> Option<Vec<u8>> {
    let text = if let Some(text) = text.strip_prefix("\"\"\"") {
        text.strip_suffix("\"\"\"")?
    } else {
        text.strip_prefix('"')?.strip_suffix('"')?
    };
    let mut input = text.bytes().peekable();
    let mut bytes = Vec::new();
    while let Some(byte) = input.next() {
        if byte != b'\\' {
            bytes.push(byte);
            continue;
        }
        let escaped = input.next()?;
        let value = match escaped {
            b'n' => b'\n',
            b't' => b'\t',
            b'v' => 11,
            b'f' => 12,
            b'a' => 7,
            b'\\' => b'\\',
            b'"' => b'"',
            b'\n' => continue,
            b'\r' if input.peek() == Some(&b'\n') => {
                input.next();
                continue;
            }
            b'0'..=b'7' => {
                let mut value = escaped - b'0';
                for _ in 0..2 {
                    if let Some(&digit @ b'0'..=b'7') = input.peek() {
                        input.next();
                        value = value.wrapping_mul(8).wrapping_add(digit - b'0');
                    } else {
                        break;
                    }
                }
                value
            }
            b'x' => {
                let mut value = 0u8;
                let mut count = 0;
                for _ in 0..2 {
                    if let Some(digit) = input.peek().and_then(|byte| (*byte as char).to_digit(16))
                    {
                        input.next();
                        value = value * 16 + digit as u8;
                        count += 1;
                    } else {
                        break;
                    }
                }
                if count == 0 {
                    return None;
                }
                value
            }
            other => other,
        };
        bytes.push(value);
    }
    Some(bytes)
}

pub(in crate::ast) fn real_value(expr: &Expr, dimensions: &PackedDimensions) -> bool {
    match expr {
        Expr::Ident(name) => {
            dimensions.get(name).is_some_and(|ty| ty.real)
                || dimensions.parameter_values.get(name).is_some_and(|value| {
                    matches!(value, Expr::Literal(text) if text.contains(['.', 'e', 'E']) && text.replace('_', "").parse::<f64>().is_ok())
                        || matches!(value, Expr::Call { callee: Callee::System(function), .. }
                            if function.is_real_math() || matches!(function, SystemFunction::Bitstoreal | SystemFunction::Bitstoshortreal | SystemFunction::Itor))
                })
        }
        Expr::Literal(text) => {
            text.contains(['.', 'e', 'E']) && text.replace('_', "").parse::<f64>().is_ok()
        }
        Expr::Call {
            callee:
                Callee::System(
                    SystemFunction::Ln
                    | SystemFunction::Log10
                    | SystemFunction::Exp
                    | SystemFunction::Sqrt
                    | SystemFunction::Floor
                    | SystemFunction::Ceil
                    | SystemFunction::Sin
                    | SystemFunction::Cos
                    | SystemFunction::Tan
                    | SystemFunction::Asin
                    | SystemFunction::Acos
                    | SystemFunction::Atan
                    | SystemFunction::Sinh
                    | SystemFunction::Cosh
                    | SystemFunction::Tanh
                    | SystemFunction::Asinh
                    | SystemFunction::Acosh
                    | SystemFunction::Atanh
                    | SystemFunction::Pow
                    | SystemFunction::Atan2
                    | SystemFunction::Hypot
                    | SystemFunction::Bitstoreal
                    | SystemFunction::Bitstoshortreal
                    | SystemFunction::Itor
                    | SystemFunction::Realtime,
                ),
            ..
        } => true,
        Expr::Call {
            callee: Callee::Subroutine(name),
            ..
        } => dimensions
            .functions
            .get(name)
            .is_some_and(|function| function.return_real),
        // A sampled value has the type of its operand.
        Expr::Call {
            callee: Callee::System(SystemFunction::Sampled | SystemFunction::Past),
            args,
            ..
        } => matches!(args.first(), Some(Some(operand)) if real_value(operand, dimensions)),
        Expr::Unary {
            op: UnaryOp::Plus | UnaryOp::Minus | UnaryOp::ToTwoState,
            expr,
        } => real_value(expr, dimensions),
        Expr::Binary {
            left,
            op: BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Pow,
            right,
        } => real_value(left, dimensions) || real_value(right, dimensions),
        Expr::Mux {
            then_expr,
            else_expr,
            ..
        } => real_value(then_expr, dimensions) || real_value(else_expr, dimensions),
        _ => false,
    }
}

// Integer zero-divisor guards must not change real arithmetic or its type.
pub(in crate::ast) fn remove_real_division_guards(expr: Expr, dims: &PackedDimensions) -> Expr {
    match expr {
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            if matches!(&*then_expr, Expr::Literal(text) if text == crate::DIV_ZERO_UNKNOWN_LITERAL)
                && matches!(&*else_expr, Expr::Binary { left, op: BinaryOp::Div, right } if real_value(left, dims) || real_value(right, dims))
            {
                return remove_real_division_guards(*else_expr, dims);
            }
            Expr::Mux {
                condition: Box::new(remove_real_division_guards(*condition, dims)),
                then_expr: Box::new(remove_real_division_guards(*then_expr, dims)),
                else_expr: Box::new(remove_real_division_guards(*else_expr, dims)),
            }
        }
        Expr::Unary { op, expr } => Expr::Unary {
            op,
            expr: Box::new(remove_real_division_guards(*expr, dims)),
        },
        Expr::Binary { left, op, right } => Expr::Binary {
            left: Box::new(remove_real_division_guards(*left, dims)),
            op,
            right: Box::new(remove_real_division_guards(*right, dims)),
        },
        Expr::Resize {
            expr,
            width,
            signed,
        } => Expr::Resize {
            expr: Box::new(remove_real_division_guards(*expr, dims)),
            width,
            signed,
        },
        Expr::Call {
            callee,
            args,
            arg_order,
        } => Expr::Call {
            callee,
            args: args
                .into_iter()
                .map(|arg| arg.map(|arg| remove_real_division_guards(arg, dims)))
                .collect(),
            arg_order,
        },
        expr => expr,
    }
}

/// Preserve the native member path before flattening it to bit coordinates.
pub(in crate::ast) fn member_context(
    mut root: ExprId,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    dims: &PackedDimensions,
) -> Converted<Option<(Vec<Expr>, Type)>> {
    while let ExprKind::Select { expression, .. } = native.expression(root).kind {
        root = expression;
    }
    if !matches!(native.expression(root).kind, ExprKind::Member { .. }) {
        return Ok(None);
    }
    let (mut parts, qualified) = path(root, native, tree, dims)?;
    if let Some(name) = qualified {
        parts[0] = name;
    }
    packed_structs::member_access(parts, dims, &tree.state)
}

fn cast_shape(
    target: ExprId,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    dims: &PackedDimensions,
) -> Converted<Option<VariableDimensions>> {
    let ty = if let Some(name) = call_name(&tree.state, target, native) {
        dims.type_aliases.get(&name).cloned()
    } else if let ExprKind::Type(id) = native.expression(target).kind {
        type_binding::binding::bind_in(id, native, tree, dims)?
    } else {
        None
    };
    Ok(ty
        .filter(|ty| !ty.members.is_empty())
        .map(|ty| procedural::dimensions_from_type(&ty)))
}

/// Output/inout copy-out is an assignment to the actual, whose nominal tagged
/// type must match the formal. Concatenations and system outputs are integral.
fn check_tagged_copyout(
    root: ExprId,
    formal: Option<&VariableDimensions>,
    tree: &AnalysisContext<'_>,
    dims: &PackedDimensions,
) -> Converted<()> {
    let native = tree.declarations();
    let mut pending = vec![(root, formal)];
    while let Some((root, formal)) = pending.pop() {
        let root = ungroup(root, native);
        if let ExprKind::Concatenation(parts) = &native.expression(root).kind {
            pending.extend(parts.iter().map(|id| (*id, None)));
            continue;
        }
        let Some(actual) = bound_patterns::native_value_type(root, tree, dims)? else {
            continue;
        };
        if actual.tagged_identity().is_some()
            && formal
                .and_then(|shape| shape.pattern_type(&tree.state, dims))
                .as_ref()
                != Some(&actual)
        {
            return Err(unsupported(
                "copy-out to a tagged union requires the same union type",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod string_literal_tests {
    use super::packed_string_literal;

    #[test]
    fn packed_string_literal_triple_delimiters_preserve_content() {
        assert_eq!(packed_string_literal("\"\"\"\"\"\""), Some("8'h0".into()));
        assert_eq!(
            packed_string_literal("\"\"\"A\"B\nC\"\"\""),
            Some("40'h4122420a43".into())
        );
        assert_eq!(
            packed_string_literal("\"\"\"A\r\nB\"\"\""),
            Some("32'h410d0a42".into())
        );
    }

    #[test]
    fn packed_string_literal_escaped_newlines_ignore_lf_and_crlf() {
        for delimiter in ["\"", "\"\"\""] {
            for newline in ["\n", "\r\n"] {
                let literal = format!("{delimiter}ab\\{newline}cd{delimiter}");
                assert_eq!(packed_string_literal(&literal), Some("32'h61626364".into()));
            }
        }
    }
}
