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
    session: &mut Session,
    env: &ConstEnv,
    aliases: &TypeAliases,
    context: Option<&PackedDimensions>,
) -> Option<usize> {
    let mut context = context
        .cloned()
        .unwrap_or_else(|| PackedDimensions::new(HashMap::default(), env, aliases));
    context.const_env = env.clone().into();
    context.type_aliases = aliases.clone().into();
    query_value(root, tree, session, &context)
        .ok()
        .flatten()
        .and_then(|value| expr_to_const(session, value))
        .and_then(|value| eval_ast_const_expr(&tree.functions, &value, env))
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
    let session = &mut Session::default();
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
                session,
                &ConstEnv::default(),
                &TypeAliases::default(),
                Some(&input),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(values, [Some(32), Some(16), Some(16), Some(8), Some(32)]);
    assert_eq!(SYNTAX_TYPE_DISCOVERIES.with(|queries| queries.get()), 0);
}

#[test]
fn queries_still_discover_names_in_unselected_conditional_arms() {
    let session = &mut Session::default();
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
            session,
            &ConstEnv::default(),
            &TypeAliases::default(),
            Some(&PackedDimensions::default()),
        ),
        Some(16)
    );
    assert!(SYNTAX_TYPE_DISCOVERIES.with(|queries| queries.get()) > 0);
}

#[test]
fn function_result_queries_preserve_expression_bodies() {
    let session = &mut Session::default();
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
    let env = const_env_from_parameters(&tree, session, module.parameters());
    let aliases = type_aliases_from_scope_with_env(&tree, session, &env).unwrap();
    let mut dimensions =
        packed_dimensions_from_ports_and_signals(module.ports(), module.signals(), &env, &aliases);
    dimensions.extend(parameter_packed_dimensions(module.parameters()));
    dimensions.parameter_values =
        parameter_value_env(&tree, session, module.parameters(), &env).into();
    let functions = functions_from_scope(&tree, session, &env, &dimensions).unwrap();
    assert!(functions["f"].body.is_some(), "{:?}", functions["f"]);
}

#[test]
fn complete_scope_queries_match_syntax_discovery_without_rewalking_declarations() {
    let session = &mut Session::default();
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
    let env = const_env_from_parameters(&tree, session, module.parameters());
    let aliases = type_aliases_from_scope_with_env(&tree, session, &env).unwrap();
    let mut dimensions =
        packed_dimensions_from_ports_and_signals(module.ports(), module.signals(), &env, &aliases);
    dimensions.extend(parameter_packed_dimensions(module.parameters()));
    dimensions.parameter_values =
        parameter_value_env(&tree, session, module.parameters(), &env).into();
    let functions = functions_from_scope(&tree, session, &env, &dimensions).unwrap();
    dimensions
        .function_return_types
        .extend(functions.iter().map(|(name, f)| {
            (
                name.clone(),
                FunctionReturnMetadata {
                    semantic_type: None,
                    arguments: Some(function_argument_metadata(&f.params)),
                    width: f.return_width,
                    first_packed_dimension_width: f.return_first_packed_dimension_width,
                    dimensions: f.return_dimensions,
                    signed: f.return_signed,
                    is_2state: f.return_is_2state,
                    string: f.return_string,
                    chandle: false,
                    pattern_type: f.return_type.clone(),
                    enum_domain: None,
                    virtual_interface: None,
                    container: None,
                },
            )
        }));
    dimensions.functions = Arc::new(functions);
    let calls: Vec<_> = native_calls(&tree);
    let mut query = |dimensions: &PackedDimensions| {
        calls
            .iter()
            .map(|call| query_number(*call, &tree, session, &env, &aliases, Some(dimensions)))
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
    let session = &mut Session::default();
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
        session,
        &ConstEnv::default(),
        &TypeAliases::default(),
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
    let session = &mut Session::default();
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
        let env = const_env_from_parameters(&tree, session, module.parameters());
        let aliases = type_aliases_from_scope_with_env(&tree, session, &env).unwrap();
        let dimensions = packed_dimensions_from_ports_and_signals(
            module.ports(),
            module.signals(),
            &env,
            &aliases,
        );
        let item = generate::items(&tree, session, &env, &aliases)
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
        let mut query = |dimensions: &PackedDimensions| {
            query_number(call, &tree, session, &item.env, &aliases, Some(dimensions))
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
    let session = &mut Session::default();
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
        session,
        &ConstEnv::default(),
        &TypeAliases::default(),
        &ScopedMap::default(),
        &HashMap::default(),
    )
    .unwrap();
}

#[test]
fn unpacked_dimensions_discovers_local_dpi_return_types() {
    let session = &mut Session::default();
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
            query_number(
                call,
                &tree,
                session,
                &ConstEnv::default(),
                &TypeAliases::default(),
                None
            ),
            Some(0)
        );
    }
}

