use std::{fmt::Write, path::Path};

use super::*;

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
                name: "f".into(),
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

fn module_node(tree: &SyntaxTree) -> RefNode<'_> {
    tree.into_iter()
        .find(|node| {
            matches!(
                node,
                RefNode::ModuleDeclarationAnsi(_) | RefNode::ModuleDeclarationNonansi(_)
            )
        })
        .unwrap()
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
    let tree = crate::syntax::parse_source(&code, Path::new("parameter_prefix.sv")).unwrap();
    PARAMETER_BINDINGS.with(|count| count.set(0));
    let parameters = parameters_from_module_node(
        module_node(&tree),
        &tree,
        &HashMap::default(),
        &HashMap::default(),
        &HashMap::default(),
    )
    .unwrap();
    assert_eq!(PARAMETER_BINDINGS.with(|count| count.get()), 128);
    assert_eq!(parameters.len(), 128);
    assert_eq!(const_env_from_parameters(&parameters)["P127"], 128);
    PARAMETER_BINDINGS.with(|count| count.set(0));
    let enums = enum_member_constants_from_module_node(
        module_node(&tree),
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
    let tree = crate::syntax::parse_source(
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
    let node = module_node(&tree);
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
    let aliases = type_aliases_from_module_node_with_env(node.clone(), &tree, &base).unwrap();
    for n in [8, 3, 16, 8] {
        let overrides = HashMap::from_iter([("N".into(), ConstExpr::Literal(n.to_string()))]);
        let incremental =
            parameters_from_module_node(node.clone(), &tree, &aliases, &base, &overrides).unwrap();
        let mut rebuilt = Vec::new();
        for declaration in scope_declarations(node.clone()) {
            let sv_parser::PackageOrGenerateItemDeclaration::ParameterDeclaration(parameter) =
                declaration
            else {
                continue;
            };
            parameters_from_ref_node(
                RefNode::ParameterDeclaration(&parameter.0),
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
    let tree = crate::syntax::parse_source(code, Path::new("package.sv")).unwrap();
    let package = tree
        .into_iter()
        .find(|node| matches!(node, RefNode::PackageDeclaration(_)))
        .unwrap();
    let parameters = parameters_from_module_node(
        package.clone(),
        &tree,
        &HashMap::default(),
        &HashMap::default(),
        &HashMap::default(),
    )
    .unwrap();
    let env = const_env_from_parameters(&parameters);
    assert_eq!((env["W"], env["N"]), (4, 8));
    let aliases = type_aliases_from_module_node_with_env(package.clone(), &tree, &env).unwrap();
    assert_eq!(aliases["word_t"].packed_ranges().len(), 1);
    let enums = enum_member_constants_from_module_node(
        package.clone(),
        &tree,
        &env,
        &aliases,
        &HashMap::default(),
    )
    .unwrap();
    assert_eq!((enums.numbers["IDLE"], enums.numbers["RUN"]), (0, 2));
    let items = generate::items(package, &tree, &env, &aliases).unwrap();
    assert_eq!(items.len(), 5);
    assert!(
        items
            .iter()
            .all(|item| matches!(item.node, ScopeItem::Package(_)))
    );
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
        let tree = crate::syntax::parse_source(&code, Path::new("ranged_prefix.sv")).unwrap();
        DECLARATION_ENV_COPIES.with(|copies| copies.set(0));
        let parameters = parameters_from_module_node(
            module_node(&tree),
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
            const_env_from_parameters(&parameters)[&format!("P{}", count - 1)],
            count as i128
        );
    }
}

#[test]
fn borrowed_range_context_matches_copied_context_with_aliases_and_stale_values() {
    let tree = crate::syntax::parse_source(
        "module Top(); parameter N=4; typedef logic signed [3:0] word_t; parameter word_t [N-1:0] A='1; parameter logic [N-1:0] B=A+1; parameter logic [N-1:0] C=LATER+1; parameter LATER=7; endmodule",
        Path::new("borrowed_ranges.sv"),
    ).unwrap();
    let node = module_node(&tree);
    let base = HashMap::from_iter([("N".into(), 99), ("LATER".into(), 17)]);
    let copied_base = base.clone();
    let aliases = type_aliases_from_module_node_with_env(node.clone(), &tree, &base).unwrap();
    for n in [3, 8, 2, 3] {
        let overrides = HashMap::from_iter([("N".into(), ConstExpr::Literal(n.to_string()))]);
        let mut borrowed = Vec::new();
        let mut copied = Vec::new();
        let mut borrowed_env = ParameterEnvironment::new(&borrowed, &base);
        // The same contents at a different address deliberately select the
        // former copy-and-mask path, providing an independent width context.
        let mut copied_env = ParameterEnvironment::new(&copied, &copied_base);
        DECLARATION_ENV_COPIES.with(|copies| copies.set(0));
        for declaration in scope_declarations(node.clone()) {
            let sv_parser::PackageOrGenerateItemDeclaration::ParameterDeclaration(parameter) =
                declaration
            else {
                continue;
            };
            parameters_from_ref_node_with_environment(
                RefNode::ParameterDeclaration(&parameter.0),
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
        for declaration in scope_declarations(node.clone()) {
            let sv_parser::PackageOrGenerateItemDeclaration::ParameterDeclaration(parameter) =
                declaration
            else {
                continue;
            };
            parameters_from_ref_node_with_environment(
                RefNode::ParameterDeclaration(&parameter.0),
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
    let tree = crate::syntax::parse_source(
        "module Top(); localparam logic [3:0] P='x; localparam logic [7:0] Q='1; endmodule",
        Path::new("four_state_ranges.sv"),
    )
    .unwrap();
    let base = HashMap::from_iter([("P".into(), 99)]);
    DECLARATION_ENV_COPIES.with(|copies| copies.set(0));
    let parameters = parameters_from_module_node(
        module_node(&tree),
        &tree,
        &HashMap::default(),
        &base,
        &HashMap::default(),
    )
    .unwrap();
    assert_eq!(DECLARATION_ENV_COPIES.with(|copies| copies.get()), 2);
    assert_eq!(parameters[1].declared_width(), Some(8));
    assert_eq!(const_env_from_parameters(&parameters)["Q"], 255);
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
        let tree =
            crate::syntax::parse_source(&code, Path::new("four_state_prefix_scaling.sv")).unwrap();
        DECLARATION_ENV_COPIES.with(|copies| copies.set(0));
        LITERAL_ENV_COPIES.with(|copies| copies.set(0));
        let source = Source::from_syntax(&tree).unwrap();
        let parameters = source.modules()[0].parameters();
        let values = parameter_value_env(parameters, &const_env_from_parameters(parameters));
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
    let tree = crate::syntax::parse_source(
        "module Top(); parameter logic [3:0] P='x; parameter logic [P:0] Q='0; parameter logic [129:0] W='z; parameter logic [7:0] R='1; endmodule",
        Path::new("literal_prefix_projection.sv"),
    ).unwrap();
    for inherited_p in [None, Some(2)] {
        let base = inherited_p
            .map(|value| HashMap::from_iter([("P".into(), value)]))
            .unwrap_or_default();
        let copied_base = base.clone();
        let mut borrowed = Vec::new();
        let mut copied = Vec::new();
        let mut borrowed_env = ParameterEnvironment::new(&borrowed, &base);
        let mut copied_env = ParameterEnvironment::new(&copied, &copied_base);
        for declaration in scope_declarations(module_node(&tree)) {
            let sv_parser::PackageOrGenerateItemDeclaration::ParameterDeclaration(declaration) =
                declaration
            else {
                continue;
            };
            parameters_from_ref_node_with_environment(
                RefNode::ParameterDeclaration(&declaration.0),
                &tree,
                &mut borrowed,
                false,
                &base,
                &HashMap::default(),
                &HashMap::default(),
                &mut borrowed_env,
            )
            .unwrap();
            parameters_from_ref_node_with_environment(
                RefNode::ParameterDeclaration(&declaration.0),
                &tree,
                &mut copied,
                false,
                &base,
                &HashMap::default(),
                &HashMap::default(),
                &mut copied_env,
            )
            .unwrap();
            let rebuilt = ParameterEnvironment::new(&borrowed, &base);
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
    let value = parameter_value_env(std::slice::from_ref(&substituted), &evaluation_constants)
        .remove(parameter.name())?;
    let value = substitute_expr_idents(value, literals);
    let value = fold_const_integral_expr_preserving_mask(value, &evaluation_constants);
    matches!(value, Expr::Literal(_)).then_some(value)
}

#[test]
fn borrowed_literal_environment_matches_self_masked_copy_with_types_and_unknowns() {
    let mut constants = HashMap::from_iter([("A".into(), -2)]);
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
                let borrowed = parameter.resolved_literal(&env, &types, &literals);
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
        let tree =
            crate::syntax::parse_source(&code, Path::new("generate_literal_prefix_scaling.sv"))
                .unwrap();
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
    // The former two-stage caller: numeric fallback discards its literal,
    // then a nonnumeric result requires literal resolution again.
    let value = parameter.resolved_value(constants, types).or_else(|| {
        let literal = parameter.resolved_literal(constants, types, literals)?;
        eval_ast_const_expr(&expr_to_const(literal)?, constants)
    });
    let literal = if value.is_none() {
        parameter.resolved_literal(constants, types, literals)
    } else {
        None
    };
    (value, literal)
}

#[test]
fn retained_parameter_literals_match_separate_resolution_across_types_and_dependencies() {
    let mut constants = HashMap::from_iter([("A".into(), -2)]);
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
            name: "$countones".into(),
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
                let retained = parameter.resolved_value_and_literal(&env, &types, &literals);
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
        numeric.resolved_value_and_literal(&constants, &types, &literals),
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
        two_state.resolved_value_and_literal(&constants, &types, &literals),
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
        let tree = crate::syntax::parse_source(&code, Path::new("single_parameter_resolution.sv"))
            .unwrap();
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
        let mut env = HashMap::default();
        let mut types = HashMap::default();
        let mut literals = HashMap::default();
        for parameter in parameters {
            let (value, literal) = if repeated {
                separately_resolved_value_and_literal(parameter, &env, &types, &literals)
            } else {
                parameter.resolved_value_and_literal(&env, &types, &literals)
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
