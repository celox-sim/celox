use std::path::Path;

use super::*;

fn native_calls(tree: &SyntaxTree) -> Vec<celox_sv_syntax::ExprId> {
    use celox_sv_syntax::ExprKind;
    let native = tree.declarations();
    let mut calls = native
        .expression_nodes()
        .filter_map(|(id, expression)| {
            let ExprKind::Call {
                expression: callee, ..
            } = &expression.kind
            else {
                return None;
            };
            let ExprKind::Name(name) = &native.expression(*callee).kind else {
                return None;
            };
            SystemFunction::from_name(&name.spelling)
                .filter(|f| f.is_data_query())
                .map(|_| (id, expression.tokens.start))
        })
        .collect::<Vec<_>>();
    calls.sort_by_key(|(_, start)| *start);
    calls.into_iter().map(|(id, _)| id).collect()
}
fn query_number(
    root: celox_sv_syntax::ExprId,
    tree: &AnalysisContext<'_>,
    env: &HashMap<String, i128>,
    aliases: &HashMap<String, Type>,
    context: Option<&PackedDimensions>,
) -> Option<usize> {
    let mut context = context
        .cloned()
        .unwrap_or_else(|| PackedDimensions::new(HashMap::default(), env, aliases));
    context.const_env = env.clone().into();
    context.type_aliases = aliases.clone();
    query_value(root, tree, &context)
        .ok()
        .flatten()
        .and_then(|value| expr_to_const(&tree.state, value))
        .and_then(|value| eval_ast_const_expr(&tree.state, &value, env))
        .and_then(|value| usize::try_from(value).ok())
}

#[test]
fn repeated_literal_port_defaults_do_not_rediscover_declarations() {
    for count in [8, 32, 128] {
        let ports = (0..count)
            .map(|i| format!("input logic [7:0] p{i}[2] = '{{2{{8'd60-1}}}}"))
            .collect::<Vec<_>>()
            .join(",");
        let code = format!("module Top({ports}, output wire [7:0] y); assign y=p0[0]; endmodule");
        let tree =
            crate::source_tree::parse_scope_fixture(&code, Path::new("literal-port-defaults.sv"))
                .unwrap();
        let tree = AnalysisContext::unspecialized(&tree);
        SYNTAX_TYPE_DISCOVERIES.with(|queries| queries.set(0));
        let source = Source::from_syntax(&tree).unwrap();
        assert_eq!(source.modules()[0].ports().len(), count + 1);
        assert_eq!(
            SYNTAX_TYPE_DISCOVERIES.with(|queries| queries.get()),
            0,
            "{count} ports"
        );
    }
}

