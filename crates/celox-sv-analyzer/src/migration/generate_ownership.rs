//! Generate ownership through production elaboration and lexical metadata.
use super::*;
use celox_sv_syntax::{BodyItemKind, Declarations, ExprKind, ForStep};

#[cfg(test)]
mod tests {
    use super::*;
    fn names(tree: &ValidatedSyntax) -> Vec<String> {
        let source = crate::ast::Source::from_syntax(tree).unwrap();
        source.modules()[0]
            .signals()
            .iter()
            .map(|s| s.name().to_owned())
            .collect()
    }
    #[test]
    fn native_generate_condition_selector_labels_and_loop_operands_drive_production() {
        for source in [
            "module M;if(2+1==1)begin:yes logic x;end else begin:no logic x;end endmodule",
            "module M;case(4+1)3:begin:yes logic x;end default:begin:no logic x;end endcase endmodule",
            "module M;case(3)(4+1):begin:yes logic x;end default:begin:no logic x;end endcase endmodule",
        ] {
            let mut tree =
                super::super::parse_source(source, Path::new("native-generate-values.sv")).unwrap();
            assert_eq!(names(&tree), ["no.x"]);
            for expression in &mut tree.declarations.expressions {
                if let ExprKind::Binary { operator, .. } = &mut expression.kind
                    && operator == "+"
                {
                    *operator = "-".into()
                }
            }
            assert_eq!(names(&tree), ["yes.x"]);
        }
        let mut tree = super::super::parse_source(
            "module M;for(genvar i=(2+1);i<(8+1);i+=(2+1))begin:g logic x;end endmodule",
            Path::new("native-generate-loop.sv"),
        )
        .unwrap();
        assert_eq!(names(&tree), ["g[3].x", "g[6].x"]);
        for expression in &mut tree.declarations.expressions {
            if let ExprKind::Binary { operator, .. } = &mut expression.kind
                && operator == "+"
            {
                *operator = "-".into()
            }
        }
        assert_eq!(
            names(&tree),
            (1..7).map(|i| format!("g[{i}].x")).collect::<Vec<_>>()
        );
    }
    #[test]
    fn native_genvar_step_operator_and_block_label_drive_production() {
        let mut tree = super::super::parse_source(
            "module M;for(genvar i=1;i<8;i+=1)begin:g logic x;end endmodule",
            Path::new("native-genvar-step.sv"),
        )
        .unwrap();
        for item in &mut tree.declarations.body_items {
            match &mut item.kind {
                BodyItemKind::Loop {
                    step: ForStep::Assignment { operator, .. },
                    ..
                } => *operator = "<<=".into(),
                BodyItemKind::Block {
                    label: Some(name), ..
                } => name.spelling = "h".into(),
                _ => {}
            }
        }
        assert_eq!(names(&tree), ["h[1].x", "h[2].x", "h[4].x"]);
    }
    #[test]
    fn native_generate_membership_is_authoritative_and_iteration_names_are_checked() {
        let mut tree = super::super::parse_source(
            "module M;if(1)begin:g logic x,y;int z;end endmodule",
            Path::new("native-gen-shape.sv"),
        )
        .unwrap();
        for item in &mut tree.declarations.body_items {
            if let BodyItemKind::Block { items, .. } = &mut item.kind {
                items.pop();
            }
        }
        assert_eq!(names(&tree), ["g.x", "g.y"]);
        let mut tree = super::super::parse_source(
            "module M;for(genvar i=0;i<1;i+=1)begin:g logic x;end endmodule",
            Path::new("native-gen-name.sv"),
        )
        .unwrap();
        let left = tree
            .declarations
            .body_items
            .iter()
            .find_map(|item| match item.kind {
                BodyItemKind::Loop {
                    step: ForStep::Assignment { left, .. },
                    ..
                } => Some(left),
                _ => None,
            })
            .unwrap();
        let tokens = tree.declarations.expression(left).tokens;
        for expression in &mut tree.declarations.expressions {
            if expression.tokens == tokens
                && let ExprKind::Name(name) = &mut expression.kind
            {
                name.spelling = "other".into()
            }
        }
        let error = crate::ast::Source::from_syntax(&tree).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("genvar update targets a different variable"),
            "{error}"
        );
    }
    #[test]
    fn deep_native_generate_condition_groups_bind_through_scope_jobs() {
        let path = Path::new("deep-native-gen-condition.sv");
        let mut tree =
            super::super::parse_source("module M;if(1)begin:g logic x;end endmodule", path)
                .unwrap();
        // Isolate the native parser/binder from the remaining full legacy
        // parser's nesting limit. Native IDs retain the same body/type shape.
        *tree.declarations = Declarations::parse(
            &format!(
                "module M;if({}1{})begin:g logic x;end endmodule",
                "(".repeat(10_000),
                ")".repeat(10_000)
            ),
            path,
        )
        .unwrap();
        assert_eq!(names(&tree), ["g.x"]);
    }
}

