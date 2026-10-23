use std::{fmt::Write, path::Path};

use super::*;

#[test]
fn unresolved_local_parameter_masks_imported_bindings() {
    let tree = crate::source_tree::parse_scope_fixture(
        "module C #(parameter logic [7:0] W, parameter N = W[3:0]); endmodule",
        Path::new("required_local_parameter.sv"),
    )
    .unwrap();
    let tree = AnalysisContext::unspecialized(&tree);
    let state = tree.state.as_ref();
    let imported = scope::ScopeSymbols {
        const_env: [("W".into(), 18)].into_iter().collect(),
        parameter_values: [("W".into(), Expr::Literal("8'd18".into()))]
            .into_iter()
            .collect(),
        ..Default::default()
    };
    let _scope = scope::install(&tree.state, Arc::new(imported.clone()));
    let parameters = parameters_from_scope(
        &tree,
        &HashMap::default(),
        &imported.const_env,
        &HashMap::default(),
    )
    .unwrap();
    assert!(parameters[0].value().is_none());
    assert!(
        parameters[1].value().is_none(),
        "required W must not use an imported default"
    );
    let values =
        parameter_value_env_with_imported(state, &parameters, &imported.const_env, &imported);
    assert!(!values.contains_key("W"));
    let mut numeric = imported.const_env.clone();
    extend_const_env_with_parameters(state, &mut numeric, &parameters);
    assert!(!numeric.contains_key("W"));
    let overrides = [("W".into(), ConstExpr::Literal("21".into()))]
        .into_iter()
        .collect();
    let specialized =
        parameters_from_scope(&tree, &HashMap::default(), &imported.const_env, &overrides).unwrap();
    let values = const_env_from_parameters(state, &specialized);
    assert_eq!(values["W"], 21);
    assert_eq!(values["N"], 5);
}

#[test]
fn short_circuit_proof_does_not_inherit_a_required_parameter_literal() {
    let tree = crate::source_tree::parse_scope_fixture(
        "module C #(parameter logic FLAG, parameter N = FLAG && 1000000000'd0); endmodule",
        Path::new("required_short_circuit_flag.sv"),
    )
    .unwrap();
    let tree = AnalysisContext::unspecialized(&tree);
    let imported = scope::ScopeSymbols {
        parameter_values: [("FLAG".into(), Expr::Literal("1'b0".into()))]
            .into_iter()
            .collect(),
        ..Default::default()
    };
    let _scope = scope::install(&tree.state, Arc::new(imported));
    let error = parameters_from_scope(
        &tree,
        &HashMap::default(),
        &HashMap::default(),
        &HashMap::default(),
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("constant materialization limit"),
        "{error}"
    );
}

#[test]
fn typed_constant_lookup_matches_full_tables_and_visits_only_referenced_names() {
    use std::cell::Cell;

    let mut env = HashMap::from_iter([("A".into(), -1), ("B".into(), 3)]);
    insert_parameter_type_markers(
        &mut env,
        "A",
        ExprType {
            width: 8,
            signed: true,
        },
    );
    insert_parameter_type_markers(
        &mut env,
        "B",
        ExprType {
            width: 4,
            signed: false,
        },
    );
    for index in 0..4096 {
        insert_parameter_type_markers(
            &mut env,
            &format!("unrelated{index}"),
            ExprType {
                width: 32,
                signed: false,
            },
        );
    }
    let a = || Box::new(ConstExpr::Ident("A".into()));
    let b = || Box::new(ConstExpr::Ident("B".into()));
    let cases = [
        (ConstExpr::Literal("'x".into()), 0),
        (*a(), 1),
        (ConstExpr::Ident("unknown".into()), 1),
        (
            ConstExpr::Select {
                expr: a(),
                bit: b(),
            },
            2,
        ),
        (
            ConstExpr::Function {
                callee: Callee::Subroutine("f".into()),
                args: vec![*a(), *b()],
                site: Some(17),
            },
            2,
        ),
        (
            ConstExpr::Unary {
                op: UnaryOp::Minus,
                expr: a(),
            },
            1,
        ),
        (
            ConstExpr::Binary {
                left: a(),
                op: BinaryOp::Add,
                right: b(),
            },
            2,
        ),
        (
            ConstExpr::Mux {
                condition: b(),
                then_expr: a(),
                else_expr: Box::new(ConstExpr::Literal("'z".into())),
            },
            2,
        ),
    ];
    let types = parameter_types_from_const_env(&env);
    for (expr, expected_lookups) in cases {
        let lookups = Cell::new(0);
        let direct = substitute_typed_parameter_literals_with_lookup(expr.clone(), &env, &|name| {
            lookups.set(lookups.get() + 1);
            parameter_type_from_const_env(&env, name)
        });
        assert_eq!(
            direct,
            substitute_typed_parameter_literals(expr, &env, &types)
        );
        assert_eq!(lookups.get(), expected_lookups);
    }
}

#[test]
fn incremental_collection_binds_each_prefix_entry_once() {
    let mut code = String::from("module Top;\n");
    for i in 0..128 {
        let value = if i == 0 {
            "1".into()
        } else {
            format!("P{} + 1", i - 1)
        };
        writeln!(code, "localparam int P{i} = {value};").unwrap();
    }
    code.push_str("endmodule\n");
    let tree =
        crate::source_tree::parse_scope_fixture(&code, Path::new("parameter_prefix.sv")).unwrap();
    let tree = AnalysisContext::unspecialized(&tree);
    let state = tree.state.as_ref();
    PARAMETER_BINDINGS.with(|count| count.set(0));
    let parameters = parameters_from_scope(
        &tree,
        &HashMap::default(),
        &HashMap::default(),
        &HashMap::default(),
    )
    .unwrap();
    assert_eq!(PARAMETER_BINDINGS.with(|count| count.get()), 128);
    assert_eq!(parameters.len(), 128);
    assert_eq!(const_env_from_parameters(state, &parameters)["P127"], 128);
    PARAMETER_BINDINGS.with(|count| count.set(0));
    let enums = enum_member_constants_from_scope(
        &tree,
        &HashMap::default(),
        &HashMap::default(),
        &HashMap::default(),
    )
    .unwrap();
    assert!(enums.numbers.is_empty());
    assert_eq!(PARAMETER_BINDINGS.with(|count| count.get()), 0);
}

