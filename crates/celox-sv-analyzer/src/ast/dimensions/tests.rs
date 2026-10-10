use std::path::Path;

use super::*;

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
    let source = Source::from_syntax(&tree).unwrap();
    let module = &source.modules()[0];
    let env = const_env_from_parameters(module.parameters());
    let node = tree
        .into_iter()
        .find(|node| matches!(node, RefNode::ModuleDeclarationAnsi(_)))
        .unwrap();
    let aliases = type_aliases_from_module_node_with_env(node.clone(), &tree, &env).unwrap();
    let mut dimensions =
        packed_dimensions_from_ports_and_signals(module.ports(), module.signals(), &env, &aliases);
    dimensions.extend(parameter_packed_dimensions(module.parameters()));
    dimensions.parameter_values = parameter_value_env(module.parameters(), &env).into();
    let functions = functions_from_module_node(node, &tree, &env, &dimensions).unwrap();
    dimensions
        .function_return_types
        .extend(functions.iter().map(|(name, f)| {
            (
                name.clone(),
                FunctionReturnMetadata {
                    width: f.return_width,
                    first_packed_dimension_width: f.return_first_packed_dimension_width,
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
        let source = Source::from_syntax(&tree).unwrap();
        let module = &source.modules()[0];
        let env = const_env_from_parameters(module.parameters());
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
