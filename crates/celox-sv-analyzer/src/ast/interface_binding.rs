//! Interface instances bound in semantic scopes, retaining authored syntax.
use super::*;

mod bind_children;
mod bind_instances;
mod bind_ports;

pub(super) type InstanceKey = (celox_sv_syntax::BodyItemId, usize);

pub(super) struct Bindings<'a> {
    pub values: &'a HashMap<String, ConstExpr>,
    pub types: &'a [(String, Type)],
    pub export_symbols: bool,
    pub plan: Option<&'a interfaces::NativeModulePlan>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct InstanceBinding {
    pub module: String,
    pub connections: HashMap<String, Vec<PortConnection>>,
    pub parameters: Vec<ParameterOverride>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct ScopeBinding {
    pub members: HashMap<String, String>,
    pub types: HashMap<String, String>,
    pub arrays: HashMap<String, ArrayBinding>,
    pub instances: HashMap<InstanceKey, InstanceBinding>,
}

/// Elements in declaration order, independent of the numeric index origins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ArrayBinding {
    pub elements: Vec<String>,
    pub shape: Vec<usize>,
    /// Authored bounds, retained for selecting actual interface array elements.
    pub ranges: Vec<(i128, i128)>,
}

/// Compatibility shorthand for an unindexed interface-array value parameter.
/// Follow the first element in declaration order, including nonzero bounds.
/// Runtime members and type parameters retain their ordinary indexed lookup.
pub(super) fn unindexed_parameter(
    path: &[String],
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dimensions: &PackedDimensions,
    root: celox_sv_syntax::ExprId,
) -> Option<String> {
    let (member, handle) = path.split_last()?;
    let root_name = handle.first()?;
    if dimensions.get(root_name).is_some()
        || dimensions.const_env.contains_key(root_name)
        || dimensions.parameter_values.contains_key(root_name)
    {
        return None;
    }
    let imported = scope::imported(tree);
    let array = imported.interface_arrays.get(&handle.join("."))?;
    let element = array.elements.first()?;
    let bound = imported
        .interface_members
        .get(&format!("{element}.{member}"))?;
    if !imported.parameter_values.contains_key(bound) {
        return None;
    }
    warnings::append(
        &mut session.diagnostics,
        [warnings::AnalyzerWarning::InterfaceArrayParameter {
            path: path.join("."),
            element: element.clone(),
            site: procedural::source_span(tree.declarations().expression(root).tokens, tree),
        }],
    );
    Some(bound.clone())
}

/// The interfaces a module may instantiate or receive, by name: those of the
/// interface sources of the design and those its own source declares, each
/// with the path of its source.
fn interface_definitions<'a>(
    tree: &AnalysisContext<'_>,
    path: &'a Path,
    packages: &'a packages::Packages,
) -> Converted<HashMap<String, (SyntaxTree, &'a Path)>> {
    let mut definitions = HashMap::default();
    for (interface_tree, interface_path) in packages.interface_sources.values() {
        for handle in interface_tree.root_scopes()? {
            let scoped = interface_tree.scoped(handle)?;
            if scoped.root_unit()?.kind == celox_sv_syntax::UnitKind::Interface {
                let name = instances::normalize_identifier(&scoped.root_name()?);
                definitions.insert(name, (scoped, interface_path.as_path()));
            }
        }
    }
    for handle in tree.root_scopes()? {
        let scoped = tree.scoped(handle)?;
        if scoped.root_unit()?.kind == celox_sv_syntax::UnitKind::Interface {
            let name = instances::normalize_identifier(&scoped.root_name()?);
            definitions.entry(name).or_insert((scoped, path));
        }
    }
    Ok(definitions)
}