#[test]
fn incremental_prefix_matches_rebuilding_with_inherited_values_and_overrides() {
    let tree = crate::source_tree::parse_scope_fixture(
        r#"
        module Top;
            typedef logic signed [3:0] nibble_t;
            parameter N = 4;
            parameter logic signed [N-1:0] MASK = '1;
            parameter SIZE = $bits(MASK);
            parameter CAST = 3'(N + 1);
            parameter nibble_t NEG = -1;
            parameter bit [3:0] TWO = 'x;
            parameter UNKNOWN = 4'bxxxx;
            parameter FORWARD = LATER + 1;
            parameter LATER = 7;
        endmodule
    "#,
        Path::new("parameter_prefix_equivalence.sv"),
    )
    .unwrap();
    let tree = AnalysisContext::unspecialized(&tree);
    let mut base =
        HashMap::from_iter([("N".into(), 99), ("MASK".into(), 255), ("LATER".into(), 9)]);
    insert_parameter_type_markers(
        &mut base,
        "MASK",
        ExprType {
            width: 32,
            signed: false,
        },
    );
    let aliases = type_aliases_from_scope_with_env(&tree, &base).unwrap();
    for n in [8, 3, 16, 8] {
        let overrides = HashMap::from_iter([("N".into(), ConstExpr::Literal(n.to_string()))]);
        let incremental = parameters_from_scope(&tree, &aliases, &base, &overrides).unwrap();
        let mut rebuilt = Vec::new();
        for id in tree.body_membership().unwrap() {
            if !matches!(
                tree.declarations().body_item(id).kind,
                celox_sv_syntax::BodyItemKind::Parameter(_)
            ) {
                continue;
            }
            parameters_from_native(
                &ownership::body(id, &tree).unwrap(),
                &tree,
                &mut rebuilt,
                false,
                &base,
                &aliases,
                &overrides,
            )
            .unwrap();
        }
        assert_eq!(incremental, rebuilt);
        assert_eq!(incremental[1].declared_width(), Some(n));
        assert_eq!(
            incremental[2].value(),
            Some(&ConstExpr::Literal(n.to_string()))
        );
        assert_eq!(incremental[4].declared_signed(), Some(true));
        assert_eq!(
            incremental[6].value(),
            Some(&ConstExpr::Literal("4'bxxxx".into()))
        );
        assert_eq!(
            incremental[7].value(),
            Some(&ConstExpr::Binary {
                left: Box::new(ConstExpr::Ident("LATER".into())),
                op: BinaryOp::Add,
                right: Box::new(ConstExpr::Literal("1".into())),
            })
        );
    }
}

#[test]
fn parameter_prefix_is_fresh_for_each_specialization() {
    let source = crate::ParsedSource::parse(
        r#"
        module Top #(parameter N = 4, parameter M = N + 1,
                     parameter logic signed [M-1:0] MASK = '1)
                    (output logic [31:0] bits);
            assign bits = $bits(MASK);
        endmodule
    "#,
        Path::new("parameter_prefix_specializations.sv"),
    )
    .unwrap();
    for n in [4, 12, 2, 4] {
        let ir = source
            .analyze_module_with_parameter_expr_overrides(
                "Top",
                &HashMap::from_iter([("N".into(), crate::ir::ConstExpr::Literal(n.to_string()))]),
                &ModuleInterfaces::default(),
            )
            .unwrap();
        assert_eq!(
            ir.modules()[0].parameters()[2].declared_width(),
            Some(n + 1)
        );
        assert_eq!(ir.modules()[0].parameters()[2].resolved_value(), Some(-1));
        assert_eq!(
            ir.modules()[0].assignments()[0].rhs(),
            &crate::ir::Expr::Literal((n + 1).to_string())
        );
    }
}

#[test]
fn collects_the_declarations_of_a_package() {
    let code = "package p;\n\
                localparam int W = 4;\n\
                parameter int N = W * 2;\n\
                typedef logic [W-1:0] word_t;\n\
                typedef enum logic [1:0] { IDLE, RUN = 2 } state_t;\n\
                function automatic int twice(int x); return x * 2; endfunction\n\
                endpackage\n";
    let tree = crate::source_tree::parse_scope_fixture(code, Path::new("package.sv")).unwrap();
    let tree = AnalysisContext::unspecialized(&tree);
    let state = tree.state.as_ref();
    let parameters = parameters_from_scope(
        &tree,
        &HashMap::default(),
        &HashMap::default(),
        &HashMap::default(),
    )
    .unwrap();
    let env = const_env_from_parameters(state, &parameters);
    assert_eq!((env["W"], env["N"]), (4, 8));
    let aliases = type_aliases_from_scope_with_env(&tree, &env).unwrap();
    assert_eq!(aliases["word_t"].packed_ranges().len(), 1);
    let enums =
        enum_member_constants_from_scope(&tree, &env, &aliases, &HashMap::default()).unwrap();
    assert_eq!((enums.numbers["IDLE"], enums.numbers["RUN"]), (0, 2));
    let items = generate::items(&tree, &env, &aliases).unwrap();
    assert_eq!(items.len(), 5);
    let membership = tree.body_membership().unwrap();
    assert!(items.iter().all(|item| membership.contains(&item.id)));
}

#[test]
fn numeric_ranged_headers_do_not_copy_the_environment() {
    for count in [16, 64, 256] {
        let mut code = String::from("module Top();\n");
        for index in 0..count {
            let value = if index == 0 {
                "1".into()
            } else {
                format!("P{} + 1", index - 1)
            };
            writeln!(code, "localparam logic [31:0] P{index} = {value};").unwrap();
        }
        code.push_str("endmodule");
        let tree =
            crate::source_tree::parse_scope_fixture(&code, Path::new("ranged_prefix.sv")).unwrap();
        let tree = AnalysisContext::unspecialized(&tree);
        let state = tree.state.as_ref();
        DECLARATION_ENV_COPIES.with(|copies| copies.set(0));
        let parameters = parameters_from_scope(
            &tree,
            &HashMap::default(),
            &HashMap::default(),
            &HashMap::default(),
        )
        .unwrap();
        assert_eq!(DECLARATION_ENV_COPIES.with(|copies| copies.get()), 0);
        assert_eq!(parameters.len(), count);
        assert!(
            parameters
                .iter()
                .all(|parameter| parameter.declared_width() == Some(32))
        );
        assert_eq!(
            const_env_from_parameters(state, &parameters)[&format!("P{}", count - 1)],
            count as i128
        );
    }
}

