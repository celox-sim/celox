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
        for item in module_non_port_items(node.clone()) {
            let Some(sv_parser::PackageOrGenerateItemDeclaration::ParameterDeclaration(parameter)) =
                package_or_generate_declaration_from_non_port_item(item)
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
