use super::*;

fn integral(declarations: &str, expression: &str, width: u128, signed: bool, four_state: bool) {
    let (session, ty) = query(declarations, expression);
    let TyData::Packed(packed) = session.types.data(ty) else {
        panic!("`{expression}` is not integral")
    };
    assert_eq!(
        (packed.bits(), packed.signed, packed.four_state),
        (width, signed, four_state),
        "{expression}"
    );
    assert_eq!(packed.element, PackedElement::Bits, "{expression}");
}

#[test]
fn size_casts_preserve_operand_signing_and_state_with_one_new_dimension() {
    // 6.24.1: a size cast has one [n-1:0] range and the operand's signing/state.
    let declarations = "bit signed[1:0][3:0] a; logic[5:0] b;";
    for (expression, width, signed, four_state) in [
        ("13'(a)", 13, true, false),
        ("3'(b)", 3, false, true),
        ("(3+4)'(a)", 7, true, false),
        ("5'(unsigned'(a))", 5, false, false),
        ("signed'(11'(b))", 11, true, true),
    ] {
        integral(declarations, expression, width, signed, four_state);
    }
    let (session, ty) = query(declarations, "13'(a)");
    let TyData::Packed(packed) = session.types.data(ty) else {
        unreachable!()
    };
    assert_eq!(packed.dims.as_ref(), &[Range { left: 12, right: 0 }]);
}

#[test]
fn sign_and_size_casts_remove_enum_and_aggregate_identity() {
    // 6.24.1: these casts produce packed arrays of bit or logic.
    let declarations = "typedef enum bit signed[4:0] {A,B} E; E e; typedef struct packed {bit[4:0] value;} S; S s;";
    integral(declarations, "unsigned'(e)", 5, false, false);
    integral(declarations, "7'(e)", 7, true, false);
    integral(declarations, "signed'(s)", 5, true, false);
}

#[test]
fn declared_casts_keep_target_type_and_nominal_identity() {
    // 6.24.1, 6.19.4: query the result type; compatibility stays in binding.
    let declarations =
        "typedef enum logic[2:0] {A,B} E; typedef struct packed {bit[4:0] value;} S;";
    let (session, ty) = query(declarations, "E'(3'd1)");
    assert!(matches!(
        session.types.data(ty),
        TyData::Packed(semantic_types::Packed {
            element: PackedElement::Enum {
                decl: Decl::Named(_),
                array: false
            },
            ..
        })
    ));
    let (session, ty) = query(declarations, "S'(5'd1)");
    assert!(matches!(
        session.types.data(ty),
        TyData::Packed(semantic_types::Packed {
            element: PackedElement::Aggregate(_),
            ..
        })
    ));
    for (expression, expected) in [
        ("real'(1)", TyData::Real),
        ("shortreal'(1)", TyData::Shortreal),
        ("string'(16'h4142)", TyData::String),
    ] {
        let (session, ty) = query("", expression);
        assert_eq!(session.types.data(ty), &expected);
    }
    integral("", "byte'(4'hf)", 8, true, false);
    integral("", "integer'(1)", 32, true, true);
}

#[test]
fn const_casts_preserve_nonintegral_and_named_types() {
    // 6.24.1: const changes value mutability, not its type.
    let declarations = "class C; endclass C c; string s; typedef enum {A,B} E; E e; string a[2];";
    for (expression, kind) in [
        ("const'(c)", "class"),
        ("const'(s)", "string"),
        ("const'(e)", "enum"),
        ("const'(a)", "array"),
    ] {
        let (session, ty) = query(declarations, expression);
        assert!(
            match (kind, session.types.data(ty)) {
                ("class", TyData::Class(_))
                | ("string", TyData::String)
                | ("array", TyData::Array { .. }) => true,
                ("enum", TyData::Packed(packed)) =>
                    matches!(packed.element, PackedElement::Enum { .. }),
                _ => false,
            },
            "{expression}"
        );
    }
}

#[test]
fn conditional_numeric_types_use_both_branches_and_ignore_predicate_width() {
    // 11.4.11, 11.6.1: no constant folding or assignment context chooses a type.
    let declarations = "bit signed[3:0] a; bit signed[7:0] b; logic[2:0] u; logic[127:0] pred;";
    integral(declarations, "pred ? a : b", 8, true, true);
    integral(declarations, "1 ? a : u", 4, false, true);
    integral(declarations, "0 ? u : a", 4, false, true);
    integral(declarations, "1.0 ? a : b", 8, true, false);
    for (expression, expected) in [
        ("pred ? r : a", TyData::Real),
        ("pred ? a : s", TyData::Shortreal),
        ("pred ? s : r", TyData::Real),
    ] {
        let (session, ty) = query("bit pred; real r; shortreal s; int a;", expression);
        assert_eq!(session.types.data(ty), &expected);
    }
}

#[test]
fn matching_conditional_branches_keep_named_and_nonintegral_types() {
    // 11.4.11: retain the matching type instead of reducing to layout bits.
    let declarations = "bit p; class C; endclass C a,b; string s,t; typedef enum {A,B} E; E e,f; string x[2],y[2];";
    for (expression, kind) in [
        ("p?a:b", "class"),
        ("p?s:t", "string"),
        ("p?e:f", "enum"),
        ("p?x:y", "array"),
    ] {
        let (session, ty) = query(declarations, expression);
        assert!(
            match (kind, session.types.data(ty)) {
                ("class", TyData::Class(_))
                | ("string", TyData::String)
                | ("array", TyData::Array { .. }) => true,
                ("enum", TyData::Packed(packed)) =>
                    matches!(packed.element, PackedElement::Enum { .. }),
                _ => false,
            },
            "{expression}"
        );
    }
    integral(
        "bit p; typedef enum logic[2:0] {A,B} E; typedef enum logic[2:0] {C,D} F; E e; F f;",
        "p?e:f",
        3,
        false,
        true,
    );
}