/// The state [`analyze_module`] binds the interfaces of a module in: the
/// module and the interfaces it may use, and what the phases bound so far.
/// The phases run in the order `analyze_module` calls them. The session and
/// the context that binds in the module are passed to each phase.
struct ModuleBinding<'a> {
    packages: &'a packages::Packages,
    bindings: Bindings<'a>,
    interfaces: &'a InterfaceLookup<'a>,
    /// See [`interface_definitions`].
    definitions: HashMap<String, (SyntaxTree, &'a Path)>,
    name: String,
    plan: Option<&'a interfaces::NativeModulePlan>,
    /// The parameter overrides of the module itself.
    overrides: HashMap<String, ConstExpr>,
    /// The parameter overrides as supplied, with those of the interface ports.
    supplied: &'a HashMap<String, ConstExpr>,
    interface_ports: &'a [interfaces::NativePortPlan],
    /// The ports of the module that are not interface ports.
    view_ports: port_owners::Selection<'a>,
    /// The interface instantiations the module declares directly.
    instantiations: Vec<celox_sv_syntax::BodyItemId>,
    /// The symbols the module imports, with the interface members bound so far.
    imported: scope::ScopeSymbols,
    /// The ports and symbols of the module itself, as last collected.
    caller_ports: Vec<Port>,
    caller_symbols: scope::ScopeSymbols,
    /// The assignments that connect the ports of the interface instances.
    port_assignments: Vec<Assignment>,
}

pub(super) fn analyze_module(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    path: &Path,
    packages: &packages::Packages,
    bindings: Bindings<'_>,
    interfaces: &InterfaceLookup<'_>,
    imported: scope::ScopeSymbols,
) -> Converted<Option<Module>> {
    let definitions = interface_definitions(tree, path, packages)?;
    if definitions.is_empty() {
        return Ok(None);
    }
    let name = tree.root_name()?;
    let plan = bindings.plan;
    let overrides = bindings.values;
    let interface_ports = plan.map(|plan| plan.ports.as_slice()).unwrap_or_default();
    let mut view_ports = port_owners::Selection::authored(tree)?;
    remove_interface_ports(&mut view_ports, interface_ports)?;
    let supplied = overrides;
    let overrides = overrides
        .iter()
        .filter(|(name, _)| {
            !interface_ports
                .iter()
                .any(|port| name.starts_with(&format!("{}$", port.name)))
        })
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect::<HashMap<_, _>>();
    // Direct interface declarations retain native ownership before the
    // structural view removes their original nodes. Explicit generate regions
    // remain in the later generated-item scheduling path.
    let mut instantiations = Vec::new();
    for id in tree.body_membership()? {
        if let celox_sv_syntax::BodyItemKind::Instance(native) =
            tree.declarations().body_item(id).kind
            && definitions.contains_key(&instances::normalize_identifier(
                &tree.declarations().instantiation(native).name.spelling,
            ))
        {
            instantiations.push(id);
        }
    }
    let selected_header_tree = view_ports.project(tree)?;
    let projected_tree = retain_body(
        &selected_header_tree,
        |id| Ok(!instantiations.contains(&id)),
    )?;
    let projected_context = tree.child(&projected_tree, bindings.values);
    let tree = &projected_context;
    let has_generated_interfaces =
        native_body_items(tree)?
            .iter()
            .any(|id| match tree.declarations().body_item(*id).kind {
                celox_sv_syntax::BodyItemKind::Instance(id) => {
                    definitions.contains_key(&instances::normalize_identifier(
                        &tree.declarations().instantiation(id).name.spelling,
                    ))
                }
                _ => false,
            });
    if instantiations.is_empty() && interface_ports.is_empty() && !has_generated_interfaces {
        return Ok(None);
    }
    let mut binding = ModuleBinding {
        packages,
        bindings,
        interfaces,
        definitions,
        name,
        plan,
        overrides,
        supplied,
        interface_ports,
        view_ports,
        instantiations,
        imported,
        caller_ports: Vec::new(),
        caller_symbols: scope::ScopeSymbols::default(),
        port_assignments: Vec::new(),
    };
    let array_scope = binding.array_scope(tree, session)?;
    binding.bind_ports(tree, session, &array_scope)?;
    let caller = binding.caller(tree, session)?;
    let tree = &tree.importing(Arc::new(binding.caller_symbols.clone()));
    let tree = &tree.with_functions(caller.constant_functions.clone());
    binding.bind_instances(tree, session)?;
    binding.bind_children(tree, session)?;
    binding.finish(tree, session)
}

