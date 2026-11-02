use super::*;

#[test]
fn scalar_parameters_keep_their_declared_kind_and_state() {
    // IEEE 1800-2023 6.20.2: an explicit type fixes the parameter type.
    for (declaration, width, signed, four_state) in [
        ("parameter int P=1;", 32, true, false),
        ("parameter integer P=1;", 32, true, true),
        ("parameter bit P=1;", 1, false, false),
        ("parameter logic signed [0:4] P=1;", 5, true, true),
        ("parameter byte unsigned P=1;", 8, false, false),
    ] {
        let (session, ty) = query(declaration, "P");
        let TyData::Packed(packed) = session.types.data(ty) else {
            panic!("not integral")
        };
        assert_eq!(
            (packed.bits(), packed.signed, packed.four_state),
            (width, signed, four_state),
            "{declaration}"
        );
    }
}

#[test]
fn inferred_integral_parameters_are_logic_vectors_of_the_final_width() {
    // 6.20.2: inference does not preserve a two-state integer atom keyword.
    for (value, width, signed) in [
        ("8'sd1", 8, true),
        ("7'd1", 7, false),
        ("'1", 1, false),
        ("42", 32, true),
        ("\"hi\"", 16, false),
    ] {
        let (session, ty) = query(&format!("parameter P={value};"), "P");
        let TyData::Packed(packed) = session.types.data(ty) else {
            panic!("not integral")
        };
        assert_eq!(
            (
                packed.bits(),
                packed.signed,
                packed.four_state,
                packed.keyword
            ),
            (width, signed, true, None),
            "{value}"
        );
    }
}

#[test]
fn parameter_types_include_string_real_and_shortreal() {
    for (declaration, expected) in [
        ("parameter string P=\"hi\";", TyData::String),
        ("parameter real P=1.25;", TyData::Real),
        ("parameter shortreal P=1.25;", TyData::Shortreal),
        ("parameter P=1.25;", TyData::Real),
    ] {
        let (session, ty) = query(declaration, "P");
        assert_eq!(session.types.data(ty), &expected, "{declaration}");
    }
}

#[test]
fn typed_enum_parameters_keep_nominal_identity() {
    let (session, a, b) = super::enum_constants::pair(
        "typedef enum logic[2:0] {A,B} E; parameter E P=A; E e;",
        "P",
        "e",
    );
    assert!(session.types.matches(a, b));
}

#[test]
fn packed_parameter_selects_use_the_declared_bounds() {
    let (session, ty) = query("parameter bit signed[0:4] P=1;", "P[0:2]");
    let TyData::Packed(packed) = session.types.data(ty) else {
        panic!("not integral")
    };
    assert_eq!(
        (packed.bits(), packed.signed, packed.four_state),
        (3, false, false)
    );
}

#[test]
fn local_parameters_hide_outer_constants_and_variables_hide_parameters() {
    for (local, expected) in [
        (
            "localparam bit P=1;",
            TyData::Packed(semantic_types::Packed {
                element: PackedElement::Bits,
                four_state: false,
                signed: false,
                signed_element_depth: None,
                dims: Box::new([]),
                keyword: None,
            }),
        ),
        ("string P;", TyData::String),
    ] {
        let source = format!(
            "module M; parameter int P=1; initial begin {local} $display(P); end endmodule"
        );
        let (session, ty) = super::enum_constants::lexical_query(&source, "P");
        assert_eq!(session.types.data(ty), &expected);
    }
}

#[test]
fn unpacked_parameter_queries_keep_the_array_layer() {
    let (session, ty) = query("parameter bit[2:0] P[2]='{1,2};", "P");
    assert!(matches!(session.types.data(ty), TyData::Array { .. }));
}

#[test]
fn retained_parameter_types_are_frozen_per_override_and_can_cross_tables() {
    let syntax = crate::source_tree::parse_scope_fixture(
        "module M #(parameter N=4, parameter bit signed[0:N-1] P=1); endmodule",
        Path::new("parameter-type-overrides.sv"),
    )
    .unwrap();
    let tree = AnalysisContext::unspecialized(&syntax);
    let mut session = Session::default();
    let mut bindings = Vec::new();
    for width in [4, 9] {
        let parameters = crate::ast::parameters_from_scope(
            &tree,
            &mut session,
            &HashMap::default(),
            &HashMap::default(),
            &HashMap::from_iter([("N".into(), ConstExpr::Literal(width.to_string()))]),
        )
        .unwrap();
        bindings.push(parameters[1].semantic_type.clone().unwrap());
    }
    let mut unrelated = semantic_types::TyTable::default();
    unrelated.intern(TyData::String);
    for (binding, width) in bindings.iter().zip([4, 9]) {
        let ty = binding.type_id(&mut unrelated).unwrap();
        let TyData::Packed(packed) = unrelated.data(ty) else {
            panic!("not integral")
        };
        assert_eq!(
            (packed.bits(), packed.signed, packed.four_state),
            (width, true, false)
        );
        assert_eq!(
            packed.dims.as_ref(),
            &[Range {
                left: 0,
                right: width as i128 - 1
            }]
        );
    }
}