#[test]
fn null_has_its_own_type_and_class_conditionals_keep_the_handle_type() {
    // 11.4.11 a,b: null/null stays null; one null preserves the class branch.
    for expression in ["null", "1 ? null : null", "const'(null)"] {
        let (session, ty) = query("", expression);
        assert_eq!(session.types.data(ty), &TyData::Null);
        assert_eq!(session.types.spell(ty), "null");
    }
    for expression in ["1 ? c : null", "0 ? null : c"] {
        let (session, ty) = query("class C; endclass C c;", expression);
        assert!(matches!(session.types.data(ty), TyData::Class(_)));
    }
}

#[test]
fn assignment_and_copy_expressions_keep_the_static_destination_or_source_type() {
    // 11.3.6 and 8.12: querying these expressions must not perform the write/copy.
    integral(
        "logic signed[4:0] a; logic[15:0] b;",
        "(a=b)",
        5,
        true,
        true,
    );
    integral(
        "logic signed[4:0] a; logic[15:0] b;",
        "(a+=b)",
        5,
        true,
        true,
    );
    let (session, ty) = query("string a,b;", "(a=b)");
    assert_eq!(session.types.data(ty), &TyData::String);
    let (session, ty) = query("class C; endclass C c;", "new c");
    assert!(matches!(session.types.data(ty), TyData::Class(_)));
}

#[test]
fn casts_and_conditionals_do_not_allocate_calls_or_evaluate_values() {
    let declarations =
        "int calls; function logic signed[4:0] next(); calls++; return calls; endfunction";
    integral(declarations, "9'(next())", 9, true, true);
    integral(declarations, "1 ? next() : 5'sd1 / 5'sd0", 5, true, true);
    let declarations = "function int width(); return 9; endfunction bit signed[3:0] a;";
    let (session, ty) = query_in(declarations, "width()'(a)", true);
    let TyData::Packed(packed) = session.types.data(ty) else {
        panic!("not integral")
    };
    assert_eq!(
        (packed.bits(), packed.signed, packed.four_state),
        (9, true, false)
    );
}

#[test]
fn invalid_or_contextual_operands_are_not_replaced_with_a_guessed_type() {
    for expression in [
        "0'(1)",
        "170141183460469231731687303715884105728'(1)",
        "(-1)'(1)",
        "5'(1.0)",
        "signed'(1.0)",
        "unsigned'(null)",
        "1 ? missing : 1",
        "missing ? 1 : 1",
        "0 ? 1 : missing",
        "int'(missing)",
        "1 ? null : 1",
        "new i",
    ] {
        let source = format!("module M; int i; assign out={expression}; endmodule");
        let tree = crate::source_tree::parse_scope_fixture(
            &source,
            Path::new("unsupported-conversion.sv"),
        )
        .unwrap();
        let tree = AnalysisContext::unspecialized(&tree);
        let id = tree.declarations().assign_statements[0].assignments[0].right;
        let mut session = Session::default();
        assert!(
            type_of(
                &tree,
                &mut session,
                id,
                tree.declarations(),
                &PackedDimensions::default()
            )
            .is_err(),
            "{expression}"
        );
        assert_eq!(session.next_call_site, 0);
    }
}

#[test]
fn deeply_nested_casts_and_conditionals_use_a_worklist() {
    std::thread::Builder::new()
        .stack_size(512 * 1024)
        .spawn(|| {
            let expression = format!("{}a{}", "signed'(".repeat(2048), ")".repeat(2048));
            integral("bit[4:0] a;", &expression, 5, true, false);
            let expression = format!("{}a", "p ? a : ".repeat(2048));
            integral("bit p; bit signed[4:0] a;", &expression, 5, true, false);
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn cast_and_conditional_results_remain_typed_receivers() {
    let declarations = "class C; int value; function string text(); return \"a\"; endfunction endclass typedef C H; H a,b; bit pred;";
    for expression in [
        "(pred?a:b).value",
        "(pred?a:null).value",
        "const'(a).value",
        "H'(a).value",
    ] {
        integral(declarations, expression, 32, true, false);
    }
    for expression in ["(pred?a:b).text()", "H'(a).text()"] {
        let (session, ty) = query(declarations, expression);
        assert_eq!(session.types.data(ty), &TyData::String);
    }
    let (session, ty) = query("bit p; string a,b;", "(p?a:b).toupper()");
    assert_eq!(session.types.data(ty), &TyData::String);
}

#[test]
fn four_state_predicates_promote_integral_branches_and_following_casts() {
    // 11.3.4, 11.4.11: an ambiguous predicate merges differing bits to X;
    // 6.24.1: subsequent size/sign casts preserve that four-state domain.
    let declarations = "logic p; bit [3:0] a,b; bit signed[6:0] s; bit q;";
    for (expression, width, signed) in [
        ("p?a:b", 4, false),
        ("p?a:a", 4, false),
        ("p?s:s", 7, true),
        ("p?s:a", 7, false),
        ("9'(p?a:b)", 9, false),
        ("signed'(p?a:b)", 4, true),
        ("const'(p?a:b)", 4, false),
        ("unsigned'(9'(p?s:s))", 9, false),
        ("q ? (p?a:b) : a", 4, false),
    ] {
        integral(declarations, expression, width, signed, true);
    }
    integral(declarations, "q?a:b", 4, false, false);
    integral(declarations, "1.0?a:b", 4, false, false);
    for expression in ["p?r:s", "p?s:r"] {
        let (session, ty) = query("logic p; real r; shortreal s;", expression);
        assert_eq!(session.types.data(ty), &TyData::Real);
    }
}
