use super::*;

pub(super) fn pair(declarations: &str, left: &str, right: &str) -> (Session, TyId, TyId) {
    let source =
        format!("module M; {declarations} assign out1={left}; assign out2={right}; endmodule");
    let syntax =
        crate::source_tree::parse_scope_fixture(&source, Path::new("enum-constant-types.sv"))
            .unwrap();
    let tree = AnalysisContext::unspecialized(&syntax);
    let native = tree.declarations();
    let a = native.assign_statements[0].assignments[0].right;
    let b = native.assign_statements[1].assignments[0].right;
    let mut session = Session::default();
    let dims = dimensions::query_operand_dimensions_id(
        b,
        &tree,
        &mut session,
        &PackedDimensions::default(),
    )
    .unwrap();
    let sites = session.next_call_site;
    let a =
        type_of(&tree, &mut session, a, native, &dims).unwrap_or_else(|e| panic!("{left}: {e}"));
    let b =
        type_of(&tree, &mut session, b, native, &dims).unwrap_or_else(|e| panic!("{right}: {e}"));
    assert_eq!(session.next_call_site, sites);
    (session, a, b)
}

fn enum_type(
    table: &semantic_types::TyTable,
    id: TyId,
    width: u128,
    signed: bool,
    four_state: bool,
) {
    let TyData::Packed(packed) = table.data(id) else {
        panic!("not packed")
    };
    assert!(matches!(
        packed.element,
        PackedElement::Enum { array: false, .. }
    ));
    assert_eq!(
        (packed.bits(), packed.signed, packed.four_state),
        (width, signed, four_state)
    );
}

#[test]
fn enum_constants_match_their_declared_variable_including_reversed_bounds() {
    // 6.19, 6.22.1: an enumerator has its declaration's enum type.
    for (base, width, signed, four_state) in [
        ("int", 32, true, false),
        ("logic signed[0:4]", 5, true, true),
        ("bit[4:0]", 5, false, false),
    ] {
        let (session, a, b) = pair(&format!("typedef enum {base} {{A,B}} E; E e;"), "A", "e");
        enum_type(&session.types, a, width, signed, four_state);
        assert!(session.types.matches(a, b));
    }
}

#[test]
fn aliases_do_not_change_the_constant_owner_and_other_enums_remain_distinct() {
    let (session, a, b) = pair("typedef enum {A,B} E; typedef E Alias; Alias e;", "A", "e");
    assert!(session.types.matches(a, b));
    let (session, a, b) = pair(
        "typedef enum {A=0,B=1} E; typedef enum {C=0,D=1} F;",
        "A",
        "C",
    );
    assert!(!session.types.matches(a, b));
    assert!(!session.types.equivalent(a, b));
}

#[test]
fn unknown_enum_constants_do_not_require_a_numeric_value_or_domain() {
    // 6.19: X/Z members are legal for an explicit four-state base.
    for constant in ["X", "Z"] {
        let (session, a, b) = pair(
            "typedef enum logic[1:0] {X=2'bxx,Z=2'bzz} E; E e;",
            constant,
            "e",
        );
        enum_type(&session.types, a, 2, false, true);
        assert!(session.types.matches(a, b));
    }
}

#[test]
fn constants_of_array_typedefs_still_have_scalar_enum_types() {
    // 6.19: dimensions around a typedef do not make its enumerators arrays.
    for dimensions in ["[2]", "[$]", "[]"] {
        let (session, a, _) = pair(
            &format!("typedef enum logic[0:3] {{A,B}} E {dimensions};"),
            "A",
            "A",
        );
        enum_type(&session.types, a, 4, false, true);
    }
}

#[test]
fn enum_methods_use_the_receivers_semantic_declaration_type() {
    for method in ["first()", "last()", "next()", "prev()", "first"] {
        let (session, a, b) = pair("typedef enum {A,B} E; E e;", &format!("A.{method}"), "e");
        assert!(session.types.matches(a, b));
    }
    let (session, a, b) = pair("typedef enum {A,B} E;", "A.name()", "A.num()");
    assert_eq!(session.types.data(a), &TyData::String);
    let TyData::Packed(packed) = session.types.data(b) else {
        panic!("not packed")
    };
    assert_eq!(
        (packed.bits(), packed.signed, packed.four_state),
        (32, true, false)
    );
}

pub(super) fn lexical_query(source: &str, wanted: &str) -> (Session, TyId) {
    let syntax =
        crate::source_tree::parse_scope_fixture(source, Path::new("lexical-enum-type.sv")).unwrap();
    let tree = AnalysisContext::unspecialized(&syntax);
    let native = tree.declarations();
    let id = native
        .expression_nodes()
        .filter_map(|(id, node)| match &node.kind {
            ExprKind::Name(name) if name.spelling == wanted => Some(id),
            ExprKind::Member {
                name, separator, ..
            } if name.spelling == wanted && separator == "::" => Some(id),
            _ => None,
        })
        .last()
        .unwrap();
    let mut session = Session::default();
    let dims = dimensions::query_operand_dimensions_id(
        id,
        &tree,
        &mut session,
        &PackedDimensions::default(),
    )
    .unwrap();
    let sites = session.next_call_site;
    let ty = type_of(&tree, &mut session, id, native, &dims)
        .unwrap_or_else(|error| panic!("{wanted}: {error}"));
    assert_eq!(session.next_call_site, sites);
    (session, ty)
}