fn native_body_items(tree: &SyntaxTree) -> Converted<Vec<celox_sv_syntax::BodyItemId>> {
    use celox_sv_syntax::BodyItemKind;
    let mut pending = tree.body_membership()?;
    let mut seen = HashSet::default();
    let mut items = Vec::new();
    while let Some(id) = pending.pop() {
        if !seen.insert(id) {
            return Err(AnalyzerError::Parse(
                "repeated native interface body owner".into(),
            ));
        }
        items.push(id);
        match &tree.declarations().body_item(id).kind {
            BodyItemKind::Region(children)
            | BodyItemKind::Block {
                items: children, ..
            }
            | BodyItemKind::Unconverted {
                items: children, ..
            } => pending.extend(children),
            BodyItemKind::Conditional {
                then_body,
                otherwise,
                ..
            } => {
                pending.push(*then_body);
                pending.extend(otherwise);
            }
            BodyItemKind::Case { arms, .. } => pending.extend(arms.iter().map(|arm| arm.body)),
            BodyItemKind::Loop { body, .. } => pending.push(*body),
            _ => {}
        }
    }
    Ok(items)
}

/// Delay declarations that reference unbound interface instances or ports (and declarations
/// depending on those names) until the instances have populated semantic scopes.
fn binding_declaration_view<'a>(
    selected: &port_owners::Selection<'a>,
    instances: &[celox_sv_syntax::BodyItemId],
    ports: &[interfaces::NativePortPlan],
    tree: &AnalysisContext<'_>,
    imported: &scope::ScopeSymbols,
) -> Converted<(port_owners::Selection<'a>, SyntaxTree)> {
    let mut selected = selected.clone();
    let mut deferred = instances
        .iter()
        .filter_map(|id| instances::declaration_from_item(*id, tree).ok())
        .flat_map(|instance| instance.instances.iter())
        .map(|instance| instances::normalize_identifier(&instance.name.spelling))
        .chain(ports.iter().map(|port| port.name.clone()))
        .filter(|name| {
            !imported.interface_types.contains_key(name)
                && !imported.interface_arrays.contains_key(name)
        })
        .collect::<HashSet<_>>();
    let references = imports::unqualified_names(tree);
    let mut header_tree = declaration_view(tree)?;
    loop {
        let mut removed = false;
        header_tree = retain_body(&header_tree, |id| {
            if references_deferred(
                tree.declarations().body_item(id).tokens,
                &references,
                &deferred,
            ) {
                deferred.extend(imports::item_names(id, tree));
                removed = true;
                Ok(false)
            } else {
                Ok(true)
            }
        })?;
        let native_ports = selected.header_ports()?;
        let mut inherited_deferred = false;
        let mut membership = Vec::new();
        for native in native_ports {
            let inherits_type = !matches!(native.kind, celox_sv_syntax::PortKind::Explicit { .. })
                && native.data_type.is_some_and(|id| {
                    tree.declarations().data_type(id).tokens.start < native.tokens.start
                });
            inherited_deferred = (inherits_type && inherited_deferred)
                || references_deferred(native.tokens, &references, &deferred);
            membership.push(!inherited_deferred);
            if inherited_deferred {
                match &native.kind {
                    celox_sv_syntax::PortKind::Named(name)
                    | celox_sv_syntax::PortKind::Explicit { name, .. } => {
                        deferred.insert(instances::normalize_identifier(&name.spelling));
                    }
                    _ => {}
                }
                removed = true;
            }
        }
        selected.retain(&membership)?;
        if !removed {
            break;
        }
    }
    let header_tree = selected.project(&header_tree)?;
    Ok((selected, header_tree))
}

fn references_deferred(
    range: celox_sv_syntax::TokenRange,
    references: &HashMap<String, Vec<usize>>,
    deferred: &HashSet<String>,
) -> bool {
    references.iter().any(|(name, uses)| {
        deferred.contains(name)
            && uses
                .iter()
                .any(|index| (range.start..range.end).contains(index))
    })
}

pub(super) fn retain_body(
    tree: &SyntaxTree,
    mut keep: impl FnMut(celox_sv_syntax::BodyItemId) -> Converted<bool>,
) -> Converted<SyntaxTree> {
    let mut membership = Vec::new();
    for id in tree.body_membership()? {
        if keep(id)? {
            membership.push(id);
        }
    }
    tree.with_body_membership(membership)
}

pub(super) fn declaration_view(tree: &SyntaxTree) -> Converted<SyntaxTree> {
    retain_body(tree, |id| {
        use celox_sv_syntax::BodyItemKind::*;
        Ok(matches!(
            tree.declarations().body_item(id).kind,
            Data(_)
                | Net(_)
                | Dpi(_)
                | Parameter(_)
                | Subroutine(_)
                | TypeAlias(_)
                | PackageBinding(_)
                | Unconverted { .. }
        ))
    })
}

