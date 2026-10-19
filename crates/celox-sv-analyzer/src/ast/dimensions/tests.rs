use std::path::Path;

use super::*;

#[test]
fn function_result_queries_preserve_expression_bodies() {
    let tree = crate::syntax::parse_source(
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
    let state = tree.state.as_ref();
    let source = Source::from_syntax(&tree).unwrap();
    let module = &source.modules()[0];
    let env = const_env_from_parameters(state, module.parameters());
    let node = tree
        .into_iter()
        .find(|node| matches!(node, RefNode::ModuleDeclarationAnsi(_)))
        .unwrap();
    let aliases = type_aliases_from_module_node_with_env(node.clone(), &tree, &env).unwrap();
    let mut dimensions =
        packed_dimensions_from_ports_and_signals(module.ports(), module.signals(), &env, &aliases);
    dimensions.extend(parameter_packed_dimensions(module.parameters()));
    dimensions.parameter_values = parameter_value_env(state, module.parameters(), &env).into();
    let functions = functions_from_module_node(node, &tree, &env, &dimensions).unwrap();
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
    let tree = crate::syntax::parse_source(code, Path::new("query_context.sv")).unwrap();
    let tree = AnalysisContext::unspecialized(&tree);
    let state = tree.state.as_ref();
    let source = Source::from_syntax(&tree).unwrap();
    let module = &source.modules()[0];
    let env = const_env_from_parameters(state, module.parameters());
    let node = tree
        .into_iter()
        .find(|node| matches!(node, RefNode::ModuleDeclarationAnsi(_)))
        .unwrap();
    let aliases = type_aliases_from_module_node_with_env(node.clone(), &tree, &env).unwrap();
    let mut dimensions =
        packed_dimensions_from_ports_and_signals(module.ports(), module.signals(), &env, &aliases);
    dimensions.extend(parameter_packed_dimensions(module.parameters()));
    dimensions.parameter_values = parameter_value_env(state, module.parameters(), &env).into();
    let functions = functions_from_module_node(node, &tree, &env, &dimensions).unwrap();
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
                },
            )
        }));
    dimensions.functions = Arc::new(functions);
    let calls: Vec<_> = tree
        .into_iter()
        .filter_map(|node| match node {
            RefNode::SystemTfCall(call) => Some(call),
            _ => None,
        })
        .collect();
    let query = |dimensions: &PackedDimensions| {
        calls
            .iter()
            .map(|call| {
                unpacked_dimensions_call_value(call, &tree, &env, &aliases, Some(dimensions))
                    .map(|count| (count, false))
                    .or_else(|| {
                        size_system_function_call_type(
                            call,
                            &tree,
                            &env,
                            &aliases,
                            Some(dimensions),
                        )
                        .map(|ty| (ty.width, ty.signed))
                    })
            })
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
    // Private query metadata includes the operand's signedness. A function's
    // first return dimension remains 2 even though its inlined body is 8 bits.
    assert_eq!(
        reused,
        vec![
            Some((8, true)),
            Some((2, true)),
            Some((4, false)),
            Some((4, false)),
            Some((16, false)),
            Some((36, false)),
            Some((3, false)),
            Some((4, false)),
            Some((4, true)),
            Some((4, true)),
            Some((8, true)),
            Some((2, true)),
            Some((4, true)),
            Some((1, false)),
            Some((5, false)),
            Some((5, false)),
            Some((4, false)),
            Some((4, false)),
            Some((3, false)),
            Some((5, false)),
            Some((2, false)),
            Some((1, false)),
            Some((0, false)),
            Some((0, false)),
            Some((0, false)),
        ]
    );
}

