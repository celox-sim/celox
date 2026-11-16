//! Actual caller-bound pattern structure/value/key/count ownership.
use super::*;
use crate::ast::{BoundPattern, Source};
use crate::{ModuleInterfaces, ParsedSource};
use celox_sv_syntax::ExprKind;
fn pattern(expression: &str) -> BoundPattern {
    let mut tree = parse_source(
        &format!("module M;C#(.P({expression}))c();endmodule"),
        Path::new("native-override-pattern.sv"),
    )
    .unwrap();
    for node in &mut tree.declarations.expressions {
        if let ExprKind::Binary { operator, .. } = &mut node.kind
            && operator == "+"
        {
            *operator = "-".into();
        }
    }
    Source::from_syntax(&tree).unwrap().modules()[0].instances()[0].parameter_overrides()[0]
        .bound_pattern()
        .unwrap()
        .clone()
}
fn analyze(pattern: BoundPattern, dimensions: &str) -> Result<crate::Ir, crate::AnalyzerError> {
    let child = ParsedSource::parse(
        &format!("module C#(parameter logic{dimensions} P='0);endmodule"),
        Path::new("native-pattern-child.sv"),
    )
    .unwrap();
    let packages = ParsedSource::analyze_packages(&[&child]).unwrap();
    child.analyze_module(
        "C",
        &HashMap::default(),
        &[],
        &[("P".into(), pattern)],
        &ModuleInterfaces::default(),
        &packages,
        &mut packages.design(),
    )
}
/// The left operands of the leaves `left - 2` of a pattern of bytes: the
/// pattern is the value of the packed parameter, most significant element
/// first.
fn leaves(pattern: BoundPattern, dimensions: &str) -> Vec<i128> {
    let ir = analyze(pattern, dimensions).unwrap();
    let parameter = ir.modules()[0]
        .parameters()
        .iter()
        .find(|parameter| parameter.name() == "P")
        .expect("the pattern gives a value parameter");
    let Some(crate::ir::ConstExpr::Literal(text)) = parameter.value() else {
        panic!("{:?}", parameter.value())
    };
    let literal = crate::typecheck::parse_integral_literal(text).unwrap();
    let value = u128::try_from(literal.value).unwrap();
    (0..literal.width / 8)
        .rev()
        .map(|element| ((value >> (8 * element)) & 0xff) as i128 + 2)
        .collect()
}
#[test]
fn native_pattern_list_and_keyed_values_bind_in_the_caller() {
    for (expression, expected) in [
        ("'{10+2,8+2}", vec![10, 8]),
        ("'{1+1:10+2,default:8+2}", vec![8, 10]),
    ] {
        assert_eq!(
            leaves(pattern(expression), "[1:0][7:0]"),
            expected,
            "{expression}"
        );
    }
}
#[test]
fn native_pattern_type_key_is_rejected_for_a_packed_integer_parameter() {
    // A type key covers array or structure elements, not the bits of a
    // packed integer parameter (10.9.1-2).
    let error = analyze(pattern("'{bit:0}"), "[7:0]").unwrap_err();
    assert!(
        error
            .to_string()
            .contains("type key in an integer assignment pattern"),
        "{error}"
    );
    assert!(analyze(pattern("'{7:1,default:0}"), "[7:0]").is_ok());
}
#[test]
fn native_pattern_repetition_count_and_leaf_operators_drive_specialization() {
    assert_eq!(leaves(pattern("'{3+1{10+2}}"), "[1:0][7:0]"), vec![10, 10]);
}
#[test]
fn native_nested_pattern_structure_preserves_element_order() {
    assert_eq!(
        leaves(pattern("'{'{10+2,8+2},'{6+2,4+2}}"), "[1:0][1:0][7:0]"),
        vec![10, 8, 6, 4]
    );
}

#[test]
fn native_pattern_keys_and_item_counts_are_authoritative() {
    for keyed in [false, true] {
        let mut tree = parse_source(
            "module M;C#(.P('{0:1,default:2}))c();endmodule",
            Path::new("native-pattern-guard.sv"),
        )
        .unwrap();
        for node in &mut tree.declarations.expressions {
            if let ExprKind::Pattern { items, .. } = &mut node.kind {
                if keyed {
                    items[0].key = None;
                } else {
                    items.pop();
                }
            }
        }
        if keyed {
            assert!(Source::from_syntax(&tree).is_err());
        } else {
            let source = Source::from_syntax(&tree).unwrap();
            assert_eq!(
                source.modules()[0].instances()[0].parameter_overrides()[0].bound_pattern(),
                Some(&pattern("'{0:1}"))
            );
        }
    }
}