fn formal_dimensions(
    port_name: &str,
    tree: &AnalysisContext<'_>,
) -> Converted<Vec<celox_sv_syntax::Dimension>> {
    tree.root_unit()?
        .ports
        .iter()
        .find(|port| match &port.kind {
            celox_sv_syntax::PortKind::Named(name)
            | celox_sv_syntax::PortKind::Explicit { name, .. } => {
                instances::normalize_identifier(&name.spelling) == port_name
            }
            _ => false,
        })
        .map(|port| port.dimensions.clone())
        .ok_or_else(|| unsupported(format!("missing interface port `{port_name}`")))
}

struct ConnectionShape {
    unpacked: Vec<usize>,
    width: usize,
    origin: ConstExpr,
    ascending: bool,
}

/// Single packed dimensions retain authored indices in analyzer expressions;
/// unpacked and multi-packed selections already use flat storage coordinates.
fn connection_slice_origin(
    actual: &Expr,
    functions: &const_functions::ConstantFunctions,
    dimensions: &PackedDimensions,
) -> Converted<(ConstExpr, bool)> {
    let (name, selected_origin) = match actual {
        Expr::Ident(name) => (name, None),
        Expr::Select { expr, lsb, .. } => {
            let Expr::Ident(name) = expr.as_ref() else {
                return Ok((ConstExpr::Literal("0".into()), false));
            };
            (name, Some(lsb.clone()))
        }
        _ => return Ok((ConstExpr::Literal("0".into()), false)),
    };
    if let Some(shape) = dimensions.get(name)
        && shape.unpacked.is_empty()
        && let [dimension] = shape.packed.as_slice()
        && !dimension.normalize_single
    {
        let left = eval_ast_const_expr(functions, &dimension.left, &dimensions.const_env)
            .ok_or_else(|| unsupported("interface connection packed bound"))?;
        let right = eval_ast_const_expr(functions, &dimension.right, &dimensions.const_env)
            .ok_or_else(|| unsupported("interface connection packed bound"))?;
        return Ok((
            selected_origin.unwrap_or_else(|| dimension.right.clone()),
            left < right,
        ));
    }
    Ok((
        selected_origin.unwrap_or_else(|| ConstExpr::Literal("0".into())),
        false,
    ))
}

/// IEEE 1800-2023 23.3.3.5. Packed actuals distribute MSB-first;
/// unpacked actuals use the compiler's declaration-order element layout.
fn distribute_port_connection(
    actual: Expr,
    shape: &ConnectionShape,
    port_shape: &[usize],
    port_width: usize,
    elements: &ArrayBinding,
    ordinal: usize,
) -> Converted<Expr> {
    let actual_shape = shape.unpacked.as_slice();
    let actual_width = shape.width;
    if actual_shape == port_shape && actual_width == port_width {
        return Ok(actual);
    }
    let packed = actual_shape.is_empty() && port_shape.is_empty();
    let distributed_shape = elements
        .shape
        .iter()
        .chain(port_shape)
        .copied()
        .collect::<Vec<_>>();
    if (!packed && actual_shape != distributed_shape)
        || port_width.checked_mul(elements.elements.len()) != Some(actual_width)
    {
        return Err(unsupported(format!(
            "interface array connection shape/width: {actual_width} bits {actual_shape:?} for {} ports of {port_width} bits {port_shape:?}",
            elements.elements.len(),
        )));
    }
    let offset = if packed {
        elements.elements.len() - ordinal - 1
    } else {
        ordinal
    } * port_width;
    let coordinate = |offset: usize| {
        let offset = const_expr_from_i128(offset as i128);
        if shape.ascending {
            ConstExpr::Binary {
                left: Box::new(shape.origin.clone()),
                op: BinaryOp::Sub,
                right: Box::new(offset),
            }
        } else {
            add_expr(shape.origin.clone(), offset)
        }
    };
    let msb = coordinate(offset + port_width - 1);
    let lsb = coordinate(offset);
    let expr = match actual {
        Expr::Select { expr, .. } if matches!(expr.as_ref(), Expr::Ident(_)) => expr,
        value => Box::new(value),
    };
    Ok(Expr::Select {
        expr,
        msb,
        lsb,
        signed: false,
    })
}

