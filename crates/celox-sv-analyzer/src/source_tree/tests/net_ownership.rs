use super::*;
use crate::ast::{BinaryOp, ConstExpr, Expr, LValue, Source};
use celox_sv_syntax::{DataTypeKind, Declarations};

#[test]
fn native_lexical_and_generate_net_ids_supply_query_and_local_dependency_types() {
    let mut tree = super::parse_source(
        "module M;wire[(10+2):0]a=0;localparam A=$bits(a);generate wire[(20+2):0]b=0;localparam B=$bits(b);wire[$bits(b)-1:0]region_z=0;endgenerate if(1)begin:g wire[(30+2):0]a=0;localparam C=$bits(a);wire[$bits(a)-1:0]z=0;end endmodule",
        Path::new("native-net-query-ownership.sv"),
    ).unwrap();
    subtract(&mut tree);
    let source = Source::from_syntax(&tree).unwrap();
    for (name, upper) in [("region_z", 18), ("g.z", 28)] {
        let signal = source.modules()[0]
            .signals()
            .iter()
            .find(|signal| signal.name() == name)
            .unwrap();
        assert_eq!(
            number(signal.r#type().packed_ranges()[0].left()),
            upper,
            "{name}"
        );
    }
    let ir = crate::analyze::analyze_source(source).unwrap();
    let parameter = ir.modules()[0]
        .parameters()
        .iter()
        .find(|p| p.name() == "A")
        .unwrap();
    assert_eq!(parameter.resolved_value(), Some(9));
}
fn number(value: &ConstExpr) -> i128 {
    let value: crate::ir::ConstExpr = value.clone().into();
    crate::typecheck::eval_const_expr(&value, &HashMap::default()).unwrap()
}
fn subtract(tree: &mut SourceTree) {
    for expression in &mut tree.declarations.expressions {
        if let celox_sv_syntax::ExprKind::Binary { operator, .. } = &mut expression.kind
            && operator == "+"
        {
            *operator = "-".into()
        }
    }
}
#[test]
fn native_net_types_dimensions_and_initializers_drive_production() {
    let mut tree = super::parse_source("module M;wire signed[(10+2):0] a[(6+2)];wire int b=8+2;typedef logic[7:0]T;wire T c[(4+2)];assign a[0]='0;assign c[0]='0;if(1)begin:g wire[(10+2):0]z=8+2;end endmodule", Path::new("native-nets.sv")).unwrap();
    subtract(&mut tree);
    let source = Source::from_syntax(&tree).unwrap();
    let module = &source.modules()[0];
    let signal = |name: &str| {
        module
            .signals()
            .iter()
            .find(|signal| signal.name() == name)
            .unwrap()
    };
    assert_eq!(number(signal("a").r#type().packed_ranges()[0].left()), 8);
    assert!(signal("a").r#type().is_signed());
    assert_eq!(number(signal("a").r#type().unpacked_ranges()[0].right()), 3);
    assert_eq!(number(signal("c").r#type().unpacked_ranges()[0].right()), 1);
    assert_eq!(number(signal("g.z").r#type().packed_ranges()[0].left()), 8);
    assert!(signal("a").is_net());
    let mut names = Vec::new();
    for process in module.comb_processes() {
        for assignment in process.assignments() {
            if let LValue::Ident(name) = assignment.lhs_value()
                && matches!(name.as_str(), "b" | "g.z")
            {
                assert!(matches!(
                    assignment.rhs(),
                    Expr::Binary {
                        op: BinaryOp::Sub,
                        ..
                    }
                ));
                names.push(name.as_str());
            }
        }
    }
    names.sort();
    assert_eq!(names, ["b", "g.z"]);
}
#[test]
fn native_net_declarators_and_initializers_are_authoritative() {
    for defect in ["missing", "name", "initializer", "count"] {
        let mut tree = super::parse_source(
            "module M;wire a,b=1;assign a=0;endmodule",
            Path::new("native-net-identity.sv"),
        )
        .unwrap();
        match defect {
            "missing" => {
                for item in &mut tree.declarations.body_items {
                    if matches!(item.kind, celox_sv_syntax::BodyItemKind::Net(_)) {
                        item.kind = celox_sv_syntax::BodyItemKind::Unconverted {
                            category: "missing net".into(),
                            items: Vec::new(),
                        };
                    }
                }
            }
            "name" => {
                tree.declarations.net_declarations[0].variables[0]
                    .name
                    .spelling = "other".into()
            }
            "initializer" => tree.declarations.net_declarations[0].variables[1].value = None,
            _ => {
                tree.declarations.net_declarations[0].variables.pop();
            }
        }
        if defect == "missing" {
            let source = Source::from_syntax(&tree).unwrap();
            assert!(source.modules()[0].signals().is_empty());
            assert_eq!(source.modules()[0].assignments().len(), 1);
        } else if matches!(defect, "name" | "initializer") {
            let source = Source::from_syntax(&tree).unwrap();
            let module = &source.modules()[0];
            let undriven = if defect == "name" { "other" } else { "b" };
            assert!(
                module
                    .signals()
                    .iter()
                    .any(|signal| signal.name() == undriven)
            );
            assert!(!module.assignments().iter().any(|assignment| {
                matches!(assignment.lhs_value(), LValue::Ident(name) if name == undriven)
            }));
        } else {
            let source = Source::from_syntax(&tree).unwrap();
            let module = &source.modules()[0];
            assert_eq!(
                module
                    .signals()
                    .iter()
                    .map(|signal| signal.name())
                    .collect::<Vec<_>>(),
                vec!["a"]
            );
            assert_eq!(module.assignments().len(), 1);
        }
    }
}
#[test]
fn native_net_qualifiers_preserve_existing_validation_and_type_ownership() {
    for code in [
        "module M;wire(strong1,weak0)a;endmodule",
        "module M;wire vectored[3:0]a;endmodule",
        "module M;wire#(1,2,3)a;endmodule",
        "module M;trireg(small)a;endmodule",
    ] {
        super::parse_source(code, Path::new("net-qualifiers.sv")).unwrap();
    }
    let mut tree = super::parse_source(
        "module M;wire logic[7:0]a=0;endmodule",
        Path::new("native-net-type.sv"),
    )
    .unwrap();
    for ty in &mut tree.declarations.types {
        if matches!(&ty.kind, DataTypeKind::Builtin(name) if name == "logic") {
            ty.kind = DataTypeKind::Builtin("bit".into());
        }
    }
    let source = Source::from_syntax(&tree).unwrap();
    assert!(matches!(
        source.modules()[0].signals()[0].r#type().kind(),
        crate::ast::TypeKind::Bit
    ));
}
#[test]
fn native_interface_net_bounds_drive_bound_signals() {
    let mut tree = super::parse_source(
        "interface I;wire[(10+2):0]a;assign a=0;endinterface module M;I bus();endmodule",
        Path::new("native-interface-net.sv"),
    )
    .unwrap();
    subtract(&mut tree);
    let source = Source::from_syntax_module_with_parameter_expr_overrides(
        &tree,
        "M",
        &HashMap::default(),
        &crate::ModuleInterfaces::default(),
    )
    .unwrap();
    let module = source
        .modules()
        .iter()
        .find(|module| module.name() == "M")
        .unwrap();
    let a = module
        .signals()
        .iter()
        .find(|signal| signal.name() == "bus$a")
        .unwrap();
    assert_eq!(number(a.r#type().packed_ranges()[0].left()), 8);
    assert!(a.is_net());
}
#[test]
fn deep_native_net_bounds_and_initializers_bind_on_the_default_stack() {
    let mut tree = super::parse_source(
        "module M;wire[(4):0]a=(3+2);endmodule",
        Path::new("native-deep-net.sv"),
    )
    .unwrap();
    let open = "(".repeat(10_000);
    let close = ")".repeat(10_000);
    tree.declarations = Declarations::parse(
        &format!("module M;wire[{open}4{close}:0]a={open}3+2{close};endmodule"),
        Path::new("native-deep-net.sv"),
    )
    .unwrap()
    .into();
    subtract(&mut tree);
    let source = Source::from_syntax(&tree).unwrap();
    assert_eq!(
        number(source.modules()[0].signals()[0].r#type().packed_ranges()[0].left()),
        4
    );
    assert!(
        source.modules()[0]
            .comb_processes()
            .iter()
            .flat_map(|process| process.assignments())
            .any(|assignment| matches!(
                assignment.rhs(),
                Expr::Binary {
                    op: BinaryOp::Sub,
                    ..
                }
            ))
    );
}
