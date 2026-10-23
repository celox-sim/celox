//! Native subroutine membership and cycle validation.
use super::*;
use celox_sv_syntax::{BodyItemId, BodyItemKind, Declarations, SubroutineId, SubroutineKind};
fn mismatch() -> AnalyzerError {
    AnalyzerError::Parse("invalid native subroutine membership".into())
}
fn item_ids(ids: &[BodyItemId], native: &Declarations) -> Converted<Vec<SubroutineId>> {
    let mut pending = ids.iter().rev().copied().collect::<Vec<_>>();
    let mut seen = HashSet::default();
    let mut result = Vec::new();
    while let Some(id) = pending.pop() {
        if !seen.insert(id) {
            return Err(mismatch());
        }
        match &native.body_item(id).kind {
            BodyItemKind::Subroutine(id) => result.push(*id),
            BodyItemKind::Unit(id) => pending.extend(native.unit(*id).items.iter().rev().copied()),
            // A class owns methods, not module/package subroutines. Merely
            // declaring a class must not lower its uninstantiated method bodies.
            BodyItemKind::Unconverted { category, .. } if category == "class" => {}
            BodyItemKind::Region(items)
            | BodyItemKind::Block { items, .. }
            | BodyItemKind::Unconverted { items, .. } => {
                pending.extend(items.iter().rev().copied())
            }
            BodyItemKind::Conditional {
                then_body,
                otherwise,
                ..
            } => {
                pending.extend(otherwise);
                pending.push(*then_body);
            }
            BodyItemKind::Case { arms, .. } => {
                pending.extend(arms.iter().rev().map(|arm| arm.body))
            }
            BodyItemKind::Loop { body, .. } => pending.push(*body),
            _ => {}
        }
    }
    Ok(result)
}
pub(super) fn item(id: BodyItemId, tree: &SyntaxTree) -> Converted<Vec<SubroutineId>> {
    Ok(item_ids(&[id], tree.declarations())?
        .into_iter()
        .filter(|id| {
            let routine = tree.declarations().subroutine(*id);
            // Out-of-block method definitions belong to their class.
            !routine.prototype
                && routine.kind != SubroutineKind::Constructor
                && routine.scope.is_empty()
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn class_methods_do_not_become_module_subroutines() {
        let tree = crate::source_tree::parse_scope_fixture(
            "module M; class C; int a; task f(int a); this.a += a; endtask endclass function int g(); return 1; endfunction endmodule",
            Path::new("class-method-owner.sv"),
        ).unwrap();
        let native = tree.declarations();
        let ids = item_ids(&tree.root_unit().unwrap().items, native).unwrap();
        assert_eq!(
            ids.iter()
                .map(|id| native.subroutine(*id).name.spelling.as_str())
                .collect::<Vec<_>>(),
            ["g"]
        );
        crate::analyze_source(
            "module M; class C; int a; task f(int a); this.a += a; endtask endclass endmodule",
            Path::new("class.sv"),
        )
        .unwrap();
    }

    #[test]
    fn trimmed_structural_header_view_uses_selected_native_subroutine_membership() {
        let tree = crate::source_tree::parse_scope_fixture("module M(output logic[$bits(g())-1:0]y);function int f();return 1;endfunction function int g();return 2;endfunction assign y=0;endmodule", Path::new("trimmed-subroutine-view.sv")).unwrap();
        let tree = tree
            .with_body_membership(tree.root_unit().unwrap().items[1..].to_vec())
            .unwrap();
        let empty = ModuleInterfaces::default();
        let lookup = InterfaceLookup {
            local: &empty,
            extra: &empty,
        };
        let module = Module::from_scope_with_symbols(
            &AnalysisContext::unspecialized(&tree),
            "M",
            &HashMap::default(),
            &lookup,
            Arc::default(),
            false,
        )
        .unwrap();
        assert_eq!(
            module
                .subroutines()
                .iter()
                .map(|routine| routine.name.as_str())
                .collect::<Vec<_>>(),
            ["g"]
        );
    }
}