fn connection_lvalue(expr: Expr) -> Converted<LValue> {
    match expr {
        Expr::Ident(name) => Ok(LValue::Ident(name)),
        Expr::Select {
            expr,
            msb,
            lsb,
            signed,
        } => {
            let Expr::Ident(name) = *expr else {
                return Err(unsupported("interface output connection"));
            };
            Ok(LValue::Select {
                name,
                msb,
                lsb,
                signed,
                is_2state: false,
                array_slice_width: None,
                array_slice_reversed: false,
                window: None,
            })
        }
        _ => Err(unsupported("interface output connection")),
    }
}

// Nested handles are private to the interface whose body has already been
// analyzed. Export its bound objects and processes without leaking those
// lexical handle paths into the caller.
fn instance_symbols(
    symbols: &scope::ScopeSymbols,
    names: &HashMap<String, String>,
) -> scope::ScopeSymbols {
    let mut bound = symbols.renamed(names);
    // The signals for virtual interfaces belong to the scope that binds the
    // instance, which declares its own.
    bound
        .state_signals
        .retain(|signal| virtual_interfaces::VirtualInterfaceSignal::of(&signal.name).is_none());
    // A package variable that the instance reads is the package's shared
    // object, which the binding scope also declares (IEEE 1800-2023 26.3),
    // not a signal of the instance.
    let (shared, own): (Vec<_>, Vec<_>) = std::mem::take(&mut bound.signals)
        .into_iter()
        .partition(|signal| signal.package_variable.is_some());
    bound.signals = own;
    for signal in shared {
        if !bound
            .state_signals
            .iter()
            .any(|known| known.name == signal.name)
        {
            bound.state_signals.push(signal);
        }
    }
    bound.interface_members.clear();
    bound.interface_types.clear();
    bound.interface_arrays.clear();
    bound.interface_scopes.clear();
    bound.interface_instances.clear();
    bound
}

fn binding_names(
    module: &Module,
    symbols: &scope::ScopeSymbols,
    handle: &str,
    imported: &mut scope::ScopeSymbols,
) -> HashMap<String, String> {
    binding_names_with_prefix(module, symbols, handle, handle, imported)
}

fn binding_names_with_prefix(
    module: &Module,
    symbols: &scope::ScopeSymbols,
    handle: &str,
    prefix: &str,
    imported: &mut scope::ScopeSymbols,
) -> HashMap<String, String> {
    let mut names = symbols.local_names();
    names.extend(
        module
            .parameters
            .iter()
            .map(|parameter| parameter.name.clone()),
    );
    names.extend(
        symbols
            .parameters
            .iter()
            .filter(|parameter| parameter.name.contains('$'))
            .map(|parameter| parameter.name.clone()),
    );
    names.extend(module.signals.iter().map(|signal| signal.name.clone()));
    names.extend(module.ports.iter().map(|port| port.name.clone()));
    names.extend(symbols.type_aliases.keys().cloned());
    names.extend(symbols.functions.keys().cloned());
    names.extend(symbols.enum_constants.numbers.keys().cloned());
    names.retain(|name| !name.contains("::"));
    let mut names: HashMap<_, _> = names
        .into_iter()
        .map(|name| {
            let bound = format!("{prefix}${name}");
            imported
                .interface_members
                .insert(format!("{handle}.{name}"), bound.clone());
            (name, bound)
        })
        .collect();
    // An imported package constant is not an instance-local declaration.
    // Bind references in exported parameter defaults to its qualified name;
    // the caller need not import the package's simple name itself.
    for (alias, target) in &symbols.aliases {
        names.entry(alias.clone()).or_insert_with(|| target.clone());
    }
    names
}

fn remove_interface_ports(
    selected: &mut port_owners::Selection<'_>,
    ports: &[interfaces::NativePortPlan],
) -> Converted<()> {
    let membership = selected
        .header_ports()?
        .into_iter()
        .map(|native| {
            let name = match &native.kind {
                celox_sv_syntax::PortKind::Named(name)
                | celox_sv_syntax::PortKind::Explicit { name, .. } => {
                    Some(instances::normalize_identifier(&name.spelling))
                }
                _ => None,
            };
            !ports.iter().any(|port| Some(&port.name) == name.as_ref())
        })
        .collect::<Vec<_>>();
    selected.retain(&membership)
}

