//! Native binding IDs drive namespace values in all owning source contexts.
use super::*;
use crate::ast::Source;
use celox_sv_syntax::{BodyItemKind, PackageBindingItemKind};
fn parsed(code: &str) -> ValidatedSyntax {
    let mut tree = parse_source(code, Path::new("native-package-owner.sv")).unwrap();
    for binding in &mut tree.declarations.package_bindings {
        for item in &mut binding.items {
            let package = match &mut item.kind {
                PackageBindingItemKind::Named { package, .. }
                | PackageBindingItemKind::Wildcard(package) => package,
                _ => continue,
            };
            if package.spelling == "p" {
                package.spelling = "q".into();
            }
        }
    }
    tree
}
const PACKAGES: &str = "package p;parameter X=3;typedef byte T;endpackage package q;parameter X=7;typedef int T;endpackage ";
#[test]
fn native_import_targets_drive_module_header_body_and_compilation_unit_values() {
    for module in [
        "module M;import p::X;parameter N=X;endmodule",
        "module M import p::X;#(parameter N=X)();endmodule",
        "import p::X;module M;parameter N=X;endmodule",
        "package r;import p::*;localparam used=X;export p::*;endpackage module M;import r::*;parameter N=X;endmodule",
    ] {
        let tree = parsed(&format!("{PACKAGES}{module}"));
        let ir = crate::analyze::analyze_source(Source::from_syntax(&tree).unwrap()).unwrap();
        assert_eq!(
            ir.modules()[0].parameters()[0].resolved_value(),
            Some(7),
            "{module}"
        );
    }
}
#[test]
fn native_import_targets_reach_generate_and_interface_dependency_consumers() {
    let tree = parsed(&format!(
        "{PACKAGES}module M;if(1)begin:g import p::X;logic[X-1:0]a;end endmodule"
    ));
    let ir = crate::analyze::analyze_source(Source::from_syntax(&tree).unwrap()).unwrap();
    assert_eq!(
        ir.modules()[0]
            .signals()
            .iter()
            .find(|signal| signal.name() == "g.a")
            .unwrap()
            .r#type()
            .resolved_width(),
        Some(7)
    );
    let tree = parsed(&format!(
        "{PACKAGES}interface I;import p::*;T a;endinterface module M;I bus();endmodule"
    ));
    let source = Source::from_syntax_module_with_parameter_expr_overrides(
        &tree,
        "M",
        &HashMap::default(),
        &crate::ModuleInterfaces::default(),
    )
    .unwrap();
    let ir = crate::analyze::analyze_source(source).unwrap();
    assert_eq!(
        ir.modules()
            .iter()
            .find(|module| module.name() == "M")
            .unwrap()
            .signals()
            .iter()
            .find(|signal| signal.name() == "bus$a")
            .unwrap()
            .r#type()
            .resolved_width(),
        Some(32)
    );
}
#[test]
fn removed_header_body_and_class_import_ids_are_not_recovered() {
    for code in [
        "module M import p::*;#(parameter N=X)();endmodule",
        "module M;import p::*;parameter N=X;endmodule",
        "module M;class C;import p::*;endclass endmodule",
    ] {
        let mut tree = parsed(&format!("{PACKAGES}{code}"));
        Source::from_syntax(&tree).unwrap();
        if code.contains("M import") {
            tree.declarations.units[2].package_bindings.clear();
        } else {
            let binding = tree
                .declarations
                .body_items
                .iter_mut()
                .find(|item| matches!(item.kind, BodyItemKind::PackageBinding(_)))
                .unwrap();
            binding.kind = BodyItemKind::Unconverted {
                category: "missing import".into(),
                items: Vec::new(),
            };
        }
        let source = Source::from_syntax(&tree).unwrap();
        if !code.contains("class") {
            assert!(
                matches!(source.modules()[0].parameters()[0].value(), Some(crate::ast::ConstExpr::Ident(name)) if name == "X")
            );
        }
    }
}

#[test]
fn native_binding_item_membership_and_export_role_are_authoritative() {
    let mut tree = parsed(&format!(
        "{PACKAGES}module M(output T y);import p::X,p::T;assign y=0;endmodule"
    ));
    Source::from_syntax(&tree).unwrap();
    tree.declarations.package_bindings[0].items.pop();
    let error = Source::from_syntax(&tree).unwrap_err();
    assert!(
        matches!(&error, AnalyzerError::Unsupported(detail) if detail == "unsupported port data type"),
        "{error}"
    );
    let mut tree = parsed(&format!(
        "{PACKAGES}package r;import p::*;localparam used=X;export *::*;endpackage module M;import r::*;parameter N=X;endmodule"
    ));
    Source::from_syntax(&tree).unwrap();
    let named = tree.declarations.package_bindings[0].items[0].kind.clone();
    tree.declarations
        .package_bindings
        .iter_mut()
        .find(|binding| matches!(binding.items[0].kind, PackageBindingItemKind::All))
        .unwrap()
        .items[0]
        .kind = named;
    let ir = crate::analyze::analyze_source(Source::from_syntax(&tree).unwrap()).unwrap();
    assert_eq!(ir.modules()[0].parameters()[0].resolved_value(), Some(7));
}
