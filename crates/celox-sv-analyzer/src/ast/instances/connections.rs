//! Native authored port connections and interface binding identities.
use super::*;
use celox_sv_syntax::{ConnectionValue, InstanceConnectionKind, InstanceDeclarator};

pub(super) fn connections(
    key: interface_binding::InstanceKey,
    native: &InstanceDeclarator,
    tree: &AnalysisContext<'_>,
    dimensions: &PackedDimensions,
    interface: Option<&ModuleInterface>,
) -> Result<Vec<PortConnection>, AnalyzerError> {
    let imported = scope::imported(&tree.state);
    let binding = imported.interface_instances.get(&key);
    if native.connections.is_empty() {
        return Ok(Vec::new());
    }
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
                lowered.push(expression_connection(formal.clone(), id, tree, dimensions)?);
            }
        }
    } else {
        for connection in &native.connections {
            match &connection.kind {
                InstanceConnectionKind::Wildcard => {}
                InstanceConnectionKind::Named { name, value } => {
                    let formal = normalize_identifier(&name.spelling);
                    if let Some(connections) =
                        binding.and_then(|binding| binding.connections.get(&formal))
                    {
                        lowered.extend(connections.iter().cloned());
                        continue;
                    }
                    if let ConnectionValue::Expression(id) = value {
                        lowered.push(expression_connection(formal, *id, tree, dimensions)?);
                        continue;
                    }
                    let actual_expr = match value {
                        ConnectionValue::Implicit => Some(Expr::Ident(formal.clone())),
                        ConnectionValue::Empty => None,
                        ConnectionValue::Expression(id) => Some(expression_binding::value(
                            *id,
                            tree.declarations(),
                            tree,
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
    }
    Ok(lowered)
}

fn expression_connection(
    formal: String,
    root: celox_sv_syntax::ExprId,
    tree: &AnalysisContext<'_>,
    dims: &PackedDimensions,
) -> Converted<PortConnection> {
    let native = tree.declarations();
    let root = expression_binding::ungroup(root, native);
    if matches!(
        native.expression(root).kind,
        celox_sv_syntax::ExprKind::Pattern { target: None, .. }
    ) {
        let mut connection = PortConnection::new(formal.clone(), formal, None);
        connection.bound_pattern = Some(bound_patterns::from_port(root, tree, dims)?);
        return Ok(connection);
    }
    let expr = expression_binding::value(root, native, tree, dims)?;
    let actual = expr_ident_name(&expr).unwrap_or_else(|| formal.clone());
    Ok(PortConnection::new(formal, actual, Some(expr)))
}
