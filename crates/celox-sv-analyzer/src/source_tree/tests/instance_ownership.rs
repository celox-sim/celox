use super::*;
use celox_sv_syntax::{Declarations, InstanceConnectionKind};
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{BinaryOp, Expr, Source};
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
    fn native_instance_list_order_retains_each_instances_typed_operands() {
        let mut tree=super::parse_source(
            "module C(input int a);endmodule module M(input int x);C first(.a(x+1)),second(.a(x+2));endmodule",
            Path::new("native-instance-list-order.sv"),
        ).unwrap();
        tree.declarations.instantiations[0].instances.swap(0, 1);
        subtract(&mut tree);
        let source = Source::from_syntax(&tree).unwrap();
        let module = source.modules().iter().find(|m| m.name() == "M").unwrap();
        let instances = module.instances();
        assert_eq!(
            instances.iter().map(|i| i.name()).collect::<Vec<_>>(),
            ["second", "first"]
        );
        for (instance, right) in instances.iter().zip(["2", "1"]) {
            assert!(
                matches!(instance.port_connections()[0].actual_expr(),Some(Expr::Binary{op:BinaryOp::Sub,right:operand,..}) if matches!(operand.as_ref(),Expr::Literal(value) if value==right))
            );
        }
    }

    #[test]
    fn native_interface_pending_ids_preserve_direct_region_conditional_and_loop_bindings() {
        let mut tree=super::parse_source(
            "interface I#(parameter int N=0);logic[N:0]a;endinterface module M;I#(.N(8+2))direct_();generate I#(.N(10+2))region_();endgenerate if(1)begin:g I#(.N(12+2))nested_();end for(genvar i=0;i<2;i++)begin:loop_ I#(.N(16+i))bus();end endmodule",
            Path::new("native-interface-pending-identities.sv"),
        ).unwrap();
        subtract(&mut tree);
        let source = Source::from_syntax_module_with_parameter_expr_overrides(
            &tree,
            "M",
            &HashMap::default(),
            &crate::ModuleInterfaces::default(),
        )
        .unwrap();
        let ir = crate::analyze::analyze_source(source).unwrap();
        let module = ir.modules().iter().find(|m| m.name() == "M").unwrap();
        let members = module
            .signals()
            .iter()
            .filter(|s| s.name().ends_with("$a"))
            .map(|s| (s.name(), s.r#type().resolved_width()))
            .collect::<Vec<_>>();
        assert_eq!(members.len(), 5, "{members:?}");
        for (path, width) in [
            ("direct_", 7),
            ("region_", 9),
            ("nested_", 11),
            ("[0]", 17),
            ("[1]", 16),
        ] {
            assert!(
                members
                    .iter()
                    .any(|(name, actual)| name.contains(path) && *actual == Some(width)),
                "{path}: {members:?}"
            );
        }
    }

    #[test]
    fn interface_pending_declarations_do_not_retry_removed_native_identity() {
        for invocation in ["I bus();", "if(1)begin:g I bus();end"] {
            let mut tree = super::parse_source(
                &format!("interface I;logic a;endinterface module M;{invocation} endmodule"),
                Path::new("native-interface-missing-identity.sv"),
            )
            .unwrap();
            for item in &mut tree.declarations.body_items {
                if matches!(item.kind, celox_sv_syntax::BodyItemKind::Instance(_)) {
                    item.kind = celox_sv_syntax::BodyItemKind::Unconverted {
                        category: "missing instance".into(),
                        items: Vec::new(),
                    };
                }
            }
            let source = Source::from_syntax_module_with_parameter_expr_overrides(
                &tree,
                "M",
                &HashMap::default(),
                &crate::ModuleInterfaces::default(),
            )
            .unwrap();
            assert!(source.modules()[0].instances().is_empty(), "{invocation}");
            assert!(source.modules()[0].signals().is_empty(), "{invocation}");
        }
    }
    #[test]
    fn native_instance_identity_arrays_and_connection_operands_drive_production() {
        let code = "module C(input int a);endmodule module D(input int b);endmodule module M(input int x);C old[(6+2):0](.a(x+1));if(1)begin:g C z(.a(x+2));end endmodule";
        let mut tree =
            super::parse_source(code, Path::new("native-instance-ownership.sv")).unwrap();
        subtract(&mut tree);
        for declaration in &mut tree.declarations.instantiations {
            declaration.name.spelling = "D".into();
            for instance in &mut declaration.instances {
                instance.name.spelling = format!("new_{}", instance.name.spelling);
                for connection in &mut instance.connections {
                    if let InstanceConnectionKind::Named { name, .. } = &mut connection.kind {
                        name.spelling = "b".into()
                    }
                }
            }
        }
        let source = Source::from_syntax(&tree).unwrap();
        let module = source
            .modules()
            .iter()
            .find(|module| module.name() == "M")
            .unwrap();
        assert_eq!(module.instances().len(), 2);
        assert_eq!(module.instances()[0].name(), "new_old");
        assert_eq!(module.instances()[0].array_range(), Some((4, 0)));
        assert_eq!(module.instances()[1].name(), "g.new_z");
        for instance in module.instances() {
            assert_eq!(instance.module_name(), "D");
            assert_eq!(instance.port_connections()[0].formal(), "b");
            assert!(matches!(
                instance.port_connections()[0].actual_expr(),
                Some(Expr::Binary {
                    op: BinaryOp::Sub,
                    ..
                })
            ));
        }
    }
    #[test]
    fn native_instance_ordered_implicit_empty_and_wildcard_contracts() {
        let mut tree = super::parse_source("module C(input int a,b,c);endmodule module M(input int a,b,c);C x(a+1,,c),y(.a,.b(),.c(c+1));endmodule", Path::new("native-port-forms.sv")).unwrap();
        subtract(&mut tree);
        let source = Source::from_syntax(&tree).unwrap();
        let module = source
            .modules()
            .iter()
            .find(|module| module.name() == "M")
            .unwrap();
        let instances = module.instances();
        assert_eq!(instances[0].port_connections().len(), 2);
        assert_eq!(instances[0].port_connections()[1].formal(), "c");
        assert!(matches!(
            instances[0].port_connections()[0].actual_expr(),
            Some(Expr::Binary {
                op: BinaryOp::Sub,
                ..
            })
        ));
        assert!(
            matches!(instances[1].port_connections()[0].actual_expr(), Some(Expr::Ident(name)) if name == "a")
        );
        assert!(instances[1].port_connections()[1].actual_expr().is_none());
        assert!(matches!(
            instances[1].port_connections()[2].actual_expr(),
            Some(Expr::Binary {
                op: BinaryOp::Sub,
                ..
            })
        ));
        let wildcard = super::parse_source(
            "module C(input int a);endmodule module M(input int a);C z(.*);endmodule",
            Path::new("native-wildcard.sv"),
        )
        .unwrap();
        assert!(
            matches!(Source::from_syntax(&wildcard), Err(AnalyzerError::Unsupported(message)) if message == "wildcard port connection")
        );
    }
    #[test]
    fn native_instance_membership_and_connection_classes_are_authoritative() {
        for defect in ["missing", "connection", "instance-count"] {
            let mut tree = super::parse_source(
                "module C(input int a);endmodule module M(input int a);C x(.a(a));endmodule",
                Path::new("native-instance-identity.sv"),
            )
            .unwrap();
            match defect {
                "missing" => {
                    for item in &mut tree.declarations.body_items {
                        if matches!(item.kind, celox_sv_syntax::BodyItemKind::Instance(_)) {
                            item.kind = celox_sv_syntax::BodyItemKind::Unconverted {
                                category: "missing instance".into(),
                                items: Vec::new(),
                            };
                        }
                    }
                }
                "connection" => {
                    tree.declarations.instantiations[0].instances[0].connections[0].kind =
                        InstanceConnectionKind::Ordered(None)
                }
                _ => {
                    tree.declarations.instantiations[0].instances.clear();
                }
            }
            let source = Source::from_syntax(&tree).unwrap();
            let module = source
                .modules()
                .iter()
                .find(|module| module.name() == "M")
                .unwrap();
            if defect == "connection" {
                assert_eq!(module.instances().len(), 1);
                assert!(module.instances()[0].port_connections().is_empty());
            } else {
                assert!(module.instances().is_empty());
            }
        }
    }
    #[test]
    fn deep_native_instance_bounds_and_values_bind_on_the_default_stack() {
        let path = Path::new("native-deep-instance.sv");
        let mut tree = super::parse_source(
            "module C(input int a);endmodule module M;C c[(4):0]((3+2));endmodule",
            path,
        )
        .unwrap();
        let open = "(".repeat(10_000);
        let close = ")".repeat(10_000);
        *tree.declarations = Declarations::parse(&format!("module C(input int a);endmodule module M;C c[{open}4{close}:0]({open}3+2{close});endmodule"), path).unwrap();
        subtract(&mut tree);
        let source = Source::from_syntax(&tree).unwrap();
        let module = source
            .modules()
            .iter()
            .find(|module| module.name() == "M")
            .unwrap();
        assert_eq!(module.instances()[0].array_range(), Some((4, 0)));
        assert!(matches!(
            module.instances()[0].port_connections()[0].actual_expr(),
            Some(Expr::Binary {
                op: BinaryOp::Sub,
                ..
            })
        ));
    }
    #[test]
    fn native_empty_instance_connections_preserve_the_authored_empty_list() {
        let tree = super::parse_source(
            "module C;endmodule module M;C a(),b(,);endmodule",
            Path::new("native-empty-connections.sv"),
        )
        .unwrap();
        assert!(
            tree.declarations.instantiations[0].instances[0]
                .connections
                .is_empty()
        );
        assert_eq!(
            tree.declarations.instantiations[0].instances[1]
                .connections
                .len(),
            2
        );
        let source = Source::from_syntax(&tree).unwrap();
        let module = source
            .modules()
            .iter()
            .find(|module| module.name() == "M")
            .unwrap();
        assert_eq!(module.instances().len(), 2);
        assert!(
            module
                .instances()
                .iter()
                .all(|instance| instance.port_connections().is_empty())
        );
    }
}

#[cfg(test)]
mod parameter_tests {
    use super::*;
    use crate::ast::{ConstExpr, Source};
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
    fn native_override_names_operators_and_type_bounds_drive_production() {
        let mut tree = super::parse_source("module C#(parameter N=0,M=0,parameter type T=int)();endmodule module M;C#(.N((8+2)),.T(logic[(10+2):0]))named_();C#((8+2),0,logic[(10+2):0])ordered_();endmodule", Path::new("native-overrides.sv")).unwrap();
        subtract(&mut tree);
        tree.declarations.instantiations[0]
            .parameters
            .as_mut()
            .unwrap()[0]
            .name
            .as_mut()
            .unwrap()
            .spelling = "M".into();
        let source = Source::from_syntax(&tree).unwrap();
        let module = source
            .modules()
            .iter()
            .find(|module| module.name() == "M")
            .unwrap();
        let named = module.instances()[0].parameter_overrides();
        assert_eq!(named[0].name(), "M");
        assert_eq!(number(named[0].value().unwrap()), 6);
        let ordered = module.instances()[1].parameter_overrides();
        assert_eq!(ordered[0].name(), "N");
        assert_eq!(number(ordered[0].value().unwrap()), 6);
        for overrides in [named, ordered] {
            let ty = overrides
                .iter()
                .find(|parameter| parameter.name() == "T")
                .unwrap()
                .bound_type()
                .unwrap();
            assert_eq!(number(ty.packed_ranges()[0].left()), 8);
        }
    }
    #[test]
    fn native_interface_override_values_bind_before_entering_the_interface() {
        let mut tree=super::parse_source("interface I#(parameter int N=0);logic[N:0]a;endinterface module M;I#(.N(8+2))bus();endmodule",Path::new("native-interface-overrides.sv")).unwrap();
        subtract(&mut tree);
        let source = Source::from_syntax_module_with_parameter_expr_overrides(
            &tree,
            "M",
            &HashMap::default(),
            &crate::ModuleInterfaces::default(),
        )
        .unwrap();
        let ir = crate::analyze::analyze_source(source).unwrap();
        let module = ir
            .modules()
            .iter()
            .find(|module| module.name() == "M")
            .unwrap();
        let a = module
            .signals()
            .iter()
            .find(|signal| signal.name() == "bus$a")
            .unwrap();
        assert_eq!(a.r#type().resolved_width(), Some(7));
    }
    #[test]
    fn native_override_membership_and_expression_classes_are_authoritative() {
        let code = "module C#(parameter N=0,parameter type T=int)();endmodule module M;C#(.N(8+2),.T(logic))c();endmodule";
        let mut tree = super::parse_source(code, Path::new("native-override-shape.sv")).unwrap();
        tree.declarations.instantiations[0]
            .parameters
            .as_mut()
            .unwrap()
            .pop();
        let source = Source::from_syntax(&tree).unwrap();
        let module = source
            .modules()
            .iter()
            .find(|module| module.name() == "M")
            .unwrap();
        let overrides = module.instances()[0].parameter_overrides();
        assert_eq!(overrides.len(), 1);
        assert_eq!(overrides[0].name(), "N");
        let mut tree = super::parse_source(code, Path::new("native-override-class.sv")).unwrap();
        let root = tree.declarations.instantiations[0]
            .parameters
            .as_ref()
            .unwrap()[1]
            .value
            .unwrap();
        let tokens = tree.declarations.expression(root).tokens;
        tree.declarations
            .expressions
            .iter_mut()
            .find(|expr| expr.tokens == tokens)
            .unwrap()
            .kind = celox_sv_syntax::ExprKind::Literal("7".into());
        let source = Source::from_syntax(&tree).unwrap();
        let module = source
            .modules()
            .iter()
            .find(|module| module.name() == "M")
            .unwrap();
        let override_ = &module.instances()[0].parameter_overrides()[1];
        assert!(override_.bound_type().is_none());
        assert_eq!(number(override_.value().unwrap()), 7);
    }
    #[test]
    fn deep_native_override_groups_and_type_bounds_bind_on_the_default_stack() {
        let path = Path::new("native-deep-overrides.sv");
        let mut tree=super::parse_source("module C#(parameter N=0,parameter type T=int)();endmodule module M;C#(.N((8+2)),.T(logic[(4):0]))c();endmodule",path).unwrap();
        let open = "(".repeat(10_000);
        let close = ")".repeat(10_000);
        *tree.declarations=Declarations::parse(&format!("module C#(parameter N=0,parameter type T=int)();endmodule module M;C#(.N({open}8+2{close}),.T(logic[{open}4{close}:0]))c();endmodule"),path).unwrap();
        subtract(&mut tree);
        let source = Source::from_syntax(&tree).unwrap();
        let module = source
            .modules()
            .iter()
            .find(|module| module.name() == "M")
            .unwrap();
        let values = module.instances()[0].parameter_overrides();
        assert_eq!(number(values[0].value().unwrap()), 6);
        assert_eq!(
            number(values[1].bound_type().unwrap().packed_ranges()[0].left()),
            4
        );
    }
    #[test]
    fn native_override_special_forms_preserve_existing_diagnostics() {
        for (value, diagnostic) in [
            ("1:2:3", "mintypmax expression"),
            ("1 inside {1,2}", "parameter override expression"),
        ] {
            let code = format!(
                "module C#(parameter N=0)();endmodule module M;C#(.N({value}))c();endmodule"
            );
            let tree =
                super::parse_source(&code, Path::new("native-unsupported-override.sv")).unwrap();
            assert!(
                matches!(Source::from_syntax(&tree),Err(AnalyzerError::Unsupported(message)) if message==diagnostic),
                "{value}"
            );
        }
        let tree = super::parse_source(
            "module C#(parameter N=0)();endmodule module M;C#(.N($))c();endmodule",
            Path::new("native-unbounded-override.sv"),
        )
        .unwrap();
        let source = Source::from_syntax(&tree).unwrap();
        let module = source
            .modules()
            .iter()
            .find(|module| module.name() == "M")
            .unwrap();
        assert!(
            matches!(module.instances()[0].parameter_overrides()[0].value(),Some(ConstExpr::Literal(value)) if value=="$")
        );
    }

    #[test]
    fn native_named_packed_override_dimensions_drive_production() {
        let path = Path::new("native-named-type-overrides.sv");
        let mut tree=super::parse_source("module C#(parameter type T=int)();endmodule module M;typedef logic signed[3:0]A;C#(.T(A[(10+2):0]))one();endmodule",path).unwrap();
        subtract(&mut tree);
        let source = Source::from_syntax(&tree).unwrap();
        let module = source
            .modules()
            .iter()
            .find(|module| module.name() == "M")
            .unwrap();
        let ty = module.instances()[0].parameter_overrides()[0]
            .bound_type()
            .unwrap();
        assert_eq!(
            ty.packed_ranges()
                .iter()
                .map(|range| number(range.left()))
                .collect::<Vec<_>>(),
            [8, 3]
        );
        assert!(!ty.is_signed());

        // The remaining legacy full validator cannot parse a named type with
        // two packed ranges. Isolate native unbound type/value normalization
        // with an independently parsed native fixture and an original builtin
        // type that supplies the same two bound-primary identities.
        let mut tree=super::parse_source("module C#(parameter type T=int)();endmodule module M;typedef logic signed[3:0]A;C#(.T(logic[(10+2):0][(6+2):0]))two();endmodule",path).unwrap();
        *tree.declarations=Declarations::parse("module C#(parameter type T=int)();endmodule module M;typedef logic signed[3:0]A;C#(.T(A[(10+2):0][(6+2):0]))two();endmodule",path).unwrap();
        subtract(&mut tree);
        let source = Source::from_syntax(&tree).unwrap();
        let module = source
            .modules()
            .iter()
            .find(|module| module.name() == "M")
            .unwrap();
        let ty = module.instances()[0].parameter_overrides()[0]
            .bound_type()
            .unwrap();
        assert_eq!(
            ty.packed_ranges()
                .iter()
                .map(|range| number(range.left()))
                .collect::<Vec<_>>(),
            [8, 4, 3]
        );
        assert!(!ty.is_signed());
    }
}