#[test]
fn native_branch_scope_ids_select_dpi_query_metadata_after_child_reordering() {
    let code = r#"module M;
        if(1) begin:first
            import "DPI-C" function int f();
            localparam int N=$bits(f());logic[N-1:0]x;
        end else begin:second
            import "DPI-C" function byte f();
            localparam int N=$bits(f());logic[N-1:0]x;
        end
        endmodule"#;
    let mut tree = parse_source(code, Path::new("native-generate-dpi-child.sv")).unwrap();
    for item in &mut tree.declarations.body_items {
        if let BodyItemKind::Conditional {
            then_body,
            otherwise: Some(otherwise),
            ..
        } = &mut item.kind
        {
            std::mem::swap(then_body, otherwise);
        }
    }
    let source = crate::ast::Source::from_syntax(&tree).unwrap();
    let ir = crate::analyze::analyze_source(source).unwrap();
    let signal = ir.modules()[0]
        .signals()
        .iter()
        .find(|signal| signal.name() == "second.x")
        .unwrap();
    assert_eq!(signal.r#type().resolved_width(), Some(8));
    assert_eq!(
        ir.modules()[0].dpi_imports()[0]
            .return_type()
            .unwrap()
            .width(),
        8
    );
}

#[test]
fn lexical_generate_query_metadata_preserves_regions_cases_and_loops() {
    let code = r#"module M;
        import "DPI-C" function int f();
        parameter int ROOT=$bits(f());
        generate
            case(1)
                1:begin:c
                    import "DPI-C" function byte f();
                    localparam int N=$bits(f());logic[N-1:0]x;
                end
                default:begin:no
                    import "DPI-C" function longint f();
                end
            endcase
            for(genvar i=0;i<2;i++)begin:loop_
                import "DPI-C" function shortint h();
                localparam int N=$bits(h());logic[N-1:0]x;
            end
        endgenerate
        endmodule"#;
    let tree = parse_source(code, Path::new("native-generate-query-kinds.sv")).unwrap();
    let source = crate::ast::Source::from_syntax_module_with_parameter_expr_overrides(
        &tree,
        "M",
        &HashMap::default(),
        &crate::ModuleInterfaces::default(),
    )
    .unwrap();
    let ir = crate::analyze::analyze_source(source).unwrap();
    let module = ir.modules().iter().find(|m| m.name() == "M").unwrap();
    assert_eq!(module.parameters()[0].resolved_value(), Some(32));
    for (name, width) in [("c.x", 8), ("loop_[0].x", 16), ("loop_[1].x", 16)] {
        let signal = module
            .signals()
            .iter()
            .find(|signal| signal.name() == name)
            .unwrap();
        assert_eq!(signal.r#type().resolved_width(), Some(width));
    }
}

#[test]
fn generate_membership_and_optional_children_are_authoritative() {
    for (code, change) in [
        ("module M;generate logic x;endgenerate endmodule", 0),
        ("module M;if(1)begin:g logic x;end endmodule", 1),
        ("module M;if(1)begin:g logic x;end endmodule", 2),
        (
            "module M;if(1)begin:g logic x;end else begin:h logic x;end endmodule",
            3,
        ),
        ("module M;case(1)1:begin:g logic x;end endcase endmodule", 4),
    ] {
        let mut tree = parse_source(code, Path::new("native-generate-membership.sv")).unwrap();
        for item in &mut tree.declarations.body_items {
            match (&mut item.kind, change) {
                (BodyItemKind::Region(items), 0) => items.clear(),
                (kind @ BodyItemKind::Conditional { .. }, 1)
                | (kind @ BodyItemKind::Block { .. }, 2) => {
                    *kind = BodyItemKind::Unconverted {
                        category: "missing generate identity".into(),
                        items: Vec::new(),
                    }
                }
                (BodyItemKind::Conditional { otherwise, .. }, 3) => *otherwise = None,
                (BodyItemKind::Case { arms, .. }, 4) => arms[0].colon = false,
                _ => {}
            }
        }
        if change == 4 {
            assert!(
                matches!(
                    crate::ast::Source::from_syntax(&tree),
                    Err(AnalyzerError::Parse(_))
                ),
                "{code}"
            );
        } else {
            let source = crate::ast::Source::from_syntax(&tree).unwrap();
            let names = source.modules()[0]
                .signals()
                .iter()
                .map(|signal| signal.name())
                .collect::<Vec<_>>();
            if change == 3 {
                assert_eq!(names, ["g.x"]);
            } else {
                assert!(names.is_empty(), "{code}: {names:?}");
            }
        }
    }
}
