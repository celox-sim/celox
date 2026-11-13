//! Native authored port connections and interface binding identities.
use super::*;
use celox_sv_syntax::{ConnectionValue, InstanceConnectionKind, InstanceDeclarator};

pub(super) fn connections(
    key: interface_binding::InstanceKey,
    native: &InstanceDeclarator,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dimensions: &PackedDimensions,
    interface: Option<&ModuleInterface>,
) -> Result<Vec<PortConnection>, AnalyzerError> {
    let imported = scope::imported(tree);
    let binding = imported.interface_instances.get(&key);
    if native.connections.is_empty() {
        return Ok(Vec::new());
    }
    // The hierarchy below a module is elaborated from its declarations
    // before the hierarchical names of the module are resolved against it
    // (IEEE 1800-2023 23.6): an actual with such a name is left out there,
    // and bound when the module is analyzed with the objects the names
    // denote.
    let declarations_only = imported.declarations_only;
    let resolved = move |connection: Converted<PortConnection>| match connection {
        Err(AnalyzerError::HierarchicalReference { .. }) if declarations_only => Ok(None),
        connection => connection.map(Some),
    };
    let ordered = native
        .connections
        .iter()
        .all(|connection| matches!(connection.kind, InstanceConnectionKind::Ordered(_)));
    if !ordered
        && native
            .connections
            .iter()
            .any(|connection| matches!(connection.kind, InstanceConnectionKind::Ordered(_)))
    {
        return Err(unsupported("mixed named and ordered port connection"));
    }
    let mut lowered = Vec::new();
    if ordered {
        if native
            .connections
            .iter()
            .all(|connection| matches!(connection.kind, InstanceConnectionKind::Ordered(None)))
        {
            return Ok(lowered);
        }
        let ports = interface
            .map(|interface| interface.ports.as_slice())
            .ok_or_else(|| unsupported("ordered port connection"))?;
        for (position, connection) in native.connections.iter().enumerate() {
            let InstanceConnectionKind::Ordered(value) = connection.kind else {
                unreachable!()
            };
            let formal = ports
                .get(position)
                .ok_or_else(|| unsupported("ordered port connection"))?;
            if let Some(connections) = binding.and_then(|binding| binding.connections.get(formal)) {
                lowered.extend(connections.iter().cloned());
                continue;
            }
            if let Some(id) = value {
                lowered.extend(resolved(expression_connection(
                    formal.clone(),
                    id,
                    tree,
                    session,
                    dimensions,
                ))?);
            }
        }
    } else {
        let mut wildcard = false;
        let mut named = HashSet::default();
        for connection in &native.connections {
            match &connection.kind {
                InstanceConnectionKind::Wildcard => wildcard = true,
                InstanceConnectionKind::Named { name, value } => {
                    let formal = normalize_identifier(&name.spelling);
                    named.insert(formal.clone());
                    if let Some(connections) =
                        binding.and_then(|binding| binding.connections.get(&formal))
                    {
                        lowered.extend(connections.iter().cloned());
                        continue;
                    }
                    if let ConnectionValue::Expression(id) = value {
                        lowered.extend(resolved(expression_connection(
                            formal, *id, tree, session, dimensions,
                        ))?);
                        continue;
                    }
                    let actual_expr = match value {
                        ConnectionValue::Implicit => Some(Expr::Ident(formal.clone())),
                        ConnectionValue::Empty => None,
                        ConnectionValue::Expression(id) => Some(expression_binding::value(
                            *id,
                            tree.declarations(),
                            tree,
                            session,
                            dimensions,
                        )?),
                    };
                    let actual = actual_expr
                        .as_ref()
                        .and_then(expr_ident_name)
                        .unwrap_or_else(|| formal.clone());
                    lowered.push(PortConnection::new(formal, actual, actual_expr));
                }
                InstanceConnectionKind::Ordered(_) => unreachable!(),
            }
        }
        if wildcard {
            // `.*` implicitly connects each remaining port to a
            // same-named signal or interface of the instantiating scope; an
            // explicit connection, including an empty one, takes its place
            // (23.3.2.4).
            let ports = interface
                .map(|interface| interface.ports.as_slice())
                .ok_or_else(|| unsupported("wildcard port connection"))?;
            for formal in ports {
                if named.contains(formal)
                    || lowered
                        .iter()
                        .any(|connection| connection.formal() == formal)
                {
                    continue;
                }
                if let Some(connections) =
                    binding.and_then(|binding| binding.connections.get(formal))
                {
                    lowered.extend(connections.iter().cloned());
                    continue;
                }
                if dimensions.get(formal).is_some() {
                    lowered.push(PortConnection::new(
                        formal.clone(),
                        formal.clone(),
                        Some(Expr::Ident(formal.clone())),
                    ));
                }
            }
        }
    }
    Ok(lowered)
}

fn expression_connection(
    formal: String,
    root: celox_sv_syntax::ExprId,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dims: &PackedDimensions,
) -> Converted<PortConnection> {
    let native = tree.declarations();
    let root = expression_binding::ungroup(root, native);
    if matches!(
        native.expression(root).kind,
        celox_sv_syntax::ExprKind::Pattern { target: None, .. }
    ) {
        let mut connection = PortConnection::new(formal.clone(), formal, None);
        connection.bound_pattern = Some(bound_patterns::from_port(root, tree, session, dims)?);
        return Ok(connection);
    }
    let expr = expression_binding::value(root, native, tree, session, dims)?;
    let actual = unpacked_structs::native_member(root, native, dims)
        .or_else(|| match &native.expression(root).kind {
            celox_sv_syntax::ExprKind::Name(name)
                if dims
                    .get(&normalize_identifier(&name.spelling))
                    .is_some_and(|ty| ty.unpacked_struct.is_some()) =>
            {
                Some(normalize_identifier(&name.spelling))
            }
            _ => None,
        })
        .or_else(|| expr_ident_name(&expr))
        .unwrap_or_else(|| formal.clone());
    Ok(PortConnection::new(formal, actual, Some(expr)))
}
