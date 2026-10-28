//! Preliminary query metadata follows native lexical ownership, never an old
//! expression/statement pairing. Reentrant range queries see completed prefixes.
use super::*;
use celox_sv_syntax::{
    BodyItemId, BodyItemKind, DataTypeKind, ExprId, LocalDeclaration, ParameterKind, StatementId,
    StatementKind, SubroutineId, SubroutineKind, TokenRange, TypeAliasTarget,
};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Key {
    // Arena incarnation only disambiguates temporary reentrancy caches. Native
    // scope/body handles below select semantic ownership; this address never does.
    arena: usize,
    scope: crate::source_tree::ScopeHandle,
    path: Vec<BodyItemId>,
}
#[derive(Default)]
pub(in crate::ast) struct State {
    partial: RefCell<HashMap<Key, PackedDimensions>>,
    /// The class method signatures that discovery last published in each
    /// scope, by arena and scope.
    class_signatures:
        RefCell<HashMap<(usize, crate::source_tree::ScopeHandle), std::rc::Rc<ClassSignatures>>>,
}

/// The signature entries that discovering a scope's own declarations added
/// or changed, and the tables of the context they were bound in, held so
/// that their identities stay unique.
struct ClassSignatures {
    const_env: SharedMap<i128>,
    type_aliases: SharedMap<Type>,
    function_return_types: SharedMap<FunctionReturnMetadata>,
    specializations: usize,
    shapes: Vec<(String, Vec<VariableDimensions>)>,
    names: Vec<(String, Vec<String>)>,
    returns: Vec<(String, FunctionReturnMetadata)>,
}
struct Guard<'a>(&'a State, Key);
impl Drop for Guard<'_> {
    fn drop(&mut self) {
        self.0.partial.borrow_mut().remove(&self.1);
    }
}
fn publish(state: &State, key: &Key, context: &PackedDimensions) {
    state
        .partial
        .borrow_mut()
        .insert(key.clone(), context.clone());
}
fn contains(outer: TokenRange, inner: TokenRange) -> bool {
    outer.start <= inner.start && inner.end <= outer.end
}
fn name(name: &celox_sv_syntax::Name) -> String {
    instances::normalize_identifier(&name.spelling)
}

