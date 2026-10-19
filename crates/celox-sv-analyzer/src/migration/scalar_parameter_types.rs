//! Native scalar declaration metadata/widths through actual parameter binding.
use super::*;
use crate::ast::Source;
use celox_sv_syntax::{DataTypeKind, ExprKind};
fn parsed(code: &str) -> ValidatedSyntax {
    let mut tree = parse_source(code, Path::new("native-scalar-parameter-type.sv")).unwrap();
    for node in &mut tree.declarations.expressions {
        if let ExprKind::Binary { operator, .. } = &mut node.kind
            && operator == "+"
        {
            *operator = "-".into();
        }
    }
    tree
}
fn ir(tree: &ValidatedSyntax) -> crate::ir::Ir {
    crate::analyze::analyze_source(Source::from_syntax(tree).unwrap()).unwrap()
}
#[test]
fn native_scalar_declared_bounds_control_header_and_body_value_coercion() {
    for code in [
        "module M#(parameter logic[3+1:0]P=8'hff)();endmodule",
        "module M#(logic[3+1:0]P=8'hff)();endmodule",
        "module M;localparam logic[3+1:0]P=8'hff;endmodule",
        "module M;localparam[3+1:0]P='1;endmodule",
    ] {
        let ir = ir(&parsed(code));
        let parameter = &ir.modules()[0].parameters()[0];
        assert_eq!(parameter.declared_width(), Some(3), "{code}");
        assert_eq!(parameter.resolved_width(), Some(3), "{code}");
        assert_eq!(parameter.resolved_value(), Some(7), "{code}");
    }
}
#[test]
fn native_scalar_bound_environment_respects_module_parameter_overrides() {
    let tree = parsed("module M#(parameter int W=4,parameter logic[W+1:0]P='1)();endmodule");
    for width in [4, 8] {
        let source = Source::from_syntax_with_module_parameter_overrides(
            &tree,
            "M",
            &HashMap::from_iter([("W".into(), width)]),
        )
        .unwrap();
        let ir = crate::analyze::analyze_source(source).unwrap();
        let parameter = &ir.modules()[0].parameters()[1];
        assert_eq!(parameter.declared_width(), Some(width as usize));
        assert_eq!(parameter.resolved_value(), Some((1 << width) - 1));
    }
}
#[test]
fn native_scalar_alias_enclosure_and_signed_elements_remain_separate() {
    let tree = parsed(
        "module M;typedef bit signed[3:0]T;parameter T[3+1:1]P='1;parameter int N=P[1];endmodule",
    );
    let ir = ir(&tree);
    let parameters = ir.modules()[0].parameters();
    assert_eq!(parameters[0].declared_width(), Some(8));
    assert_eq!(parameters[0].declared_signed(), Some(false));
    assert_eq!(parameters[0].resolved_value(), Some(255));
    assert_eq!(parameters[1].resolved_value(), Some(-1));
}
#[test]
fn native_scalar_integer_atom_signing_and_state_kind_affect_resolved_values() {
    let mut signed = parsed("module M;parameter int P='1;endmodule");
    for data in &mut signed.declarations.types {
        if matches!(&data.kind,DataTypeKind::Builtin(name) if name=="int") {
            data.signed = Some(false);
        }
    }
    let ir = ir(&signed);
    let parameter = &ir.modules()[0].parameters()[0];
    assert_eq!(parameter.declared_signed(), Some(false));
    assert_eq!(parameter.resolved_value(), Some(0xffff_ffff));
    let mut state = parsed("module M;parameter integer P=32'hx;endmodule");
    for data in &mut state.declarations.types {
        if let DataTypeKind::Builtin(name) = &mut data.kind
            && name == "integer"
        {
            *name = "int".into();
        }
    }
    let ir = crate::analyze::analyze_source(Source::from_syntax(&state).unwrap()).unwrap();
    assert_eq!(ir.modules()[0].parameters()[0].resolved_value(), Some(0));
}
#[test]
fn native_scalar_implicit_type_controls_inferred_parameter_type() {
    let mut tree = parsed("module M;parameter int P=1;endmodule");
    for data in &mut tree.declarations.types {
        if matches!(data.kind, DataTypeKind::Builtin(_)) {
            data.kind = DataTypeKind::Implicit;
        }
    }
    let ir = ir(&tree);
    let parameter = &ir.modules()[0].parameters()[0];
    assert_eq!(parameter.declared_width(), None);
    assert_eq!(parameter.resolved_width(), Some(32));
    assert_eq!(parameter.resolved_value(), Some(1));
}
#[test]
fn native_scalar_extreme_declared_widths_report_materialization_errors() {
    let tree =
        parsed("module M;localparam logic[128'h7fffffffffffffffffffffffffffffff:0]P='0;endmodule");
    let error = Source::from_syntax(&tree).unwrap_err();
    assert!(
        error.to_string().contains("materialization limit"),
        "{error}"
    );
}
#[test]
fn scalar_datatype_bearing_bounds_keep_their_explicit_original_width_owner() {
    let tree = parsed("module M;localparam logic[$bits(logic[3:0]):0]P='0;endmodule");
    ir(&tree);
}
