//! ANSI port production regressions; no original port identity index.
use super::*;
use celox_sv_syntax::{Declarations, PortKind};
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{ConstExpr, PortDirection, Source, TypeKind};
    use celox_sv_syntax::{DataTypeKind, Direction, ExprKind};
    fn number(value: &ConstExpr) -> i128 {
        crate::typecheck::eval_const_expr(&value.clone().into(), &HashMap::default()).unwrap()
    }
    fn subtract(tree: &mut SourceTree) {
        for expression in &mut tree.declarations.expressions {
            if let ExprKind::Binary { operator, .. } = &mut expression.kind
                && operator == "+"
            {
                *operator = "-".into();
            }
        }
    }
    #[test]
    fn native_ansi_port_types_bounds_names_and_directions_drive_production() {
        let mut tree = super::super::parse_source("module M(input wire signed[(10+2):0] a[(6+2)],b,output var logic[(8+2):0] c[(4+2)]);assign c[0]=0;endmodule", Path::new("native-ports.sv")).unwrap();
        subtract(&mut tree);
        let PortKind::Named(name) = &mut tree.declarations.units[0].ports[0].kind else {
            unreachable!()
        };
        name.spelling = "native_a".into();
        tree.declarations.units[0].ports[0].direction = Some(Direction::Inout);
        for ty in &mut tree.declarations.types {
            if matches!(&ty.kind, DataTypeKind::Builtin(name) if name == "logic") {
                ty.kind = DataTypeKind::Builtin("bit".into());
            }
        }
        let source = Source::from_syntax(&tree).unwrap();
        let ports = source.modules()[0].ports();
        assert_eq!(ports[0].name(), "native_a");
        assert_eq!(
            crate::ast::Source::module_interfaces_from_syntax(&tree).unwrap()["M"].ports[0],
            "native_a"
        );
        assert_eq!(ports[0].direction(), PortDirection::Inout);
        assert_eq!(ports[1].direction(), PortDirection::Input);
        assert!(ports[0].r#type().is_signed());
        assert_eq!(number(ports[0].r#type().packed_ranges()[0].left()), 8);
        assert_eq!(number(ports[0].r#type().unpacked_ranges()[0].right()), 3);
        assert_eq!(number(ports[1].r#type().packed_ranges()[0].left()), 8);
        assert!(ports[1].r#type().unpacked_ranges().is_empty());
        assert_eq!(ports[2].r#type().kind(), TypeKind::Bit);
        assert_eq!(number(ports[2].r#type().packed_ranges()[0].left()), 6);
        assert_eq!(number(ports[2].r#type().unpacked_ranges()[0].right()), 1);
    }
    #[test]
    fn native_ansi_port_owner_membership_and_dimensions_are_authoritative() {
        for change in 0..2 {
            let mut tree = super::super::parse_source(
                "module M(input logic[3:0]a[2],b);endmodule",
                Path::new("port-identity.sv"),
            )
            .unwrap();
            match change {
                0 => {
                    tree.declarations.units[0].ports.pop();
                }
                _ => tree.declarations.units[0].ports[0].dimensions.clear(),
            }
            let source = Source::from_syntax(&tree).unwrap();
            let ports = source.modules()[0].ports();
            if change == 0 {
                assert_eq!(ports.len(), 1);
                assert_eq!(ports[0].name(), "a");
            } else {
                assert_eq!(ports.len(), 2);
                assert!(ports[0].r#type().unpacked_ranges().is_empty());
            }
        }
    }
    #[test]
    fn native_ansi_ports_cover_aliases_nested_units_and_external_headers() {
        let mut tree=super::super::parse_source("package p;typedef logic[3:0]T;endpackage extern module E(input int a);module M(input p::T[(8+2):0]a);module N(input wire signed[(6+2):0]b);endmodule endmodule",Path::new("port-owners.sv")).unwrap();
        subtract(&mut tree);
        let source = Source::from_syntax(&tree).unwrap();
        let ports = source
            .modules()
            .iter()
            .find(|module| module.name() == "M")
            .unwrap()
            .ports();
        assert_eq!(number(ports[0].r#type().packed_ranges()[0].left()), 6);
        assert_eq!(number(ports[1].r#type().packed_ranges()[0].left()), 4);
        assert!(ports[1].r#type().is_signed());
    }
    #[test]
    fn native_interface_scalar_port_bounds_survive_structural_views() {
        let mut tree = super::super::parse_source("interface I(input logic[(10+2):0]a);logic[(6+2):0]b;assign b=a;endinterface module M(input logic[(10+2):0]a,output logic[(6+2):0]y);I bus(a);assign y=bus.b;endmodule",Path::new("interface-scalar-ports.sv")).unwrap();
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
        assert_eq!(
            number(module.ports()[0].r#type().packed_ranges()[0].left()),
            8
        );
        assert_eq!(
            number(module.ports()[1].r#type().packed_ranges()[0].left()),
            4
        );
        let b = module
            .signals()
            .iter()
            .find(|signal| signal.name() == "bus$b")
            .unwrap();
        assert_eq!(number(b.r#type().packed_ranges()[0].left()), 4);
        crate::analyze::analyze_source(source).unwrap();
    }
    #[test]
    fn native_explicit_ports_and_unsupported_dimensions_keep_existing_behavior() {
        let tree = super::super::parse_source(
            "module M(input logic a,output .z(a));endmodule",
            Path::new("explicit-port.sv"),
        )
        .unwrap();
        let source = Source::from_syntax(&tree).unwrap();
        assert_eq!(source.modules()[0].ports()[1].name(), "z");
        assert_eq!(
            source.modules()[0].ports()[1].direction(),
            PortDirection::Output
        );
        for dimension in ["[]", "[$]", "[int]"] {
            let tree = super::super::parse_source(
                &format!("module M(input var logic a{dimension});endmodule"),
                Path::new("unsupported-port-array.sv"),
            )
            .unwrap();
            let result = Source::from_syntax(&tree);
            assert!(
                matches!(&result, Err(AnalyzerError::Unsupported(message)) if message.contains("array dimension")),
                "{dimension}: {result:?}"
            );
        }
    }
    #[test]
    fn native_ports_retain_authored_escaped_names() {
        let tree = super::super::parse_source(
            r"module M(input logic \a.b );endmodule",
            Path::new("escaped-port.sv"),
        )
        .unwrap();
        let source = Source::from_syntax(&tree).unwrap();
        assert_eq!(source.modules()[0].ports()[0].name(), r"\a.b");
    }
    #[test]
    fn deep_native_ansi_port_bounds_bind_without_recursive_native_walks() {
        let mut tree = super::super::parse_source(
            "module M(input wire[(4):0]a[(3+2)]);endmodule",
            Path::new("deep-ports.sv"),
        )
        .unwrap();
        let open = "(".repeat(10_000);
        let close = ")".repeat(10_000);
        // Isolate the native binder from the remaining full legacy validator.
        *tree.declarations = Declarations::parse(
            &format!("module M(input wire[{open}4{close}:0]a[{open}3+2{close}]);endmodule"),
            Path::new("deep-ports.sv"),
        )
        .unwrap();
        subtract(&mut tree);
        let source = Source::from_syntax(&tree).unwrap();
        let ty = source.modules()[0].ports()[0].r#type();
        assert_eq!(number(ty.packed_ranges()[0].left()), 4);
        assert_eq!(number(ty.unpacked_ranges()[0].right()), 0);
    }
}