#[test]
fn borrowed_range_context_matches_copied_context_with_aliases_and_stale_values() {
    let tree = crate::source_tree::parse_scope_fixture(
        "module Top(); parameter N=4; typedef logic signed [3:0] word_t; parameter word_t [N-1:0] A='1; parameter logic [N-1:0] B=A+1; parameter logic [N-1:0] C=LATER+1; parameter LATER=7; endmodule",
        Path::new("borrowed_ranges.sv"),
    ).unwrap();
    let tree = AnalysisContext::unspecialized(&tree);
    let state = tree.state.as_ref();
    let base = HashMap::from_iter([("N".into(), 99), ("LATER".into(), 17)]);
    let copied_base = base.clone();
    let aliases = type_aliases_from_scope_with_env(&tree, &base).unwrap();
    for n in [3, 8, 2, 3] {
        let overrides = HashMap::from_iter([("N".into(), ConstExpr::Literal(n.to_string()))]);
        let mut borrowed = Vec::new();
        let mut copied = Vec::new();
        let mut borrowed_env = ParameterEnvironment::new(state, &borrowed, &base, &[]);
        // The same contents at a different address deliberately select the
        // former copy-and-mask path, providing an independent width context.
        let mut copied_env = ParameterEnvironment::new(state, &copied, &copied_base, &[]);
        DECLARATION_ENV_COPIES.with(|copies| copies.set(0));
        for id in tree.body_membership().unwrap() {
            if !matches!(
                tree.declarations().body_item(id).kind,
                celox_sv_syntax::BodyItemKind::Parameter(_)
            ) {
                continue;
            }
            parameters_from_native_with_environment(
                &ownership::body(id, &tree).unwrap(),
                &tree,
                &mut borrowed,
                false,
                &base,
                &aliases,
                &overrides,
                &mut borrowed_env,
            )
            .unwrap();
        }
        assert_eq!(DECLARATION_ENV_COPIES.with(|copies| copies.get()), 0);
        for id in tree.body_membership().unwrap() {
            if !matches!(
                tree.declarations().body_item(id).kind,
                celox_sv_syntax::BodyItemKind::Parameter(_)
            ) {
                continue;
            }
            parameters_from_native_with_environment(
                &ownership::body(id, &tree).unwrap(),
                &tree,
                &mut copied,
                false,
                &base,
                &aliases,
                &overrides,
                &mut copied_env,
            )
            .unwrap();
        }
        assert_eq!(DECLARATION_ENV_COPIES.with(|copies| copies.get()), 6);
        assert_eq!(borrowed, copied);
    }
}

#[test]
fn four_state_prefix_keeps_the_separate_range_projection() {
    let tree = crate::source_tree::parse_scope_fixture(
        "module Top(); localparam logic [3:0] P='x; localparam logic [7:0] Q='1; endmodule",
        Path::new("four_state_ranges.sv"),
    )
    .unwrap();
    let tree = AnalysisContext::unspecialized(&tree);
    let state = tree.state.as_ref();
    let base = HashMap::from_iter([("P".into(), 99)]);
    DECLARATION_ENV_COPIES.with(|copies| copies.set(0));
    let parameters =
        parameters_from_scope(&tree, &HashMap::default(), &base, &HashMap::default()).unwrap();
    assert_eq!(DECLARATION_ENV_COPIES.with(|copies| copies.get()), 2);
    assert_eq!(parameters[1].declared_width(), Some(8));
    assert_eq!(const_env_from_parameters(state, &parameters)["Q"], 255);
}

#[test]
fn four_state_parameter_chains_do_not_copy_growing_environments() {
    for count in [16, 64, 256] {
        let mut code = String::from("module Top();\n");
        for index in 0..count {
            let value = if index == 0 {
                "'x".into()
            } else {
                format!("P{}+1", index - 1)
            };
            writeln!(code, "localparam logic [31:0] P{index}={value};").unwrap();
        }
        code.push_str("endmodule");
        let tree = crate::source_tree::parse_scope_fixture(
            &code,
            Path::new("four_state_prefix_scaling.sv"),
        )
        .unwrap();
        let tree = AnalysisContext::unspecialized(&tree);
        let state = tree.state.as_ref();
        DECLARATION_ENV_COPIES.with(|copies| copies.set(0));
        LITERAL_ENV_COPIES.with(|copies| copies.set(0));
        let source = Source::from_syntax(&tree).unwrap();
        let parameters = source.modules()[0].parameters();
        let values = parameter_value_env(
            state,
            parameters,
            &const_env_from_parameters(state, parameters),
        );
        assert_eq!(values.len(), count);
        for parameter in parameters {
            assert_eq!(parameter.declared_width(), Some(32));
            let Expr::Literal(value) = &values[parameter.name()] else {
                panic!("unknown value must fold")
            };
            let value = typecheck::parse_integral_literal(value).unwrap();
            assert_eq!(value.width, 32);
            assert_eq!(value.mask, u32::MAX.into());
        }
        let ir = crate::analyze::analyze_source(source).unwrap();
        assert_eq!(ir.modules()[0].parameters().len(), count);
        assert!(
            ir.modules()[0]
                .parameters()
                .iter()
                .all(|parameter| parameter.resolved_value().is_none())
        );
        assert_eq!(DECLARATION_ENV_COPIES.with(|copies| copies.get()), 0);
        assert_eq!(LITERAL_ENV_COPIES.with(|copies| copies.get()), 0);
    }
}