#[test]
fn outer_enumerators_survive_a_typedef_name_shadow() {
    let (session, ty) = lexical_query(
        "module M; typedef enum bit[4:0] {A,B} E; initial begin typedef enum bit[2:0] {C,D} E; out=A; end endmodule",
        "A",
    );
    enum_type(&session.types, ty, 5, false, false);
}

#[test]
fn class_enum_constants_keep_the_specialization_and_inherited_owner() {
    for scope in ["C", "D"] {
        let source = format!(
            "module M; class C; typedef enum bit[4:0] {{A,B}} E; endclass class D extends C; endclass assign out={scope}::A; endmodule"
        );
        let (session, ty) = lexical_query(&source, "A");
        enum_type(&session.types, ty, 5, false, false);
    }
    let (session, ty) = lexical_query(
        "module M; class C #(int N=5); typedef enum bit[N-1:0] {A,B} E; endclass assign out=C#(7)::A; endmodule",
        "A",
    );
    enum_type(&session.types, ty, 7, false, false);
}

#[test]
fn generate_enum_constants_keep_their_scalar_bounds() {
    let (session, ty) = lexical_query(
        "module M; if(1) begin:g typedef enum bit[4:0] {A,B} E; assign out=A; end endmodule",
        "A",
    );
    enum_type(&session.types, ty, 5, false, false);
}

#[test]
fn frozen_constant_bindings_work_in_independent_tables_without_caller_names() {
    let source = "module M; typedef enum bit[4:0] {A,B} E; assign out=A; endmodule";
    let syntax =
        crate::source_tree::parse_scope_fixture(source, Path::new("enum-table-owner.sv")).unwrap();
    let tree = AnalysisContext::unspecialized(&syntax);
    let id = tree.declarations().assign_statements[0].assignments[0].right;
    let mut session = Session::default();
    let dims = dimensions::query_operand_dimensions_id(
        id,
        &tree,
        &mut session,
        &PackedDimensions::default(),
    )
    .unwrap();
    let binding = dims.constant_types["A"].clone();
    let mut unrelated = semantic_types::TyTable::default();
    unrelated.intern(TyData::String);
    let ty = binding.type_id(&mut unrelated).unwrap();
    enum_type(&unrelated, ty, 5, false, false);
}

#[test]
fn changed_constant_bindings_invalidate_expression_memos_and_values_do_not_choose_types() {
    let source = "module M; typedef enum bit[3:0] {A,B} E; typedef enum bit[7:0] {C,D} F; assign out=A; endmodule";
    let syntax =
        crate::source_tree::parse_scope_fixture(source, Path::new("enum-memo.sv")).unwrap();
    let tree = AnalysisContext::unspecialized(&syntax);
    let id = tree.declarations().assign_statements[0].assignments[0].right;
    let mut session = Session::default();
    let mut dims = dimensions::query_operand_dimensions_id(
        id,
        &tree,
        &mut session,
        &PackedDimensions::default(),
    )
    .unwrap();
    dims.scope_types_complete = true;
    let first = type_of(&tree, &mut session, id, tree.declarations(), &dims).unwrap();
    dims.const_env.insert("A".into(), 12345);
    let same = type_of(&tree, &mut session, id, tree.declarations(), &dims).unwrap();
    assert!(session.types.matches(first, same));
    let changed = dims.constant_types["C"].clone();
    Arc::make_mut(&mut dims.constant_types).insert("A".into(), changed);
    let second = type_of(&tree, &mut session, id, tree.declarations(), &dims).unwrap();
    enum_type(&session.types, second, 8, false, false);
    assert!(!session.types.matches(first, second));
}

#[test]
fn packages_preserve_enum_bindings_across_reexports_aliases_and_filtering() {
    let path = Path::new("package-enum-bindings.sv");
    let syntax=crate::source_tree::parse_source("package p; typedef enum bit[4:0] {A,B} E; endpackage package q; import p::*; export p::*; endpackage module M; endmodule",path).unwrap();
    let packages = packages::Packages::analyze(&[(&syntax, path)]).unwrap();
    let mut symbols = packages.get("q").unwrap().symbols.clone();
    assert!(symbols.enum_constants.declarations.contains_key("p::A"));
    symbols.alias("LocalA", "p::A");
    let mut design = packages.design();
    let a = symbols.enum_constants.declarations["LocalA"]
        .type_id(&mut design.types)
        .unwrap();
    enum_type(&design.types, a, 5, false, false);
    let symbols = symbols.without(&HashSet::from_iter(["LocalA".into()]));
    assert!(!symbols.enum_constants.declarations.contains_key("LocalA"));
    assert!(symbols.enum_constants.declarations.contains_key("p::A"));
}

