use super::*;
use crate::ast::{BinaryOp, ConstExpr, Expr, Stmt};
use celox_sv_syntax::DataDeclaration;

#[test]
fn native_body_data_item_order_drives_variable_and_alias_initialization() {
    let mut tree = super::parse_source(
        "module M;int a=8+1,b=9+2;typedef logic[7:0]T;T c=10+3,d=11+4;endmodule",
        Path::new("native-data-initializer-order.sv"),
    )
    .unwrap();
    tree.declarations.units[0].items.swap(0, 2);
    for expression in &mut tree.declarations.expressions {
        if let celox_sv_syntax::ExprKind::Binary { operator, .. } = &mut expression.kind
            && operator == "+"
        {
            *operator = "-".into();
        }
    }
    let source = crate::ast::Source::from_syntax(&tree).unwrap();
    let assignments = source.modules()[0]
        .initial_processes()
        .iter()
        .flat_map(|p| p.body())
        .map(|statement| {
            let Stmt::Assign {
                lhs: crate::ast::LValue::Ident(name),
                rhs: Expr::Binary {
                    op: BinaryOp::Sub, ..
                },
                ..
            } = statement
            else {
                panic!("{statement:?}")
            };
            name.as_str()
        })
        .collect::<Vec<_>>();
    assert_eq!(assignments, ["c", "d", "a", "b"]);
}

#[test]
fn explicit_constructed_body_data_keeps_existing_ordinary_siblings() {
    let tree = super::parse_source(
        "module M(output int y);int dynamic_array[]=new[2],ordinary=3;initial y=ordinary;endmodule",
        Path::new("native-data-constructed-siblings.sv"),
    )
    .unwrap();
    let source = crate::ast::Source::from_syntax(&tree).unwrap();
    assert!(
        source.modules()[0]
            .signals()
            .iter()
            .any(|s| s.name() == "ordinary")
    );
    assert!(source.modules()[0].initial_processes().iter().flat_map(|p|p.body()).any(|s|matches!(s,Stmt::Assign{lhs:crate::ast::LValue::Ident(name),rhs:Expr::Literal(value),..} if name=="ordinary" && value=="3")));
}
fn number(value: &ConstExpr) -> i128 {
    let value: crate::ir::ConstExpr = value.clone().into();
    crate::typecheck::eval_const_expr(&value, &HashMap::default()).unwrap()
}
#[test]
fn native_body_variable_types_dimensions_and_initializers_drive_production() {
    let mut tree=super::parse_source("module M;logic[(10+2):0] a[(6+2)];int b=8+2;typedef logic[7:0] T;T c[(4+2)];if(1)begin:g logic[(10+2):0] z;int q=8+2;end endmodule",Path::new("native-body-ownership.sv")).unwrap();
    for expression in &mut tree.declarations.expressions {
        if let celox_sv_syntax::ExprKind::Binary { operator, .. } = &mut expression.kind
            && operator == "+"
        {
            *operator = "-".into()
        }
    }
    let source = crate::ast::Source::from_syntax(&tree).unwrap();
    let module = &source.modules()[0];
    let signal = |name: &str| {
        module
            .signals()
            .iter()
            .find(|signal| signal.name() == name)
            .unwrap()
    };
    assert_eq!(number(signal("a").r#type().packed_ranges()[0].left()), 8);
    assert_eq!(number(signal("a").r#type().unpacked_ranges()[0].right()), 3);
    assert_eq!(number(signal("c").r#type().unpacked_ranges()[0].right()), 1);
    assert_eq!(number(signal("g.z").r#type().packed_ranges()[0].left()), 8);
    let mut targets = Vec::new();
    for process in module.initial_processes() {
        for statement in process.body() {
            if let Stmt::Assign {
                lhs: crate::ast::LValue::Ident(name),
                rhs,
                ..
            } = statement
            {
                if matches!(name.as_str(), "b" | "g.q") {
                    assert!(
                        matches!(
                            rhs,
                            Expr::Binary {
                                op: BinaryOp::Sub,
                                ..
                            }
                        ),
                        "{rhs:?}"
                    );
                    targets.push(name.as_str());
                }
            }
        }
    }
    targets.sort();
    assert_eq!(targets, ["b", "g.q"]);
}
#[test]
fn native_interface_body_variable_bounds_drive_the_bound_signal() {
    let mut tree = super::parse_source(
        "interface I;logic[(10+2):0] a;endinterface module M;I bus();endmodule",
        Path::new("native-interface-body-type.sv"),
    )
    .unwrap();
    for expression in &mut tree.declarations.expressions {
        if let celox_sv_syntax::ExprKind::Binary { operator, .. } = &mut expression.kind
            && operator == "+"
        {
            *operator = "-".into()
        }
    }
    let source = crate::ast::Source::from_syntax_module_with_parameter_expr_overrides(
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
    let signal = module
        .signals()
        .iter()
        .find(|signal| signal.name() == "bus$a")
        .unwrap();
    assert_eq!(number(signal.r#type().packed_ranges()[0].left()), 8);
}
#[test]
fn native_body_data_membership_and_initializers_are_authoritative() {
    for defect in ["missing", "unconverted", "name", "count", "initializer"] {
        let mut tree = super::parse_source(
            "module M;int a=1,b=2;endmodule",
            Path::new("native-body-class.sv"),
        )
        .unwrap();
        match defect {
            "missing" => {
                for item in &mut tree.declarations.body_items {
                    if matches!(item.kind, celox_sv_syntax::BodyItemKind::Data(_)) {
                        item.kind = celox_sv_syntax::BodyItemKind::Unconverted {
                            category: "missing data".into(),
                            items: Vec::new(),
                        };
                    }
                }
            }
            "unconverted" => {
                let DataDeclaration::Variables(value) = &tree.declarations.data_declarations[0]
                else {
                    panic!()
                };
                tree.declarations.data_declarations[0] = DataDeclaration::Unconverted(value.tokens);
            }
            _ => {
                let DataDeclaration::Variables(value) = &mut tree.declarations.data_declarations[0]
                else {
                    panic!()
                };
                match defect {
                    "name" => value.variables[0].name.spelling = "other".into(),
                    "count" => {
                        value.variables.pop();
                    }
                    _ => value.variables[0].value = None,
                }
            }
        }
        if defect == "missing" {
            let source = crate::ast::Source::from_syntax(&tree).unwrap();
            assert!(source.modules()[0].signals().is_empty());
            assert!(source.modules()[0].initial_processes().is_empty());
        } else if defect == "unconverted" {
            assert!(crate::ast::Source::from_syntax(&tree).is_err(), "{defect}");
        } else {
            let source = crate::ast::Source::from_syntax(&tree).unwrap();
            let module = &source.modules()[0];
            let mut names = module
                .signals()
                .iter()
                .map(|signal| signal.name())
                .collect::<Vec<_>>();
            names.sort();
            assert_eq!(
                names,
                match defect {
                    "name" => vec!["b", "other"],
                    "count" => vec!["a"],
                    _ => vec!["a", "b"],
                }
            );
            let initialized = module
                .initial_processes()
                .iter()
                .flat_map(|process| process.body())
                .filter_map(|statement| match statement {
                    Stmt::Assign {
                        lhs: crate::ast::LValue::Ident(name),
                        ..
                    } => Some(name.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(
                initialized,
                match defect {
                    "name" => vec!["other", "b"],
                    "count" => vec!["a"],
                    _ => vec!["b"],
                }
            );
        }
    }
}