fn connection_expression(
    instance: &celox_sv_syntax::InstanceDeclarator,
    formal: &str,
    interface: Option<&ModuleInterface>,
) -> Converted<Option<celox_sv_syntax::ExprId>> {
    use celox_sv_syntax::{ConnectionValue, InstanceConnectionKind};
    let found = if instance.connections.iter().any(|connection| {
        matches!(
            connection.kind,
            InstanceConnectionKind::Named { .. } | InstanceConnectionKind::Wildcard
        )
    }) {
        let named = instance
            .connections
            .iter()
            .find_map(|connection| match &connection.kind {
                InstanceConnectionKind::Named { name, value }
                    if instances::normalize_identifier(&name.spelling) == formal =>
                {
                    Some(match value {
                        ConnectionValue::Expression(root) => Some(Some(*root)),
                        ConnectionValue::Implicit => Some(None),
                        ConnectionValue::Empty => None,
                    })
                }
                _ => None,
            });
        // `.*` connects a port no connection names as `.name` does
        // (IEEE 1800-2023 23.3.2.4); `.name()` still leaves it open.
        match named {
            Some(connection) => connection,
            None => instance
                .connections
                .iter()
                .any(|connection| matches!(connection.kind, InstanceConnectionKind::Wildcard))
                .then_some(None),
        }
    } else {
        interface
            .and_then(|interface| interface.ports.iter().position(|name| name == formal))
            .and_then(|index| instance.connections.get(index))
            .and_then(|connection| match connection.kind {
                InstanceConnectionKind::Ordered(Some(root)) => Some(Some(root)),
                _ => None,
            })
    };
    found.ok_or_else(|| unsupported(format!("missing interface connection `{formal}`")))
}

fn instance_elements(
    handle: &str,
    dimensions: &[celox_sv_syntax::Dimension],
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    env: &ConstEnv,
    aliases: &TypeAliases,
) -> Converted<ArrayBinding> {
    let context = PackedDimensions::new(HashMap::default(), env, aliases);
    let ranges = constant_binding::bind_unpacked_ranges(dimensions, tree, session, &context)?;
    validate_unpacked_dimension_sizes(&tree.functions, &ranges, env)?;
    let mut shape = Vec::new();
    let mut bounds = Vec::new();
    let mut elements = vec![handle.to_string()];
    for range in ranges {
        let left = eval_ast_const_expr(&tree.functions, &range.left, env)
            .ok_or_else(|| unsupported("interface array bound"))?;
        let right = eval_ast_const_expr(&tree.functions, &range.right, env)
            .ok_or_else(|| unsupported("interface array bound"))?;
        let count = usize::try_from(left.abs_diff(right))
            .ok()
            .and_then(|count| count.checked_add(1))
            .filter(|count| {
                elements
                    .len()
                    .checked_mul(*count)
                    .is_some_and(|count| count <= 4096)
            })
            .ok_or_else(|| unsupported("interface array elaboration limit exceeded"))?;
        shape.push(count);
        bounds.push((left, right));
        elements = elements
            .into_iter()
            .flat_map(|prefix| {
                (0..count).map(move |offset| {
                    let index = if left <= right {
                        left + offset as i128
                    } else {
                        left - offset as i128
                    };
                    format!("{prefix}[{index}]")
                })
            })
            .collect();
    }
    Ok(ArrayBinding {
        elements,
        shape,
        ranges: bounds,
    })
}

/// The interface instance, or selection of instances, of connection actual
/// `actual` when it ends in the name of a modport: `bus` of `bus.mp`. A name
/// whose last component is part of the path of an instance is unchanged.
fn without_modport(
    actual: celox_sv_syntax::ExprId,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    env: &ConstEnv,
    scope: &scope::ScopeSymbols,
) -> celox_sv_syntax::ExprId {
    use celox_sv_syntax::ExprKind;
    let native = tree.declarations();
    let ExprKind::Member {
        expression,
        separator,
        ..
    } = &native
        .expression(expression_binding::ungroup(actual, native))
        .kind
    else {
        return actual;
    };
    if separator != "." {
        return actual;
    }
    let mut known = |id| {
        actual_handle_key(id, tree, session, env, &scope.type_aliases).is_ok_and(|handle| {
            scope.interface_types.contains_key(&handle)
                || scope.interface_arrays.contains_key(&handle)
        })
    };
    if known(actual) {
        return actual;
    }
    // The instance may be an element or a slice of an array of instances.
    let mut array = expression_binding::ungroup(*expression, native);
    while let ExprKind::Select { expression, .. } = &native.expression(array).kind {
        array = expression_binding::ungroup(*expression, native);
    }
    if known(*expression) || known(array) {
        *expression
    } else {
        actual
    }
}