#[test]
fn unpacked_dimensions_preserves_runtime_indexed_range_types() {
    let session = &mut Session::default();
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
        query_number(
            call,
            &tree,
            session,
            &ConstEnv::default(),
            &TypeAliases::default(),
            None
        ),
        Some(1)
    );
}

#[test]
fn preliminary_unpacked_queries_resolve_preceding_declaration_ranges() {
    let session = &mut Session::default();
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
        let aliases = type_aliases_from_scope(&tree, session).unwrap();
        let calls = native_calls(&tree);
        assert_eq!(
            query_number(
                calls[0],
                &tree,
                session,
                &ConstEnv::default(),
                &aliases,
                None
            ),
            Some(1),
            "{declaration}",
        );
        assert_eq!(
            query_number(
                calls[1],
                &tree,
                session,
                &ConstEnv::default(),
                &aliases,
                None
            ),
            Some(if declaration.contains("b [") { 16 } else { 2 }),
            "{declaration}",
        );
    }
}

#[test]
fn preliminary_unpacked_queries_discover_array_parameters_before_dependent_ranges() {
    let session = &mut Session::default();
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
                session,
                &ConstEnv::default(),
                &TypeAliases::default(),
                None,
            ),
            Some(1),
            "{declaration}"
        );
        assert_eq!(
            query_number(
                calls[1],
                &tree,
                session,
                &ConstEnv::default(),
                &TypeAliases::default(),
                None,
            ),
            Some(2),
            "{declaration}"
        );
    }
}

#[test]
fn preliminary_unpacked_queries_prefer_values_over_imported_typedefs() {
    let session = &mut Session::default();
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
    let aliases = type_aliases_from_scope(&tree, session).unwrap();
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
        query_number(call, &tree, session, &ConstEnv::default(), &aliases, None),
        Some(0)
    );
}

#[test]
fn recursive_generate_dimension_discovery_uses_explicit_specialization() {
    let session = &mut Session::default();
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
            query_context_for_operand(operand, context, session, &PackedDimensions::default())
                .unwrap();
        let data = dimensions.get("g.data").unwrap();
        assert_eq!(
            eval_ast_const_expr(
                &context.functions,
                &data.packed[0].width,
                &dimensions.const_env
            ),
            Some(expected),
        );
    }
}