#[test]
fn literal_arithmetic_queries_keep_widths_and_skip_scope_discovery() {
    let code = "module Top; assign y=$bits(8'd60-1)+$bits(-8'sd1+16'sd2)+$bits(1'b0 ? 8'hff : 16'hffff)+$bits(~8'hff)+$bits(32'd1<<64'd40); endmodule";
    let tree =
        crate::source_tree::parse_scope_fixture(code, Path::new("literal-query-types.sv")).unwrap();
    let tree = AnalysisContext::unspecialized(&tree);
    let input = PackedDimensions::default();
    SYNTAX_TYPE_DISCOVERIES.with(|queries| queries.set(0));
    let values = native_calls(&tree)
        .into_iter()
        .map(|root| {
            query_number(
                root,
                &tree,
                &HashMap::default(),
                &HashMap::default(),
                Some(&input),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(values, [Some(32), Some(16), Some(16), Some(8), Some(32)]);
    assert_eq!(SYNTAX_TYPE_DISCOVERIES.with(|queries| queries.get()), 0);
}

#[test]
fn queries_still_discover_names_in_unselected_conditional_arms() {
    let code = "module Top; logic [15:0] data; assign y=$bits(1'b0 ? data : 8'hff); endmodule";
    let tree =
        crate::source_tree::parse_scope_fixture(code, Path::new("conditional-query-types.sv"))
            .unwrap();
    let tree = AnalysisContext::unspecialized(&tree);
    SYNTAX_TYPE_DISCOVERIES.with(|queries| queries.set(0));
    assert_eq!(
        query_number(
            native_calls(&tree)[0],
            &tree,
            &HashMap::default(),
            &HashMap::default(),
            Some(&PackedDimensions::default())
        ),
        Some(16)
    );
    assert!(SYNTAX_TYPE_DISCOVERIES.with(|queries| queries.get()) > 0);
}

#[test]
fn function_result_queries_preserve_expression_bodies() {
    let tree = crate::source_tree::parse_scope_fixture(
        r#"
        module Top(output int y);
            localparam logic [31:0] P = 32'bx;
            function int g(output int x); x = 99; g = 0; endfunction
            function int f(input int P);
                f = P;
                f = f + $unpacked_dimensions(g(f)) + $unpacked_dimensions(g(P));
            endfunction
            assign y = f(9);
        endmodule
        "#,
        Path::new("function_result_query.sv"),
    )
    .unwrap();
    let tree = AnalysisContext::unspecialized(&tree);
    let source = Source::from_syntax(&tree).unwrap();
    let module = &source.modules()[0];
    let env = const_env_from_parameters(&tree.state, module.parameters());
    let aliases = type_aliases_from_scope_with_env(&tree, &env).unwrap();
    let mut dimensions =
        packed_dimensions_from_ports_and_signals(module.ports(), module.signals(), &env, &aliases);
    dimensions.extend(parameter_packed_dimensions(module.parameters()));
    dimensions.parameter_values =
        parameter_value_env(&tree.state, module.parameters(), &env).into();
    let functions = functions_from_scope(&tree, &env, &dimensions).unwrap();
    assert!(functions["f"].body.is_some(), "{:?}", functions["f"]);
}

#[test]
fn complete_scope_queries_match_syntax_discovery_without_rewalking_declarations() {
    let code = r#"
        module Top(input logic signed [1:0][3:0] a, input logic [7:0] b);
            typedef struct packed { logic signed [3:0] nibble; logic flag; } pair_t;
            pair_t p;
            logic [2:0] memory [0:2][1:4];
            localparam logic signed [3:0] P = -1;
            function automatic logic signed [1:0][3:0] f(input logic [7:0] x);
                return x;
            endfunction
            logic [31:0] y;
            assign y = $bits(a) + $size(a) + $bits(a[0]) + $size(a[0])
                     + $bits({a, b}) + $bits(memory) + $size(memory) + $size(memory[0])
                     + $bits(p.nibble) + $size(p.nibble) + $bits(f(b)) + $size(f(b))
                     + $bits(P) + $bits(P[0]) + $bits(pair_t) + $size(pair_t)
                     + $size(a, 2) + $size(memory, 2) + $size(memory, 3)
                     + $size(pair_t, 1)
                     + $unpacked_dimensions(memory) + $unpacked_dimensions(memory[0])
                     + $unpacked_dimensions(a) + $unpacked_dimensions(p.nibble)
                     + $unpacked_dimensions(pair_t);
        endmodule
    "#;
    let tree =
        crate::source_tree::parse_scope_fixture(code, Path::new("query_context.sv")).unwrap();
    let tree = AnalysisContext::unspecialized(&tree);
    let source = Source::from_syntax(&tree).unwrap();
    let module = &source.modules()[0];
    let env = const_env_from_parameters(&tree.state, module.parameters());
    let aliases = type_aliases_from_scope_with_env(&tree, &env).unwrap();
    let mut dimensions =
        packed_dimensions_from_ports_and_signals(module.ports(), module.signals(), &env, &aliases);
    dimensions.extend(parameter_packed_dimensions(module.parameters()));
    dimensions.parameter_values =
        parameter_value_env(&tree.state, module.parameters(), &env).into();
    let functions = functions_from_scope(&tree, &env, &dimensions).unwrap();
    dimensions
        .function_return_types
        .extend(functions.iter().map(|(name, f)| {
            (
                name.clone(),
                FunctionReturnMetadata {
                    arguments: Some(function_argument_metadata(&f.params)),
                    width: f.return_width,
                    first_packed_dimension_width: f.return_first_packed_dimension_width,
                    dimensions: f.return_dimensions,
                    signed: f.return_signed,
                    is_2state: f.return_is_2state,
                    string: f.return_string,
                    pattern_type: f.return_type.clone(),
                },
            )
        }));
    dimensions.functions = Arc::new(functions);
    let calls: Vec<_> = native_calls(&tree);
    let query = |dimensions: &PackedDimensions| {
        calls
            .iter()
            .map(|call| query_number(*call, &tree, &env, &aliases, Some(dimensions)))
            .collect::<Vec<_>>()
    };
    SYNTAX_TYPE_DISCOVERIES.with(|count| count.set(0));
    let discovered = query(&dimensions);
    assert!(SYNTAX_TYPE_DISCOVERIES.with(|count| count.get()) > 0);
    dimensions.scope_types_complete = true;
    SYNTAX_TYPE_DISCOVERIES.with(|count| count.set(0));
    let reused = query(&dimensions);
    assert_eq!(SYNTAX_TYPE_DISCOVERIES.with(|count| count.get()), 0);
    assert_eq!(reused, discovered);
    // A function's first return dimension remains 2 even though its body is 8 bits.
    assert_eq!(
        reused,
        vec![
            Some(8),
            Some(2),
            Some(4),
            Some(4),
            Some(16),
            Some(36),
            Some(3),
            Some(4),
            Some(4),
            Some(4),
            Some(8),
            Some(2),
            Some(4),
            Some(1),
            Some(5),
            Some(5),
            Some(4),
            Some(4),
            Some(3),
            Some(5),
            Some(2),
            Some(1),
            Some(0),
            Some(0),
            Some(0),
        ]
    );
}

#[test]
fn preliminary_function_scope_discovers_shadowing_formals() {
    let tree = crate::source_tree::parse_scope_fixture(
        r#"
        module Top;
            logic [7:0] x;
            function automatic int f(input logic [3:0] x);
                return $bits(x);
            endfunction
        endmodule
    "#,
        Path::new("preliminary_scope.sv"),
    )
    .unwrap();
    let tree = AnalysisContext::unspecialized(&tree);
    let call = native_calls(&tree)[0];
    let dimensions = PackedDimensions::default();
    assert!(!dimensions.scope_types_complete);
    SYNTAX_TYPE_DISCOVERIES.with(|count| count.set(0));
    let ty = query_number(
        call,
        &tree,
        &HashMap::default(),
        &HashMap::default(),
        Some(&dimensions),
    )
    .unwrap();
    assert_eq!(ty, 4);
    assert!(SYNTAX_TYPE_DISCOVERIES.with(|count| count.get()) > 0);
}

#[test]
fn completed_query_metadata_is_local_to_each_specialization() {
    let source = crate::ParsedSource::parse(
        r#"
        module Top #(parameter N = 8) (input logic [N-1:0] a, output logic [31:0] y);
            assign y = $bits(a);
        endmodule
    "#,
        Path::new("query_specializations.sv"),
    )
    .unwrap();
    for n in [4, 16, 8, 4] {
        let ir = source
            .analyze_module_with_parameter_expr_overrides(
                "Top",
                &HashMap::from_iter([("N".into(), crate::ir::ConstExpr::Literal(n.to_string()))]),
                &ModuleInterfaces::default(),
            )
            .unwrap();
        assert_eq!(
            ir.modules()[0].assignments()[0].rhs(),
            &crate::ir::Expr::Literal(n.to_string())
        );
    }
}

#[test]
fn local_values_shadow_outer_typedefs_in_completed_generate_scopes() {
    for (declaration, expected_width) in [
        ("logic [3:0] T;", 4),
        ("localparam T = 1;", 32),
        ("localparam T = 'x;", 1),
    ] {
        let code = format!(
            r#"
            module Top(output logic [31:0] y);
                typedef logic [7:0] T;
                if (1) begin : g
                    {declaration}
                    assign y = $bits(T);
                end
            endmodule
        "#
        );
        let tree =
            crate::source_tree::parse_scope_fixture(&code, Path::new("typedef_shadow.sv")).unwrap();
        let tree = AnalysisContext::unspecialized(&tree);
        let source = Source::from_syntax(&tree).unwrap();
        let module = &source.modules()[0];
        let env = const_env_from_parameters(&tree.state, module.parameters());
        let aliases = type_aliases_from_scope_with_env(&tree, &env).unwrap();
        let dimensions = packed_dimensions_from_ports_and_signals(
            module.ports(),
            module.signals(),
            &env,
            &aliases,
        );
        let item = generate::items(&tree, &env, &aliases)
            .unwrap()
            .into_iter()
            .find(|item| {
                matches!(
                    tree.declarations().body_item(item.id).kind,
                    celox_sv_syntax::BodyItemKind::Assign(_)
                )
            })
            .unwrap();
        let call = native_calls(&tree)[0];
        let mut local = item.dimensions(&dimensions);
        let query = |dimensions: &PackedDimensions| {
            query_number(call, &tree, &item.env, &aliases, Some(dimensions))
        };
        let discovered = query(&local);
        local.scope_types_complete = true;
        SYNTAX_TYPE_DISCOVERIES.with(|count| count.set(0));
        let reused = query(&local);
        assert_eq!(
            SYNTAX_TYPE_DISCOVERIES.with(|count| count.get()),
            0,
            "{declaration}"
        );
        assert_eq!(reused, discovered, "{declaration}");
        assert_eq!(reused.unwrap(), expected_width, "{declaration}");
    }
}

#[test]
fn nonansi_validation_leaves_generate_queries_to_elaboration() {
    let code = r#"
        module Top(y);
            output y;
            if (0) begin : inactive
                localparam N = $unpacked_dimensions(missing);
                localparam M = $dimensions(missing);
            end else begin : active
                assign y = 0;
            end
        endmodule
    "#;
    let tree =
        crate::source_tree::parse_scope_fixture(code, Path::new("nonansi_validation.sv")).unwrap();
    let tree = AnalysisContext::unspecialized(&tree);
    crate::ast::validation::reject_silently_ignored_constructs(
        &tree,
        &HashMap::default(),
        &HashMap::default(),
        &ScopedMap::default(),
        &HashMap::default(),
    )
    .unwrap();
}

#[test]
fn unpacked_dimensions_discovers_local_dpi_return_types() {
    let source = r#"
        module Top(output int y);
            import "DPI-C" function int f();
            localparam N = $unpacked_dimensions(f());
            assign y = N + $unpacked_dimensions(f());
        endmodule
    "#;
    let tree = crate::source_tree::parse_scope_fixture(source, Path::new("dpi_query.sv")).unwrap();
    let tree = AnalysisContext::unspecialized(&tree);
    Source::from_syntax(&tree).unwrap();
    for call in native_calls(&tree) {
        assert_eq!(
            query_number(call, &tree, &HashMap::default(), &HashMap::default(), None),
            Some(0)
        );
    }
}

#[test]
fn unpacked_dimensions_preserves_runtime_indexed_range_types() {
    let source = r#"
        module Top(input int index_value, output int y);
            logic [7:0] a [0:1][0:2];
            assign y = $unpacked_dimensions(a[0][index_value +: 1]);
        endmodule
    "#;
    let tree = crate::source_tree::parse_scope_fixture(source, Path::new("runtime_query_range.sv"))
        .unwrap();
    let tree = AnalysisContext::unspecialized(&tree);
    Source::from_syntax(&tree).unwrap();
    let call = native_calls(&tree)[0];
    assert_eq!(
        query_number(call, &tree, &HashMap::default(), &HashMap::default(), None),
        Some(1)
    );
}

#[test]
fn preliminary_unpacked_queries_resolve_preceding_declaration_ranges() {
    for declaration in [
        "module Top; logic [7:0] a [0:1]; logic [$unpacked_dimensions(a):0] b;",
        "module Top(input logic [7:0] a [0:1], output logic [$unpacked_dimensions(a):0] b);",
        "module Top; logic [7:0] a [0:1], b [0:$unpacked_dimensions(a)];",
        "module Top; wire [7:0] a [0:1], b [0:$unpacked_dimensions(a)];",
        "module Top; typedef logic [7:0] byte_t; byte_t a [0:1], b [0:$unpacked_dimensions(a)];",
    ] {
        let code = format!("{declaration} localparam W = $bits(b); endmodule");
        let tree = crate::source_tree::parse_scope_fixture(&code, Path::new("preceding_query.sv"))
            .unwrap();
        let tree = AnalysisContext::unspecialized(&tree);
        let aliases = type_aliases_from_scope(&tree).unwrap();
        let calls = native_calls(&tree);
        assert_eq!(
            query_number(calls[0], &tree, &HashMap::default(), &aliases, None),
            Some(1),
            "{declaration}",
        );
        assert_eq!(
            query_number(calls[1], &tree, &HashMap::default(), &aliases, None),
            Some(if declaration.contains("b [") { 16 } else { 2 }),
            "{declaration}",
        );
    }
}

#[test]
fn preliminary_unpacked_queries_discover_array_parameters_before_dependent_ranges() {
    for declaration in [
        "module Top #(parameter logic [7:0] A [0:1] = '{default:0}) (output logic [$unpacked_dimensions(A):0] b);",
        "module Top; localparam logic [7:0] A [0:1] = '{default:0}; logic [$unpacked_dimensions(A):0] b;",
        "module Top; logic [7:0] a [0:1]; localparam logic [7:0] A [0:$unpacked_dimensions(a)] = '{default:0}; logic [1:0] b;",
    ] {
        let code = format!("{declaration} localparam W = $bits(b); endmodule");
        let tree =
            crate::source_tree::parse_scope_fixture(&code, Path::new("array_parameter_query.sv"))
                .unwrap();
        let tree = AnalysisContext::unspecialized(&tree);
        let calls = native_calls(&tree);
        assert_eq!(
            query_number(
                calls[0],
                &tree,
                &HashMap::default(),
                &HashMap::default(),
                None
            ),
            Some(1),
            "{declaration}"
        );
        assert_eq!(
            query_number(
                calls[1],
                &tree,
                &HashMap::default(),
                &HashMap::default(),
                None
            ),
            Some(2),
            "{declaration}"
        );
    }
}

#[test]
fn preliminary_unpacked_queries_prefer_values_over_imported_typedefs() {
    let tree = crate::source_tree::parse_scope_fixture(
        r#"
        package P;
            typedef logic [7:0] row_t [0:1];
        endpackage
        module Top;
            import P::*;
            logic row_t;
            localparam N = $unpacked_dimensions(row_t);
        endmodule
        "#,
        Path::new("imported_query.sv"),
    )
    .unwrap();
    let tree = AnalysisContext::unspecialized(&tree);
    let aliases = type_aliases_from_scope(&tree).unwrap();
    // The aliases came from P; the query belongs to Top's native source unit.
    let module_scope = tree
        .root_scopes()
        .unwrap()
        .into_iter()
        .find(|handle| {
            handle.unit(tree.declarations()).unwrap().kind == celox_sv_syntax::UnitKind::Module
        })
        .unwrap();
    let tree = tree.scoped(module_scope).unwrap();
    let tree = AnalysisContext::unspecialized(&tree);
    let call = native_calls(&tree)[0];
    assert_eq!(
        query_number(call, &tree, &HashMap::default(), &aliases, None),
        Some(0)
    );
}

#[test]
fn recursive_generate_dimension_discovery_uses_explicit_specialization() {
    let tree = crate::source_tree::parse_scope_fixture(
        r#"
        module Top #(parameter logic [255:0] P = '0,
                     parameter logic [3:0] Q = '0)(output int y);
            if (P === 256'h8000000000000000000000000000000000000000000000000000000000000000) begin : g
                logic [12:0] data;
            end else if (Q === 4'bxz01) begin : g
                logic [8:0] data;
            end else begin : g
                logic [4:0] data;
            end
            assign y = $bits(g.data);
        endmodule
        "#,
        Path::new("explicit_generate_dimensions.sv"),
    )
    .unwrap();
    let tree = AnalysisContext::unspecialized(&tree);
    let wide = HashMap::from_iter([(
        "P".into(),
        ConstExpr::Literal(
            "256'h8000000000000000000000000000000000000000000000000000000000000000".into(),
        ),
    )]);
    let four_state = HashMap::from_iter([("Q".into(), ConstExpr::Literal("4'bxz01".into()))]);
    let wide_context = AnalysisContext::new(&tree, &wide);
    let four_state_context = AnalysisContext::new(&tree, &four_state);
    let default_context = AnalysisContext::unspecialized(&tree);
    let target = native_calls(&tree)[0];
    let celox_sv_syntax::ExprKind::Call { arguments, .. } =
        &tree.declarations().expression(target).kind
    else {
        panic!()
    };
    let operand = arguments[0].value.unwrap();
    // Keep the contexts alive together and revisit each specialization, so the
    // recursive discovery cannot depend on whichever binding was installed last.
    for (context, expected) in [
        (&wide_context, 13),
        (&default_context, 5),
        (&four_state_context, 9),
        (&wide_context, 13),
        (&default_context, 5),
    ] {
        let dimensions =
            query_context_for_operand(operand, context, &PackedDimensions::default()).unwrap();
        let data = dimensions.get("g.data").unwrap();
        assert_eq!(
            eval_ast_const_expr(&context.state, &data.packed[0].width, &dimensions.const_env),
            Some(expected),
        );
    }
}

#[test]
fn preliminary_literal_queries_do_not_discover_scope_declarations() {
    let tree = crate::source_tree::parse_scope_fixture(
        r#"module Top;
            logic [7:0] unrelated [0:31];
            localparam A = $bits(123);
            localparam B = $bits((8'shff));
            localparam C = $bits(4'bx);
            localparam D = $bits("ab");
            localparam E = $bits('1);
        endmodule"#,
        Path::new("literal_query.sv"),
    )
    .unwrap();
    let tree = AnalysisContext::unspecialized(&tree);
    let input = PackedDimensions::default();
    let calls = native_calls(&tree);
    assert_eq!(calls.len(), 5);
    // Keep the discovery path as an independent reference for the result.
    let discovered = calls
        .iter()
        .map(|call| {
            let celox_sv_syntax::ExprKind::Call { arguments, .. } =
                &tree.declarations().expression(*call).kind
            else {
                unreachable!()
            };
            let context =
                query_operand_dimensions_id(arguments[0].value.unwrap(), &tree, &input).unwrap();
            query_number(
                *call,
                &tree,
                &HashMap::default(),
                &HashMap::default(),
                Some(&context),
            )
        })
        .collect::<Vec<_>>();
    SYNTAX_TYPE_DISCOVERIES.with(|count| count.set(0));
    let direct = calls
        .iter()
        .map(|call| {
            query_number(
                *call,
                &tree,
                &HashMap::default(),
                &HashMap::default(),
                Some(&input),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(&direct[..4], &[Some(32), Some(8), Some(4), Some(16)]);
    assert_eq!(direct, discovered);
    assert_eq!(SYNTAX_TYPE_DISCOVERIES.with(|count| count.get()), 0);
}
