//! Native expression jobs and shared semantic IR kernels.
use super::*;
use std::rc::Rc;
mod enter;
mod finish;
mod selected_values;
mod subroutine_calls;
use enter::*;
use finish::*;
pub(in crate::ast) use selected_values::*;
use subroutine_calls::*;

pub(in crate::ast) fn ungroup(mut id: ExprId, native: &Declarations) -> ExprId {
    while let ExprKind::Group(inner) = native.expression(id).kind {
        id = inner;
    }
    id
}

/// Whether `root` is an assignment pattern without a type, or passes its
/// assignment-like context on to one: a parenthesized expression and the
/// second and third operands of a conditional are in the context of the
/// expression itself (IEEE 1800-2023 10.8).
pub(in crate::ast) fn reaches_untyped_pattern(root: ExprId, native: &Declarations) -> bool {
    let mut pending = vec![root];
    while let Some(id) = pending.pop() {
        match &native.expression(id).kind {
            ExprKind::Group(inner) => pending.push(*inner),
            ExprKind::Conditional {
                then_expression,
                else_expression,
                ..
            } => pending.extend([*then_expression, *else_expression]),
            ExprKind::Pattern { target: None, .. } => return true,
            _ => {}
        }
    }
    false
}

pub(in crate::ast) fn call_name(
    tree: &AnalysisContext<'_>,
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
            tree,
            &call_name(tree, *expression, native)?,
            &instances::normalize_identifier(&name.spelling),
        )),
        _ => None,
    }
}

