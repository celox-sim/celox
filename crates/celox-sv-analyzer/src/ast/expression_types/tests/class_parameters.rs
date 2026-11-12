use super::*;

fn packed(table: &semantic_types::TyTable, id: TyId, width: u128, signed: bool, four_state: bool) {
    let TyData::Packed(packed) = table.data(id) else {
        panic!("not integral")
    };
    assert_eq!(
        (packed.bits(), packed.signed, packed.four_state),
        (width, signed, four_state)
    );
}

#[test]
fn class_parameter_ports_keep_explicit_scalar_types() {
    for (declaration, width, signed, four_state) in [
        ("bit P=1", 1, false, false),
        ("logic signed[0:4] P=1", 5, true, true),
        ("int P=1", 32, true, false),
    ] {
        let (session, ty) = query(&format!("class C #({declaration}); endclass"), "C#()::P");
        packed(&session.types, ty, width, signed, four_state);
    }
}

#[test]
fn class_body_localparams_keep_scalar_kinds() {
    for (declaration, expected) in [
        ("localparam string P=\"hi\";", TyData::String),
        ("localparam real P=1.25;", TyData::Real),
        ("localparam shortreal P=1.25;", TyData::Shortreal),
    ] {
        let (session, ty) = query(&format!("class C; {declaration} endclass"), "C::P");
        assert_eq!(session.types.data(ty), &expected);
    }
}

#[test]
fn class_overrides_and_dependent_localparams_freeze_each_specialization() {
    for (n, expression, width) in [(3, "C#(3)::P", 3), (7, "C#(7)::P", 7)] {
        let (session, ty) = query(
            "class C #(int N=1); localparam bit signed[0:N-1] P=1; endclass",
            expression,
        );
        packed(&session.types, ty, width, true, false);
        let TyData::Packed(packed) = session.types.data(ty) else {
            unreachable!()
        };
        assert_eq!(
            packed.dims.as_ref(),
            &[Range {
                left: 0,
                right: n - 1
            }]
        );
    }
}

#[test]
fn class_parameters_of_type_parameters_use_the_actual_type() {
    for (actual, width, four_state) in [("bit[2:0]", 3, false), ("logic[6:0]", 7, true)] {
        let (session, ty) = query(
            "class C #(type T=int, T P=1); endclass",
            &format!("C#({actual})::P"),
        );
        packed(&session.types, ty, width, false, four_state);
    }
}

#[test]
fn inherited_class_parameters_and_derived_shadows_keep_their_types() {
    for (body, width, four_state) in [("", 5, false), ("localparam logic[8:0] P=1;", 9, true)] {
        let (session, ty) = query(
            &format!(
                "class B #(int N=5); localparam bit[N-1:0] P=1; endclass class D extends B; {body} endclass"
            ),
            "D::P",
        );
        packed(&session.types, ty, width, false, four_state);
    }
}

#[test]
fn class_handle_parameter_selections_use_the_specialized_declaration() {
    let (session, ty) = query(
        "class C #(int N=5); localparam bit[N-1:0] P=1; endclass C#(7) h;",
        "h.P",
    );
    packed(&session.types, ty, 7, false, false);
}

#[test]
fn class_enum_parameters_keep_nominal_identity() {
    let (session, a, b) = super::enum_constants::pair(
        "typedef enum logic[3:0] {A,B} E; class C #(E P=A); endclass E e;",
        "C#(E'(0))::P",
        "e",
    );
    assert!(session.types.matches(a, b));
    for expression in ["C::P", "D::P"] {
        let (session, a, b) = super::enum_constants::pair(
            "class C; typedef enum logic[3:0] {A,B} E; localparam E P=A; E e; endclass class D extends C; endclass C h;",
            expression,
            "h.e",
        );
        assert!(session.types.matches(a, b));
    }
}