#[test]
fn package_parameter_bindings_survive_reexport_and_renaming() {
    let path = Path::new("package-parameter-types.sv");
    let syntax=crate::source_tree::parse_source(
        "package p; parameter shortreal R=1.25; parameter bit[0:4] P=1; endpackage package q; import p::*; export p::*; endpackage module M; endmodule",path,
    ).unwrap();
    let packages = packages::Packages::analyze(&[(&syntax, path)]).unwrap();
    let consumer = crate::source_tree::parse_scope_fixture(
        "module Consumer; import q::*; assign out=P; endmodule",
        path,
    )
    .unwrap();
    let mut symbols = imports::resolve_imports(&consumer, &packages, None)
        .unwrap()
        .symbols;
    symbols.alias("LocalP", "p::P");
    let mut design = packages.design();
    let parameter = symbols
        .parameters
        .iter()
        .find(|p| p.name() == "LocalP")
        .unwrap();
    let ty = parameter
        .semantic_type
        .as_ref()
        .unwrap()
        .type_id(&mut design.types)
        .unwrap();
    let TyData::Packed(packed) = design.types.data(ty) else {
        panic!("not integral")
    };
    assert_eq!(packed.dims.as_ref(), &[Range { left: 0, right: 4 }]);
    let parameter = symbols
        .parameters
        .iter()
        .find(|p| p.name() == "p::R")
        .unwrap();
    let ty = parameter
        .semantic_type
        .as_ref()
        .unwrap()
        .type_id(&mut design.types)
        .unwrap();
    assert_eq!(design.types.data(ty), &TyData::Shortreal);
}

#[test]
fn preliminary_queries_preserve_final_scalar_parameter_bindings() {
    // A scalar bit has no packed range, just like an implicit genvar binding.
    // Its declaration binding keeps it distinct from the latter's logic type.
    let syntax = crate::source_tree::parse_scope_fixture(
        "module M; parameter bit P=1; assign out=P; endmodule",
        Path::new("scalar-parameter-binding.sv"),
    )
    .unwrap();
    let tree = AnalysisContext::unspecialized(&syntax);
    let id = tree.declarations().assign_statements[0].assignments[0].right;
    let mut session = Session::default();
    let parameters = crate::ast::parameters_from_scope(
        &tree,
        &mut session,
        &HashMap::default(),
        &HashMap::default(),
        &HashMap::default(),
    )
    .unwrap();
    let env = crate::ast::parameters::const_env_from_parameters(&tree, &mut session, &parameters);
    let mut input = PackedDimensions::new(HashMap::default(), &env, &HashMap::default());
    Arc::make_mut(&mut input.constant_types)
        .insert("P".into(), parameters[0].semantic_type.clone().unwrap());
    let dims = dimensions::query_operand_dimensions_id(id, &tree, &mut session, &input).unwrap();
    let ty = type_of(&tree, &mut session, id, tree.declarations(), &dims).unwrap();
    let TyData::Packed(packed) = session.types.data(ty) else {
        panic!("not integral")
    };
    assert!(!packed.four_state);
}

#[test]
fn generated_parameters_keep_each_iterations_type_and_shadow_the_outer_one() {
    let syntax=crate::source_tree::parse_scope_fixture(
        "module M; parameter int P=1; for(genvar i=0;i<2;i++) begin:g localparam bit[0:i+2] P=1; assign out=P; end endmodule",
        Path::new("generate-parameter-types.sv"),
    ).unwrap();
    let tree = AnalysisContext::unspecialized(&syntax);
    let mut session = Session::default();
    let items = generate::items(
        &tree,
        &mut session,
        &HashMap::default(),
        &HashMap::default(),
    )
    .unwrap();
    let mut widths = Vec::new();
    for item in &items {
        let dims = item.dimensions(&PackedDimensions::default());
        let Some(binding) = dims.constant_types.get("P") else {
            continue;
        };
        let ty = binding.type_id(&mut session.types).unwrap();
        let TyData::Packed(packed) = session.types.data(ty) else {
            panic!("not integral")
        };
        assert!(!packed.four_state);
        widths.push(packed.bits());
    }
    widths.sort_unstable();
    widths.dedup();
    assert_eq!(widths, vec![3, 4]);
}

