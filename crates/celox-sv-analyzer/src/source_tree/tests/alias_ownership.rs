//! Alias body identity through production typedef and enum consumers.
use super::*;
use crate::ast::Source;
use celox_sv_syntax::{BodyItemKind, ExprKind, TypeAliasTarget};

fn parsed(code: &str) -> SourceTree {
    let mut tree = parse_source(code, Path::new("native-alias-ownership.sv")).unwrap();
    for expression in &mut tree.declarations.expressions {
        if let ExprKind::Binary { operator, .. } = &mut expression.kind
            && operator == "+"
        {
            *operator = "-".into();
        }
    }
    tree
}
fn number(value: &crate::ast::ConstExpr) -> i128 {
    crate::typecheck::eval_const_expr(&value.clone().into(), &HashMap::default()).unwrap()
}

#[test]
fn native_alias_membership_orders_typed_operands_without_an_original_lookup() {
    let mut tree =
        parsed("module M;typedef logic[10+2:0]A;typedef logic[8+2:0]B;A a;B b;endmodule");
    tree.declarations.units[0].items.swap(0, 1);
    let source = Source::from_syntax(&tree).unwrap();
    let signals = source.modules()[0].signals();
    for (name, left) in [("a", 8), ("b", 6)] {
        let signal = signals.iter().find(|v| v.name() == name).unwrap();
        assert_eq!(number(signal.r#type().packed_ranges()[0].left()), left);
    }
}

#[test]
fn alias_consumers_require_native_body_identity_target_and_dimension_class() {
    for code in [
        "module M;typedef logic[3:0]T;T value;endmodule",
        "module M(output logic[3:0]value);typedef enum logic[3:0]{A=1}T;assign value=A;endmodule",
    ] {
        let mut tree = parsed(code);
        let item = tree
            .declarations
            .body_items
            .iter_mut()
            .find(|item| matches!(item.kind, BodyItemKind::TypeAlias(_)))
            .unwrap();
        item.kind = BodyItemKind::Unconverted {
            category: "missing alias".into(),
            items: Vec::new(),
        };
        let result = Source::from_syntax(&tree);
        if let Ok(source) = result {
            assert!(
                source.modules()[0]
                    .signals()
                    .iter()
                    .all(|signal| signal.name() != "value")
            );
        }
        let mut tree = parsed(code);
        tree.declarations.type_aliases[0].target = TypeAliasTarget::Forward(None);
        let result = Source::from_syntax(&tree);
        if let Ok(source) = result {
            assert!(
                source.modules()[0]
                    .signals()
                    .iter()
                    .all(|signal| signal.name() != "value")
            );
        }
    }
    let mut tree = parsed("module M;typedef logic T[2];T value;endmodule");
    tree.declarations.type_aliases[0].dimensions.clear();
    let source = Source::from_syntax(&tree).unwrap();
    assert!(
        source.modules()[0].signals()[0]
            .r#type()
            .unpacked_ranges()
            .is_empty()
    );
}

#[test]
fn native_alias_ids_preserve_compilation_package_region_and_interface_scopes() {
    let tree = parsed(
        "typedef logic[12+2:0]U;package p;typedef logic[10+2:0]P;endpackage interface I;typedef logic[8+2:0]T;T a;endinterface module M;U u;p::P p_;generate typedef logic[6+2:0]R;R r;endgenerate I bus();endmodule",
    );
    let source = Source::from_syntax_module_with_parameter_expr_overrides(
        &tree,
        "M",
        &HashMap::default(),
        &crate::ModuleInterfaces::default(),
    )
    .unwrap();
    let module = source.modules().iter().find(|v| v.name() == "M").unwrap();
    for (name, left) in [("u", 10), ("p_", 8), ("r", 4), ("bus$a", 6)] {
        let signal = module.signals().iter().find(|v| v.name() == name).unwrap();
        assert_eq!(number(signal.r#type().packed_ranges()[0].left()), left);
    }
    crate::analyze::analyze_source(source).unwrap();
}