#[test]
fn method_scopes_hide_enclosing_parameter_types_with_the_class_binding() {
    for declaration in ["bit[4:0] P=1", "P=$"] {
        let source = format!(
            "module M; parameter int P=9; class C #({declaration}); function int f(); return P; endfunction endclass C#() h; assign out=h; endmodule"
        );
        let syntax = crate::source_tree::parse_scope_fixture(
            &source,
            Path::new("class-parameter-body-scope.sv"),
        )
        .unwrap();
        let tree = AnalysisContext::unspecialized(&syntax);
        let native = tree.declarations();
        let root = native.assign_statements[0].assignments[0].right;
        let parameter = native
            .expression_nodes()
            .filter_map(|(id, node)| {
                matches!(&node.kind,ExprKind::Name(name) if name.spelling=="P").then_some(id)
            })
            .last()
            .unwrap();
        let mut session = Session::default();
        let dims = dimensions::query_operand_dimensions_id(
            root,
            &tree,
            &mut session,
            &PackedDimensions::default(),
        )
        .unwrap();
        let owner = type_of(&tree, &mut session, root, native, &dims).unwrap();
        let owner = class_name(&session.types, owner).unwrap();
        let (mut body, _) =
            classes::specializations::body_scope(&mut session, &owner, &dims, &HashMap::default());
        body.class_scope = Some(owner);
        if declaration == "P=$" {
            assert!(
                type_of(&tree, &mut session, parameter, native, &body).is_err(),
                "an unbounded class parameter must not acquire the enclosing int type"
            );
        } else {
            let ty = type_of(&tree, &mut session, parameter, native, &body).unwrap();
            packed(&session.types, ty, 5, false, false);
        }
    }
}

#[test]
fn package_class_parameter_types_survive_foreign_specialization_and_inheritance() {
    let path = Path::new("package-class-parameter-types.sv");
    let syntax = crate::source_tree::parse_source(
        "package pkg; class C #(int N=3); localparam bit signed[0:N-1] P=1; endclass endpackage",
        path,
    )
    .unwrap();
    let packages = packages::Packages::analyze(&[(&syntax, path)]).unwrap();
    let consumer = crate::source_tree::parse_scope_fixture(
        "module M; import pkg::*; class D extends C#(7); endclass assign out=D::P; endmodule",
        path,
    )
    .unwrap();
    let symbols = Arc::new(
        imports::resolve_imports(&consumer, &packages, None)
            .unwrap()
            .symbols,
    );
    let tree = AnalysisContext::unspecialized(&consumer).importing(symbols);
    let design = packages.design();
    let mut session = Session {
        foreign: design.foreign,
        types: design.types,
        ..Default::default()
    };
    let root = tree.declarations().assign_statements[0].assignments[0].right;
    let dims = dimensions::query_operand_dimensions_id(
        root,
        &tree,
        &mut session,
        &PackedDimensions::default(),
    )
    .unwrap();
    let ty = type_of(&tree, &mut session, root, tree.declarations(), &dims).unwrap();
    packed(&session.types, ty, 7, true, false);
}

#[test]
fn derived_members_hide_inherited_constants_after_package_export() {
    for (base, derived, width, four_state) in [
        ("localparam bit[4:0] P=1;", "static logic[8:0] P;", 9, true),
        (
            "typedef enum bit[4:0] {P=1} E;",
            "static logic[8:0] P;",
            9,
            true,
        ),
        (
            "localparam bit[4:0] P=1;",
            "static function logic[8:0] P(); return '1; endfunction",
            9,
            true,
        ),
        ("static logic[8:0] P;", "localparam bit[4:0] P=1;", 5, false),
    ] {
        let path = Path::new("package-property-constant-shadow.sv");
        let syntax=crate::source_tree::parse_source(
            &format!("package pkg; class B; {base} endclass class D extends B; {derived} endclass endpackage"),path,
        ).unwrap();
        let packages = packages::Packages::analyze(&[(&syntax, path)]).unwrap();
        let consumer = crate::source_tree::parse_scope_fixture(
            "module M; assign out=pkg::D::P; endmodule",
            path,
        )
        .unwrap();
        let symbols = Arc::new(
            imports::resolve_imports(&consumer, &packages, None)
                .unwrap()
                .symbols,
        );
        let tree = AnalysisContext::unspecialized(&consumer).importing(symbols);
        let design = packages.design();
        let mut session = Session {
            foreign: design.foreign,
            types: design.types,
            ..Default::default()
        };
        let root = tree.declarations().assign_statements[0].assignments[0].right;
        let dims = dimensions::query_operand_dimensions_id(
            root,
            &tree,
            &mut session,
            &PackedDimensions::default(),
        )
        .unwrap();
        let ty = type_of(&tree, &mut session, root, tree.declarations(), &dims).unwrap();
        packed(&session.types, ty, width, false, four_state);
    }
}
