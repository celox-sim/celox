//! Parameter owners reach production consumers without a source-wide lookup.
use super::*;
use crate::ast::Source;
use celox_sv_syntax::{BodyItemKind, ParameterKind};

#[test]
fn native_parameter_membership_controls_body_order_and_header_roles() {
    let mut tree = parse_source(
        "module M#(parameter A=1,localparam B=2,parameter C=3);parameter X=4;parameter Y=5;endmodule",
        Path::new("native-parameter-order.sv"),
    ).unwrap();
    tree.declarations.units[0].items.swap(0, 1);
    let source = Source::from_syntax(&tree).unwrap();
    let parameters = source.modules()[0].parameters();
    assert_eq!(
        parameters.iter().map(|p| p.name()).collect::<Vec<_>>(),
        ["A", "B", "C", "Y", "X"]
    );
    for name in ["B", "X", "Y"] {
        let overrides = [(name.into(), 9)].into_iter().collect();
        assert!(matches!(
            Source::from_syntax_with_module_parameter_overrides(&tree, "M", &overrides),
            Err(AnalyzerError::LocalParameterOverride { .. })
        ));
    }
    let overrides = ["A", "C"]
        .into_iter()
        .map(|name| (name.into(), 9))
        .collect();
    let source =
        Source::from_syntax_with_module_parameter_overrides(&tree, "M", &overrides).unwrap();
    let ir = crate::analyze::analyze_source(source).unwrap();
    assert_eq!(
        ir.modules()[0]
            .parameters()
            .iter()
            .map(|p| p.resolved_value())
            .collect::<Vec<_>>(),
        [Some(9), Some(2), Some(9), Some(5), Some(4)]
    );
}

#[test]
fn missing_native_parameter_membership_or_default_is_never_retried() {
    let code = "module M#(parameter A=1);localparam B=2;endmodule";
    for edit in 0..3 {
        let mut tree = parse_source(code, Path::new("native-parameter-missing.sv")).unwrap();
        match edit {
            0 => tree.declarations.units[0]
                .parameters
                .as_mut()
                .unwrap()
                .clear(),
            1 => {
                let item = tree
                    .declarations
                    .body_items
                    .iter_mut()
                    .find(|item| matches!(item.kind, BodyItemKind::Parameter(_)))
                    .unwrap();
                item.kind = BodyItemKind::Unconverted {
                    category: "missing parameter".into(),
                    items: Vec::new(),
                };
            }
            _ => {
                let ParameterKind::Value { default, .. } =
                    &mut tree.declarations.parameter_declarations[0].parameters[0].kind
                else {
                    panic!()
                };
                *default = None;
            }
        }
        let source = Source::from_syntax(&tree).unwrap();
        let ir = crate::analyze::analyze_source(source).unwrap();
        let actual = ir.modules()[0]
            .parameters()
            .iter()
            .map(|parameter| (parameter.name(), parameter.resolved_value()))
            .collect::<Vec<_>>();
        let expected = match edit {
            0 => vec![("B", Some(2))],
            1 => vec![("A", Some(1))],
            _ => vec![("A", Some(1)), ("B", None)],
        };
        assert_eq!(actual, expected, "edit{edit}");
    }
}

#[test]
fn unused_class_header_parameter_selection_retains_original_validation() {
    let tree = parse_source(
        "module M;localparam logic[1:0][3:0]A=8'hab;class C#(parameter P=A[0][1]);endclass endmodule",
        Path::new("unused-class-parameter-selection.sv"),
    ).unwrap();
    Source::from_syntax(&tree).unwrap();
    let tree = parse_source(
        "module M;localparam logic[1:0][3:0]A=8'hab;class C#(parameter P=A[0][unknown()]);endclass endmodule",
        Path::new("unused-class-parameter-diagnostic.sv"),
    ).unwrap();
    assert!(Source::from_syntax(&tree).is_err());
}