struct Owners {
    frames: Vec<Vec<BodyItemId>>,
    path: Vec<BodyItemId>,
    subroutine: Option<SubroutineId>,
    statements: Vec<StatementId>,
}
fn owners(tree: &AnalysisContext<'_>, target: TokenRange) -> Converted<Owners> {
    let native = tree.declarations();
    let root = tree.body_membership()?;
    let mut result = Owners {
        frames: vec![root.clone()],
        path: Vec::new(),
        subroutine: None,
        statements: Vec::new(),
    };
    let mut pending = root.into_iter().rev().collect::<Vec<_>>();
    let mut seen = HashSet::default();
    while let Some(id) = pending.pop() {
        if !seen.insert(id) {
            return Err(AnalyzerError::Parse(
                "duplicate native query scope ownership".into(),
            ));
        }
        let item = native.body_item(id);
        if !contains(item.tokens, target) {
            continue;
        }
        match &item.kind {
            BodyItemKind::Region(items) => pending.extend(items.iter().rev()),
            BodyItemKind::Block { items, .. } | BodyItemKind::Unconverted { items, .. } => {
                result.path.push(id);
                result.frames.push(items.clone());
                pending.extend(items.iter().rev());
            }
            BodyItemKind::Conditional {
                then_body,
                otherwise,
                ..
            } => {
                for child in std::iter::once(then_body).chain(otherwise) {
                    if contains(native.body_item(*child).tokens, target) {
                        result.path.push(id);
                        result.frames.push(vec![*child]);
                        pending.push(*child);
                    }
                }
            }
            BodyItemKind::Case { arms, .. } => {
                for arm in arms {
                    if contains(native.body_item(arm.body).tokens, target) {
                        result.path.push(id);
                        result.frames.push(vec![arm.body]);
                        pending.push(arm.body);
                    }
                }
            }
            BodyItemKind::Loop { body, .. } => {
                if contains(native.body_item(*body).tokens, target) {
                    result.path.push(id);
                    result.frames.push(vec![*body]);
                    pending.push(*body);
                }
            }
            BodyItemKind::Subroutine(id) => {
                result.subroutine = Some(*id);
                result.statements = native.subroutine(*id).items.clone();
            }
            BodyItemKind::Procedure(id) => result.statements = vec![native.procedure(*id).body],
            BodyItemKind::Unit(_) => {
                return Err(AnalyzerError::Parse(
                    "query belongs to another native unit scope".into(),
                ));
            }
            _ => {}
        }
    }
    Ok(result)
}
fn direct(ids: &[BodyItemId], tree: &AnalysisContext<'_>) -> Converted<Vec<BodyItemId>> {
    let mut result = Vec::new();
    let mut pending = ids.iter().rev().copied().collect::<Vec<_>>();
    let mut seen = HashSet::default();
    while let Some(id) = pending.pop() {
        if !seen.insert(id) {
            return Err(AnalyzerError::Parse(
                "duplicate native declaration ownership".into(),
            ));
        }
        match &tree.declarations().body_item(id).kind {
            BodyItemKind::Region(items) => pending.extend(items.iter().rev()),
            _ => result.push(id),
        }
    }
    Ok(result)
}
fn type_with_dimensions(
    ty: Type,
    dimensions: &[celox_sv_syntax::Dimension],
    tree: &AnalysisContext<'_>,
    context: &PackedDimensions,
) -> Converted<Type> {
    containers::with_dimensions(ty, dimensions, tree, context)
}
fn shape(context: &mut PackedDimensions, name: String, ty: &Type, local: bool) {
    let value = procedural::dimensions_from_type(ty);
    if local {
        extend_query_local_dimensions(context, [(name, value)]);
    } else {
        context.insert(name, value);
    }
}
// Binding the declaration's type/ranges may reenter discovery and needs the
// completed prefix. The following table edit itself performs no queries. Drop
// only our own old snapshot during that edit, so ScopedMap need not copy its
// growing overlay on every declaration, then publish before the next query.
fn publish_shape(
    tree: &AnalysisContext<'_>,
    key: &Key,
    context: &mut PackedDimensions,
    name: String,
    ty: &Type,
    local: bool,
) {
    tree.state.dimensions.partial.borrow_mut().remove(key);
    shape(context, name, ty, local);
    publish(&tree.state.dimensions, key, context);
}
fn variables(
    declaration: &celox_sv_syntax::VariableDeclaration,
    tree: &AnalysisContext<'_>,
    context: &mut PackedDimensions,
    key: &Key,
    local: bool,
) -> Converted<()> {
    let Some(base) = type_binding::bind(declaration.data_type, tree, context)? else {
        return Ok(());
    };
    for variable in declaration.variables.iter().filter(|variable| {
        type_binding::ordinary_variable(variable, declaration.data_type, tree.declarations())
    }) {
        let ty = type_with_dimensions(base.clone(), &variable.dimensions, tree, context)?;
        publish_shape(tree, key, context, name(&variable.name), &ty, local);
    }
    Ok(())
}
fn parameter(
    parameter: &celox_sv_syntax::Parameter,
    tree: &AnalysisContext<'_>,
    context: &mut PackedDimensions,
    key: &Key,
    local: bool,
) -> Converted<()> {
    let n = name(&parameter.name);
    match &parameter.kind {
        ParameterKind::Type { default: Some(id) } => {
            if !context.type_aliases.contains_key(&n)
                && let Some(ty) = type_binding::bind(*id, tree, context)?
            {
                context.type_aliases.insert(n, ty);
            }
        }
        ParameterKind::Value {
            data_type,
            dimensions,
            default,
        } => {
            if local
                && let Some(value) = default
                    .and_then(|id| constant_binding::bind(id, tree, context).ok().flatten())
                    .and_then(|value| eval_ast_const_expr(&tree.state, &value, &context.const_env))
            {
                context.const_env.insert(n.clone(), value);
            }
            // Elaborated bindings may shadow this authored parameter (notably
            // an implicit genvar localparam). Its scalar type markers then own
            // the type; restoring the outer declaration would lose that scope.
            let scalar_binding = !local
                && context.const_env.contains_key(&parameter_width_marker(&n))
                && !context
                    .const_env
                    .contains_key(&parameter_dimensions_marker(&n));
            if !scalar_binding && let Some(base) = type_binding::bind(*data_type, tree, context)? {
                let ty = type_with_dimensions(base, dimensions, tree, context)?;
                if !dimensions.is_empty() || !ty.packed_ranges().is_empty() {
                    publish_shape(tree, key, context, n, &ty, local);
                }
            }
        }
        _ => {}
    }
    publish(&tree.state.dimensions, key, context);
    Ok(())
}
fn declaration(
    id: BodyItemId,
    tree: &AnalysisContext<'_>,
    context: &mut PackedDimensions,
    key: &Key,
) -> Converted<()> {
    let native = tree.declarations();
    match &native.body_item(id).kind {
        BodyItemKind::Data(id) => {
            if let celox_sv_syntax::DataDeclaration::Variables(data)
            | celox_sv_syntax::DataDeclaration::Constructed(data) = native.data_declaration(*id)
            {
                variables(data, tree, context, key, false)?;
            }
        }
        BodyItemKind::Net(id) => {
            let data = native.net_declaration(*id);
            if let Some(base) = type_binding::bind(data.data_type, tree, context)? {
                for variable in &data.variables {
                    let ty =
                        type_with_dimensions(base.clone(), &variable.dimensions, tree, context)?;
                    publish_shape(tree, key, context, name(&variable.name), &ty, false);
                }
            }
        }
        BodyItemKind::Parameter(id) => {
            for item in &native.parameter_declaration(*id).parameters {
                parameter(item, tree, context, key, false)?;
            }
        }
        BodyItemKind::TypeAlias(id) => {
            let alias = native.type_alias(*id);
            if let TypeAliasTarget::DataType(id) = alias.target
                && let Some(base) = type_binding::bind(id, tree, context)?
            {
                let ty = type_with_dimensions(base, &alias.dimensions, tree, context)?;
                context.type_aliases.insert(name(&alias.name), ty);
                publish(&tree.state.dimensions, key, context);
            }
        }
        _ => {}
    }
    Ok(())
}
fn formal_types(
    subroutine: &celox_sv_syntax::Subroutine,
    tree: &AnalysisContext<'_>,
    context: &PackedDimensions,
) -> Converted<Vec<(FunctionArgumentMetadata, Type)>> {
    let mut result = Vec::new();
    let mut local = context.clone();
    let mut direction = ParamDirection::Input;
    let mut previous: Option<Type> = None;
    let groups = subroutine.ports.as_deref().into_iter().chain(
        subroutine
            .port_declarations
            .iter()
            .map(|group| group.ports.as_slice()),
    );
    for ports in groups {
        for port in ports {
            if let Some(d) = port.direction {
                if port.static_reference {
                    return Err(unsupported("ref static subroutine argument"));
                }
                direction = ParamDirection::declared(d, port.constant);
            }
            let data = tree.declarations().data_type(port.data_type);
            let omitted = matches!(data.kind, DataTypeKind::Implicit)
                && data.signed.is_none()
                && data.packed_dimensions.is_empty();
            let mut base = if port.direction.is_none() && omitted && previous.is_some() {
                previous.clone().unwrap()
            } else {
                let Some(mut ty) = type_binding::bind(port.data_type, tree, &local)? else {
                    return Err(unsupported("unsupported function formal data type"));
                };
                if matches!(data.kind, DataTypeKind::Implicit) {
                    ty.kind = TypeKind::Logic;
                }
                ty
            };
            previous = Some(base.clone());
            base = type_with_dimensions(base, &port.dimensions, tree, &local)?;
            let n = port
                .name
                .as_ref()
                .map(name)
                .ok_or_else(|| unsupported("subroutine argument without a name"))?;
            shape(&mut local, n.clone(), &base, true);
            result.push((
                FunctionArgumentMetadata {
                    name: n,
                    direction,
                    has_default: port.default.is_some(),
                },
                base,
            ));
        }
    }
    Ok(result)
}
fn return_metadata(
    state: &AnalysisState,
    ty: Option<&Type>,
    arguments: Arc<[FunctionArgumentMetadata]>,
    context: &PackedDimensions,
) -> FunctionReturnMetadata {
    let widths = ty.map(|ty| {
        ty.packed_ranges()
            .iter()
            .map(|range| {
                usize::try_from(
                    eval_ast_const_expr(state, range.left(), &context.const_env)?.abs_diff(
                        eval_ast_const_expr(state, range.right(), &context.const_env)?,
                    ),
                )
                .ok()?
                .checked_add(1)
            })
            .collect::<Option<Vec<_>>>()
    });
    let width = ty
        .and_then(|ty| expr_type_from_type(state, ty, &context.const_env))
        .map(|ty| ty.width);
    FunctionReturnMetadata {
        arguments: Some(arguments),
        width,
        first_packed_dimension_width: widths
            .as_ref()
            .and_then(|widths| widths.as_ref())
            .and_then(|widths| widths.first().copied())
            .or(width),
        dimensions: ty.map(|ty| {
            ty.unpacked_ranges().len()
                + if matches!(ty.kind(), TypeKind::Real | TypeKind::Shortreal) {
                    0
                } else {
                    ty.packed_ranges().len()
                }
        }),
        signed: ty.is_some_and(Type::is_signed),
        is_2state: ty.is_some_and(|ty| ty.kind() == TypeKind::Bit),
        string: ty.is_some_and(|ty| ty.kind() == TypeKind::String),
        pattern_type: ty
            .filter(|ty| {
                !ty.members.is_empty()
                    || !ty.unpacked_ranges.is_empty()
                    || ty.packed_ranges.len() > 1
            })
            .and_then(|ty| {
                ty.pattern_target(&const_functions::Environment {
                    constants: &*context.const_env,
                    functions: &state.const_functions,
                })
            }),
    }
}
fn signature(
    id: SubroutineId,
    tree: &AnalysisContext<'_>,
    context: &mut PackedDimensions,
) -> Converted<()> {
    let subroutine = tree.declarations().subroutine(id);
    let formals = formal_types(subroutine, tree, context)?;
    let result = return_type(subroutine, tree, context)?;
    publish_signature(name(&subroutine.name), formals, result, tree, context);
    Ok(())
}
/// The return type of a function, `None` for a task.
fn return_type(
    subroutine: &celox_sv_syntax::Subroutine,
    tree: &AnalysisContext<'_>,
    context: &PackedDimensions,
) -> Converted<Option<Option<Type>>> {
    if subroutine.kind != SubroutineKind::Function {
        return Ok(None);
    }
    let mut ty = subroutine
        .return_type
        .map(|id| type_binding::bind(id, tree, context))
        .transpose()?
        .flatten();
    if subroutine.return_type.is_some_and(|id| {
        matches!(
            tree.declarations().data_type(id).kind,
            DataTypeKind::Implicit
        )
    }) && let Some(ty) = &mut ty
    {
        ty.kind = TypeKind::Logic;
    }
    Ok(Some(ty))
}
/// The methods of `class` as subroutines `class::method`: a method that is
/// not static takes the handle `this` before its formals, and the
/// constructor, declared or implicit, returns `this` once it has
/// initialized it (IEEE 1800-2023 8.6, 8.7, 8.10).
fn class_signatures(
    class: &celox_sv_syntax::ClassDeclaration,
    class_name: &str,
    tree: &AnalysisContext<'_>,
    context: &mut PackedDimensions,
) -> Converted<()> {
    let Some(handle) = classes::handle_type(tree, class_name) else {
        return Ok(());
    };
    // Formals and results are bound in the scope of the specialization.
    let mut scope = context.clone();
    let (env, aliases) = classes::specializations::shared_scope(
        tree,
        class_name,
        &context.const_env,
        &context.type_aliases,
    );
    scope.const_env = env;
    scope.type_aliases = aliases;
    let this = (
        FunctionArgumentMetadata {
            name: classes::THIS.into(),
            direction: ParamDirection::Input,
            has_default: false,
        },
        handle.clone(),
    );
    let mut constructor = false;
    for method in &class.methods {
        let subroutine = tree.declarations().subroutine(method.subroutine);
        let mut formals = formal_types(subroutine, tree, &scope)?;
        if !method.qualifiers.is_static {
            formals.insert(0, this.clone());
        }
        let result = if subroutine.kind == SubroutineKind::Constructor {
            constructor = true;
            Some(Some(handle.clone()))
        } else {
            return_type(subroutine, tree, &scope)?
        };
        // A method that may be virtual also has a dispatcher (8.20).
        if !method.qualifiers.is_static && subroutine.kind != SubroutineKind::Constructor {
            publish_signature(
                classes::Class::dispatcher_name(class_name, &name(&subroutine.name)),
                formals.clone(),
                result.clone(),
                tree,
                context,
            );
        }
        publish_signature(
            classes::Class::subroutine_name(class_name, &name(&subroutine.name)),
            formals,
            result,
            tree,
            context,
        );
    }
    for method in ["randomize", "srandom", "rand_mode"] {
        let mut formals = vec![(
            FunctionArgumentMetadata {
                name: classes::THIS.into(),
                direction: ParamDirection::Input,
                has_default: false,
            },
            handle.clone(),
        )];
        let (argument, ty) = match method {
            "randomize" => (
                "$selection",
                // Signature discovery must not require an otherwise unused class
                // to have a supported layout. A call validates the layout before
                // using this selection formal; lowering reports layout errors.
                classes::class(tree, class_name, &context.const_env, &context.type_aliases)
                    .map_or_else(
                        |_| classes::unsigned_type(1),
                        |layout| layout.selection_type(),
                    ),
            ),
            "srandom" => ("seed", procedural::int_type()),
            _ => ("on_off", classes::unsigned_type(1)),
        };
        formals.push((
            FunctionArgumentMetadata {
                name: argument.into(),
                direction: ParamDirection::Input,
                has_default: false,
            },
            ty,
        ));
        publish_signature(
            classes::Class::subroutine_name(class_name, method),
            formals,
            Some((method == "randomize").then(procedural::int_type)),
            tree,
            context,
        );
    }
    if !constructor && !class.interface {
        publish_signature(
            classes::Class::subroutine_name(class_name, "new"),
            vec![this],
            Some(Some(handle.clone())),
            tree,
            context,
        );
    }
    // `$cast` to a variable of the class (8.16).
    let formal = |name: &str, direction| {
        (
            FunctionArgumentMetadata {
                name: name.into(),
                direction,
                has_default: false,
            },
            handle.clone(),
        )
    };
    publish_signature(
        classes::Class::cast_name(class_name),
        vec![
            formal(procedural::CAST_DESTINATION, ParamDirection::Inout),
            formal(procedural::CAST_SOURCE, ParamDirection::Input),
        ],
        Some(Some(procedural::int_type())),
        tree,
        context,
    );
    Ok(())
}
/// Publish the signatures of the methods of every class the current scope
/// declares, of each specialization of a parameterized class that the scope
/// names (IEEE 1800-2023 8.25).
pub(in crate::ast) fn all_class_signatures(
    tree: &AnalysisContext<'_>,
    context: &mut PackedDimensions,
) {
    classes::specializations::collect(tree, context);
    // The classes of other scopes keep the signatures their scopes publish.
    for class in classes::scope_declarations(tree) {
        for class_name in classes::specializations_of(tree, class) {
            let _ = class_signatures(class, &class_name, tree, context);
        }
    }
    tree.state.class_specializations_frozen.set(true);
}
/// The signatures of the classes of the current scope and of its other
/// subroutines, bound once from `base` and the scope's own declarations
/// `scope`, without those around a query, and kept while `base` and the set
/// of specializations stay the same. Each query and each of its discovery
/// passes publishes them again instead of binding every class again.
fn scope_class_signatures(
    tree: &AnalysisContext<'_>,
    scope: &[BodyItemId],
    key: &Key,
    base: &PackedDimensions,
) -> Converted<std::rc::Rc<ClassSignatures>> {
    let cache_key = (
        tree.declarations() as *const _ as usize,
        tree.scope_handle(),
    );
    let specializations = tree.state.class_specializations.borrow().len();
    if let Some(cached) = tree
        .state
        .dimensions
        .class_signatures
        .borrow()
        .get(&cache_key)
        .filter(|cached| {
            cached.specializations == specializations
                && (cached.const_env.identity() == base.const_env.identity()
                    || *cached.const_env == *base.const_env)
                && (cached.type_aliases.identity() == base.type_aliases.identity()
                    || *cached.type_aliases == *base.type_aliases)
                && (cached.function_return_types.identity()
                    == base.function_return_types.identity()
                    || *cached.function_return_types == *base.function_return_types)
        })
    {
        return Ok(cached.clone());
    }
    #[cfg(test)]
    super::CLASS_SIGNATURE_BINDINGS.with(|count| count.set(count.get() + 1));
    let mut bound = base.clone();
    settle(&[scope.to_vec()], tree, key, &mut bound, Classes::Bind)?;
    // Only the methods of the scope's classes, `class::method`: the scope's
    // other subroutines are discovered with each query, where a generate
    // block may hide them.
    let classes = classes::scope_declarations(tree)
        .flat_map(|class| classes::specializations_of(tree, class))
        .collect::<HashSet<_>>();
    fn changed<V: Clone + PartialEq>(
        classes: &HashSet<String>,
        before: &HashMap<String, V>,
        after: &HashMap<String, V>,
    ) -> Vec<(String, V)> {
        after
            .iter()
            .filter(|(name, value)| {
                name.rsplit_once("::")
                    .is_some_and(|(owner, _)| classes.contains(owner))
                    && before.get(*name) != Some(*value)
            })
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect()
    }
    let signatures = std::rc::Rc::new(ClassSignatures {
        const_env: base.const_env.clone(),
        type_aliases: base.type_aliases.clone(),
        function_return_types: base.function_return_types.clone(),
        specializations: tree.state.class_specializations.borrow().len(),
        shapes: changed(
            &classes,
            &base.subroutine_param_shapes,
            &bound.subroutine_param_shapes,
        ),
        names: changed(
            &classes,
            &base.subroutine_param_names,
            &bound.subroutine_param_names,
        ),
        returns: changed(
            &classes,
            &base.function_return_types,
            &bound.function_return_types,
        ),
    });
    tree.state
        .dimensions
        .class_signatures
        .borrow_mut()
        .insert(cache_key, signatures.clone());
    Ok(signatures)
}