#[test]
fn preliminary_function_scope_discovers_shadowing_formals() {
    let tree = crate::syntax::parse_source(
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
    let call = tree
        .into_iter()
        .find_map(|node| match node {
            RefNode::SystemTfCall(call) => Some(call),
            _ => None,
        })
        .unwrap();
    let dimensions = PackedDimensions::default();
    assert!(!dimensions.scope_types_complete);
    SYNTAX_TYPE_DISCOVERIES.with(|count| count.set(0));
    let ty = size_system_function_call_type(
        call,
        &tree,
        &HashMap::default(),
        &HashMap::default(),
        Some(&dimensions),
    )
    .unwrap();
    assert_eq!(ty.width, 4);
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
        let tree = crate::syntax::parse_source(&code, Path::new("typedef_shadow.sv")).unwrap();
        let tree = AnalysisContext::unspecialized(&tree);
        let state = tree.state.as_ref();
        let source = Source::from_syntax(&tree).unwrap();
        let module = &source.modules()[0];
        let env = const_env_from_parameters(state, module.parameters());
        let node = tree
            .into_iter()
            .find(|node| matches!(node, RefNode::ModuleDeclarationAnsi(_)))
            .unwrap();
        let aliases = type_aliases_from_module_node_with_env(node.clone(), &tree, &env).unwrap();
        let dimensions = packed_dimensions_from_ports_and_signals(
            module.ports(),
            module.signals(),
            &env,
            &aliases,
        );
        let item = generate::items(node, &tree, &env, &aliases)
            .unwrap()
            .into_iter()
            .find(|item| {
                item.node
                    .node()
                    .into_iter()
                    .any(|node| matches!(node, RefNode::SystemTfCall(_)))
            })
            .unwrap();
        let call = item
            .node
            .node()
            .into_iter()
            .find_map(|node| match node {
                RefNode::SystemTfCall(call) => Some(call),
                _ => None,
            })
            .unwrap();
        let mut local = item.dimensions(&dimensions);
        let query = |dimensions: &PackedDimensions| {
            size_system_function_call_type(call, &tree, &item.env, &aliases, Some(dimensions))
                .map(|ty| (ty.width, ty.signed))
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
        assert_eq!(reused.unwrap().0, expected_width, "{declaration}");
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
    let tree = crate::syntax::parse_source(code, Path::new("nonansi_validation.sv")).unwrap();
    let tree = AnalysisContext::unspecialized(&tree);
    let node = tree
        .into_iter()
        .find(|node| matches!(node, RefNode::ModuleDeclarationNonansi(_)))
        .unwrap();
    crate::ast::validation::reject_silently_ignored_constructs(
        node,
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
    let tree = crate::syntax::parse_source(source, Path::new("dpi_query.sv")).unwrap();
    let tree = AnalysisContext::unspecialized(&tree);
    Source::from_syntax(&tree).unwrap();
    for call in tree.into_iter().filter_map(|node| match node {
        RefNode::SystemTfCall(call) => Some(call),
        _ => None,
    }) {
        assert_eq!(
            unpacked_dimensions_call_value(
                call,
                &tree,
                &HashMap::default(),
                &HashMap::default(),
                None
            ),
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
    let tree = crate::syntax::parse_source(source, Path::new("runtime_query_range.sv")).unwrap();
    let tree = AnalysisContext::unspecialized(&tree);
    Source::from_syntax(&tree).unwrap();
    let call = tree
        .into_iter()
        .find_map(|node| match node {
            RefNode::SystemTfCall(call) => Some(call),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        unpacked_dimensions_call_value(call, &tree, &HashMap::default(), &HashMap::default(), None),
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
        let tree = crate::syntax::parse_source(&code, Path::new("preceding_query.sv")).unwrap();
        let tree = AnalysisContext::unspecialized(&tree);
        let module = tree
            .into_iter()
            .find(|node| {
                matches!(
                    node,
                    RefNode::ModuleDeclarationAnsi(_) | RefNode::ModuleDeclarationNonansi(_)
                )
            })
            .unwrap();
        let aliases = type_aliases_from_module_node(module, &tree).unwrap();
        let calls = tree
            .into_iter()
            .filter_map(|node| match node {
                RefNode::SystemTfCall(call) => Some(call),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            unpacked_dimensions_call_value(calls[0], &tree, &HashMap::default(), &aliases, None),
            Some(1),
            "{declaration}",
        );
        assert_eq!(
            size_system_function_call_type(calls[1], &tree, &HashMap::default(), &aliases, None)
                .map(|ty| ty.width),
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
            crate::syntax::parse_source(&code, Path::new("array_parameter_query.sv")).unwrap();
        let tree = AnalysisContext::unspecialized(&tree);
        let calls = tree
            .into_iter()
            .filter_map(|node| match node {
                RefNode::SystemTfCall(call) => Some(call),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            unpacked_dimensions_call_value(
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
            size_system_function_call_type(
                calls[1],
                &tree,
                &HashMap::default(),
                &HashMap::default(),
                None
            )
            .map(|ty| ty.width),
            Some(2),
            "{declaration}"
        );
    }
}

#[test]
fn preliminary_unpacked_queries_prefer_values_over_imported_typedefs() {
    let tree = crate::syntax::parse_source(
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
    let package = tree
        .into_iter()
        .find(|node| matches!(node, RefNode::PackageDeclaration(_)))
        .unwrap();
    let aliases = type_aliases_from_module_node(package, &tree).unwrap();
    let call = tree
        .into_iter()
        .find_map(|node| match node {
            RefNode::SystemTfCall(call) => Some(call),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        unpacked_dimensions_call_value(call, &tree, &HashMap::default(), &aliases, None),
        Some(0)
    );
}

#[test]
fn recursive_generate_dimension_discovery_uses_explicit_specialization() {
    let tree = crate::syntax::parse_source(
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
    let wide = HashMap::from_iter([(
        "P".into(),
        ConstExpr::Literal(
            "256'h8000000000000000000000000000000000000000000000000000000000000000".into(),
        ),
    )]);
    let four_state = HashMap::from_iter([("Q".into(), ConstExpr::Literal("4'bxz01".into()))]);
    let wide_context = AnalysisContext::new(&tree, &wide);
    let state = wide_context.state.as_ref();
    let four_state_context = AnalysisContext::new(&tree, &four_state);
    let default_context = AnalysisContext::unspecialized(&tree);
    let target = (&tree)
        .into_iter()
        .find(|node| matches!(node, RefNode::SystemTfCall(_)))
        .unwrap();
    // Keep the contexts alive together and revisit each specialization, so the
    // recursive discovery cannot depend on whichever binding was installed last.
    for (context, expected) in [
        (&wide_context, 13),
        (&default_context, 5),
        (&four_state_context, 9),
        (&wide_context, 13),
        (&default_context, 5),
    ] {
        let dimensions = containing_packed_dimensions(
            target.clone(),
            context,
            &HashMap::default(),
            &HashMap::default(),
        )
        .unwrap();
        let data = dimensions.get("g.data").unwrap();
        assert_eq!(
            eval_ast_const_expr(state, &data.packed[0].width, &dimensions.const_env),
            Some(expected),
        );
    }
}