#[test]
fn nested_generate_signals_hide_enclosing_parameter_types() {
    for declaration in ["logic [7:0] P;", "wire [7:0] P;", "bit [7:0] P;"] {
        let source = format!(
            "module M; if(1) begin:g localparam bit P=1; if(1) begin:h {declaration} assign out=P; end end endmodule"
        );
        let syntax = crate::source_tree::parse_scope_fixture(
            &source,
            Path::new("generate-signal-type-shadow.sv"),
        )
        .unwrap();
        let tree = AnalysisContext::unspecialized(&syntax);
        let mut session = Session::default();
        let items = generate::items(
            &tree,
            &mut session,
            &HashMap::default(),
            &HashMap::default(),
        )
        .unwrap();
        let mut checked = false;
        for item in items {
            let celox_sv_syntax::BodyItemKind::Assign(id) =
                tree.declarations().body_item(item.id).kind
            else {
                continue;
            };
            let root = tree.declarations().assign_statement(id).assignments[0].right;
            let input = item.dimensions(&PackedDimensions::default());
            assert!(
                !input.constant_types.contains_key("P"),
                "{declaration}: stale parameter type"
            );
            assert!(
                input.constant_types.contains_key("g.P"),
                "the outer qualified binding must remain visible"
            );
            let scoped = item.enter(&tree);
            let dims = dimensions::query_operand_dimensions_id(root, &scoped, &mut session, &input)
                .unwrap();
            let ty = type_of(&scoped, &mut session, root, scoped.declarations(), &dims).unwrap();
            let TyData::Packed(packed) = session.types.data(ty) else {
                panic!("not integral")
            };
            assert_eq!(packed.bits(), 8, "{declaration}");
            assert_eq!(packed.four_state, !declaration.starts_with("bit"));
            checked = true;
        }
        assert!(checked);
    }
}

#[test]
fn nested_generate_imports_replace_enclosing_parameter_types() {
    // IEEE 1800-2023 26.3: an imported name denotes the package declaration.
    for (declaration, enumeration, width) in [
        ("typedef enum logic[4:0] {P=1} E;", true, 5),
        ("parameter bit[6:0] P=1;", false, 7),
    ] {
        for import in ["import pkg::P;", "import pkg::*;"] {
            let path = Path::new("generate-import-type-shadow.sv");
            let package = crate::source_tree::parse_source(
                &format!("package pkg; {declaration} endpackage"),
                path,
            )
            .unwrap();
            let packages = packages::Packages::analyze(&[(&package, path)]).unwrap();
            let source = format!(
                "module M; if(1) begin:g localparam bit P=1; if(1) begin:h {import} assign out=P; end end endmodule"
            );
            let syntax = crate::source_tree::parse_scope_fixture(&source, path).unwrap();
            let symbols = Arc::new(
                imports::resolve_imports(&syntax, &packages, None)
                    .unwrap()
                    .symbols,
            );
            let tree = AnalysisContext::unspecialized(&syntax).importing(symbols.clone());
            let mut session = Session::default();
            let mut input = PackedDimensions::new(
                HashMap::default(),
                &symbols.const_env,
                &symbols.type_aliases,
            );
            input.constant_types = Arc::new(symbols.enum_constants.declarations.clone());
            for parameter in &symbols.parameters {
                if let Some(ty) = &parameter.semantic_type {
                    Arc::make_mut(&mut input.constant_types)
                        .insert(parameter.name().into(), ty.clone());
                }
            }
            let expected = input.constant_types["pkg::P"]
                .type_id(&mut session.types)
                .unwrap();
            let items = generate::items(
                &tree,
                &mut session,
                &symbols.const_env,
                &symbols.type_aliases,
            )
            .unwrap();
            let mut checked = false;
            for item in items {
                let celox_sv_syntax::BodyItemKind::Assign(id) =
                    tree.declarations().body_item(item.id).kind
                else {
                    continue;
                };
                let root = tree.declarations().assign_statement(id).assignments[0].right;
                let dims = item.dimensions(&input);
                let scoped = item.enter(&tree);
                let ty =
                    type_of(&scoped, &mut session, root, scoped.declarations(), &dims).unwrap();
                assert!(
                    session.types.matches(ty, expected),
                    "{declaration} {import}: the import must select the package declaration"
                );
                let TyData::Packed(packed) = session.types.data(ty) else {
                    panic!("not integral")
                };
                assert_eq!(
                    matches!(packed.element, PackedElement::Enum { array: false, .. }),
                    enumeration
                );
                assert_eq!(packed.bits(), width);
                let outer = dims.constant_types["g.P"]
                    .type_id(&mut session.types)
                    .unwrap();
                let TyData::Packed(outer) = session.types.data(outer) else {
                    panic!("not integral")
                };
                assert_eq!(outer.bits(), 1);
                assert_eq!(outer.element, PackedElement::Bits);
                checked = true;
            }
            assert!(checked);
        }
    }
}
