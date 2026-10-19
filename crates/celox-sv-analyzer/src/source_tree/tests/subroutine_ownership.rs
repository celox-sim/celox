use super::*;
#[cfg(test)]
mod tests {
    use super::*;
    use celox_sv_syntax::{BodyItemKind, Direction};
    #[test]
    fn native_header_fields_and_membership_are_authoritative() {
        let mut tree=super::super::parse_source("module M;function automatic int f(input int a=1+2);return a;endfunction task t;output int a; a=3;endtask endmodule",Path::new("subroutine-join.sv")).unwrap();
        for edit in 0..4 {
            let mut native = tree.declarations.clone();
            match edit {
                0 => native.subroutines[0].name.spelling = "other".into(),
                1 => native.subroutines[0].ports.as_mut().unwrap()[0].default = None,
                2 => {
                    native.subroutines[1].port_declarations[0].ports[0].direction =
                        Some(Direction::Input)
                }
                _ => {
                    native
                        .body_items
                        .iter_mut()
                        .find(|item| matches!(item.kind, BodyItemKind::Subroutine(_)))
                        .unwrap()
                        .kind = BodyItemKind::Unconverted {
                        category: "missing function".into(),
                        items: Vec::new(),
                    };
                }
            }
            let previous = std::mem::replace(&mut tree.declarations, native);
            let source = crate::ast::Source::from_syntax(&tree).unwrap();
            let routines = source.modules()[0].subroutines();
            match edit {
                0 => assert!(routines.iter().any(|routine| routine.name == "other")),
                1 => assert!(
                    routines
                        .iter()
                        .find(|routine| routine.name == "f")
                        .unwrap()
                        .params[0]
                        .default
                        .is_none()
                ),
                2 => assert_eq!(
                    routines
                        .iter()
                        .find(|routine| routine.name == "t")
                        .unwrap()
                        .params[0]
                        .direction,
                    crate::procedural::ParamDirection::Input
                ),
                _ => assert!(routines.iter().all(|routine| routine.name != "f")),
            }
            tree.declarations = previous;
        }
    }
    #[test]
    fn prototypes_constructors_and_epsilon_formals_have_explicit_classes() {
        for source in [
            "class C;extern function int f(input int a);pure virtual task t(input int a);function new(int a);super.new(a);endfunction endclass module M;endmodule",
            "module M;function int f();return 1;endfunction task t();endtask endmodule",
            "function int f(input int a);return a;endfunction module M;endmodule",
            "checker C(input logic a);assert property(a);endchecker function int f();return 1;endfunction module M;endmodule",
            "module M;class C;function new();endfunction function int f(input int a);return a;endfunction endclass endmodule",
        ] {
            super::super::parse_source(source, Path::new("subroutine-classes.sv"))
                .unwrap_or_else(|error| panic!("{source}: {error}"));
        }
    }
}

#[test]
fn native_subroutine_item_order_and_default_expression_ids_reach_real_bodies() {
    let mut tree=parse_source("module M(output int y);function automatic int f(input int x=8+2);return x;endfunction function automatic int g(input int x=6+2);return x;endfunction assign y=f()+g();endmodule", Path::new("native-subroutine-owners.sv")).unwrap();
    tree.declarations.units[0].items.swap(0, 1);
    for value in &mut tree.declarations.expressions {
        if let celox_sv_syntax::ExprKind::Binary { operator, .. } = &mut value.kind
            && operator == "+"
        {
            *operator = "-".into();
        }
    }
    let source = crate::ast::Source::from_syntax(&tree).unwrap();
    let routines = source.modules()[0].subroutines();
    assert_eq!(
        routines
            .iter()
            .map(|routine| routine.name.as_str())
            .collect::<Vec<_>>(),
        ["g", "f"]
    );
    for routine in routines {
        assert!(matches!(
            routine.params[0].default.as_ref(),
            Some(crate::ast::Expr::Binary {
                op: crate::ast::BinaryOp::Sub,
                ..
            })
        ));
    }
    crate::analyze::analyze_source(source).unwrap();
}
#[test]
fn captured_class_method_identity_is_required_by_existing_module_consumers() {
    let mut tree=parse_source("module M(output int y);class C;function automatic int f(input int x=3);return x;endfunction endclass assign y=f();endmodule", Path::new("captured-class-method.sv")).unwrap();
    crate::ast::Source::from_syntax(&tree).unwrap();
    let class = tree.declarations.units[0].items[0];
    let tokens = tree.declarations.body_item(class).tokens;
    let celox_sv_syntax::BodyItemKind::Unconverted { items, .. } = &mut tree
        .declarations
        .body_items
        .iter_mut()
        .find(|item| item.tokens == tokens)
        .unwrap()
        .kind
    else {
        panic!()
    };
    items.clear();
    let source = crate::ast::Source::from_syntax(&tree).unwrap();
    assert!(
        source.modules()[0]
            .subroutines()
            .iter()
            .all(|routine| routine.name != "f")
    );
}

#[test]
fn ignored_program_methods_do_not_corrupt_native_function_query_scope() {
    let tree=parse_source("program unused;function int helper();return 1;endfunction endprogram module M(input logic[1:0]a);function automatic int g(input logic[3:0]a);logic[$bits(a)-1:0]x;return $bits(x);endfunction localparam N=g(0);endmodule",Path::new("native-subroutine-query-scopes.sv")).unwrap();
    let ir =
        crate::analyze::analyze_source(crate::ast::Source::from_syntax(&tree).unwrap()).unwrap();
    // The separate expression-form constant-function compiler currently folds
    // N using the module's shadowed width (2), also on immutable81ebab922.
    // This retirement preserves that pre-existing behavior; runtime formals
    // and body-local type discovery still use the proper native scope.
    let g = ir.modules()[0]
        .subroutines()
        .iter()
        .find(|routine| routine.name == "g")
        .unwrap();
    assert_eq!(g.params[0].r#type.resolved_width(), Some(4));
    let local = ir.modules()[0]
        .locals()
        .iter()
        .find(|local| local.name.starts_with("x@"))
        .unwrap();
    assert_eq!(local.r#type.resolved_width(), Some(4));
}
