//! Array/pattern parameter type and dimension ownership through real storage.
use super::*;
use crate::ast::Source;
use celox_sv_syntax::{DataTypeKind, ExprKind, ParameterKind};
fn parsed(code: &str) -> SourceTree {
    let mut tree = parse_source(code, Path::new("native-array-parameter-type.sv")).unwrap();
    for node in &mut tree.declarations.expressions {
        if let ExprKind::Binary { operator, .. } = &mut node.kind
            && operator == "+"
        {
            *operator = "-".into();
        }
    }
    tree
}
fn widths(tree: &SourceTree) -> Vec<usize> {
    let ir = crate::analyze::analyze_source(Source::from_syntax(tree).unwrap()).unwrap();
    ir.modules()[0]
        .signals()
        .iter()
        .map(|signal| signal.r#type().resolved_width().unwrap())
        .collect()
}
#[test]
fn native_array_parameter_packed_bounds_drive_header_and_body_storage_widths() {
    for code in [
        "module M#(parameter logic[3+1:0]P[2]='{1,2})();endmodule",
        "module M#(logic[3+1:0]P[2]='{1,2})();endmodule",
        "module M;localparam logic[3+1:0]P[2]='{1,2};endmodule",
        "module M;localparam logic[3+1:0]P/*name trivia*/[2]='{1,2};endmodule",
    ] {
        assert_eq!(widths(&parsed(code)), [6], "{code}");
    }
}
#[test]
fn native_inherited_array_parameter_prefixes_share_the_original_type_owner() {
    let tree = parsed("module M#(parameter logic[3+1:0]P[2]='{1,2},Q[2]='{3,4})();endmodule");
    assert_eq!(widths(&tree), [6, 6]);
}
#[test]
fn native_array_parameter_unpacked_bounds_drive_storage_and_default_count() {
    let tree = parsed("module M;localparam byte P[3+1:1]='{3+1{1}};endmodule");
    assert_eq!(widths(&tree), [16]);
}
#[test]
fn native_array_parameter_primitive_kind_and_signing_reach_ir() {
    let mut tree = parsed("module M;localparam logic[7:0]P[2]='{1,2};endmodule");
    for ty in &mut tree.declarations.types {
        if let DataTypeKind::Builtin(name) = &mut ty.kind
            && name == "logic"
        {
            *name = "bit".into();
            ty.signed = Some(true);
        }
    }
    let ir = crate::analyze::analyze_source(Source::from_syntax(&tree).unwrap()).unwrap();
    let ty = ir.modules()[0].signals()[0].r#type();
    assert_eq!(ty.kind(), crate::ir::TypeKind::Bit);
    assert!(ty.is_signed());
    assert_eq!(ty.resolved_width(), Some(16));
}
#[test]
fn native_array_parameter_role_changes_remove_value_storage() {
    let mut tree = parsed("module M#(parameter byte P[2]='{1,2})();endmodule");
    tree.declarations.units[0].parameters.as_mut().unwrap()[0].kind =
        ParameterKind::Type { default: None };
    let source = Source::from_syntax(&tree).unwrap();
    assert!(source.modules()[0].signals().is_empty());
    assert!(source.modules()[0].parameters().is_empty());
}
#[test]
fn class_parameter_headers_have_explicit_unconverted_declaration_coverage() {
    let tree = parsed("class C#(parameter int P=1);endclass module M;endmodule");
    let source = Source::from_syntax(&tree).unwrap();
    assert_eq!(source.modules().len(), 1);
    assert_eq!(source.modules()[0].name(), "M");
}

#[test]
fn array_parameter_datatype_bearing_bounds_retain_the_original_width_owner() {
    for expression in ["$bits(logic[3:0])", "$bits(int)", "int'(4)"] {
        let tree = parsed(&format!(
            "module M;localparam logic[{expression}:0]P[2]='{{0,0}};endmodule"
        ));
        // Accepted-source compatibility only: the original nested datatype
        // range interpretation is deliberately outside this source_tree slice.
        let source = Source::from_syntax(&tree).unwrap();
        crate::analyze::analyze_source(source).unwrap();
    }
}
