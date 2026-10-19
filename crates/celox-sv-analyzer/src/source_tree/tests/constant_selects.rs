//! Production native constant-selection ownership/compatibility regressions.
use super::*;
use crate::ast::{BinaryOp, ConstExpr, Source};
use celox_sv_syntax::{Declarations, DimensionKind, ExprKind};
fn subtract(tree: &mut SourceTree) {
    for expression in &mut tree.declarations.expressions {
        if let ExprKind::Binary { operator, .. } = &mut expression.kind
            && operator == "+"
        {
            *operator = "-".into();
        }
    }
}
#[test]
fn native_constant_indices_drive_header_values_widths_and_overrides() {
    let mut tree=parse_source("module M#(parameter logic[7:0]P=8'b10,parameter int I=2,parameter int N=P[I+1])(output logic[N:0]y);assign y=0;endmodule",Path::new("native-constant-select.sv")).unwrap();
    subtract(&mut tree);
    for (override_, expected) in [(None, 1), (Some(4), 0)] {
        let overrides = override_
            .map(|value| HashMap::from_iter([("I".into(), value)]))
            .unwrap_or_default();
        let source =
            Source::from_syntax_with_module_parameter_overrides(&tree, "M", &overrides).unwrap();
        let ir = crate::analyze::analyze_source(source).unwrap();
        assert_eq!(
            ir.modules()[0].parameters()[2].resolved_value(),
            Some(expected)
        );
        assert_eq!(
            ir.modules()[0].ports()[0].r#type().resolved_width(),
            Some(expected as usize + 1)
        );
    }
}
#[test]
fn native_constant_bounds_extract_multidimensional_parameter_elements() {
    let mut tree = parse_source(
        "module M;parameter logic[1:0][3:0]A=8'hab;wire[A[1+1][1+0]:0]x;assign x=0;endmodule",
        Path::new("native-constant-element.sv"),
    )
    .unwrap();
    subtract(&mut tree);
    let source = Source::from_syntax(&tree).unwrap();
    let ir = crate::analyze::analyze_source(source).unwrap();
    let x = ir.modules()[0]
        .signals()
        .iter()
        .find(|signal| signal.name() == "x")
        .unwrap();
    assert_eq!(x.r#type().resolved_width(), Some(2));
}
#[test]
fn native_constant_selected_names_cover_packages_and_escaped_identifiers() {
    for code in [
        "package p;parameter logic[7:0]P=8'b10;endpackage module M;parameter N=p::P[2+1];endmodule",
        r"module M;parameter logic[7:0] \p.q =8'b10;parameter N=\p.q [2+1];endmodule",
    ] {
        let mut tree = parse_source(code, Path::new("native-constant-select-names.sv")).unwrap();
        subtract(&mut tree);
        let source = Source::from_syntax(&tree).unwrap();
        let ir = crate::analyze::analyze_source(source).unwrap();
        assert_eq!(
            ir.modules()[0]
                .parameters()
                .last()
                .unwrap()
                .resolved_value(),
            Some(1)
        );
    }
}
#[test]
fn native_lightweight_value_indices_drive_instance_overrides() {
    let mut tree=parse_source("module C#(parameter N=0)();endmodule module M;parameter logic[7:0]P=8'b10;C#(.N(P[2+1]))child();endmodule",Path::new("native-value-select.sv")).unwrap();
    subtract(&mut tree);
    let source = Source::from_syntax(&tree).unwrap();
    let module = source
        .modules()
        .iter()
        .find(|module| module.name() == "M")
        .unwrap();
    let value = module.instances()[0].parameter_overrides()[0]
        .value()
        .unwrap();
    let ConstExpr::Select { bit, .. } = value else {
        panic!("{value:?}")
    };
    assert!(matches!(
        bit.as_ref(),
        ConstExpr::Binary {
            op: BinaryOp::Sub,
            ..
        }
    ));
    crate::analyze::analyze_source(source).unwrap();
}
#[test]
fn native_constant_selects_follow_native_name_count_and_index_class() {
    for change in [0, 1, 2] {
        let mut tree = parse_source(
            "module M;parameter logic[1:0][3:0]P=8'hab;wire[P[0][1]:0]x;assign x=0;endmodule",
            Path::new("native-constant-select-identity.sv"),
        )
        .unwrap();
        let ids = tree
            .declarations
            .expressions
            .iter()
            .filter_map(|expression| match &expression.kind {
                ExprKind::Select {
                    expression,
                    dimension,
                } => Some((*expression, dimension.kind.clone())),
                _ => None,
            })
            .collect::<Vec<_>>();
        if change == 0 {
            for expression in &mut tree.declarations.expressions {
                if let ExprKind::Name(name) = &mut expression.kind
                    && name.spelling == "P"
                {
                    name.spelling = "other".into();
                }
            }
        } else if change == 1 {
            let range = tree.declarations().expression(ids[1].0).tokens;
            let ExprKind::Select {
                expression: base, ..
            } = tree.declarations().expression(ids[1].0).kind
            else {
                unreachable!()
            };
            for expression in &mut tree.declarations.expressions {
                if expression.tokens == range {
                    continue;
                }
                if let ExprKind::Select { expression, .. } = &mut expression.kind
                    && *expression == ids[1].0
                {
                    *expression = base;
                }
            }
        } else {
            for expression in &mut tree.declarations.expressions {
                if let ExprKind::Select { dimension, .. } = &mut expression.kind
                    && let DimensionKind::Index(index) = dimension.kind
                {
                    dimension.kind = DimensionKind::Range {
                        left: index,
                        right: index,
                    };
                    break;
                }
            }
        }
        if change == 1 {
            let source = Source::from_syntax(&tree).unwrap();
            assert_eq!(
                crate::typecheck::eval_const_expr(
                    &source.modules()[0].signals()[0].r#type().packed_ranges()[0]
                        .left()
                        .clone()
                        .into(),
                    &HashMap::default()
                ),
                Some(10)
            );
        } else {
            assert!(Source::from_syntax(&tree).is_err(), "{change}");
        }
    }
}
#[test]
fn deep_native_constant_indices_bind_on_the_default_stack() {
    let path = Path::new("deep-native-constant-index.sv");
    let mut tree = parse_source(
        "module M;parameter logic[7:0]P=8'b10;parameter N=P[(2+1)];endmodule",
        path,
    )
    .unwrap();
    *tree.declarations = Declarations::parse(
        &format!(
            "module M;parameter logic[7:0]P=8'b10;parameter N=P[{}2+1{}];endmodule",
            "(".repeat(10_000),
            ")".repeat(10_000)
        ),
        path,
    )
    .unwrap();
    subtract(&mut tree);
    let source = Source::from_syntax(&tree).unwrap();
    let ir = crate::analyze::analyze_source(source).unwrap();
    assert_eq!(ir.modules()[0].parameters()[1].resolved_value(), Some(1));
}
