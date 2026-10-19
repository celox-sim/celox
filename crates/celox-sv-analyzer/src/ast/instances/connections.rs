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
                let actual_expr =
                    expression_binding::value(id, tree.declarations(), tree, dimensions)?;
                let actual = expr_ident_name(&actual_expr).unwrap_or_else(|| formal.clone());
                lowered.push(PortConnection::new(
                    formal.clone(),
                    actual,
                    Some(actual_expr),
                ));
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