/// Bind an interface actual as a selection of elaborated instances. Selections
/// consume authored dimensions; a slice retains its dimension while an index
/// removes it. Elements stay in declaration order for left-to-left port mapping.
fn connection_array(
    actual: Option<celox_sv_syntax::ExprId>,
    implicit: &str,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    env: &ConstEnv,
    scope: &scope::ScopeSymbols,
) -> Converted<ArrayBinding> {
    use celox_sv_syntax::ExprKind;
    let native = tree.declarations();
    // A connection may name a modport of the interface instance,
    // `.p(bus.mp)` (IEEE 1800-2023 25.5), which the interface plan has
    // checked against that of the port. The instance is what is connected.
    let actual = actual.map(|actual| without_modport(actual, tree, session, env, scope));
    let mut selections = Vec::new();
    let mut root = actual;
    while let Some(id) = root {
        let id = expression_binding::ungroup(id, native);
        if let ExprKind::Select {
            expression,
            dimension,
        } = &native.expression(id).kind
        {
            selections.push(&dimension.kind);
            root = Some(*expression);
        } else {
            root = Some(id);
            break;
        }
    }
    let handle = match root {
        Some(root) => actual_handle_key(root, tree, session, env, &scope.type_aliases)?,
        None => implicit.to_string(),
    };
    let Some(array) = scope.interface_arrays.get(&handle) else {
        // A selection in a hierarchical prefix may name a generated scope;
        // retain the existing scalar-handle resolution for that case.
        return Ok(ArrayBinding {
            elements: vec![match actual {
                Some(actual) => actual_handle_key(actual, tree, session, env, &scope.type_aliases)?,
                None => handle,
            }],
            shape: Vec::new(),
            ranges: Vec::new(),
        });
    };
    if selections.is_empty() {
        return Ok(array.clone());
    }
    if selections.len() > array.shape.len() {
        return Err(unsupported("too many interface array selections"));
    }
    if selections
        .iter()
        .filter(|selection| {
            matches!(
                selection,
                celox_sv_syntax::DimensionKind::Range { .. }
                    | celox_sv_syntax::DimensionKind::Indexed { .. }
            )
        })
        .count()
        > 1
    {
        return Err(unsupported("interface array slices in multiple dimensions"));
    }
    let context = PackedDimensions::new(HashMap::default(), env, &scope.type_aliases);
    let mut evaluate = |root| -> Converted<i128> {
        constant_binding::bind(root, tree, session, &context)?
            .and_then(|value| eval_ast_const_expr(&tree.functions, &value, env))
            .ok_or_else(|| unsupported("nonconstant interface array selection"))
    };
    let selections = selections.into_iter().rev().collect::<Vec<_>>();
    let mut offsets = vec![0];
    let mut shape = Vec::new();
    let mut ranges = Vec::new();
    for (dimension, (&size, &(left, right))) in array.shape.iter().zip(&array.ranges).enumerate() {
        let (selected_left, selected_right, retain) = match selections.get(dimension) {
            None => (left, right, true),
            Some(celox_sv_syntax::DimensionKind::Index(index)) => {
                let index = evaluate(*index)?;
                (index, index, false)
            }
            Some(selection) => {
                let (start, end) = selection_range(selection, left <= right, &mut evaluate)?;
                (start, end, true)
            }
        };
        if selected_left < left.min(right)
            || selected_left > left.max(right)
            || selected_right < left.min(right)
            || selected_right > left.max(right)
        {
            return Err(unsupported(
                "interface array selection outside its declared range",
            ));
        }
        if selected_left != selected_right && (selected_left < selected_right) != (left < right) {
            return Err(unsupported("reversed interface array selection"));
        }
        // Bounds were checked against an elaborated dimension (at most 4096
        // elements), so these differences and products fit in usize.
        let start = left.abs_diff(selected_left) as usize;
        let end = left.abs_diff(selected_right) as usize;
        offsets = offsets
            .into_iter()
            .flat_map(|prefix| (start..=end).map(move |index| prefix * size + index))
            .collect();
        if retain {
            shape.push(end - start + 1);
            ranges.push((selected_left, selected_right));
        }
    }
    Ok(ArrayBinding {
        elements: offsets
            .into_iter()
            .map(|offset| array.elements[offset].clone())
            .collect(),
        shape,
        ranges,
    })
}