#[test]
fn literal_prefix_range_contexts_match_copied_projection_and_track_inherited_shadows() {
    let tree = crate::source_tree::parse_scope_fixture(
        "module Top(); parameter logic [3:0] P='x; parameter logic [P:0] Q='0; parameter logic [129:0] W='z; parameter logic [7:0] R='1; endmodule",
        Path::new("literal_prefix_projection.sv"),
    ).unwrap();
    let tree = AnalysisContext::unspecialized(&tree);
    let state = tree.state.as_ref();
    for inherited_p in [None, Some(2)] {
        let base = inherited_p
            .map(|value| HashMap::from_iter([("P".into(), value)]))
            .unwrap_or_default();
        let copied_base = base.clone();
        let mut borrowed = Vec::new();
        let mut copied = Vec::new();
        let mut borrowed_env = ParameterEnvironment::new(state, &borrowed, &base, &[]);
        let mut copied_env = ParameterEnvironment::new(state, &copied, &copied_base, &[]);
        for id in tree.body_membership().unwrap() {
            if !matches!(
                tree.declarations().body_item(id).kind,
                celox_sv_syntax::BodyItemKind::Parameter(_)
            ) {
                continue;
            }
            parameters_from_native_with_environment(
                &ownership::body(id, &tree).unwrap(),
                &tree,
                &mut borrowed,
                false,
                &base,
                &HashMap::default(),
                &HashMap::default(),
                &mut borrowed_env,
            )
            .unwrap();
            parameters_from_native_with_environment(
                &ownership::body(id, &tree).unwrap(),
                &tree,
                &mut copied,
                false,
                &base,
                &HashMap::default(),
                &HashMap::default(),
                &mut copied_env,
            )
            .unwrap();
            let rebuilt = ParameterEnvironment::new(state, &borrowed, &base, &[]);
            assert_eq!(
                borrowed_env.has_inherited_literal_shadow,
                rebuilt.has_inherited_literal_shadow
            );
        }
        assert_eq!(borrowed, copied);
        assert_eq!(borrowed[0].declared_width(), Some(4));
        // The old separate projection intentionally retains inherited P for
        // width evaluation, while initializer lowering masks its numeric value.
        assert_eq!(borrowed[1].declared_width(), inherited_p.map(|_| 3));
        assert_eq!(borrowed[2].declared_width(), Some(130));
        assert_eq!(borrowed[3].declared_width(), Some(8));
    }
}

fn copied_resolved_literal(
    parameter: &Parameter,
    constants: &HashMap<String, i128>,
    types: &HashMap<String, ExprType>,
    literals: &HashMap<String, Expr>,
) -> Option<Expr> {
    let state = &crate::ast::AnalysisState::default();
    let mut evaluation_constants = constants.clone();
    evaluation_constants.remove(parameter.name());
    let mut substituted = parameter.clone();
    substituted.value = parameter
        .value
        .clone()
        .map(|value| substitute_typed_parameter_literals(value, &evaluation_constants, types));
    if let Some(ty) = parameter.resolved_type(types) {
        substituted.declared_width = Some(ty.width);
        substituted.declared_signed = Some(ty.signed);
    }
    let value = parameter_value_env(
        state,
        std::slice::from_ref(&substituted),
        &evaluation_constants,
    )
    .remove(parameter.name())?;
    let value = substitute_expr_idents(value, literals);
    let value = fold_const_integral_expr_preserving_mask(state, value, &evaluation_constants);
    matches!(value, Expr::Literal(_)).then_some(value)
}

#[test]
fn borrowed_literal_environment_matches_self_masked_copy_with_types_and_unknowns() {
    let state = &crate::ast::AnalysisState::default();
    let mut constants =
        HashMap::from_iter([("A".into(), -2), (unbounded_parameter_marker("U"), 1)]);
    insert_parameter_type_markers(
        &mut constants,
        "A",
        ExprType {
            width: 8,
            signed: true,
        },
    );
    for index in 0..4096 {
        insert_parameter_type_markers(
            &mut constants,
            &format!("unrelated{index}"),
            ExprType {
                width: 32,
                signed: false,
            },
        );
    }
    let types = parameter_types_from_const_env(&constants);
    let literals = HashMap::from_iter([("X".into(), Expr::Literal("8'bxz010101".into()))]);
    let cases = [
        ConstExpr::Literal("'x".into()),
        ConstExpr::Literal("129'bz".into()),
        ConstExpr::Ident("X".into()),
        ConstExpr::Ident("missing".into()),
        ConstExpr::Ident("P".into()),
        ConstExpr::Binary {
            left: Box::new(ConstExpr::Ident("A".into())),
            op: BinaryOp::Shr,
            right: Box::new(ConstExpr::Literal("1".into())),
        },
        ConstExpr::Binary {
            left: Box::new(ConstExpr::Ident("X".into())),
            op: BinaryOp::BitAnd,
            right: Box::new(ConstExpr::Literal("8'h0f".into())),
        },
    ];
    for stale_self in [None, Some(91)] {
        for value in &cases {
            let mut env = constants.clone();
            if let Some(value) = stale_self {
                env.insert("P".into(), value);
            }
            for (width, signed, two_state) in
                [(8, false, false), (129, true, false), (8, true, true)]
            {
                let parameter = Parameter::new(
                    "P".into(),
                    Some(value.clone()),
                    Some(width),
                    Some(signed),
                    two_state,
                    true,
                    false,
                );
                LITERAL_ENV_COPIES.with(|copies| copies.set(0));
                let borrowed = parameter.resolved_literal(state, &env, &types, &literals);
                assert_eq!(
                    LITERAL_ENV_COPIES.with(|copies| copies.get()),
                    usize::from(stale_self.is_some())
                );
                assert_eq!(
                    borrowed,
                    copied_resolved_literal(&parameter, &env, &types, &literals),
                    "{value:?}, stale={stale_self:?}, width={width}"
                );
            }
        }
    }
}

