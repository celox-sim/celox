//! Native typed caller patterns reuse the existing constant fold/shape kernel.
use super::*;
use crate::ast::{BoundPattern, Source};
use crate::{ModuleInterfaces, ParsedSource};
use celox_sv_syntax::{DataTypeKind, ExprKind};
fn bound(declaration: &str, expression: &str, atom: bool) -> BoundPattern {
    let mut tree = parse_source(
        &format!("module M;{declaration}C#(.P({expression}))c();endmodule"),
        Path::new("native-constant-typed-pattern.sv"),
    )
    .unwrap();
    for node in &mut tree.declarations.expressions {
        if let ExprKind::Binary { operator, .. } = &mut node.kind
            && operator == "+"
        {
            *operator = "-".into();
        }
    }
    if atom {
        for ty in &mut tree.declarations.types {
            if let DataTypeKind::Builtin(name) = &mut ty.kind
                && name == "byte"
            {
                *name = "shortint".into();
            }
        }
    }
    Source::from_syntax(&tree).unwrap().modules()[0].instances()[0].parameter_overrides()[0]
        .bound_pattern()
        .unwrap()
        .clone()
}
fn initialized(pattern: BoundPattern, declaration: &str) -> Vec<i128> {
    let child = ParsedSource::parse(
        &format!("module C#(parameter {declaration});endmodule"),
        Path::new("native-constant-pattern-child.sv"),
    )
    .unwrap();
    let packages = ParsedSource::analyze_packages(&[&child]).unwrap();
    let ir = child
        .analyze_module(
            "C",
            &HashMap::default(),
            &[],
            &[("P".into(), pattern)],
            &ModuleInterfaces::default(),
            &packages,
        )
        .unwrap();
    let crate::ir::Stmt::Assign { rhs, .. } = &ir.modules()[0].initial_processes()[0].body()[0]
    else {
        panic!()
    };
    let mut pending = vec![rhs];
    let mut values = Vec::new();
    while let Some(value) = pending.pop() {
        match value {
            crate::ir::Expr::Unary {
                op: crate::ir::UnaryOp::ToTwoState,
                expr,
            } => pending.push(expr),
            crate::ir::Expr::Concat(parts) => pending.extend(parts.iter().rev()),
            crate::ir::Expr::Resize { expr, .. } => pending.push(expr),
            crate::ir::Expr::Literal(text) => {
                let literal = crate::typecheck::parse_integral_literal(text).unwrap();
                values.push(crate::typecheck::integral_literal_as_i128(&literal, false).unwrap());
            }
            _ => panic!("{value:?}"),
        }
    }
    values
}
#[test]
fn native_constant_typed_alias_and_struct_pattern_values_reach_child_storage() {
    for (declaration, value) in [
        ("typedef logic[1:0][7:0]T;", "T'{10+2,8+2}"),
        (
            "typedef struct packed{logic[7:0]x;logic[7:0]z;}T;",
            "T'{x:10+2,z:8+2}",
        ),
    ] {
        assert_eq!(
            initialized(bound(declaration, value, false), "logic[15:0]P='0"),
            [0x0806]
        );
    }
}
#[test]
fn native_constant_typed_atom_target_metadata_reaches_folded_value() {
    assert_eq!(
        initialized(bound("", "byte'{default:1}", true), "logic[15:0]P='0"),
        [0xffff]
    );
}
#[test]
fn native_constant_typed_unpacked_patterns_preserve_element_conversion_and_order() {
    assert_eq!(
        initialized(
            bound("typedef byte T[2];", "T'{10+2,8+2}", false),
            "byte P[2]='{0,0}"
        ),
        [6, 8]
    );
}
#[test]
fn native_constant_typed_pattern_missing_items_never_retry_original_values() {
    let mut tree = parse_source(
        "module M;typedef logic[1:0][7:0]T;C#(.P(T'{10,8}))c();endmodule",
        Path::new("native-constant-pattern-guard.sv"),
    )
    .unwrap();
    for expr in &mut tree.declarations.expressions {
        if let ExprKind::Pattern { items, .. } = &mut expr.kind {
            items.pop();
        }
    }
    assert!((Source::from_syntax(&tree)).is_err());
}

#[test]
fn unsupported_constant_typed_target_forms_keep_their_diagnostic() {
    let tree = parse_source(
        "module M;typedef logic[1:0]T;C#(.P(type(T)'{default:1}))c();endmodule",
        Path::new("native-constant-pattern-target.sv"),
    )
    .unwrap();
    let error = Source::from_syntax(&tree).unwrap_err();
    assert!(matches!(error, AnalyzerError::Unsupported(_)));
    assert!(
        error.to_string().contains("assignment pattern type"),
        "{error}"
    );
}