fn selection_range(
    selection: &celox_sv_syntax::DimensionKind,
    ascending: bool,
    evaluate: &mut impl FnMut(celox_sv_syntax::ExprId) -> Converted<i128>,
) -> Converted<(i128, i128)> {
    use celox_sv_syntax::DimensionKind;
    match selection {
        DimensionKind::Range { left, right } => Ok((evaluate(*left)?, evaluate(*right)?)),
        DimensionKind::Indexed {
            base,
            operator,
            width,
        } => {
            let base = evaluate(*base)?;
            let width = evaluate(*width)?;
            let extent = width
                .checked_sub(1)
                .filter(|_| width > 0)
                .ok_or_else(|| unsupported("nonpositive interface array selection width"))?;
            let (low, high) = if operator == "+:" {
                (Some(base), base.checked_add(extent))
            } else {
                (base.checked_sub(extent), Some(base))
            };
            let (low, high) = low
                .zip(high)
                .ok_or_else(|| unsupported("interface array selection bound overflow"))?;
            Ok(if ascending { (low, high) } else { (high, low) })
        }
        _ => Err(unsupported("interface array selection")),
    }
}

/// Whether native expression `root` has the form of an interface handle
/// that [`actual_handle_key`] resolves: a name followed by member and
/// index selects. A package-scoped name such as `p::q` never names an
/// interface instance, as packages hold none (IEEE 1800-2023 26.2).
pub(super) fn is_handle_path(
    mut root: celox_sv_syntax::ExprId,
    native: &celox_sv_syntax::Declarations,
) -> bool {
    use celox_sv_syntax::{DimensionKind, ExprKind};
    loop {
        root = expression_binding::ungroup(root, native);
        match &native.expression(root).kind {
            ExprKind::Name(_) => return true,
            ExprKind::Member {
                expression,
                separator,
                ..
            } if separator == "." => root = *expression,
            ExprKind::Select {
                expression,
                dimension,
            } if matches!(dimension.kind, DimensionKind::Index(_)) => root = *expression,
            _ => return false,
        }
    }
}

pub(super) fn actual_handle_key(
    root: celox_sv_syntax::ExprId,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    env: &ConstEnv,
    aliases: &TypeAliases,
) -> Converted<String> {
    use celox_sv_syntax::{DimensionKind, ExprKind};
    enum Suffix {
        Member(String),
        Index(celox_sv_syntax::ExprId),
    }
    let native = tree.declarations();
    let mut root = root;
    let mut suffixes = Vec::new();
    let mut handle = loop {
        root = expression_binding::ungroup(root, native);
        match &native.expression(root).kind {
            ExprKind::Name(name) => break instances::normalize_identifier(&name.spelling),
            ExprKind::Member {
                expression,
                name,
                separator,
            } if separator == "." => {
                suffixes.push(Suffix::Member(instances::normalize_identifier(
                    &name.spelling,
                )));
                root = *expression;
            }
            ExprKind::Select {
                expression,
                dimension,
            } => {
                let DimensionKind::Index(index) = dimension.kind else {
                    return Err(unsupported("interface handle expression"));
                };
                suffixes.push(Suffix::Index(index));
                root = *expression;
            }
            _ => return Err(unsupported("interface handle expression")),
        }
    };
    let context = PackedDimensions::new(HashMap::default(), env, aliases);
    for suffix in suffixes.into_iter().rev() {
        match suffix {
            Suffix::Member(name) => {
                handle.push('.');
                handle.push_str(&name);
            }
            Suffix::Index(root) => {
                let value = constant_binding::bind(root, tree, session, &context)?
                    .and_then(|value| eval_ast_const_expr(&tree.functions, &value, env))
                    .ok_or_else(|| unsupported("nonconstant interface index"))?;
                handle.push_str(&format!("[{value}]"));
            }
        }
    }
    Ok(handle)
}
