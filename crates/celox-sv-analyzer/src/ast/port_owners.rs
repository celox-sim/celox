//! Explicit native header membership for structural module projections.
//! Filtering a header filters its native membership at the same operation.
use super::*;
use celox_sv_syntax::BodyItemKind;
#[cfg(test)]
use celox_sv_syntax::PortKind;
#[derive(Clone)]
pub(super) struct Selection<'a> {
    unit: &'a celox_sv_syntax::Unit,
    ports: Vec<usize>,
}
fn mismatch() -> AnalyzerError {
    AnalyzerError::Parse("native ANSI header membership is invalid".into())
}
impl<'a> Selection<'a> {
    pub(super) fn authored(tree: &'a SyntaxTree) -> Converted<Self> {
        let unit = tree.root_unit()?;
        Ok(Self {
            unit,
            ports: tree.header_port_ids()?,
        })
    }
    pub(super) fn project(&self, tree: &SyntaxTree) -> Converted<SyntaxTree> {
        if !std::ptr::eq(self.unit, tree.root_unit()?) {
            return Err(mismatch());
        }
        tree.with_header_membership(self.ports.clone())
    }
    pub(super) fn retain(&mut self, kept: &[bool]) -> Converted<()> {
        if kept.len() != self.ports.len() {
            return Err(mismatch());
        }
        self.ports = self
            .ports
            .iter()
            .zip(kept)
            .filter_map(|(id, keep)| keep.then_some(*id))
            .collect();
        Ok(())
    }
}
/// Native root/header and selected body membership supply every port owner.
pub(super) fn scope<'a>(
    selected: Option<&Selection<'_>>,
    tree: &'a SyntaxTree,
) -> Converted<Vec<&'a celox_sv_syntax::Port>> {
    let mut owners = Vec::new();
    if let Some(unit) = tree.scope_handle().unit(tree.declarations())
        && unit.kind != celox_sv_syntax::UnitKind::Package
    {
        let ids = if let Some(selected) = selected {
            if !std::ptr::eq(unit, selected.unit) {
                return Err(mismatch());
            }
            selected.ports.clone()
        } else {
            tree.header_port_ids()?
        };
        let mut seen = HashSet::default();
        for id in ids {
            if !seen.insert(id) {
                return Err(mismatch());
            }
            owners.push(unit.ports.get(id).ok_or_else(mismatch)?);
        }
    } else if selected.is_some() {
        return Err(mismatch());
    }
    let native = tree.declarations();
    let mut pending = tree.body_membership()?;
    let mut seen = HashSet::default();
    while let Some(id) = pending.pop() {
        if !seen.insert(id) {
            return Err(mismatch());
        }
        match &native.body_item(id).kind {
            BodyItemKind::Unit(id) => {
                let unit = native.unit(*id);
                if unit.port_style == celox_sv_syntax::PortStyle::Ansi {
                    owners.extend(&unit.ports);
                }
                pending.extend(&unit.items);
            }
            BodyItemKind::Region(items)
            | BodyItemKind::Block { items, .. }
            | BodyItemKind::Unconverted { items, .. } => pending.extend(items),
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
    owners.sort_unstable_by_key(|port| port.tokens.start);
    Ok(owners)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn parse(code: &str) -> SyntaxTree {
        crate::source_tree::parse_scope_fixture(code, Path::new("header-selection.sv")).unwrap()
    }
    #[test]
    fn authored_defaults_belong_to_native_port_owners() {
        let tree = parse("module M(input logic a=0);endmodule");
        let tree = AnalysisContext::unspecialized(&tree);
        let ports = scope(None, &tree).unwrap();
        assert_eq!(ports.len(), 1);
        assert!(ports[0].default.is_some());
        assert!(matches!(&ports[0].kind, PortKind::Named(name) if name.spelling == "a"));
    }
    #[test]
    fn explicit_port_without_a_new_header_keeps_native_inherited_type() {
        let tree = parse("module M(input wire[7:0] a,.b(a));endmodule");
        let tree = AnalysisContext::unspecialized(&tree);
        let ports = ports_from_scope(
            &AnalysisContext::unspecialized(&tree),
            &HashMap::default(),
            &HashMap::default(),
            None,
        )
        .unwrap();
        assert_eq!(ports.len(), 2);
        assert_eq!(ports[1].name(), "b");
        assert_eq!(ports[1].direction(), PortDirection::Input);
        assert_eq!(
            typecheck::eval_const_expr(
                &ports[1].r#type().packed_ranges()[0].left().clone().into(),
                &HashMap::default()
            ),
            Some(7)
        );
    }
    #[test]
    fn a_structural_header_projection_carries_explicit_native_membership() {
        let tree = parse(
            "module M(input wire[3:0]a,input wire[7:0]b);endmodule module N(input wire z);endmodule",
        );
        let tree = AnalysisContext::unspecialized(&tree);
        let mut selection = Selection::authored(&tree).unwrap();
        selection.retain(&[false, true]).unwrap();
        assert_eq!(scope(None, &tree).unwrap().len(), 2);
        let ports = ports_from_scope(
            &AnalysisContext::unspecialized(&tree),
            &HashMap::default(),
            &HashMap::default(),
            Some(&selection),
        )
        .unwrap();
        assert_eq!(ports.len(), 1);
        assert_eq!(ports[0].name(), "b");
        assert_eq!(
            typecheck::eval_const_expr(
                &ports[0].r#type().packed_ranges()[0].left().clone().into(),
                &HashMap::default()
            ),
            Some(7)
        );
        let projected = selection.project(&tree).unwrap();
        let metadata = scope(None, &projected).unwrap();
        assert_eq!(metadata.len(), 1);
        assert!(matches!(&metadata[0].kind, PortKind::Named(name) if name.spelling == "b"));
        assert!(selection.retain(&[true, false]).is_err());
        let second = tree.root_scopes().unwrap()[1];
        let other = tree.scoped(second).unwrap();
        assert!(scope(Some(&selection), &other).is_err());
        selection.ports.push(1);
        assert!(scope(Some(&selection), &tree).is_err());
    }
}