pub(in crate::ast) fn callable_name(
    tree: &AnalysisContext<'_>,
    id: ExprId,
    native: &Declarations,
) -> Option<String> {
    if let Some(name) = call_name(tree, id, native) {
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
    let handle = call_name(tree, *expression, native)?;
    scope::imported(tree)
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
    session: &mut Session,
    id: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
) -> Converted<Option<String>> {
    if let Some(name) = callable_name(tree, id, native) {
        return Ok(Some(name));
    }
    if let Some((name, _, objects::Receiver::Static)) =
        objects::method(tree, session, id, native, dimensions)?
    {
        return Ok(Some(name));
    }
    interface_member_name(tree, session, id, native, dimensions)
}

/// The flattened name of the member `id` selects of an interface handle:
/// an instance or a port, or an element of an array of them that the
/// constant environment selects (IEEE 1800-2023 25.3).
pub(in crate::ast) fn interface_member_name(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    id: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
) -> Converted<Option<String>> {
    let ExprKind::Member {
        expression,
        separator,
        name,
    } = &native.expression(ungroup(id, native)).kind
    else {
        return Ok(None);
    };
    // The handles of the scope are those of its own arena: an expression of
    // another arena names none of them.
    if !std::ptr::eq(native, tree.declarations()) {
        return Ok(None);
    }
    // Only a call through an interface handle names an interface method;
    // other receivers, such as the package-scoped `p::q` of
    // `p::q.push_back(x)`, are bound as values.
    if separator != "." || !interface_binding::is_handle_path(*expression, native) {
        return Ok(None);
    }
    let handle = interface_binding::actual_handle_key(
        *expression,
        tree,
        session,
        &dimensions.const_env,
        &dimensions.type_aliases,
    )?;
    Ok(scope::imported(tree)
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

/// The class that cast target `target` names, as in `C'(h)` or
/// `C#(N)'(h)`, by the type the cast has.
pub(in crate::ast) fn cast_target_class(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    target: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
) -> Option<String> {
    // Only a type expression can name a class; a size is not evaluated here.
    if !matches!(
        native.expression(ungroup(target, native)).kind,
        ExprKind::Type(_)
            | ExprKind::Name(_)
            | ExprKind::Member { .. }
            | ExprKind::Parameterized { .. }
    ) {
        return None;
    }
    let target = expression_types::cast_type(tree, session, target, native, dimensions)?;
    expression_types::class_name(&session.types, target)
}

/// Whether `target'(operand)` casts to a class handle type, which a static
/// cast does only as an assignment (IEEE 1800-2023 6.24.1): a handle of a
/// class that derives from the target's. A downcast needs the run-time check
/// of `$cast` (8.16), and a class is no destination of a bit-stream cast
/// (6.24.3).
fn class_cast(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    target: ExprId,
    operand: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
) -> Converted<bool> {
    let Some(class) = cast_target_class(tree, session, target, native, dimensions) else {
        return Ok(false);
    };
    // The operand's type decides whether the cast is valid, so it must be
    // known: a conditional whose arms have no common type has none
    // (11.4.11).
    let Ok(source) = expression_types::type_of(tree, session, operand, native, dimensions) else {
        if matches!(
            native.expression(ungroup(operand, native)).kind,
            ExprKind::Conditional { .. }
        ) {
            return Err(AnalyzerError::InvalidCast(format!(
                "the conditional operand of a cast to class `{class}` has no common type \
                 (IEEE 1800-2023 11.4.11)"
            )));
        }
        return Err(unsupported(format!(
            "static cast to class `{class}` of an operand whose type is not resolved"
        )));
    };
    if !matches!(
        session.types.data(source),
        semantic_types::TyData::Class(_) | semantic_types::TyData::Null
    ) {
        return Err(AnalyzerError::InvalidCast(format!(
            "`{}` is no class handle to cast to class `{class}`",
            session.types.spell(source)
        )));
    }
    objects::check_assignment(tree, session, operand, native, dimensions, &class, || {
        "static cast".to_owned()
    })
    .map_err(|error| match error {
        AnalyzerError::IncompatibleClassHandle { actual, target, .. } => {
            AnalyzerError::InvalidCast(format!(
                "a handle of class `{actual}` is not assignment compatible with class \
                 `{target}`; a downcast needs `$cast`"
            ))
        }
        error => error,
    })?;
    Ok(true)
}

/// The stream layout of the target of cast `target'(operand)`, when its
/// type is known, after checking that the stream lowering gives a layout to
/// both the operand and the target (6.24.3): one it cannot give is an
/// unsupported cast rather than a stream of another shape.
fn stream_layouts(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    target: ExprId,
    operand: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
) -> Converted<Option<semantic_types::StreamLayout>> {
    use semantic_types::StreamRole;
    // A stream operand's items are laid out where it is packed.
    if !streams::is_stream(operand, native)
        && let Ok(source) = expression_types::type_of(tree, session, operand, native, dimensions)
    {
        session
            .types
            .stream_layout(source, StreamRole::Operand)
            .map_err(unsupported)?;
    }
    let names_type = match &native.expression(ungroup(target, native)).kind {
        ExprKind::Type(_) => true,
        _ => call_name(tree, target, native)
            .is_some_and(|name| dimensions.type_aliases.contains_key(&name)),
    };
    if !names_type {
        return Ok(None);
    }
    let Some(target) = expression_types::cast_type(tree, session, target, native, dimensions)
    else {
        return Ok(None);
    };
    // A streaming concatenation is the operand only of a cast to a
    // bit-stream type (IEEE 1800-2023 11.4.14, 6.24.3), which a real, a
    // chandle or a virtual interface is not.
    if streams::is_stream(operand, native)
        && matches!(
            session.types.data(target),
            semantic_types::TyData::Real
                | semantic_types::TyData::Shortreal
                | semantic_types::TyData::Chandle
                | semantic_types::TyData::VirtualInterface { .. }
        )
    {
        return Err(AnalyzerError::InvalidStream(format!(
            "a streaming concatenation cast to `{}`, which is not a bit-stream type",
            session.types.spell(target)
        )));
    }
    session
        .types
        .stream_layout(target, StreamRole::Target)
        .map(Some)
        .map_err(unsupported)
}

/// Whether type cast `target'(operand)` converts between bit-stream types
/// that are not both integral (IEEE 1800-2023 6.24.3): its operand packs
/// into the target as the stream `{>>{operand}}` would, index 0 first.
fn bit_stream_cast(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    target: ExprId,
    operand: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
) -> bool {
    // A size or signing cast is no bit-stream cast; its size is not
    // evaluated here.
    let names_type = match &native.expression(ungroup(target, native)).kind {
        ExprKind::Type(_) => true,
        _ => call_name(tree, target, native)
            .is_some_and(|name| dimensions.type_aliases.contains_key(&name)),
    };
    if !names_type {
        return false;
    }
    let Some(target) = expression_types::cast_type(tree, session, target, native, dimensions)
    else {
        return false;
    };
    let Ok(source) = expression_types::type_of(tree, session, operand, native, dimensions) else {
        return false;
    };
    let integral = |id| matches!(session.types.data(id), semantic_types::TyData::Packed(_));
    // An integral value converts to a string by its own rule, zero-filled
    // on the left to whole characters (6.16), not by the stream's
    // right-padded bytes.
    let string = matches!(session.types.data(target), semantic_types::TyData::String);
    // Fixed sizes that differ are reported where the stream is packed.
    !(integral(source) && (integral(target) || string))
        && matches!(
            session.types.castable(source, target),
            Ok(semantic_types::Castable::BitStream { .. })
                | Err(semantic_types::Mismatch::Size { .. })
        )
}

pub(in crate::ast) fn cast_target_native(
    target: ExprId,
    operand: ExprId,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dimensions: &PackedDimensions,
) -> Converted<casts::RuntimeCastTarget> {
    let target = ungroup(target, native);
    if let ExprKind::Type(id) = native.expression(target).kind
        && matches!(&native.data_type(id).kind, celox_sv_syntax::DataTypeKind::Builtin(name) if name == "string")
    {
        return Ok(casts::RuntimeCastTarget::String);
    }
    if let ExprKind::Type(id) = native.expression(target).kind
        && let Some(ty) = type_binding::binding::bind_in(id, native, tree, session, dimensions)?
        && matches!(ty.kind(), TypeKind::Real | TypeKind::Shortreal)
    {
        return Ok(casts::RuntimeCastTarget::Real {
            short: ty.kind() == TypeKind::Shortreal,
        });
    }
    // The operand of a size or signing cast is integral (6.24.1).
    let check_integral = |session: &mut Session, cast: &str| {
        let Ok(operand) = expression_types::type_of(tree, session, operand, native, dimensions)
        else {
            return Ok(());
        };
        if matches!(
            session.types.data(operand),
            semantic_types::TyData::Packed(_)
        ) {
            return Ok(());
        }
        Err(AnalyzerError::InvalidCast(format!(
            "the operand of a {cast} cast is `{}`, not an integral value",
            session.types.spell(operand)
        )))
    };
    if let ExprKind::Name(name) = &native.expression(target).kind
        && matches!(name.spelling.as_str(), "signed" | "unsigned")
    {
        check_integral(session, "signing")?;
        return Ok(casts::RuntimeCastTarget::Signing(name.spelling == "signed"));
    }
    let named = call_name(tree, target, native);
    let (ty, numeric, two_state) = if let Some(ty) = named
        .as_ref()
        .and_then(|name| dimensions.type_aliases.get(name))
    {
        (
            casts::expr_type_from_type(&tree.functions, ty, &dimensions.const_env)
                .ok_or_else(|| unsupported("cast target type"))?,
            false,
            matches!(ty.kind(), TypeKind::Bit | TypeKind::Chandle),
        )
    } else if let ExprKind::Type(id) = native.expression(target).kind {
        match &native.data_type(id).kind {
            celox_sv_syntax::DataTypeKind::Builtin(name)
                if matches!(
                    name.as_str(),
                    "bit"
                        | "logic"
                        | "reg"
                        | "byte"
                        | "shortint"
                        | "int"
                        | "longint"
                        | "integer"
                        | "time"
                ) => {}
            celox_sv_syntax::DataTypeKind::Named(_) => {}
            _ => return Err(unsupported("cast target type")),
        }
        let ty = type_binding::binding::bind_in(id, native, tree, session, dimensions)?
            .ok_or_else(|| unsupported("cast target type"))?;
        (
            casts::expr_type_from_type(&tree.functions, &ty, &dimensions.const_env)
                .ok_or_else(|| unsupported("cast target type"))?,
            false,
            matches!(ty.kind(), TypeKind::Bit | TypeKind::Chandle),
        )
    } else {
        // The size is a constant expression (6.24.1), which may select a
        // member of a structure parameter.
        let value = match constant_binding::bind_in(target, native, tree, session, dimensions) {
            Ok(Some(value)) => Some(value),
            _ => {
                let value = super::bind_value(target, native, tree, session, dimensions, None)?;
                expr_to_const(session, value)
            }
        };
        let value = value
            .and_then(|value| eval_ast_const_expr(&tree.functions, &value, &dimensions.const_env))
            .and_then(|value| usize::try_from(value).ok())
            .filter(|width| *width > 0)
            .ok_or_else(|| unsupported("cast target type"))?;
        check_integral(session, "size")?;
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
    ScopeRandomize(Vec<Option<Arc<crate::ast::enums::EnumDomain>>>),
    Tagged(packed_structs::Construction),
    Unary(String),
    Binary(String),
    /// A conditional; with the layout of the container that receives it
    /// when its condition may be ambiguous, see [`containers::conditional`].
    Mux(Option<crate::system_functions::ContainerLayout>),
    Bits,
    Concat,
    Call {
        name: String,
        args: Vec<Option<ExprId>>,
        order: Vec<usize>,
        system: Option<SystemFunction>,
    },
    Cast(casts::RuntimeCastTarget),
    /// A replication; `literals` when every replicated operand is a string
    /// literal, which a non-constant count makes a string replication.
    Repeat {
        literals: bool,
    },
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

/// The operands of an expression the binder entered, each with the shape
/// that receives it, and what makes its value of theirs.
type Operands = (Finish, Vec<(ExprId, Option<Rc<VariableDimensions>>)>);

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
    session: &mut Session,
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
            session,
            dimensions,
            container,
            direction == ParamDirection::Inout,
        )?;
    }
    Ok(())
}

/// Bind value native expression `root`, which is no chandle unless `target`
/// is a chandle (see [`chandles::Permits`]).
pub(in crate::ast) fn bind_value(
    root: ExprId,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dimensions: &PackedDimensions,
    target: Option<&VariableDimensions>,
) -> Converted<Expr> {
    bind_value_in(root, native, tree, dimensions, target, false, session)
}

/// Bind value native expression `root`, which may be a chandle: a condition
/// tested as a Boolean, or a value a chandle receives (IEEE 1800-2023 6.14).
pub(in crate::ast) fn bind_chandle_value(
    root: ExprId,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dimensions: &PackedDimensions,
    target: Option<&VariableDimensions>,
) -> Converted<Expr> {
    bind_value_in(root, native, tree, dimensions, target, true, session)
}

fn bind_value_in(
    root: ExprId,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    dimensions: &PackedDimensions,
    target: Option<&VariableDimensions>,
    chandle: bool,
    session: &mut Session,
) -> Converted<Expr> {
    let mut permits = chandles::Permits::new(root, chandle);
    let mut jobs = vec![Job::Enter(root, target.cloned().map(Rc::new), None)];
    let mut values = Vec::new();
    while let Some(job) = jobs.pop() {
        match job {
            Job::Enter(id, target, scope) => {
                let dimensions = scope.as_deref().unwrap_or(dimensions);
                enter(
                    id,
                    target,
                    &scope,
                    root,
                    &mut permits,
                    &mut jobs,
                    &mut values,
                    native,
                    tree,
                    session,
                    dimensions,
                )?;
            }
            Job::Stream(id, receiver, scope) => {
                let dimensions = scope.as_deref().unwrap_or(dimensions);
                values.push(streams::pack(
                    id, receiver, native, tree, session, dimensions,
                )?);
            }
            Job::Finish(finish, count, scope) => {
                let dimensions = scope.as_deref().unwrap_or(dimensions);
                let start = values.len().checked_sub(count).ok_or_else(disagreement)?;
                let operands = values.split_off(start);
                let value = finish_value(finish, operands, native, tree, session, dimensions)?;
                values.push(value);
            }
        }
    }
    if values.len() != 1 {
        return Err(disagreement());
    }
    Ok(remove_real_division_guards(
        guard_zero_divisions(tree, session, values.pop().unwrap()),
        dimensions,
    ))
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
    crate::typecheck::unescape_string_literal(text)
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
        // A real or shortreal class property (IEEE 1800-2023 6.12).
        Expr::Call {
            callee: Callee::Object(op),
            ..
        } => matches!(&**op, ObjectOp::Load(field) if field.real),
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

/// Preserve the native member path before flattening it to bit coordinates:
/// the checks of the tags the path crosses, and the type of the target, a
/// selection of the member when the path is selected (IEEE 1800-2023
/// 10.9, 11.5.1).
pub(in crate::ast) fn member_context(
    authored: ExprId,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dims: &PackedDimensions,
) -> Converted<Option<(Vec<Expr>, Type)>> {
    let mut root = authored;
    while let ExprKind::Select { expression, .. } = native.expression(root).kind {
        root = expression;
    }
    path_context(authored, root, native, tree, session, dims)
}

/// The variable, member, class property or container element that
/// assignment target `root` selects, and its declared type.
fn target_base(
    root: ExprId,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dims: &PackedDimensions,
) -> Converted<Option<(ExprId, Type)>> {
    let mut base = ungroup(root, native);
    while let ExprKind::Select { expression, .. } = &native.expression(base).kind {
        base = ungroup(*expression, native);
    }
    // An element of a container, which a property may hold, before the
    // property.
    if let Some((element, target, ..)) = containers::selected_element(root, tree, session, dims)? {
        return Ok(Some((element, target.element.clone())));
    }
    if let Some((property, _)) = objects::property(tree, session, base, native, dims)? {
        return Ok(Some((base, property.r#type)));
    }
    Ok(dimensions::operand_shape(base, tree, session, dims)?
        .as_ref()
        .and_then(type_names::declared_type)
        .cloned()
        .map(|ty| (base, ty)))
}

/// The type of assignment target `root`, of any form: the type that its
/// selection gives the member, variable, class property or container
/// element it selects ([`selected_type`]), which is the context of the
/// value assigned to it (IEEE 1800-2023 10.9, 11.5.1).
pub(in crate::ast) fn target_type(
    root: ExprId,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dims: &PackedDimensions,
) -> Converted<Option<Type>> {
    if let Some((_, ty)) = member_context(root, native, tree, session, dims)? {
        return Ok(Some(ty));
    }
    let Some((base, whole)) = target_base(root, native, tree, session, dims)? else {
        return Ok(None);
    };
    selected_type(root, base, whole, native, tree, session, dims).map(Some)
}

/// Whether a value of type `ty` holds a tagged union, at any depth.
fn holds_tagged(ty: &Type) -> bool {
    ty.members
        .iter()
        .any(|member| member.is_tagged() || holds_tagged(member.r#type()))
}

/// Whether assignment target `root`, of any form, is a whole tagged union,
/// whose tag an integral update would change with its bits (IEEE 1800-2023
/// 7.3.2, 11.9): its type is [`target_type`]'s.
pub(in crate::ast) fn targets_tagged_union(
    root: ExprId,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dims: &PackedDimensions,
) -> Converted<bool> {
    // Only a type that holds a tagged union has one to select.
    if !target_base(root, native, tree, session, dims)?
        .is_some_and(|(_, whole)| holds_tagged(&whole))
        && member_context(root, native, tree, session, dims)?.is_none()
    {
        return Ok(false);
    }
    Ok(
        target_type(root, native, tree, session, dims)?.is_some_and(|ty| {
            ty.members
                .first()
                .is_some_and(packed_structs::PackedMember::is_tagged)
        }),
    )
}

/// The type of target `authored`, a selection of `root` of type `whole`,
/// as the select typing of expressions gives it (IEEE 1800-2023 7.4.6,
/// 11.5.1): an index of a packed dimension selects the element type, and a
/// part-select keeps the dimensions after it, so `s.data[1:0]` of a
/// `logic [3:0][3:0] data` is `logic [1:0][3:0]`. The element of `whole`, an
/// enumeration or a structure, is kept with the dimensions that remain.
pub(in crate::ast) fn selected_type(
    authored: ExprId,
    root: ExprId,
    whole: Type,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dims: &PackedDimensions,
) -> Converted<Type> {
    use semantic_types::{PackedElement, TyData};
    if authored == root {
        return Ok(whole);
    }
    use semantic_types::ArrayKind;
    let selected = expression_types::type_of(tree, session, authored, native, dims)?;
    let complete = expression_types::type_of(tree, session, root, native, dims)?;
    // The unpacked dimensions of each, outermost first, and its element.
    let layers = |session: &Session, mut id| {
        let mut kinds = Vec::new();
        while let TyData::Array { kind, element } = session.types.data(id) {
            kinds.push(kind.clone());
            id = *element;
        }
        (kinds, session.types.data(id).clone())
    };
    let (complete_layers, complete) = layers(session, complete);
    let (selected_layers, selected) = layers(session, selected);
    let mut whole = whole;
    let consumed = complete_layers
        .len()
        .checked_sub(selected_layers.len())
        .filter(|consumed| *consumed <= whole.unpacked_ranges.len())
        .ok_or_else(|| unsupported("type of a selected target"))?;
    // A selection of unpacked dimensions has the element type of `whole`,
    // with the dimensions that remain, the first a slice's.
    if !selected_layers.is_empty() || !matches!(selected, TyData::Packed(_)) {
        whole.unpacked_ranges.drain(..consumed);
        if let (Some(ArrayKind::Fixed(range)), Some(first)) =
            (selected_layers.first(), whole.unpacked_ranges.first_mut())
        {
            *first = UnpackedRange::new(
                ConstExpr::Literal(range.left.to_string()),
                ConstExpr::Literal(range.right.to_string()),
            );
        }
        return Ok(whole);
    }
    let (TyData::Packed(complete), TyData::Packed(selected)) = (complete, selected) else {
        return Err(unsupported("type of a selected target"));
    };
    whole.unpacked_ranges.clear();
    let ranges = |dims: &[semantic_types::Range]| {
        dims.iter()
            .map(|range| {
                PackedRange::new(
                    ConstExpr::Literal(range.left.to_string()),
                    ConstExpr::Literal(range.right.to_string()),
                )
            })
            .collect::<Vec<_>>()
    };
    let mut ty = match selected.element {
        PackedElement::Bits => {
            let mut ty = Type::new(if selected.four_state {
                TypeKind::Logic
            } else {
                TypeKind::Bit
            });
            ty.packed_ranges = ranges(&selected.dims);
            ty
        }
        // The dimensions of `whole` precede those of its element.
        _ => {
            let dropped = complete
                .dims
                .len()
                .checked_sub(selected.dims.len())
                .filter(|dropped| *dropped <= whole.packed_ranges.len())
                .ok_or_else(|| unsupported("type of a selected target"))?;
            let mut ty = whole;
            let kept = ty.packed_ranges.len() - dropped;
            ty.packed_ranges = ranges(&selected.dims[..kept]);
            ty
        }
    };
    ty.is_signed = selected.signed;
    Ok(ty)
}

/// [`member_context`] of member path `root`, selected as `authored`.
fn path_context(
    authored: ExprId,
    root: ExprId,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dims: &PackedDimensions,
) -> Converted<Option<(Vec<Expr>, Type)>> {
    if !matches!(native.expression(root).kind, ExprKind::Member { .. }) {
        return Ok(None);
    }
    // A member of a class property is one of the property's value, read
    // through `OBJECT_VALUE` (see `object_place`).
    if let Some((base, property, _)) = property_member_base(root, native, tree, session, dims)? {
        let steps = value_selects::steps(root, base, native).ok_or_else(disagreement)?;
        let selection = value_selects::selection(
            OBJECT_VALUE,
            Expr::Ident(OBJECT_VALUE.into()),
            &property.r#type,
            &steps,
            native,
            tree,
            session,
            dims,
        )?;
        let mut r#type = property.r#type.clone();
        for step in &steps {
            let value_selects::Step::Member(name) = step else {
                return Ok(None);
            };
            let Some(member) = r#type.members.iter().find(|member| member.name() == name) else {
                return Ok(None);
            };
            r#type = member.r#type().clone();
        }
        let r#type = selected_type(authored, root, r#type, native, tree, session, dims)?;
        return Ok(Some((selection.guards, r#type)));
    }
    if objects::property(tree, session, root, native, dims)?.is_some() {
        return Ok(None);
    }
    if let Some(element) = element_members::element_member(root, native, tree, session, dims) {
        let (_, ty) = packed_structs::element_member(&element.name, &element.members, dims)?;
        let ty = if ty.unpacked_ranges.is_empty() {
            ty.clone()
        } else {
            dimensions::operand_shape(authored, tree, session, dims)?
                .and_then(|shape| shape.pattern_type(&tree.functions, dims))
                .ok_or_else(|| unsupported("structure array member type"))?
        };
        return Ok(Some((Vec::new(), ty)));
    }
    let (mut parts, qualified) = path(root, native, tree, session, dims)?;
    if let Some(name) = qualified {
        parts[0] = name;
    }
    let Some((guards, member)) = packed_structs::member_access(parts, dims, tree)? else {
        return Ok(None);
    };
    let member = selected_type(authored, root, member, native, tree, session, dims)?;
    Ok(Some((guards, member)))
}

fn cast_shape(
    target: ExprId,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dims: &PackedDimensions,
) -> Converted<Option<VariableDimensions>> {
    let ty = if let Some(name) = call_name(tree, target, native) {
        dims.type_aliases.get(&name).cloned()
    } else if let ExprKind::Type(id) = native.expression(target).kind {
        type_binding::binding::bind_in(id, native, tree, session, dims)?
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
    session: &mut Session,
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
        let Some(actual) = bound_patterns::native_value_type(root, tree, session, dims)? else {
            continue;
        };
        if actual.tagged_identity().is_some()
            && formal
                .and_then(|shape| shape.pattern_type(&tree.functions, dims))
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

/// A value of a call of a method of a built-in class whose objects the
/// scheduler owns, or `e.triggered`. Boxed and kept out of line, so that the
/// frame of the recursive binder holds one pointer for all of them.
#[inline(never)]
fn scheduler_call(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    id: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
) -> Converted<Option<Box<Expr>>> {
    if let Some(call) =
        super::super::processes::value(tree, session, id, native, dimensions, false)?
    {
        return Ok(Some(Box::new(call)));
    }
    if let Some(call) =
        super::super::semaphores::value(tree, session, id, native, dimensions, false)?
    {
        return Ok(Some(Box::new(call)));
    }
    if let Some(call) =
        super::super::mailboxes::value(tree, session, id, native, dimensions, false)?
    {
        return Ok(Some(Box::new(call)));
    }
    // `e.triggered` (IEEE 1800-2023 15.5.3).
    if let Some(event) = super::super::events::triggered(tree, session, id, native, dimensions)? {
        let event = super::value(event, native, tree, session, dimensions)?;
        return Ok(Some(Box::new(super::super::events::call(
            celox_design::ProcessOp::Triggered,
            vec![event],
        ))));
    }
    Ok(None)
}

/// The element of a container, or the member of one, that native
/// expression `root` selects bits or members of, with its type: `d[i]` of
/// `d[i][8*j+:8]`, `q[i].m` of `q[i].m[k]` when `q[i]` is an unpacked
/// structure, and `q[i]` of `q[i].m` or `q[i].m[k]` when it is a packed
/// one. The type is an integral `[N-1:0]` vector or a packed structure,
/// whose value the element holds: by value, or in a record when it is
/// wider than 64 bits.
fn selected_container_element(
    root: ExprId,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dimensions: &PackedDimensions,
) -> Converted<Option<(ExprId, Type)>> {
    // A member of an element of unpacked structure type, selected after
    // the member.
    let mut selected = ungroup(root, native);
    let mut selects = false;
    while let ExprKind::Select { expression, .. } = &native.expression(selected).kind {
        selects = true;
        selected = ungroup(*expression, native);
    }
    if selects
        && let Some((r#type, _)) =
            containers::element_member(selected, native, tree, session, dimensions)?
    {
        return Ok(zero_based_vector(&r#type, tree, dimensions).then_some((selected, r#type)));
    }
    // The selects and members of the variable, innermost first.
    let mut chain = Vec::new();
    let mut base = ungroup(root, native);
    loop {
        match &native.expression(base).kind {
            ExprKind::Select { expression, .. } => {
                chain.push((base, false));
                base = ungroup(*expression, native);
            }
            ExprKind::Member {
                expression,
                separator,
                ..
            } if separator == "." => {
                chain.push((base, true));
                base = ungroup(*expression, native);
            }
            _ => break,
        }
    }
    chain.reverse();
    let ExprKind::Name(name) = &native.expression(base).kind else {
        return Ok(None);
    };
    let Some(shape) = dimensions.get(&instances::normalize_identifier(&name.spelling)) else {
        return Ok(None);
    };
    let Some(mut container) = shape.container.as_deref() else {
        return Ok(None);
    };
    let mut levels = shape.unpacked.len() + 1;
    while let Some(inner) = container.element.container.as_deref() {
        container = inner;
        levels += 1;
    }
    if chain.len() <= levels {
        return Ok(None);
    }
    // Each container level is selected by an element index, `[i]` or the
    // last element `[$]`, not a slice.
    let indexed = chain[..levels].iter().all(|&(id, member)| {
        !member
            && matches!(&native.expression(id).kind, ExprKind::Select { dimension, .. }
            if matches!(dimension.kind, DimensionKind::Index(_) | DimensionKind::Queue(None)))
    });
    let element = &container.element;
    // A packed structure is a `[N-1:0]` vector with members; only it has
    // members to select.
    let structure = !element.members.is_empty()
        && element.unpacked_struct().is_none()
        && matches!(element.packed_ranges(), [range]
            if eval_ast_const_expr(&tree.functions, range.right(), &dimensions.const_env)
                == Some(0));
    let member = chain[levels..].iter().any(|&(_, member)| member);
    let selectable = structure || !member && zero_based_vector(element, tree, dimensions);
    Ok((indexed && selectable).then(|| (chain[levels - 1].0, element.clone())))
}

/// The class property of packed structure type that native member path
/// `root` selects a member of, `h.p` of `h.p.m` or `h.p.m[1:0]`, with the
/// receiver as [`objects::property`] returns it.
pub(in crate::ast) fn property_member_base(
    root: ExprId,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dimensions: &PackedDimensions,
) -> Converted<Option<(ExprId, classes::Property, Option<ExprId>)>> {
    let mut id = ungroup(root, native);
    loop {
        match &native.expression(id).kind {
            ExprKind::Select { expression, .. } => id = ungroup(*expression, native),
            ExprKind::Member {
                expression,
                separator,
                ..
            } if separator == "." => {
                let inner = ungroup(*expression, native);
                if let Some((property, receiver)) =
                    objects::property(tree, session, inner, native, dimensions)?
                {
                    let packed = property.field.handle.is_none()
                        && property.r#type.container.is_none()
                        && !property.r#type.members.is_empty()
                        && match property.r#type.packed_ranges() {
                            [range] => {
                                eval_ast_const_expr(
                                    &tree.functions,
                                    range.right(),
                                    &dimensions.const_env,
                                ) == Some(0)
                            }
                            _ => false,
                        };
                    return Ok(packed.then_some((inner, property, receiver)));
                }
                id = inner;
            }
            _ => return Ok(None),
        }
    }
}

fn zero_based_vector(
    r#type: &Type,
    tree: &AnalysisContext<'_>,
    dimensions: &PackedDimensions,
) -> bool {
    let zero_based = match r#type.packed_ranges() {
        [range] => {
            eval_ast_const_expr(&tree.functions, range.right(), &dimensions.const_env) == Some(0)
                && eval_ast_const_expr(&tree.functions, range.left(), &dimensions.const_env)
                    .is_some_and(|left| left >= 0)
        }
        _ => false,
    };
    zero_based && r#type.members.is_empty()
}

/// An actual of formal `position` of subroutine `name` is assigned to the
/// formal, and is therefore a chandle exactly when the formal is one (IEEE
/// 1800-2023 6.14).
fn check_chandle_argument(
    name: &str,
    position: usize,
    actual: ExprId,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    dimensions: &PackedDimensions,
    session: &mut Session,
) -> Converted<()> {
    let Some(formal) = dimensions
        .subroutine_param_shapes
        .get(name)
        .and_then(|shapes| shapes.get(position))
    else {
        return Ok(());
    };
    chandles::check_assignment(
        formal.chandle && formal.unpacked.is_empty(),
        actual,
        tree,
        native,
        dimensions,
        session,
    )
}

/// Check actual `actual` of formal `position` of subroutine `name`, which
/// the call assigns to the formal (IEEE 1800-2023 13.5): a chandle formal
/// takes only a chandle or `null`, an unpacked array formal an array of its
/// type, and a class handle input or inout a handle its class takes (8.16).
/// Every call binds its actuals after this check; `ordinal` numbers the
/// argument in diagnostics.
pub(in crate::ast) fn check_argument(
    name: &str,
    position: usize,
    ordinal: usize,
    actual: ExprId,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    dimensions: &PackedDimensions,
    session: &mut Session,
) -> Converted<()> {
    check_chandle_argument(name, position, actual, native, tree, dimensions, session)?;
    // An object holds a fixed-size array property in a container, to which
    // the formal of an output or inout argument is not copied back (see
    // `classes::Property::storage`, and `references::check_actual` for a
    // `ref` one).
    if dimensions
        .subroutine_directions
        .get(name)
        .and_then(|directions| directions.get(position))
        .is_some_and(|direction| {
            matches!(
                direction,
                crate::procedural::ParamDirection::Output
                    | crate::procedural::ParamDirection::Inout
            )
        })
        && let Some((property, _)) =
            object_property(tree, session, ungroup(actual, native), native, dimensions)?
        && property.storage.is_some()
    {
        return Err(unsupported(
            "fixed-size array class property as an output or inout argument",
        ));
    }
    if let Some(shape) = dimensions
        .subroutine_param_shapes
        .get(name)
        .and_then(|shapes| shapes.get(position))
    {
        array_compatibility::check_native_unpacked_assignment(
            tree,
            session,
            actual,
            shape,
            || format!("argument {ordinal} of `{name}`"),
            dimensions,
        )?;
        // A handle or enumerated value passes by its formal's direction
        // (IEEE 1800-2023 13.5): an input copies the actual in, an output
        // copies the formal out to the actual, an inout does both; a `ref`
        // binds an equivalent variable (see `references::check_actual`).
        if shape.unpacked.is_empty()
            && shape.container.is_none()
            && let Some(formal) =
                shape.type_id(&mut session.types, &tree.functions, &dimensions.const_env)
            && (session.types.is_enum(formal)
                || matches!(session.types.data(formal), semantic_types::TyData::Class(_)))
        {
            use crate::procedural::ParamDirection;
            let direction = dimensions
                .subroutine_directions
                .get(name)
                .and_then(|directions| directions.get(position))
                .copied();
            let context = || format!("argument {ordinal} of `{name}`");
            if matches!(
                direction,
                Some(ParamDirection::Input | ParamDirection::Inout)
            ) {
                assignments::check_assign(
                    tree, session, actual, native, dimensions, formal, context,
                )?;
            }
            if matches!(
                direction,
                Some(ParamDirection::Output | ParamDirection::Inout)
            ) && let Some(place) =
                assignments::place_type(tree, session, actual, native, dimensions)?
            {
                assignments::check_types(tree, session, dimensions, formal, place, &context)?;
            }
        }
    }
    Ok(())
}

/// Bind actual `actual` of formal `position` of subroutine `name`, which
/// may be a chandle exactly when the formal is one.
pub(in crate::ast) fn bind_argument(
    name: &str,
    position: usize,
    actual: ExprId,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    dimensions: &PackedDimensions,
    session: &mut Session,
) -> Converted<Expr> {
    check_argument(
        name, position, position, actual, native, tree, dimensions, session,
    )?;
    let target = copied_in_target(tree, session, dimensions, name, position);
    bind_value_in(actual, native, tree, dimensions, target, false, session)
}

/// The shape that actual `position` of subroutine `name` is bound for: a
/// chandle formal receives a chandle (6.14), a virtual interface input
/// only a virtual interface of its type (25.9) and a container input a
/// container of an equivalent element type (7.6).
pub(in crate::ast) fn copied_in_target<'a>(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dimensions: &'a PackedDimensions,
    name: &str,
    position: usize,
) -> Option<&'a VariableDimensions> {
    dimensions
        .subroutine_param_shapes
        .get(name)
        .and_then(|shapes| shapes.get(position))
        .filter(|shape| {
            chandles::receives_chandle(shape)
                || (shape.container.is_some()
                    || virtual_interfaces::holds_virtual_interface(
                        shape, tree, session, dimensions,
                    ))
                    && copied_in(dimensions, name, position)
        })
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