#[test]
fn preliminary_literal_queries_do_not_discover_scope_declarations() {
    let session = &mut Session::default();
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
                query_operand_dimensions_id(arguments[0].value.unwrap(), &tree, session, &input)
                    .unwrap();
            query_number(
                *call,
                &tree,
                session,
                &ConstEnv::default(),
                &TypeAliases::default(),
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
                session,
                &ConstEnv::default(),
                &TypeAliases::default(),
                Some(&input),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(&direct[..4], &[Some(32), Some(8), Some(4), Some(16)]);
    assert_eq!(direct, discovered);
    assert_eq!(SYNTAX_TYPE_DISCOVERIES.with(|count| count.get()), 0);
}

#[test]
fn discovery_prefix_does_not_copy_a_growing_symbol_overlay() {
    let session = &mut Session::default();
    const COUNT: usize = 128;
    let declarations = (0..COUNT)
        .map(|index| format!("logic [7:0] v{index};"))
        .collect::<String>();
    let code = format!(
        "module Top; {declarations} localparam int WIDTH=$bits(v{}); endmodule",
        COUNT - 1
    );
    let native =
        crate::source_tree::parse_scope_fixture(&code, Path::new("query-prefix-copies.sv"))
            .unwrap();
    let calls = native_calls(&native);
    assert_eq!(calls.len(), 1);
    let tree = AnalysisContext::unspecialized(&native);
    super::super::scoped_map::COPIED_OVERLAY_BINDINGS.with(|count| count.set(0));
    SYNTAX_TYPE_DISCOVERIES.with(|count| count.set(0));
    assert_eq!(
        query_number(
            calls[0],
            &tree,
            session,
            &ConstEnv::default(),
            &TypeAliases::default(),
            None,
        ),
        Some(8)
    );
    assert!(SYNTAX_TYPE_DISCOVERIES.with(|count| count.get()) > 0);
    let copied = super::super::scoped_map::COPIED_OVERLAY_BINDINGS.with(|count| count.get());
    assert!(
        copied <= COUNT * 8,
        "{copied} copied bindings for {COUNT} declarations"
    );
}

#[test]
fn discovery_binds_class_signatures_independently_of_queries() {
    // Each `$bits` query during validation discovers its owners' types and
    // reaches every class signature of the scope; binding those once per
    // scope keeps the cost independent of the number of queries.
    let bindings = |queries: usize| {
        let methods = (0..queries)
            .map(|i| {
                format!(
                    "function int f{i}(bit [{i}:0] v); logic [7:0] q [{i}:0]; \
                     return $bits(v) + $size(q); endfunction"
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let source = format!(
            "package p;
                class A; {methods} endclass
                class B extends A; function int g(int x); return $bits(x); endfunction endclass
            endpackage
            module Top; endmodule"
        );
        CLASS_SIGNATURE_BINDINGS.with(|count| count.set(0));
        crate::analyze_source(&source, Path::new("class-signature-discovery.sv"))
            .expect("the package analyzes");
        CLASS_SIGNATURE_BINDINGS.with(|count| count.get())
    };
    assert_eq!(bindings(2), bindings(16));
}

#[test]
fn discovery_settles_a_scope_once_for_its_queries() {
    // The `$bits` queries of one function share its owners: their scope's
    // declarations are discovered once, however many queries there are.
    let settles = |queries: usize| {
        let uses = (0..queries)
            .map(|i| format!("$bits(v{i})"))
            .collect::<Vec<_>>()
            .join(" + ");
        let locals = (0..queries)
            .map(|i| format!("bit [{i}:0] v{i};"))
            .collect::<String>();
        let source = format!(
            "package p;
                class A; function int g(); return 1; endfunction endclass
                function int f(); {locals} return {uses}; endfunction
            endpackage
            module Top; endmodule"
        );
        SCOPE_SETTLES.with(|count| count.set(0));
        crate::analyze_source(&source, Path::new("scope-settles.sv"))
            .expect("the package analyzes");
        SCOPE_SETTLES.with(|count| count.get())
    };
    assert_eq!(settles(2), settles(16));
}

/// The widths of the signals of the first module of `source`, analyzed as
/// file `path`, and the scope discoveries its queries settled.
fn signal_widths_and_settles(source: &str, path: &str) -> (Vec<Option<usize>>, usize) {
    SCOPE_SETTLES.with(|count| count.set(0));
    let ir = crate::analyze_source(source, Path::new(path)).expect("the source analyzes");
    let widths = ir.modules()[0]
        .signals()
        .iter()
        .map(|signal| signal.r#type().resolved_width())
        .collect();
    (widths, SCOPE_SETTLES.with(|count| count.get()))
}

#[test]
fn type_operands_need_no_discovery() {
    // The size of a built-in type, and of a type the tables of the query
    // already bind, does not depend on the declarations of the scope, which
    // are not discovered for it: of a typedef of the scope, of a type
    // parameter of a class and of a typedef of a package (IEEE 1800-2023
    // 20.6.2).
    let (widths, settles) = signal_widths_and_settles(
        "package p;
            typedef bit [5:0] word_t;
            class C #(type T = bit [2:0]);
                function int size(); return $bits(T) + $bits(word_t); endfunction
            endclass
            class D; C#(bit [8:0]) c; endclass
            function int f(); return $bits(byte) + $bits((word_t)); endfunction
        endpackage
        module Top;
            import p::*;
            typedef bit [3:0] nibble_t;
            localparam int A = $bits(nibble_t);
            localparam int B = $bits(word_t) + $bits(logic [1:0]);
            localparam int E = f();
            logic [A:0] a;
            logic [B:0] b;
            logic [E:0] e;
        endmodule",
        "type-operands.sv",
    );
    assert_eq!(widths, [Some(5), Some(9), Some(15)]);
    assert_eq!(settles, 0);
}

/// The widths of the signals of the first module of `source`, analyzed as
/// file `path`, by the last part of their names, and the queries that
/// discovered the declarations of their scope.
fn named_widths_and_discoveries(source: &str, path: &str) -> (Vec<(String, usize)>, usize) {
    SYNTAX_TYPE_DISCOVERIES.with(|count| count.set(0));
    let ir = crate::analyze_source(source, Path::new(path)).expect("the source analyzes");
    let mut widths = ir.modules()[0]
        .signals()
        .iter()
        .filter_map(|signal| {
            let name = signal.name().rsplit('.').next()?.to_string();
            Some((name, signal.r#type().resolved_width()?))
        })
        .collect::<Vec<_>>();
    widths.sort();
    (widths, SYNTAX_TYPE_DISCOVERIES.with(|count| count.get()))
}

#[test]
fn marked_variable_operands_need_no_discovery() {
    // A generate block is elaborated in an environment that keeps the
    // dimensions of the variables it has declared so far. A data query of
    // such a variable reads those: the declarations of the scope are not
    // discovered for it, however many declarations and blocks query it
    // (IEEE 1800-2023 20.6.2, 20.7).
    let blocks = |count: usize| {
        (0..count)
            .map(|index| {
                format!(
                    "if (1) begin : g{index}
                        logic [15:0] near;
                        logic [3:0][7:0] p;
                        logic signed [4:0] m [2:0];
                        logic single;
                        wire [$bits(near):0] a{index};
                        wire [$bits((p)):0] b{index};
                        wire [$bits(m):0] c{index};
                        wire [$bits(single):0] d{index};
                        wire [$size(p):0] e{index};
                        wire [$left(p):0] f{index};
                        wire [$high(m, 1):0] h{index};
                        wire [$dimensions(m):0] i{index};
                        wire [$unpacked_dimensions(m):0] j{index};
                        wire [$right(m, 2):0] k{index};
                        wire [$increment(m):0] l{index};
                    end
                    "
                )
            })
            .collect::<String>()
    };
    for count in [1, 8] {
        let (widths, discoveries) = named_widths_and_discoveries(
            &format!("module Top; {} endmodule", blocks(count)),
            "marked-variables.sv",
        );
        let width = |name: &str| {
            widths
                .iter()
                .find(|(signal, _)| signal == name)
                .map(|(_, width)| *width)
        };
        for index in 0..count {
            let got = "abcdefhijkl"
                .chars()
                .map(|letter| width(&format!("{letter}{index}")))
                .collect::<Vec<_>>();
            assert_eq!(
                got,
                [17, 33, 16, 2, 5, 4, 3, 3, 2, 1, 2].map(Some),
                "block {index} of {count}"
            );
        }
        assert_eq!(discoveries, 0, "{count} blocks");
    }
}

/// An environment that keeps `name` as a variable `[left:right]`, as the
/// elaboration of a generate block keeps the variables it has declared.
fn marked_variable(name: &str, left: i128, right: i128) -> ConstEnv {
    let mut env = ConstEnv::default();
    env.insert(variable_dimensions_marker(name), 1);
    env.insert(variable_unpacked_dimensions_marker(name), 0);
    env.insert(variable_dimension_marker(name, 0, "left"), left);
    env.insert(variable_dimension_marker(name, 0, "right"), right);
    env.insert(variable_signed_marker(name), 0);
    env
}

#[test]
fn locals_hide_marked_variables() {
    // A query of a variable that the generate block around it has declared
    // reads the declaration, unless a declaration nearer to the operand
    // replaces it, which discovering the scope finds: a formal, the result
    // or a local of the subroutine, or a variable of a loop or block. A
    // variable of an outer block is
    // discovered too: the blocks between may give the names of its type
    // another meaning (IEEE 1800-2023 13.4.1, 23.9).
    for (declarations, bits, discovered) in [
        ("localparam N = $bits(t);", 16, false),
        ("if (1) begin : h localparam N = $bits(t); end", 16, true),
        (
            "function automatic int f(); logic [1:0] u; return $bits(t); endfunction",
            16,
            false,
        ),
        (
            "function automatic int f(logic [2:0] t); return $bits(t); endfunction",
            3,
            true,
        ),
        (
            "function automatic int f(); logic [1:0] t; return $bits(t); endfunction",
            2,
            true,
        ),
        (
            "function automatic int f();
                int n;
                for (byte t = 0; t < 1; t++) n = $bits(t);
                return n;
            endfunction",
            8,
            true,
        ),
        (
            "function automatic int f();
                int n;
                begin : inner
                    logic [5:0] t;
                    n = $bits(t);
                end
                return n;
            endfunction",
            6,
            true,
        ),
        (
            "function automatic logic [4:0] t(); return $bits(t); endfunction",
            5,
            true,
        ),
    ] {
        let session = &mut Session::default();
        let native = crate::source_tree::parse_scope_fixture(
            &format!("module Top; if (1) begin : g logic [15:0] t; {declarations} end endmodule"),
            Path::new("hidden-marked-variables.sv"),
        )
        .unwrap();
        let call = native_calls(&native)[0];
        let tree = AnalysisContext::unspecialized(&native);
        SYNTAX_TYPE_DISCOVERIES.with(|count| count.set(0));
        assert_eq!(
            query_number(
                call,
                &tree,
                session,
                &marked_variable("t", 15, 0),
                &TypeAliases::default(),
                None,
            ),
            Some(bits),
            "{declarations}"
        );
        assert_eq!(
            SYNTAX_TYPE_DISCOVERIES.with(|count| count.get()) > 0,
            discovered,
            "{declarations}"
        );
    }
}

#[test]
fn class_scopes_discover_marked_variables() {
    // The tables of a class specialization replace those of the scope
    // around the class, so a query bound in a class reads the declarations.
    let session = &mut Session::default();
    let native = crate::source_tree::parse_scope_fixture(
        "module Top; localparam N = $bits(t); endmodule",
        Path::new("class-marked-variables.sv"),
    )
    .unwrap();
    let call = native_calls(&native)[0];
    let tree = AnalysisContext::unspecialized(&native);
    let env = marked_variable("t", 15, 0);
    let mut context = PackedDimensions::new(HashMap::default(), &env, &TypeAliases::default());
    context.class_scope = Some("C".into());
    SYNTAX_TYPE_DISCOVERIES.with(|count| count.set(0));
    let _ = query_number(
        call,
        &tree,
        session,
        &env,
        &TypeAliases::default(),
        Some(&context),
    );
    assert!(SYNTAX_TYPE_DISCOVERIES.with(|count| count.get()) > 0);
}

#[test]
fn marked_variable_queries_read_the_environment_of_each_binding() {
    // Every binding that keeps the dimensions of a variable in its
    // environment reads them the same way: a generate block that declares
    // the name of a variable of the module again, a block nested in it and
    // the blocks of a loop, beside the queries of variables of the module,
    // which discover the scope.
    let (widths, _) = named_widths_and_discoveries(
        "module Top (input logic [15:0] s);
            logic [6:0] v;
            localparam int P = $bits(s) + $bits(v);
            logic [P:0] a;
            if (1) begin : g
                logic [8:0] s;
                wire [$bits(s):0] b;
                wire [$bits(v):0] c;
                if (1) begin : inner
                    wire [$bits(s):0] d;
                end
            end
            for (genvar i = 0; i < 2; ++i) begin : lanes
                logic [i + 2:0] lane;
                wire [$bits(lane):0] h;
            end
        endmodule",
        "marked-variable-bindings.sv",
    );
    assert_eq!(
        widths
            .iter()
            .filter(|(name, _)| ["a", "b", "c", "d", "h"].contains(&name.as_str()))
            .cloned()
            .collect::<Vec<_>>(),
        [
            ("a", 24),
            ("b", 10),
            ("c", 8),
            ("d", 10),
            ("h", 4),
            ("h", 5)
        ]
        .map(|(name, width)| (name.to_string(), width))
    );
}

#[test]
fn type_parameters_are_the_types_the_tables_bind() {
    // The tables of a query bind a type parameter of a module or class to
    // the type it is given, which its declaration does not hide (IEEE
    // 1800-2023 6.20.3).
    for declarations in [
        "module Top #(parameter type T = int); localparam N = $bits(T); endmodule",
        "module Top;
            class C #(type T = int);
                function int f(); return $bits(T); endfunction
            endclass
        endmodule",
    ] {
        let session = &mut Session::default();
        let tree = crate::source_tree::parse_scope_fixture(
            &format!("package P; typedef logic [4:0] T; endpackage {declarations}"),
            Path::new("type-parameters.sv"),
        )
        .unwrap();
        let tree = AnalysisContext::unspecialized(&tree);
        // The type the parameter is given.
        let aliases = type_aliases_from_scope(&tree, session).unwrap();
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
        SCOPE_SETTLES.with(|count| count.set(0));
        assert_eq!(
            query_number(call, &tree, session, &ConstEnv::default(), &aliases, None),
            Some(5),
            "{declarations}"
        );
        assert_eq!(SCOPE_SETTLES.with(|count| count.get()), 0, "{declarations}");
    }
}

#[test]
fn hidden_type_names_are_discovered() {
    // A declaration nearer to the operand than the type of its name hides
    // the type, also for a query whose tables hold only the type: a
    // variable, a net, a typedef, a formal, a loop variable and a
    // declaration of a generate block (IEEE 1800-2023 23.9).
    for (declarations, bits) in [
        ("wire [4:0] t; localparam N = $bits(t);", 5),
        (
            "function automatic int f(); logic [1:0] t; return $bits(t); endfunction",
            2,
        ),
        (
            "function automatic int f(); typedef logic [8:0] t; return $bits(t); endfunction",
            9,
        ),
        (
            "function automatic int f(logic [2:0] t); return $bits(t); endfunction",
            3,
        ),
        (
            "function automatic int f();
                int n;
                for (byte t = 0; t < 1; t++) n = $bits(t);
                return n;
            endfunction",
            8,
        ),
        (
            "if (1) begin : g logic [9:0] t; localparam N = $bits(t); end",
            10,
        ),
    ] {
        let session = &mut Session::default();
        let tree = crate::source_tree::parse_scope_fixture(
            &format!(
                "package P; typedef logic [6:0] t; endpackage
                module Top; import P::*; {declarations} endmodule"
            ),
            Path::new("hidden-type-names.sv"),
        )
        .unwrap();
        let tree = AnalysisContext::unspecialized(&tree);
        let aliases = type_aliases_from_scope(&tree, session).unwrap();
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
            query_number(call, &tree, session, &ConstEnv::default(), &aliases, None),
            Some(bits),
            "{declarations}"
        );
    }
}

#[test]
fn discovery_settles_the_scope_of_a_class_once_for_its_specializations() {
    // The methods of each specialization query the scope around the class
    // with the tables of the specialization: its declarations are discovered
    // from the tables around the class, the same for every specialization.
    let settles = |specializations: usize| {
        let properties = (0..specializations)
            .map(|i| format!("C#({}) c{i};", i + 1))
            .collect::<String>();
        let source = format!(
            "package p;
                class C #(int N = 2);
                    typedef bit [N-1:0] word_t;
                    function int g(); word_t v; return $bits(v); endfunction
                endclass
                class D; {properties} endclass
            endpackage
            module Top; endmodule"
        );
        SCOPE_SETTLES.with(|count| count.set(0));
        crate::analyze_source(&source, Path::new("class-scope-settles.sv"))
            .expect("the package analyzes");
        SCOPE_SETTLES.with(|count| count.get())
    };
    assert_eq!(settles(2), settles(16));
}

#[test]
fn discovery_settles_the_scope_of_a_class_once_for_its_methods() {
    // The queries of each method of a class share the scope around it:
    // which method a query is in does not change the declarations of the
    // scope, so they are discovered once however many methods there are.
    let settles = |methods: usize| {
        let functions = (0..methods)
            .map(|i| {
                format!(
                    "function int f{i}(); bit [{i}:0] a; bit [3:0] b; return $bits(a) + $bits(b); endfunction\n"
                )
            })
            .collect::<String>();
        let source = format!(
            "package p;
                class A; {functions} endclass
            endpackage
            module Top; endmodule"
        );
        SCOPE_SETTLES.with(|count| count.set(0));
        crate::analyze_source(&source, Path::new("method-scope-settles.sv"))
            .expect("the package analyzes");
        SCOPE_SETTLES.with(|count| count.get())
    };
    assert_eq!(settles(2), settles(16));
}

#[test]
fn constant_sizes_read_the_settled_scope() {
    // The size of each cast and the count of each replication is bound in
    // a disposable session, which reads the scope its caller settled
    // instead of discovering it again.
    let settles = |constants: usize| {
        let uses = (0..constants)
            .map(|i| format!("$bits(($bits(v) + {i})'(x)) + $bits({{($bits(v) + {i}){{x}}}})"))
            .collect::<Vec<_>>()
            .join(" + ");
        let source = format!(
            "package p;
                parameter int W = 3;
                class A; function int g(); return 1; endfunction endclass
                function int f(); bit x; bit [W-1:0] v; return {uses}; endfunction
            endpackage
            module Top; endmodule"
        );
        SCOPE_SETTLES.with(|count| count.set(0));
        crate::analyze_source(&source, Path::new("constant-settles.sv"))
            .expect("the package analyzes");
        SCOPE_SETTLES.with(|count| count.get())
    };
    assert_eq!(settles(2), settles(16));
}

#[test]
fn class_references_prepare_each_specialization_once() {
    // Classes naming the same specializations, which each discovery pass
    // collects again, copy the scope tables to prepare each specialization
    // once, however many references there are.
    let preparations = |references: usize| {
        let properties = (0..references)
            .map(|i| format!("C#(8) a{i}; C#(4) b{i};"))
            .collect::<String>();
        let source = format!(
            "package p;
                class C #(int N = 2); bit [N-1:0] v; endclass
                class D; {properties} endclass
                function int f(); bit [3:0] w; return $bits(w); endfunction
            endpackage
            module Top; endmodule"
        );
        SPECIALIZATION_PREPARATIONS.with(|count| count.set(0));
        crate::analyze_source(&source, Path::new("class-preparations.sv"))
            .expect("the package analyzes");
        SPECIALIZATION_PREPARATIONS.with(|count| count.get())
    };
    assert_eq!(preparations(2), preparations(16));
}

#[test]
fn class_scopes_copy_the_scope_tables_once_per_binding() {
    // The scopes of the specializations whose signatures a binding makes
    // share one copy of the tables around them.
    let copies = |specializations: usize| {
        let properties = (0..specializations)
            .map(|i| format!("C#({}) c{i};", i + 1))
            .collect::<String>();
        let source = format!(
            "package p;
                typedef bit [7:0] byte_t;
                class C #(int N = 2); function bit [N-1:0] get(byte_t x); return 0; endfunction endclass
                class D; {properties} endclass
                function int f(); bit [3:0] w; return $bits(w); endfunction
            endpackage
            module Top; endmodule"
        );
        CLASS_SCOPE_COPIES.with(|count| count.set(0));
        crate::analyze_source(&source, Path::new("class-scopes.sv")).expect("the package analyzes");
        CLASS_SCOPE_COPIES.with(|count| count.get())
    };
    assert_eq!(copies(2), copies(16));
}

#[test]
fn discovery_binds_class_signatures_once_its_declarations_settle() {
    // Each declaration's width names the next one: discovering them takes a
    // pass per declaration, which binds the class signatures only once.
    let bindings = |declarations: usize| {
        let variables = (0..declarations)
            .map(|i| format!("bit [$bits(v{}):0] v{i};", i + 1))
            .collect::<String>();
        let source = format!(
            "package p;
                class A; function int g(); return 1; endfunction endclass
                {variables}
                bit [3:0] v{declarations};
                function int f(); bit [7:0] local_v; return $bits(local_v) + $bits(v0); endfunction
            endpackage
            module Top; endmodule"
        );
        SETTLE_CLASS_BINDINGS.with(|count| count.set(0));
        crate::analyze_source(&source, Path::new("class-signature-passes.sv"))
            .expect("the package analyzes");
        SETTLE_CLASS_BINDINGS.with(|count| count.get())
    };
    assert_eq!(bindings(2), bindings(16));
}

#[test]
fn class_references_of_one_spelling_specialize_once() {
    let specializations = |references: usize| {
        let properties = (0..references)
            .map(|i| format!("C#(8) a{i}; C#(W) b{i}; C c{i};"))
            .collect::<String>();
        let source = format!(
            "package p;
                class C #(int N = 4); bit [N-1:0] v; endclass
                class D #(int W = 2); {properties} endclass
                class E extends D#(3); endclass
            endpackage
            module Top; endmodule"
        );
        CLASS_REFERENCE_SPECIALIZATIONS.with(|count| count.set(0));
        crate::analyze_source(&source, Path::new("class-references.sv"))
            .expect("the package analyzes");
        CLASS_REFERENCE_SPECIALIZATIONS.with(|count| count.get())
    };
    assert_eq!(specializations(2), specializations(16));
}

#[test]
fn shared_discovery_analyzes_like_unshared_discovery() {
    // Settled scopes and specializations shared between queries and class
    // references give the analysis that discovering each again gives.
    let sources = [
        "package p;
            typedef enum { RED, GREEN } color_t;
            class C #(int N = 4);
                bit [N-1:0] mask;
                color_t color;
                function int width(); return $bits(mask) + $bits(color); endfunction
            endclass
            class D #(int W = 2) extends C;
                C#(W) inner;
                C#(8) wide;
                function int total();
                    inner = new;
                    return width() + $bits(inner.mask) + W;
                endfunction
            endclass
            function int f(bit [7:0] v); return $bits(v) + $bits(v[3:0]); endfunction
        endpackage
        module Top(output int o);
            import p::*;
            D#(3) d;
            C#(8) c;
            initial begin
                d = new;
                c = new;
                o = d.total() + c.width() + f(8'h1);
            end
        endmodule",
        "module Top(input logic [7:0] a, output logic [7:0] y);
            class B; int x; function int get(); return x; endfunction endclass
            class S #(int K = 1) extends B; bit [K:0] s; endclass
            S#(2) h;
            localparam int W = $bits(a);
            always_comb y = a + W[7:0];
            initial begin h = new; h.x = $bits(h.s); end
        endmodule",
    ];
    for source in sources {
        let analyze = |unshared: bool| {
            UNSHARED_DISCOVERY.with(|flag| flag.set(unshared));
            let ir = crate::analyze_source(source, Path::new("shared-discovery.sv"));
            UNSHARED_DISCOVERY.with(|flag| flag.set(false));
            ir.map_err(|error| error.to_string())
        };
        let shared = analyze(false);
        let unshared = analyze(true);
        assert!(shared.is_ok(), "{shared:?}");
        assert_eq!(shared, unshared);
    }
}

#[test]
fn query_contexts_derive_only_the_variables_their_operands_name() {
    // The queries of a function's pattern assignments, in a module with
    // `others` more variables that they do not name.
    let derived = |others: usize| {
        let declarations = (0..others)
            .map(|i| format!("logic [3:0] unused{i};"))
            .collect::<String>();
        let assignments = (0..8)
            .map(|i| format!("s = '{{a: x[{i}], b: $bits(x)}};"))
            .collect::<String>();
        let source = format!(
            "module Top;
                typedef struct packed {{ logic [7:0] a; logic [7:0] b; }} s_t;
                logic [15:0] x;
                {declarations}
                function automatic s_t f();
                    s_t s;
                    {assignments}
                    return s;
                endfunction
                s_t y;
                assign y = f();
            endmodule"
        );
        QUERY_CONTEXT_VARIABLES.with(|count| count.set(0));
        crate::analyze_source(&source, Path::new("query-contexts.sv"))
            .expect("the module analyzes");
        QUERY_CONTEXT_VARIABLES.with(|count| count.get())
    };
    assert_eq!(derived(2), derived(200));
}