#[test]
fn reverse_generate_literal_chains_borrow_width_and_initializer_environments() {
    for count in [16, 64, 256] {
        let mut code = String::from("module Top(); if(1) begin:g\n");
        for index in 0..count {
            let value = if index + 1 == count {
                "'x".into()
            } else {
                format!("P{}+1", index + 1)
            };
            writeln!(code, "localparam logic [31:0] P{index}={value};").unwrap();
        }
        code.push_str("logic [$bits(P0)-1:0] s; assign s=P0; end endmodule");
        let tree = crate::source_tree::parse_scope_fixture(
            &code,
            Path::new("generate_literal_prefix_scaling.sv"),
        )
        .unwrap();
        let tree = AnalysisContext::unspecialized(&tree);
        DECLARATION_ENV_COPIES.with(|copies| copies.set(0));
        LITERAL_ENV_COPIES.with(|copies| copies.set(0));
        let source = Source::from_syntax(&tree).unwrap();
        let ir = crate::analyze::analyze_source(source).unwrap();
        assert_eq!(DECLARATION_ENV_COPIES.with(|copies| copies.get()), 0);
        assert_eq!(LITERAL_ENV_COPIES.with(|copies| copies.get()), 0);
        let module = &ir.modules()[0];
        assert_eq!(module.signals()[0].name(), "g.s");
        assert_eq!(module.signals()[0].r#type().resolved_width(), Some(32));
        let crate::ir::Expr::Literal(value) = module.assignments()[0].rhs() else {
            panic!("unknown generate value must fold")
        };
        let value = typecheck::parse_integral_literal(value).unwrap();
        assert_eq!(value.width, 32);
        assert_eq!(value.mask, u32::MAX.into());
    }
}

fn separately_resolved_value_and_literal(
    parameter: &Parameter,
    constants: &HashMap<String, i128>,
    types: &HashMap<String, ExprType>,
    literals: &HashMap<String, Expr>,
) -> (Option<i128>, Option<Expr>) {
    let state = &crate::ast::AnalysisState::default();
    // The former two-stage caller: numeric fallback discards its literal,
    // then a nonnumeric result requires literal resolution again.
    let value = parameter
        .resolved_value(state, constants, types)
        .or_else(|| {
            let literal = parameter.resolved_literal(state, constants, types, literals)?;
            eval_ast_const_expr(state, &expr_to_const(state, literal)?, constants)
        });
    let literal = if value.is_none() {
        parameter.resolved_literal(state, constants, types, literals)
    } else {
        None
    };
    (value, literal)
}

#[test]
fn retained_parameter_literals_match_separate_resolution_across_types_and_dependencies() {
    let state = &crate::ast::AnalysisState::default();
    let mut constants =
        HashMap::from_iter([("A".into(), -2), (unbounded_parameter_marker("U"), 1)]);
    insert_parameter_type_markers(
        &mut constants,
        "A",
        ExprType {
            width: 8,
            signed: true,
        },
    );
    let types = parameter_types_from_const_env(&constants);
    let literals = HashMap::from_iter([("X".into(), Expr::Literal("8'b10xz0011".into()))]);
    let values = [
        ConstExpr::Literal("$".into()),
        ConstExpr::Ident("U".into()),
        ConstExpr::Literal("-1".into()),
        ConstExpr::Literal("'x".into()),
        ConstExpr::Literal("129'h1ffffffffffffffffffffffffffffffff".into()),
        ConstExpr::Ident("X".into()),
        ConstExpr::Ident("missing".into()),
        ConstExpr::Ident("P".into()),
        ConstExpr::Select {
            expr: Box::new(ConstExpr::Ident("X".into())),
            bit: Box::new(ConstExpr::Literal("0".into())),
        },
        ConstExpr::Binary {
            left: Box::new(ConstExpr::Ident("A".into())),
            op: BinaryOp::Shr,
            right: Box::new(ConstExpr::Literal("1".into())),
        },
        ConstExpr::Binary {
            left: Box::new(ConstExpr::Ident("X".into())),
            op: BinaryOp::BitAnd,
            right: Box::new(ConstExpr::Literal("8'h0f".into())),
        },
        ConstExpr::Function {
            callee: Callee::System(SystemFunction::Countones),
            args: vec![ConstExpr::Literal("8'h81".into())],
            site: None,
        },
    ];
    for stale_self in [None, Some(91)] {
        let mut env = constants.clone();
        if let Some(value) = stale_self {
            env.insert("P".into(), value);
        }
        for value in &values {
            for (width, signed, two_state) in
                [(8, false, false), (129, true, false), (8, true, true)]
            {
                let parameter = Parameter::new(
                    "P".into(),
                    Some(value.clone()),
                    Some(width),
                    Some(signed),
                    two_state,
                    true,
                    false,
                );
                LITERAL_RESOLUTIONS.with(|calls| calls.set(0));
                let retained = parameter.resolved_value_and_literal(state, &env, &types, &literals);
                assert!(LITERAL_RESOLUTIONS.with(|calls| calls.get()) <= 1);
                assert!(!(retained.0.is_some() && retained.1.is_some()));
                assert_eq!(
                    retained,
                    separately_resolved_value_and_literal(&parameter, &env, &types, &literals),
                    "{value:?}, stale={stale_self:?}, width={width}, two-state={two_state}"
                );
            }
        }
    }
    let numeric = Parameter::new(
        "P".into(),
        Some(ConstExpr::Literal("7".into())),
        Some(8),
        Some(true),
        false,
        true,
        false,
    );
    LITERAL_RESOLUTIONS.with(|calls| calls.set(0));
    assert_eq!(
        numeric.resolved_value_and_literal(state, &constants, &types, &literals),
        (Some(7), None)
    );
    assert_eq!(LITERAL_RESOLUTIONS.with(|calls| calls.get()), 0);
    let two_state = Parameter::new(
        "P".into(),
        Some(ConstExpr::Ident("X".into())),
        Some(8),
        Some(true),
        true,
        true,
        false,
    );
    LITERAL_RESOLUTIONS.with(|calls| calls.set(0));
    assert_eq!(
        two_state.resolved_value_and_literal(state, &constants, &types, &literals),
        (Some(-125), None)
    );
    assert_eq!(LITERAL_RESOLUTIONS.with(|calls| calls.get()), 1);
}

#[test]
fn module_parameter_bindings_and_ir_resolve_each_unknown_literal_once() {
    for count in [16, 64, 256] {
        let mut code = String::from("module Top();\n");
        for index in 0..count {
            let value = if index == 0 {
                "'x".into()
            } else {
                format!("P{}+1", index - 1)
            };
            writeln!(code, "localparam logic [31:0] P{index}={value};").unwrap();
        }
        code.push_str("endmodule");
        let tree = crate::source_tree::parse_scope_fixture(
            &code,
            Path::new("single_parameter_resolution.sv"),
        )
        .unwrap();
        let tree = AnalysisContext::unspecialized(&tree);
        LITERAL_RESOLUTIONS.with(|calls| calls.set(0));
        PARAMETER_BINDINGS.with(|bindings| bindings.set(0));
        let source = Source::from_syntax(&tree).unwrap();
        let ir = crate::analyze::analyze_source(source).unwrap();
        assert_eq!(
            LITERAL_RESOLUTIONS.with(|calls| calls.get()),
            PARAMETER_BINDINGS.with(|bindings| bindings.get()) + count
        );
        assert_eq!(ir.modules()[0].parameters().len(), count);
        assert!(
            ir.modules()[0]
                .parameters()
                .iter()
                .all(|parameter| parameter.resolved_value().is_none()
                    && parameter.resolved_width() == Some(32))
        );
    }
}

#[test]
fn generate_bindings_keep_numeric_literals_and_unresolved_fallback_order() {
    let state = &crate::ast::AnalysisState::default();
    for (initializer, two_state, expected_value) in
        [("X", false, 91), ("X", true, -125), ("missing", false, 91)]
    {
        let mut env = HashMap::from_iter([("P".into(), 91)]);
        let mut types = HashMap::default();
        let mut literals = HashMap::from_iter([("X".into(), Expr::Literal("8'b10xz0011".into()))]);
        let parameter = Parameter::new(
            "P".into(),
            Some(ConstExpr::Ident(initializer.into())),
            Some(8),
            Some(true),
            two_state,
            true,
            false,
        );
        LITERAL_RESOLUTIONS.with(|calls| calls.set(0));
        constants::bind_generate_parameter_with_types(
            state,
            parameter,
            &mut env,
            &mut literals,
            &mut types,
        );
        assert_eq!(LITERAL_RESOLUTIONS.with(|calls| calls.get()), 1);
        assert_eq!(env["P"], expected_value);
        assert_eq!(
            types["P"],
            ExprType {
                width: 8,
                signed: true
            }
        );
        let Expr::Literal(value) = &literals["P"] else {
            panic!("bound literal must remain available")
        };
        let value = typecheck::parse_integral_literal(value).unwrap();
        assert_eq!(value.width, 8);
        assert!(value.signed);
        if initializer == "X" && !two_state {
            assert_ne!(value.mask, 0u8.into());
        } else {
            assert_eq!(value.mask, 0u8.into());
        }
    }
}

/// Replay the same unknown prefix with each resolver in one process. Parsing,
/// declaration discovery, backend compilation and simulation are excluded.
#[test]
#[ignore = "manual parameter resolution scaling probe"]
fn compare_retained_and_repeated_parameter_resolution() {
    use std::time::Instant;

    fn replay(parameters: &[Parameter], repeated: bool) {
        let state = &crate::ast::AnalysisState::default();
        let mut env = HashMap::default();
        let mut types = HashMap::default();
        let mut literals = HashMap::default();
        for parameter in parameters {
            let (value, literal) = if repeated {
                separately_resolved_value_and_literal(parameter, &env, &types, &literals)
            } else {
                parameter.resolved_value_and_literal(state, &env, &types, &literals)
            };
            assert!(value.is_none());
            let ty = parameter.resolved_type(&types).unwrap();
            types.insert(parameter.name().to_string(), ty);
            insert_parameter_type_markers(&mut env, parameter.name(), ty);
            let Expr::Literal(literal) = literal.unwrap() else {
                panic!("unknown literal must fold")
            };
            let bits = typecheck::parse_integral_literal(&literal).unwrap();
            assert_eq!(bits.width, 32);
            assert_eq!(bits.mask, u32::MAX.into());
            literals.insert(parameter.name().to_string(), Expr::Literal(literal));
        }
        std::hint::black_box(literals);
    }

    println!("parameters,repeated_ms,retained_ms,repeated_calls,retained_calls");
    for count in [32, 128, 512] {
        let parameters: Vec<_> = (0..count)
            .map(|index| {
                let value = if index == 0 {
                    ConstExpr::Literal("'x".into())
                } else {
                    ConstExpr::Binary {
                        left: Box::new(ConstExpr::Ident(format!("P{}", index - 1))),
                        op: BinaryOp::Add,
                        right: Box::new(ConstExpr::Literal("1".into())),
                    }
                };
                Parameter::new(
                    format!("P{index}"),
                    Some(value),
                    Some(32),
                    Some(false),
                    false,
                    true,
                    true,
                )
            })
            .collect();
        let mut samples = [Vec::new(), Vec::new()];
        let mut calls = [0, 0];
        for sample in 0..7 {
            // Alternate order to reduce systematic warm-up/load bias.
            for index in [sample % 2, (sample + 1) % 2] {
                LITERAL_RESOLUTIONS.with(|calls| calls.set(0));
                let start = Instant::now();
                replay(&parameters, index == 0);
                samples[index].push(start.elapsed().as_secs_f64() * 1000.0);
                calls[index] = LITERAL_RESOLUTIONS.with(|calls| calls.get());
                assert_eq!(calls[index], count * if index == 0 { 2 } else { 1 });
            }
        }
        for sample in &mut samples {
            sample.sort_by(f64::total_cmp);
        }
        println!(
            "{count},{:.3},{:.3},{},{}",
            samples[0][3], samples[1][3], calls[0], calls[1]
        );
    }
}

#[test]
fn referenced_value_projection_matches_full_environment() {
    let state = &crate::ast::AnalysisState::default();
    let p = |name: &str, value: ConstExpr, width: Option<usize>, two_state: bool| {
        Parameter::new(
            name.into(),
            Some(value),
            width,
            width.map(|_| true),
            two_state,
            width.is_some(),
            true,
        )
    };
    let id = |name: &str| ConstExpr::Ident(name.into());
    let parameters = vec![
        p(
            "A",
            ConstExpr::Literal("8'bxz010101".into()),
            Some(8),
            false,
        ),
        p("P", id("A"), Some(8), false),
        p(
            "resized",
            ConstExpr::Resize {
                expr: Box::new(id("A")),
                width: 16,
                signed: true,
            },
            None,
            false,
        ),
        p(
            "resized_again",
            ConstExpr::Resize {
                expr: Box::new(id("resized")),
                width: 4,
                signed: false,
            },
            None,
            false,
        ),
        p(
            "B",
            ConstExpr::Function {
                callee: Callee::Subroutine("missing".into()),
                args: vec![id("P")],
                site: None,
            },
            Some(32),
            false,
        ),
        p(
            "wide",
            ConstExpr::Literal("129'bz".into()),
            Some(129),
            false,
        ),
        p(
            "selected",
            ConstExpr::Select {
                expr: Box::new(id("A")),
                bit: Box::new(id("index")),
            },
            Some(1),
            false,
        ),
        p("index", ConstExpr::Literal("3".into()), None, false),
        p(
            "conditional",
            ConstExpr::Mux {
                condition: Box::new(id("index")),
                then_expr: Box::new(id("B")),
                else_expr: Box::new(id("wide")),
            },
            Some(32),
            true,
        ),
        p("forward", id("later"), None, false),
        p("later", ConstExpr::Literal("4'shf".into()), None, false),
        p("P", ConstExpr::Literal("8'sd7".into()), Some(8), false),
        p("self", id("self"), Some(8), false),
        p("cycle_a", id("cycle_b"), Some(8), false),
        p("cycle_b", id("cycle_a"), Some(8), false),
        // A known value can still need a preceding parameter's inferred type.
        p("typed_known", id("later"), None, false),
    ];
    for env in [
        HashMap::default(),
        HashMap::from_iter([("B".into(), -3), ("typed_known".into(), -1)]),
    ] {
        let full = parameter_value_env(state, &parameters, &env);
        for parameter in &parameters {
            let wanted = HashSet::from_iter([parameter.name().to_string()]);
            let projected = parameter_value_env_for_references(state, &parameters, &env, &wanted);
            assert_eq!(
                projected.get(parameter.name()),
                full.get(parameter.name()),
                "{}",
                parameter.name()
            );
            for (name, value) in &projected {
                assert_eq!(Some(value), full.get(name), "{name}");
            }
        }
        PARAMETER_VALUE_MATERIALIZATIONS.with(|count| count.set(0));
        assert!(
            parameter_value_env_for_references(
                state,
                &parameters,
                &env,
                &HashSet::from_iter(["not_a_parameter".into()])
            )
            .is_empty()
        );
        PARAMETER_VALUE_MATERIALIZATIONS.with(|count| assert_eq!(count.get(), 0));
    }
}

#[test]
fn numeric_projection_does_not_expand_initializer_dependencies() {
    let state = &crate::ast::AnalysisState::default();
    let parameters = vec![
        Parameter::new(
            "A".into(),
            Some(ConstExpr::Literal("'x".into())),
            Some(8),
            Some(false),
            false,
            true,
            true,
        ),
        Parameter::new(
            "B".into(),
            Some(ConstExpr::Ident("A".into())),
            Some(8),
            Some(false),
            false,
            true,
            true,
        ),
    ];
    let env = HashMap::from_iter([("B".into(), 3)]);
    PARAMETER_VALUE_MATERIALIZATIONS.with(|count| count.set(0));
    let projected = parameter_value_env_for_references(
        state,
        &parameters,
        &env,
        &HashSet::from_iter(["B".into()]),
    );
    PARAMETER_VALUE_MATERIALIZATIONS.with(|count| assert_eq!(count.get(), 1));
    assert_eq!(projected.len(), 1);
    assert_eq!(
        projected["B"],
        parameter_value_env(state, &parameters, &env)["B"]
    );
}

#[test]
#[ignore = "manual paired timing probe; no timing thresholds"]
fn compare_full_and_referenced_parameter_values() {
    // Keep the old deep expression expansion on a large stack for comparison.
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let state = &crate::ast::AnalysisState::default();
            use std::{hint::black_box, time::Instant};
            println!("parameters,full_ms,referenced_ms,full_values,referenced_values");
            for count in [32, 128, 512] {
                let parameters: Vec<_> = (0..count)
                    .map(|i| {
                        Parameter::new(
                            format!("P{i}"),
                            Some(ConstExpr::Function {
                                callee: Callee::Subroutine("uninstalled".into()),
                                args: vec![if i == 0 {
                                    ConstExpr::Literal("0".into())
                                } else {
                                    ConstExpr::Ident(format!("P{}", i - 1))
                                }],
                                site: None,
                            }),
                            Some(32),
                            Some(true),
                            true,
                            true,
                            true,
                        )
                    })
                    .collect();
                let env = HashMap::default();
                let references = HashSet::from_iter(["P0".into()]);
                let expected = parameter_value_env(state, &parameters[..1], &env)["P0"].clone();
                let run = |full: bool| {
                    let start = Instant::now();
                    let values = if full {
                        parameter_value_env(state, black_box(&parameters), &env)
                    } else {
                        parameter_value_env_for_references(
                            state,
                            black_box(&parameters),
                            &env,
                            &references,
                        )
                    };
                    assert_eq!(values.len(), if full { count } else { 1 });
                    assert_eq!(values["P0"], expected);
                    black_box(values);
                    start.elapsed().as_secs_f64() * 1000.0
                };
                let mut full = Vec::new();
                let mut referenced = Vec::new();
                for repetition in 0..7 {
                    if repetition % 2 == 0 {
                        full.push(run(true));
                        referenced.push(run(false));
                    } else {
                        referenced.push(run(false));
                        full.push(run(true));
                    }
                }
                full.sort_by(f64::total_cmp);
                referenced.sort_by(f64::total_cmp);
                println!("{count},{:.3},{:.3},{count},1", full[3], referenced[3]);
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn unbounded_binding_keeps_declared_rank_without_a_numeric_value() {
    let state = &crate::ast::AnalysisState::default();
    let mut env = HashMap::default();
    let mut types = HashMap::default();
    let mut literals = HashMap::default();
    for value in ["7", "$"] {
        let parameter = Parameter::new(
            "P".into(),
            Some(ConstExpr::Literal(value.into())),
            Some(8),
            Some(false),
            false,
            true,
            false,
        );
        assert!(bind_parameter(
            state,
            &mut env,
            &mut types,
            &mut literals,
            &parameter
        ));
        assert_eq!(env.get(&parameter_rank_marker("P")), Some(&1));
        assert_eq!(types["P"].width, 8);
    }
    assert!(!env.contains_key("P"));
    assert!(!env.contains_key(&parameter_marker("P")));
    assert_eq!(env.get(&unbounded_parameter_marker("P")), Some(&1));
    assert_eq!(literals["P"], Expr::Literal("$".into()));
}

#[test]
fn unbounded_generate_binding_clears_inherited_numeric_values() {
    let state = &crate::ast::AnalysisState::default();
    let parameter = Parameter::new(
        "P".into(),
        Some(ConstExpr::Literal("$".into())),
        Some(32),
        Some(true),
        true,
        true,
        true,
    );
    let mut env = HashMap::from_iter([
        ("P".into(), 7),
        (parameter_marker("P"), 7),
        (local_parameter_marker("P"), 7),
        (unbounded_parameter_marker("P"), 0),
    ]);
    let mut literals = HashMap::default();
    bind_generate_parameter(state, parameter, &mut env, &mut literals);
    assert_eq!(env.get(&unbounded_parameter_marker("P")), Some(&1));
    for key in [
        "P".to_string(),
        parameter_marker("P"),
        local_parameter_marker("P"),
    ] {
        assert!(!env.contains_key(&key), "numeric binding survives: {key}");
    }
    assert_eq!(literals.get("P"), Some(&Expr::Literal("$".into())));
}

#[test]
fn unbounded_projection_keeps_requested_aliases_and_omits_unused_dollars() {
    let state = &crate::ast::AnalysisState::default();
    let parameters = vec![
        Parameter::new(
            "U".into(),
            Some(ConstExpr::Literal("$".into())),
            Some(8),
            Some(false),
            false,
            true,
            false,
        ),
        Parameter::new(
            "ALIAS".into(),
            Some(ConstExpr::Ident("U".into())),
            Some(32),
            Some(true),
            true,
            true,
            true,
        ),
        Parameter::new(
            "NUMBER".into(),
            Some(ConstExpr::Literal("7".into())),
            Some(32),
            Some(true),
            true,
            true,
            true,
        ),
    ];
    for env in [
        HashMap::default(),
        HashMap::from_iter([(unbounded_parameter_marker("U"), 1), ("U".into(), 99)]),
    ] {
        let full = parameter_value_env(state, &parameters, &env);
        for name in ["U", "ALIAS", "NUMBER"] {
            let projected = parameter_value_env_for_references(
                state,
                &parameters,
                &env,
                &HashSet::from_iter([name.into()]),
            );
            assert_eq!(projected.get(name), full.get(name), "{name}");
            if name == "NUMBER" {
                assert_eq!(projected.len(), 1);
            }
        }
    }
}

#[test]
fn logical_type_inference_requires_operand_types_outside_bounded_proofs() {
    let expression = ConstExpr::Binary {
        left: Box::new(ConstExpr::Ident("missing".into())),
        op: BinaryOp::LogicAnd,
        right: Box::new(ConstExpr::Literal("0".into())),
    };
    assert_eq!(
        infer_const_expr_type(&expression, &HashMap::default()),
        None
    );
    assert_eq!(
        infer_const_expr_type_inner(&expression, &HashMap::default(), true),
        Some(ExprType {
            width: 1,
            signed: false
        }),
    );
}

#[test]
fn unbased_parameter_overrides_fill_declared_width_before_dependent_parameters() {
    let tree = crate::source_tree::parse_scope_fixture(
        "module Top #(parameter logic [57:0] FILL=0, \
         parameter logic signed [57:0] SIGNED_FILL=0, \
         parameter logic [57:0] COPY=FILL, parameter NEGATIVE=SIGNED_FILL < 0); endmodule",
        Path::new("unbased_parameter_overrides.sv"),
    )
    .unwrap();
    let tree = AnalysisContext::unspecialized(&tree);
    for (literal, unsigned, signed) in [("'1", (1i128 << 58) - 1, -1), ("'0", 0, 0)] {
        let overrides = ["FILL", "SIGNED_FILL"]
            .into_iter()
            .map(|name| (name.into(), ConstExpr::Literal(literal.into())))
            .collect();
        let mut parameters =
            parameters_from_scope(&tree, &HashMap::default(), &HashMap::default(), &overrides)
                .unwrap();
        for reapplied in [false, true] {
            if reapplied {
                apply_parameter_overrides(&mut parameters, &overrides).unwrap();
            }
            let values = const_env_from_parameters(&tree.state, &parameters);
            assert_eq!(values["FILL"], unsigned, "{literal}, reapplied={reapplied}");
            assert_eq!(values["SIGNED_FILL"], signed);
            assert_eq!(values["COPY"], unsigned);
            assert_eq!(values["NEGATIVE"], i128::from(signed < 0));
        }
    }
}

#[test]
fn inherited_alias_prefixes_bind_once_without_copying_declaration_environments() {
    for count in [16, 64, 256] {
        let mut code = String::from("module Top;\n");
        for index in 0..count {
            writeln!(code, "localparam logic [31:0] A{index}=EXTERNAL + {index};").unwrap();
        }
        code.push_str("endmodule");
        let tree = crate::source_tree::parse_scope_fixture(&code, Path::new("inherited_prefix.sv"))
            .unwrap();
        let tree = AnalysisContext::unspecialized(&tree);
        let mut base = HashMap::from_iter([("EXTERNAL".into(), 16)]);
        insert_parameter_type_markers(
            &mut base,
            "EXTERNAL",
            ExprType {
                width: 32,
                signed: false,
            },
        );
        PARAMETER_BINDINGS.with(|count| count.set(0));
        DECLARATION_ENV_COPIES.with(|count| count.set(0));
        let parameters =
            parameters_from_scope(&tree, &HashMap::default(), &base, &HashMap::default()).unwrap();
        assert_eq!(PARAMETER_BINDINGS.with(|count| count.get()), count);
        assert_eq!(DECLARATION_ENV_COPIES.with(|count| count.get()), 0);
        for (index, parameter) in parameters.iter().enumerate() {
            assert_eq!(
                parameter.resolved_value(
                    &tree.state,
                    &base,
                    &parameter_types_from_const_env(&base)
                ),
                Some(16 + index as i128)
            );
        }
    }
}

#[test]
fn inherited_evaluation_masks_pending_local_names_and_their_type_markers() {
    let mut base = HashMap::from_iter([("EXTERNAL".into(), 8), ("LOCAL".into(), 99)]);
    insert_parameter_type_markers(
        &mut base,
        "LOCAL",
        ExprType {
            width: 4,
            signed: false,
        },
    );
    let state = AnalysisState::default();
    let mut environment =
        ParameterEnvironment::new(&state, &[], &base, &["LOCAL".into(), "ALIAS".into()]);
    let alias = Parameter::new(
        "ALIAS".into(),
        Some(ConstExpr::Ident("LOCAL".into())),
        None,
        None,
        false,
        false,
        true,
    );
    environment.append(&state, &alias);
    assert!(!environment.values.contains_key("ALIAS"));
    assert!(!environment.types.contains_key("LOCAL"));
    assert_eq!(environment.bindings.get("EXTERNAL"), Some(&8));
}
