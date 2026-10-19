//! Native root ownership regression tests.
use super::*;
use celox_sv_syntax::BodyItemKind;
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Source;
    fn names(tree: &SourceTree) -> Vec<String> {
        Source::from_syntax(tree).unwrap().modules()[0]
            .signals()
            .iter()
            .map(|value| value.name().to_owned())
            .collect()
    }
    fn parameter_number(tree: &SourceTree) -> Option<i128> {
        let ir = crate::analyze::analyze_source(Source::from_syntax(tree).unwrap()).unwrap();
        ir.modules()[0]
            .parameters()
            .iter()
            .find(|value| value.name() == "N")
            .unwrap()
            .resolved_value()
    }
    #[test]
    fn native_package_item_order_and_values_reach_scope_analysis() {
        let mut tree=super::super::parse_source("package p;int a=1;int b=2;parameter int C=3;endpackage module M;import p::*;parameter int N=C;endmodule",Path::new("native-package-order.sv")).unwrap();
        tree.declarations.units[0].items.reverse();
        let handle = tree
            .root_scopes()
            .unwrap()
            .into_iter()
            .find(|handle| {
                handle
                    .unit(tree.declarations())
                    .is_some_and(|unit| unit.kind == celox_sv_syntax::UnitKind::Package)
            })
            .unwrap();
        let scoped = tree.scoped(handle).unwrap();
        let names = scoped
            .body_membership()
            .unwrap()
            .into_iter()
            .flat_map(|id| match tree.declarations().body_item(id).kind {
                BodyItemKind::Data(id) => match tree.declarations().data_declaration(id) {
                    celox_sv_syntax::DataDeclaration::Variables(declaration) => declaration
                        .variables
                        .iter()
                        .map(|variable| variable.name.value().to_string())
                        .collect(),
                    _ => Vec::new(),
                },
                _ => Vec::new(),
            })
            .collect::<Vec<_>>();
        assert_eq!(names, ["b", "a"]);
        assert_eq!(parameter_number(&tree), Some(3));
    }
    #[test]
    fn native_package_membership_is_authoritative_and_foreign_controls_are_rejected() {
        for control in [false, true] {
            let mut tree=super::super::parse_source("package p;parameter int C=3;endpackage module M;import p::*;parameter int N=C;if(1)logic x;endmodule",Path::new("native-package-membership.sv")).unwrap();
            if control {
                let kind = tree
                    .declarations
                    .body_items
                    .iter()
                    .find(|item| matches!(item.kind, BodyItemKind::Conditional { .. }))
                    .unwrap()
                    .kind
                    .clone();
                let id = tree.declarations.units[0].items[0];
                let tokens = tree.declarations.body_item(id).tokens;
                tree.declarations
                    .body_items
                    .iter_mut()
                    .find(|item| item.tokens == tokens)
                    .unwrap()
                    .kind = kind;
            } else {
                tree.declarations.units[0].items.clear();
            }
            if control {
                assert!(matches!(
                    Source::from_syntax(&tree),
                    Err(AnalyzerError::Parse(_))
                ));
            } else {
                let source = Source::from_syntax(&tree).unwrap();
                assert!(
                    matches!(source.modules()[0].parameters()[0].value(),Some(crate::ast::ConstExpr::Ident(name)) if name=="C")
                );
            }
        }
    }
    #[test]
    fn native_package_attribute_time_import_and_export_items_keep_their_consumers() {
        let tree=super::super::parse_source("package q;timeunit 1ns;timeprecision 1ps;parameter int C=2;endpackage package p;timeunit 1ns;timeprecision 1ps;(* keep=1 *) int a=1;import q::*;export q::*;function automatic int f(input int x);return x+C;endfunction endpackage module M;timeunit 1ns;timeprecision 1ps;import p::*;parameter int N=f(4);endmodule",Path::new("native-package-attributes.sv")).unwrap();
        assert_eq!(parameter_number(&tree), Some(6));
    }
    #[test]
    fn native_package_macro_escaped_name_and_explicit_compilation_unit_owner_remain_supported() {
        for source in [
            "`define P \\p \npackage `P ;parameter int C=2;endpackage module M;import p::*;parameter int N=C;endmodule",
            "parameter int U=2;module M;parameter int N=U;endmodule",
        ] {
            let tree =
                super::super::parse_source(source, Path::new("native-package-owners.sv")).unwrap();
            assert_eq!(parameter_number(&tree), Some(2));
        }
    }
    #[test]
    fn native_root_joins_preserve_escaped_names_and_string_contents() {
        for source in [
            "module M;logic \\wire ;assign \\wire =1;endmodule",
            "module M;logic \\a.b ;assign \\a.b =1;endmodule",
            "module M;parameter string S=\"\\\\escaped\";endmodule",
        ] {
            let tree =
                super::super::parse_source(source, Path::new("native-root-spelling.sv")).unwrap();
            let handle = tree.root_scopes().unwrap()[0];
            assert!(
                !tree
                    .scoped(handle)
                    .unwrap()
                    .body_membership()
                    .unwrap()
                    .is_empty()
            );
        }
    }
    #[test]
    fn native_root_order_owns_implicit_generate_ordinals() {
        let mut tree = super::super::parse_source(
            "module M;if(1)logic a;if(1)logic b;endmodule",
            Path::new("native-root-order.sv"),
        )
        .unwrap();
        assert_eq!(names(&tree), ["genblk1.a", "genblk2.b"]);
        tree.declarations.units[0].items.reverse();
        assert_eq!(names(&tree), ["genblk1.b", "genblk2.a"]);
    }
    #[test]
    fn native_root_membership_removals_are_authoritative_and_duplicates_fail() {
        for change in 0..3 {
            let mut tree = super::super::parse_source(
                "module M;logic a;if(1)begin:g logic b;end endmodule",
                Path::new("native-root-membership.sv"),
            )
            .unwrap();
            let items = &mut tree.declarations.units[0].items;
            match change {
                0 => {
                    items.pop();
                }
                1 => items.push(items[0]),
                _ => items[1] = items[0],
            }
            if change == 0 {
                assert_eq!(names(&tree), ["a"]);
            } else {
                assert!(matches!(
                    Source::from_syntax(&tree),
                    Err(AnalyzerError::Parse(_))
                ));
            }
        }
    }
    #[test]
    fn native_root_control_class_mismatch_is_not_a_leaf_retry() {
        let mut tree = super::super::parse_source(
            "module M;logic a;if(1)logic b;endmodule",
            Path::new("native-root-control.sv"),
        )
        .unwrap();
        let native = &mut tree.declarations;
        let declaration = native.body_item(native.units[0].items[0]).kind.clone();
        let control = native.units[0].items[1];
        native
            .body_items
            .iter_mut()
            .find(|item| matches!(item.kind, BodyItemKind::Conditional { .. }))
            .unwrap()
            .kind = declaration;
        assert!(matches!(
            native.body_item(control).kind,
            BodyItemKind::Data(_)
        ));
        assert!(matches!(
            Source::from_syntax(&tree),
            Err(AnalyzerError::Parse(_))
        ));
    }
    #[test]
    fn native_root_time_directives_and_nested_units_keep_separate_scopes() {
        let tree=super::super::parse_source("`default_nettype none\nmodule M;timeunit 1ns;timeprecision 1ps;logic a;`default_nettype wire\nmodule Child;wire b=0;endmodule if(1)logic c;endmodule",Path::new("native-root-scopes.sv")).unwrap();
        assert_eq!(names(&tree), ["a", "genblk1.c"]);
    }
    #[test]
    fn native_root_body_projections_retain_authored_membership_checks() {
        let tree = super::super::parse_source(
            "module M;logic a;assign a=1;if(1)logic b;endmodule",
            Path::new("native-root-projection.sv"),
        )
        .unwrap();
        let scoped = tree.scoped(tree.root_scopes().unwrap()[0]).unwrap();
        assert_eq!(scoped.body_membership().unwrap().len(), 3);
        let id = tree.declarations().units[0].items[0];
        let selected = scoped.with_body_membership(vec![id]).unwrap();
        assert_eq!(selected.body_membership().unwrap(), [id]);
        assert_eq!(
            scoped.body_membership().unwrap().len(),
            3,
            "selected views leave authored scope membership intact"
        );
        let mut native = selected.clone();
        native.declarations.units[0].items.remove(0);
        assert!(native.body_membership().is_err());
    }
    #[test]
    fn native_handles_distinguish_same_named_nested_and_source_units() {
        let tree = parse_source(
            "module M;logic a;module M;logic b;endmodule endmodule module M;logic c;endmodule",
            Path::new("same-named-scopes.sv"),
        )
        .unwrap();
        let roots = tree.root_scopes().unwrap();
        assert_eq!(roots.len(), 3);
        assert!(matches!(roots[0], ScopeHandle::SourceUnit(_)));
        assert!(matches!(roots[1], ScopeHandle::Unit(_)));
        assert!(matches!(roots[2], ScopeHandle::SourceUnit(_)));
        let names = roots
            .iter()
            .map(|handle| {
                let scoped = tree.scoped(*handle).unwrap();
                let unit = scoped.root_unit().unwrap();
                let id = unit.items[0];
                let celox_sv_syntax::BodyItemKind::Data(id) =
                    tree.declarations().body_item(id).kind
                else {
                    panic!("data")
                };
                let celox_sv_syntax::DataDeclaration::Variables(data) =
                    tree.declarations().data_declaration(id)
                else {
                    panic!("variables")
                };
                data.variables[0].name.spelling.clone()
            })
            .collect::<Vec<_>>();
        assert_eq!(names, ["a", "b", "c"]);
        let scoped = tree.scoped(roots[0]).unwrap();
        let foreign = tree.scoped(roots[2]).unwrap().root_unit().unwrap().items[0];
        assert!(
            scoped.with_body_membership(vec![foreign]).is_err(),
            "same names do not substitute a foreign native owner"
        );
        assert!(
            tree.root_unit().is_err(),
            "compilation scope cannot implicitly select a unit"
        );
    }
    #[test]
    fn selected_body_membership_rejects_duplicate_and_foreign_ids() {
        let tree = parse_source(
            "module A;logic a;endmodule module B;logic b;endmodule",
            Path::new("selected-membership.sv"),
        )
        .unwrap();
        let roots = tree.root_scopes().unwrap();
        let a = tree.scoped(roots[0]).unwrap();
        let b = tree.scoped(roots[1]).unwrap();
        let own = a.root_unit().unwrap().items[0];
        let foreign = b.root_unit().unwrap().items[0];
        assert!(a.with_body_membership(vec![own, own]).is_err());
        assert!(a.with_body_membership(vec![foreign]).is_err());
        let selected = a.with_body_membership(Vec::new()).unwrap();
        assert!(selected.body_membership().unwrap().is_empty());
        assert_eq!(
            a.body_membership().unwrap().len(),
            1,
            "authored membership survives a selected view"
        );
    }
}