#[test]
fn packed_enum_typedef_dimensions_do_not_widen_the_named_constants() {
    let (session, a, _) = pair("typedef enum bit[0:4] {A,B} [2:0] E;", "A", "A");
    enum_type(&session.types, a, 5, false, false);
}

#[test]
fn local_package_enum_declarations_shadow_wildcard_imports() {
    let path = Path::new("package-enum-shadows.sv");
    let syntax=crate::source_tree::parse_source("package p; typedef enum bit[4:0] {A,B} E; endpackage package q; import p::*; typedef enum bit[6:0] {A=7,C=8} F; endpackage module M; endmodule",path).unwrap();
    let packages = packages::Packages::analyze(&[(&syntax, path)]).unwrap();
    let mut design = packages.design();
    let binding = &packages
        .get("q")
        .unwrap()
        .symbols
        .enum_constants
        .declarations["q::A"];
    let ty = binding.type_id(&mut design.types).unwrap();
    enum_type(&design.types, ty, 7, false, false);
}

#[test]
fn inherited_parameter_shadows_cannot_reinterpret_an_enum_base() {
    let (session, ty) = lexical_query(
        "module M; class B; parameter int W=5; endclass class C extends B; typedef enum bit[W-1:0] {W=1,X=2} E; endclass assign out=C::W; endmodule",
        "W",
    );
    enum_type(&session.types, ty, 5, false, false);
}

#[test]
fn generate_member_shadows_keep_definition_site_base_bounds() {
    let syntax=crate::source_tree::parse_scope_fixture("module M; parameter int W=5; if(1) begin:g typedef enum bit[W-1:0] {W=1,X=2} E; assign out=W; end endmodule",Path::new("generate-enum-base.sv")).unwrap();
    let tree = AnalysisContext::unspecialized(&syntax);
    let mut session = Session::default();
    let env = HashMap::from_iter([("W".into(), 5)]);
    let items = generate::items(&tree, &mut session, &env, &HashMap::default()).unwrap();
    let base = PackedDimensions::new(HashMap::default(), &env, &HashMap::default());
    let dims = items.last().unwrap().dimensions(&base);
    let ty = dims.constant_types["W"]
        .type_id(&mut session.types)
        .unwrap();
    enum_type(&session.types, ty, 5, false, false);
}

#[test]
fn local_enumerators_shadow_outer_typedef_names_in_value_queries() {
    let (session, ty) = lexical_query(
        "module M; typedef int A; initial begin typedef enum bit[4:0] {A=1,B=2} E; out=A; end endmodule",
        "A",
    );
    enum_type(&session.types, ty, 5, false, false);
}

#[test]
fn shadowed_typedef_declarations_with_the_same_shape_remain_distinct() {
    let syntax=crate::source_tree::parse_scope_fixture("module M; typedef enum bit[4:0] {A,B} E; initial begin typedef enum bit[4:0] {C,D} E; out=A+C; end endmodule",Path::new("enum-declaration-shadow.sv")).unwrap();
    let tree = AnalysisContext::unspecialized(&syntax);
    let native = tree.declarations();
    let find = |wanted: &str| {
        native
            .expression_nodes()
            .find_map(|(id, node)| {
                matches!(&node.kind,ExprKind::Name(name) if name.spelling==wanted).then_some(id)
            })
            .unwrap()
    };
    let a = find("A");
    let c = find("C");
    let mut session = Session::default();
    let dims = dimensions::query_operand_dimensions_id(
        c,
        &tree,
        &mut session,
        &PackedDimensions::default(),
    )
    .unwrap();
    let outer = type_of(&tree, &mut session, a, native, &dims).unwrap();
    let inner = type_of(&tree, &mut session, c, native, &dims).unwrap();
    assert!(!session.types.matches(outer, inner));
    assert!(!session.types.equivalent(outer, inner));
}

#[test]
fn inline_enum_constants_match_their_declared_variables() {
    // Preserve inline enum collection while retaining its declaration type.
    for base in ["bit signed[0:4]", "logic signed[0:4]"] {
        let (session, constant, variable) =
            pair(&format!("enum {base} {{A,B}} value;"), "A", "value");
        enum_type(&session.types, constant, 5, true, base.starts_with("logic"));
        assert!(session.types.matches(constant, variable));
    }
}

#[test]
fn ranged_inline_enum_names_are_not_fabricated_as_scalar_constants() {
    let syntax = crate::source_tree::parse_scope_fixture(
        "module M; enum {A[1:0]} value; assign out=A; endmodule",
        Path::new("ranged-enum-name.sv"),
    )
    .unwrap();
    let tree = AnalysisContext::unspecialized(&syntax);
    let id = tree.declarations().assign_statements[0].assignments[0].right;
    let mut session = Session::default();
    if let Ok(dims) = dimensions::query_operand_dimensions_id(
        id,
        &tree,
        &mut session,
        &PackedDimensions::default(),
    ) {
        assert!(!dims.constant_types.contains_key("A"));
        assert!(type_of(&tree, &mut session, id, tree.declarations(), &dims).is_err());
    }
}
