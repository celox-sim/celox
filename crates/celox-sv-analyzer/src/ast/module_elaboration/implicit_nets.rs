//! The implicit nets that the port connections of a module's instances
//! declare (IEEE 1800-2023 6.10).

use super::*;
use celox_sv_syntax::{
    BodyItemKind, ConnectionValue, ExprId, ExprKind, InstanceConnectionKind, TokenKind,
};

impl ScopeElaboration<'_> {
    /// Declare the implicit nets of the port connections of the instances
    /// the module or program itself holds: an identifier of a port
    /// expression that no declaration visible there names is a scalar net
    /// of the default net type of the module (IEEE 1800-2023 6.10, 22.8),
    /// declared before the connections are bound. Under `default_nettype
    /// none` such an identifier is an error. An implicit `.name` or `.*`
    /// connection declares none (23.3.2.3, 23.3.2.4). Returns whether the
    /// scope declared one.
    ///
    /// The instances of generate blocks declare none here: their actuals
    /// stay as bound before.
    pub(super) fn implicit_port_nets(
        &mut self,
        syntax_tree: &AnalysisContext<'_>,
    ) -> Result<bool, AnalyzerError> {
        let Some(unit) = syntax_tree.scope_handle().unit(syntax_tree.declarations()) else {
            return Ok(false);
        };
        if unit.external
            || !matches!(
                unit.kind,
                celox_sv_syntax::UnitKind::Module | celox_sv_syntax::UnitKind::Program
            )
        {
            return Ok(false);
        }
        let native = syntax_tree.declarations();
        // The identifiers in source order, each once.
        let mut candidates: Vec<String> = Vec::new();
        let mut seen = HashSet::default();
        for id in scope_items(syntax_tree.body_membership()?, native) {
            let BodyItemKind::Instance(instantiation) = native.body_item(id).kind else {
                continue;
            };
            for declarator in &native.instantiation(instantiation).instances {
                for connection in &declarator.connections {
                    let actual = match &connection.kind {
                        InstanceConnectionKind::Ordered(Some(actual))
                        | InstanceConnectionKind::Named {
                            value: ConnectionValue::Expression(actual),
                            ..
                        } => *actual,
                        _ => continue,
                    };
                    port_expression_names(actual, native, &mut seen, &mut candidates);
                }
            }
        }
        if candidates.is_empty() {
            return Ok(false);
        }
        let declared = self.declared_names(syntax_tree, unit)?;
        // An interface instance or port connected to an interface port.
        let imported = scope::imported(syntax_tree);
        let interface = |name: &String| {
            imported.interface_types.contains_key(name)
                || imported.interface_arrays.contains_key(name)
                || imported.interface_modports.contains_key(name)
        };
        let implicit: Vec<String> = candidates
            .into_iter()
            .filter(|name| {
                !declared.contains(name)
                    && !interface(name)
                    && !expressions::declared_value(syntax_tree, name, &self.packed_dimensions)
            })
            .collect();
        let Some(first) = implicit.first() else {
            return Ok(false);
        };
        let Some(net_type) = unit.default_nettype.as_deref() else {
            return Err(AnalyzerError::UndeclaredIdentifier {
                name: first.clone(),
                context: "a port connection under `default_nettype none declares no implicit net"
                    .into(),
            });
        };
        self.signals.extend(
            implicit
                .into_iter()
                .map(|name| type_binding::implicit_net(name, net_type)),
        );
        Ok(true)
    }

    /// The names the scope of `unit` itself declares, among all its items,
    /// also those this analysis of it leaves out, or may declare through an
    /// item whose declarations the analysis does not know: any identifier
    /// of such an item counts as declared.
    fn declared_names(
        &self,
        syntax_tree: &AnalysisContext<'_>,
        unit: &celox_sv_syntax::Unit,
    ) -> Result<HashSet<String>, AnalyzerError> {
        let native = syntax_tree.declarations();
        let mut declared = imports::scope_names(syntax_tree)?;
        declared.extend(self.ports.iter().map(|port| port.name().to_string()));
        declared.extend(self.signals.iter().map(|signal| signal.name().to_string()));
        let name = |name: &celox_sv_syntax::Name| instances::normalize_identifier(&name.spelling);
        for id in scope_items(unit.items.clone(), native) {
            declared.extend(imports::item_names(id, syntax_tree));
            match &native.body_item(id).kind {
                BodyItemKind::Assertion(assertion) => {
                    declared.insert(name(&native.assertion_declaration(*assertion).name));
                }
                BodyItemKind::Modport(modports) => declared.extend(
                    native
                        .modport_declaration(*modports)
                        .modports
                        .iter()
                        .map(|modport| name(&modport.name)),
                ),
                BodyItemKind::LogicGate(gate) => declared.extend(
                    gate.instances
                        .iter()
                        .filter_map(|instance| instance.name.as_ref())
                        .map(name),
                ),
                BodyItemKind::Block {
                    leading_label,
                    label,
                    ..
                } => declared.extend(leading_label.iter().chain(label).map(name)),
                BodyItemKind::Unconverted { .. } => declared.extend(
                    native.tokens[native.body_item(id).tokens.range()]
                        .iter()
                        .filter(|token| {
                            matches!(
                                token.kind,
                                TokenKind::Identifier | TokenKind::EscapedIdentifier
                            )
                        })
                        .map(|token| instances::normalize_identifier(&token.text)),
                ),
                _ => {}
            }
        }
        Ok(declared)
    }
}

/// The items of a scope among `ids`, in source order: those of a region,
/// which is no scope of its own, in its place.
fn scope_items(
    ids: Vec<celox_sv_syntax::BodyItemId>,
    native: &celox_sv_syntax::Declarations,
) -> Vec<celox_sv_syntax::BodyItemId> {
    let mut items = Vec::new();
    let mut pending: Vec<_> = ids.into_iter().rev().collect();
    while let Some(id) = pending.pop() {
        match &native.body_item(id).kind {
            BodyItemKind::Region(inner) => pending.extend(inner.iter().rev().copied()),
            _ => items.push(id),
        }
    }
    items
}

/// The identifiers of port expression `root` that name an object as a
/// whole: an operand of an operator, of a concatenation, of a conditional
/// or of a select (IEEE 1800-2023 6.10). A name a call, a member access, a
/// cast or a package qualifies, and a system function name, is no such
/// identifier.
fn port_expression_names(
    root: ExprId,
    native: &celox_sv_syntax::Declarations,
    seen: &mut HashSet<String>,
    names: &mut Vec<String>,
) {
    let mut work = vec![root];
    while let Some(id) = work.pop() {
        match &native.expression(id).kind {
            // A system function called without arguments, such as `$time`,
            // is a name of the syntax but no identifier (IEEE 1800-2023
            // 5.6.3).
            ExprKind::Name(name) if name.spelling.starts_with('$') => {}
            ExprKind::Name(name) => {
                let name = instances::normalize_identifier(&name.spelling);
                if seen.insert(name.clone()) {
                    names.push(name);
                }
            }
            ExprKind::Group(inner) => work.push(*inner),
            ExprKind::Unary { expression, .. } => work.push(*expression),
            ExprKind::Binary { left, right, .. } => work.extend([*right, *left]),
            ExprKind::Conditional {
                condition,
                then_expression,
                else_expression,
            } => work.extend([*else_expression, *then_expression, *condition]),
            ExprKind::Concatenation(parts) => work.extend(parts.iter().rev()),
            ExprKind::Replication { expressions, .. } => work.extend(expressions.iter().rev()),
            ExprKind::Select { expression, .. } => work.push(*expression),
            _ => {}
        }
    }
}