impl ClassSignatures {
    fn publish(&self, tree: &AnalysisContext<'_>, context: &mut PackedDimensions) {
        if !self.shapes.is_empty() {
            Arc::make_mut(&mut context.subroutine_param_shapes).extend(self.shapes.iter().cloned());
        }
        if !self.names.is_empty() {
            Arc::make_mut(&mut context.subroutine_param_names).extend(self.names.iter().cloned());
        }
        if !self.returns.is_empty() {
            context
                .function_return_types
                .extend(self.returns.iter().cloned());
        }
        tree.state.class_specializations_frozen.set(true);
    }
}

/// Discover the declarations of `frames` in `context` until they settle,
/// with the class signatures `classes` gives.
fn settle(
    frames: &[Vec<BodyItemId>],
    tree: &AnalysisContext<'_>,
    key: &Key,
    context: &mut PackedDimensions,
    classes: Classes<'_>,
) -> Converted<()> {
    // Resolve forward return-range dependencies using the same published prefix
    // as value declarations. Each pass publishes useful metadata before queries
    // in subsequent ranges recurse; no native call stack follows dependency depth.
    let native = tree.declarations();
    let declarations = frames.iter().map(Vec::len).sum::<usize>();
    for _ in 0..=declarations {
        let before = context.clone();
        for frame in frames {
            for id in frame {
                if let BodyItemKind::Subroutine(id) = native.body_item(*id).kind
                    && native.subroutine(id).scope.is_empty()
                {
                    let _ = signature(id, tree, context);
                    publish(&tree.state.dimensions, key, context);
                } else if let BodyItemKind::Dpi(id) = native.body_item(*id).kind
                    && let Ok(import) = dpi::import(id, native)
                    && let Some(ty) = import.return_type()
                {
                    context.function_return_types.insert(
                        import.name().to_string(),
                        FunctionReturnMetadata {
                            arguments: Some(dpi_argument_metadata(&import)),
                            width: Some(ty.width()),
                            first_packed_dimension_width: (ty.width() > 1).then_some(ty.width()),
                            dimensions: Some(usize::from(ty.width() > 1)),
                            signed: ty.is_signed(),
                            is_2state: !ty.is_4state(),
                            string: false,
                            pattern_type: None,
                        },
                    );
                    publish(&tree.state.dimensions, key, context);
                }
            }
        }
        match classes {
            Classes::Bind => all_class_signatures(tree, context),
            Classes::Published(signatures) => signatures.publish(tree, context),
        }
        publish(&tree.state.dimensions, key, context);
        if let Ok(unit) = tree.root_unit() {
            if let Some(parameters) = &unit.parameters {
                for parameter_node in parameters {
                    let _ = parameter(parameter_node, tree, context, key, false);
                }
            }
            for index in tree.header_port_ids()? {
                let port = &unit.ports[index];
                if let Some(id) = port.data_type
                    && let Ok(Some(base)) = type_binding::bind(id, tree, context)
                    && let Ok(ty) = type_with_dimensions(base, &port.dimensions, tree, context)
                    && let celox_sv_syntax::PortKind::Named(n) = &port.kind
                {
                    publish_shape(tree, key, context, name(n), &ty, false);
                }
            }
        }
        for frame in frames {
            for id in frame {
                let _ = declaration(*id, tree, context, key);
            }
        }
        if *context == before {
            break;
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum Classes<'a> {
    /// Bind the signatures of the scope's classes in each pass.
    Bind,
    /// Publish signatures bound before.
    Published(&'a ClassSignatures),
}

fn publish_signature(
    n: String,
    formals: Vec<(FunctionArgumentMetadata, Type)>,
    result: Option<Option<Type>>,
    tree: &AnalysisContext<'_>,
    context: &mut PackedDimensions,
) {
    let arguments = formals
        .iter()
        .map(|(argument, _)| argument.clone())
        .collect::<Arc<[_]>>();
    Arc::make_mut(&mut context.subroutine_param_shapes).insert(
        n.clone(),
        formals
            .iter()
            .map(|(_, ty)| procedural::dimensions_from_type(ty))
            .collect(),
    );
    Arc::make_mut(&mut context.subroutine_param_names).insert(
        n.clone(),
        formals
            .iter()
            .map(|(argument, _)| argument.name.clone())
            .collect(),
    );
    if let Some(ty) = result {
        let metadata = return_metadata(&tree.state, ty.as_ref(), arguments, context);
        context.function_return_types.insert(n, metadata);
    }
}
fn local_declaration(
    item: &LocalDeclaration,
    tree: &AnalysisContext<'_>,
    context: &mut PackedDimensions,
    key: &Key,
) -> Converted<()> {
    match item {
        LocalDeclaration::Variables(data) | LocalDeclaration::Constructed(data) => {
            variables(data, tree, context, key, true)?
        }
        LocalDeclaration::Parameters(id) => {
            for parameter_node in &tree.declarations().parameter_declaration(*id).parameters {
                parameter(parameter_node, tree, context, key, true)?;
            }
        }
        LocalDeclaration::TypeAlias(id) => {
            let alias = tree.declarations().type_alias(*id);
            if let TypeAliasTarget::DataType(id) = alias.target
                && let Some(ty) = type_binding::bind(id, tree, context)?
            {
                let ty = type_with_dimensions(ty, &alias.dimensions, tree, context)?;
                context.type_aliases.insert(name(&alias.name), ty);
            }
        }
        _ => {}
    }
    Ok(())
}
fn statement_scope(
    ids: &[StatementId],
    target: TokenRange,
    tree: &AnalysisContext<'_>,
    context: &mut PackedDimensions,
    key: &Key,
) -> Converted<()> {
    enum Lexical {
        Statements(Vec<StatementId>),
        For(Vec<celox_sv_syntax::ForVariableDeclaration>),
        Foreach(Vec<celox_sv_syntax::Name>),
    }
    let mut scopes = vec![Lexical::Statements(ids.to_vec())];
    let mut pending = ids.iter().rev().copied().collect::<Vec<_>>();
    let mut seen = HashSet::default();
    while let Some(id) = pending.pop() {
        if !seen.insert(id) {
            return Err(AnalyzerError::Parse(
                "duplicate native query statement ownership".into(),
            ));
        }
        let statement = tree.declarations().statement(id);
        if !contains(statement.tokens, target) {
            continue;
        }
        match &statement.kind {
            StatementKind::Block { items, .. } | StatementKind::Parallel { items, .. } => {
                scopes.push(Lexical::Statements(items.clone()))
            }
            StatementKind::Loop {
                kind: celox_sv_syntax::LoopSyntax::For(header),
                ..
            } => {
                if let Some(celox_sv_syntax::ForInitialization::Declarations(declarations)) =
                    &header.initialization
                {
                    scopes.push(Lexical::For(declarations.clone()));
                }
            }
            StatementKind::Loop {
                kind: celox_sv_syntax::LoopSyntax::Foreach(header),
                ..
            } => scopes.push(Lexical::Foreach(
                header.variables.iter().flatten().cloned().collect(),
            )),
            _ => {}
        }
        pending.extend(statement.kind.children());
    }
    for lexical in scopes {
        match lexical {
            Lexical::Statements(items) => {
                for id in items {
                    if let StatementKind::Declaration(item) =
                        &tree.declarations().statement(id).kind
                    {
                        local_declaration(item, tree, context, key)?;
                        publish(&tree.state.dimensions, key, context);
                    }
                }
            }
            Lexical::For(declarations) => {
                for declaration in declarations {
                    if let Some(ty) = type_binding::bind(declaration.data_type, tree, context)? {
                        for variable in declaration.variables {
                            publish_shape(tree, key, context, name(&variable.name), &ty, true);
                        }
                    }
                }
            }
            Lexical::Foreach(names) => {
                let ty = type_binding::builtin_type("int", None)
                    .ok_or_else(|| unsupported("foreach index type"))?;
                for n in names {
                    publish_shape(tree, key, context, name(&n), &ty, true);
                }
            }
        }
    }
    Ok(())
}

fn overlay(context: &mut PackedDimensions, input: &PackedDimensions) {
    context.extend(
        input
            .iter()
            .map(|(name, shape)| (name.clone(), shape.clone())),
    );
    context.const_env.extend(
        input
            .const_env
            .iter()
            .map(|(name, value)| (name.clone(), *value)),
    );
    context.type_aliases.extend(input.type_aliases.clone());
    context
        .function_return_types
        .extend(input.function_return_types.clone());
    context.parameter_values = input.parameter_values.clone();
}
fn finish(mut context: PackedDimensions) -> PackedDimensions {
    let signedness = context
        .iter()
        .map(|(name, shape)| (name.clone(), shape.signed))
        .collect::<Vec<_>>();
    context.expression_signedness.extend(signedness);
    context.functions = Arc::default();
    context.type_query = true;
    context
}
/// Preliminary array/function query context for one authored native expression.
pub(in crate::ast) fn query_operand_dimensions_id(
    root: ExprId,
    tree: &AnalysisContext<'_>,
    input: &PackedDimensions,
) -> Converted<PackedDimensions> {
    if input.scope_types_complete {
        return Ok(finish(input.clone()));
    }
    #[cfg(test)]
    SYNTAX_TYPE_DISCOVERIES.with(|count| count.set(count.get() + 1));
    let native = tree.declarations();
    let target = native.expression(root).tokens;
    let owners = owners(tree, target)?;
    let key = Key {
        arena: native as *const _ as usize,
        scope: tree.scope_handle(),
        path: owners.path,
    };
    if let Some(mut partial) = tree.state.dimensions.partial.borrow().get(&key).cloned() {
        overlay(&mut partial, input);
        return Ok(finish(partial));
    }
    let mut context = input.clone();
    for (name, metadata) in &scope::imported(&tree.state).function_return_types {
        context
            .function_return_types
            .entry(name.clone())
            .or_insert_with(|| metadata.clone());
    }
    extend_interface_dimensions(&tree.state, &mut context);
    extend_variable_dimensions_from_const_env(&mut context, &input.const_env);
    publish(&tree.state.dimensions, &key, &context);
    let _guard = Guard(&tree.state.dimensions, key.clone());
    let frames = owners
        .frames
        .iter()
        .map(|items| direct(items, tree))
        .collect::<Converted<Vec<_>>>()?;
    // A class's signatures are bound in the scope that declares it, from
    // that scope's declarations alone, once for all of its queries.
    let base = context.clone();
    // Without classes of its own, the scope has no signatures to bind once.
    if classes::scope_declarations(tree).next().is_none() {
        settle(&frames, tree, &key, &mut context, Classes::Bind)?;
    } else {
        let classes = scope_class_signatures(tree, &frames[0], &key, &base)?;
        settle(
            &frames,
            tree,
            &key,
            &mut context,
            Classes::Published(&classes),
        )?;
    }
    overlay(&mut context, input);
    if let Some(id) = owners.subroutine {
        let subroutine = native.subroutine(id);
        for (argument, ty) in formal_types(subroutine, tree, &context)? {
            publish_shape(tree, &key, &mut context, argument.name, &ty, true);
        }
        if contains(subroutine.body_tokens, target)
            && subroutine.kind == SubroutineKind::Function
            && let Some(id) = subroutine.return_type
            && let Some(mut ty) = type_binding::bind(id, tree, &context)?
        {
            if matches!(native.data_type(id).kind, DataTypeKind::Implicit) {
                ty.kind = TypeKind::Logic;
            }
            publish_shape(tree, &key, &mut context, name(&subroutine.name), &ty, true);
        }
    }
    statement_scope(&owners.statements, target, tree, &mut context, &key)?;
    // A hierarchical operand can name a declaration in an active generate
    // branch. Use the same explicit specialization as normal elaboration;
    // typed wide/four-state overrides must remain parameter literals.
    if let Some((wanted, _)) = type_binding::argument_alias(root, native)
        && wanted.contains('.')
        && context.get(&wanted).is_none()
    {
        for item in generate::items(tree, &context.const_env, &context.type_aliases)? {
            if !matches!(
                native.body_item(item.id).kind,
                BodyItemKind::Data(_) | BodyItemKind::Net(_)
            ) {
                continue;
            }
            let declared = imports::item_names(item.id, tree)
                .into_iter()
                .find(|name| item.name(name) == wanted);
            let Some(declared) = declared else {
                continue;
            };
            let _functions = item.enter();
            let _interface_scope = item.interface_scope();
            let mut signals = Vec::new();
            declarations::signals_from_body_item(
                item.id,
                tree,
                &item.type_aliases(&context.type_aliases),
                &item.env,
                Some(&declared),
                &mut signals,
            )?;
            for signal in signals {
                context.insert(
                    item.name(signal.name()),
                    procedural::dimensions_from_type(signal.r#type()),
                );
            }
            publish(&tree.state.dimensions, &key, &context);
        }
    }
    Ok(finish(context))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn parse(code: &str) -> SyntaxTree {
        crate::source_tree::parse_scope_fixture(code, Path::new("native-query-scope.sv")).unwrap()
    }
    fn argument(tree: &AnalysisContext<'_>) -> ExprId {
        tree.declarations().expression_nodes().find_map(|(_,expression)|{
            if let celox_sv_syntax::ExprKind::Call{expression,arguments}=&expression.kind
                && matches!(&tree.declarations().expression(*expression).kind,celox_sv_syntax::ExprKind::Name(n) if n.spelling=="$bits") {
                arguments[0].value
            }else{None}
        }).unwrap()
    }
    fn width(
        state: &AnalysisState,
        shape: &VariableDimensions,
        context: &PackedDimensions,
    ) -> Option<i128> {
        shape.packed.iter().try_fold(1i128, |all, dimension| {
            all.checked_mul(eval_ast_const_expr(
                state,
                &dimension.width,
                &context.const_env,
            )?)
        })
    }
    #[test]
    fn native_query_formals_shadow_outer_constant_values() {
        let tree = parse(
            "module M;parameter N=16;function int f(input logic[3:0]N);return $bits(N);endfunction endmodule",
        );
        let tree = AnalysisContext::unspecialized(&tree);
        let input = PackedDimensions::new(
            HashMap::default(),
            &[("N".to_string(), 16)].into_iter().collect(),
            &HashMap::default(),
        );
        let context = query_operand_dimensions_id(argument(&tree), &tree, &input).unwrap();
        assert_eq!(
            width(&tree.state, context.get("N").unwrap(), &context),
            Some(4)
        );
        assert!(!context.const_env.contains_key("N"));
        assert_eq!(context.function_return_types["f"].width, Some(32));
        assert_eq!(
            context.function_return_types["f"]
                .arguments
                .as_ref()
                .unwrap()[0]
                .name,
            "N"
        );
    }
    #[test]
    fn native_generate_ancestor_dpi_shadows_root_return_metadata() {
        let tree = parse(
            "module M;import \"DPI-C\" function int f();if(1)begin:g import \"DPI-C\" function byte f();localparam N=$bits(f());end endmodule",
        );
        let tree = AnalysisContext::unspecialized(&tree);
        let input =
            PackedDimensions::new(HashMap::default(), &HashMap::default(), &HashMap::default());
        let context = query_operand_dimensions_id(argument(&tree), &tree, &input).unwrap();
        assert_eq!(context.function_return_types["f"].width, Some(8));
        assert!(context.functions.is_empty());
        assert!(context.type_query);
    }
    #[test]
    fn native_function_local_array_shapes_survive_preliminary_queries() {
        let tree = parse(
            "module M;function int f();logic[7:0]a[0:3];begin logic[2:0]a[0:1];return $bits(a);end endfunction endmodule",
        );
        let tree = AnalysisContext::unspecialized(&tree);
        let input =
            PackedDimensions::new(HashMap::default(), &HashMap::default(), &HashMap::default());
        let context = query_operand_dimensions_id(argument(&tree), &tree, &input).unwrap();
        let shape = context.get("a").unwrap();
        assert_eq!(width(&tree.state, shape, &context), Some(3));
        assert_eq!(
            eval_ast_const_expr(&tree.state, &shape.unpacked[0].width, &context.const_env),
            Some(2)
        );
    }
    #[test]
    fn native_query_discovery_retains_ordinary_constructed_siblings() {
        let tree = parse(
            "module M; function int f(); int dyn[]=new[2], ordinary=3; return $bits(ordinary); endfunction endmodule",
        );
        let tree = AnalysisContext::unspecialized(&tree);
        let context =
            query_operand_dimensions_id(argument(&tree), &tree, &PackedDimensions::default())
                .unwrap();
        assert_eq!(
            width(&tree.state, context.get("ordinary").unwrap(), &context),
            Some(32)
        );
        assert!(context.get("dyn").unwrap().container.is_some());
    }
}
