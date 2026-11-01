use super::*;
use std::path::Path;

mod conversions;
mod enum_constants;
mod values;

fn query(declarations: &str, expression: &str) -> (Session, TyId) {
    query_in(declarations, expression, false)
}

fn query_in(declarations: &str, expression: &str, constant_functions: bool) -> (Session, TyId) {
    let source = format!("module M; {declarations} assign out = {expression}; endmodule");
    let tree =
        crate::source_tree::parse_scope_fixture(&source, Path::new("expression-types.sv")).unwrap();
    let tree = AnalysisContext::unspecialized(&tree);
    let id = tree
        .declarations()
        .assign_statements
        .last()
        .unwrap()
        .assignments[0]
        .right;
    let mut session = Session::default();
    let dims = dimensions::query_operand_dimensions_id(
        id,
        &tree,
        &mut session,
        &PackedDimensions::default(),
    )
    .unwrap();
    let tree = if constant_functions {
        let functions = const_functions::module_constant_functions(
            &tree,
            &mut session,
            &dims.const_env,
            &dims.type_aliases,
            &dims.parameter_values,
        );
        tree.with_functions(functions)
    } else {
        tree
    };
    let sites = session.next_call_site;
    let ty = type_of(&tree, &mut session, id, tree.declarations(), &dims)
        .unwrap_or_else(|error| panic!("type query for `{expression}` failed: {error}"));
    assert_eq!(
        session.next_call_site, sites,
        "a type query allocated a runtime call site"
    );
    (session, ty)
}

#[test]
fn nested_unpacked_selects_keep_the_declared_element_type() {
    // IEEE 1800-2023 7.4.5: one index selects one element.
    for (decl, expr, expected) in [
        ("string a[2][3];", "a[0][1]", "string"),
        ("class C; endclass C a[2][3];", "a[0][1]", "class"),
    ] {
        let (session, ty) = query(decl, expr);
        match expected {
            "string" => assert_eq!(session.types.data(ty), &TyData::String),
            "class" => assert!(matches!(session.types.data(ty), TyData::Class(_))),
            _ => unreachable!(),
        }
    }
}

#[test]
fn array_slices_preserve_the_enum_declaration_of_their_elements() {
    // IEEE 1800-2023 6.22.1 c, 7.4.5–7.4.6.
    for select in ["a[0][1:2]", "a[0][1+:2]", "a[0][2-:2]"] {
        let (session, ty) = query("typedef enum logic[1:0] {A,B} E; E a[2][4];", select);
        let TyData::Array {
            kind: ArrayKind::Fixed(range),
            element,
        } = session.types.data(ty)
        else {
            panic!("not an array")
        };
        assert_eq!(range.size(), 2);
        assert!(matches!(
            session.types.data(*element),
            TyData::Packed(semantic_types::Packed {
                element: PackedElement::Enum {
                    decl: Decl::Named(_),
                    array: false
                },
                ..
            })
        ));
    }
}

#[test]
fn queue_range_types_do_not_require_constant_endpoints() {
    // IEEE 1800-2023 7.10.1: queue slice endpoints may be arbitrary
    // integral expressions. They do not determine the result element type.
    for declaration in ["Cell q[$];", "Cell q[$:4];"] {
        for bounds in ["lo:hi", "lo:$", "$:hi", "$:$", "lo++:hi"] {
            let declarations = format!("class Cell; int value; endclass {declaration} int lo, hi;");
            let (session, ty) = query(&declarations, &format!("q[{bounds}]"));
            let TyData::Array {
                kind: ArrayKind::Queue { bound: None },
                element,
            } = session.types.data(ty)
            else {
                panic!("a queue slice lost its unbounded queue type")
            };
            assert!(matches!(session.types.data(*element), TyData::Class(_)));
        }
    }
}

#[test]
fn last_queue_elements_keep_the_declared_type() {
    // IEEE 1800-2023 7.10 and 7.10.1: `$` selects the last element;
    // neither an empty queue nor a capacity bound changes its element type.
    for declaration in ["string q[$];", "string q[$:4];"] {
        let (session, ty) = query(declaration, "q[$]");
        assert_eq!(session.types.data(ty), &TyData::String);
    }
    let (session, ty) = query("string q[$][$];", "q[$][$]");
    assert_eq!(session.types.data(ty), &TyData::String);
    let (session, ty) = query("typedef enum { A, B } E; E q[$];", "q[$]");
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
}

#[test]
fn last_queue_elements_remain_typed_method_receivers() {
    let declarations = "class Cell; int value; function int read(); return value; endfunction endclass Cell q[$]; string words[$];";
    for expression in ["q[$].value", "q[$].read()", "words[$].len()"] {
        let (session, ty) = query(declarations, expression);
        let TyData::Packed(packed) = session.types.data(ty) else {
            panic!("a last-element selection lost its member type")
        };
        assert_eq!(packed.bits(), 32);
        assert!(packed.signed);
    }
    let (session, ty) = query(declarations, "words[$].toupper()");
    assert_eq!(session.types.data(ty), &TyData::String);
}

#[test]
fn string_character_selections_have_the_declared_byte_type() {
    // IEEE 1800-2023 6.16, Table 6-9: Str[index] returns byte,
    // independently of the index and the runtime length of the string.
    let declarations = "string text, words[2], queue[$]; int index; class Cell; string text; endclass Cell h; function string make(); return text; endfunction";
    for expression in [
        "text[index]",
        "words[index][index]",
        "queue[index][index]",
        "queue[$][index]",
        "h.text[index]",
        "make()[index]",
        "text.toupper()[index]",
    ] {
        let (session, ty) = query(declarations, expression);
        let TyData::Packed(packed) = session.types.data(ty) else {
            panic!("a string character has no integral type")
        };
        assert_eq!(packed.bits(), 8);
        assert!(packed.signed);
        assert!(!packed.four_state);
        assert_eq!(packed.keyword, Some("byte"));
    }
}

#[test]
fn selected_string_characters_support_packed_selections() {
    // IEEE 1800-2023 6.16 and 11.5.1: the byte result is an integral
    // value; its bit and part selections are unsigned.
    for (expression, width) in [
        ("text[index][bit_index]", 1),
        ("text[index][3:1]", 3),
        ("text[index][offset+:3]", 3),
    ] {
        let (session, ty) = query("string text; int index, bit_index, offset;", expression);
        let TyData::Packed(packed) = session.types.data(ty) else {
            panic!("a selected character lost its packed type")
        };
        assert_eq!(packed.bits(), width);
        assert!(!packed.signed);
        assert!(!packed.four_state);
        assert_eq!(packed.keyword, None);
    }
}

#[test]
fn runtime_queue_slices_preserve_selected_class_receivers() {
    let declarations = "class Cell; int value; function int read(); return value; endfunction endclass Cell q[$]; int lo, hi; function int next_lo(); return lo++; endfunction";
    for bounds in ["lo:hi", "lo:$", "$:hi", "$:$", "next_lo():hi"] {
        for member in ["value", "read()"] {
            let (session, ty) = query(declarations, &format!("q[{bounds}][0].{member}"));
            let TyData::Packed(packed) = session.types.data(ty) else {
                panic!("a queue slice lost its class member type")
            };
            assert_eq!(packed.bits(), 32);
            assert!(packed.signed);
        }
    }
}

#[test]
fn packed_element_selects_restore_the_signed_named_element() {
    // IEEE 1800-2023 7.4.1: the selected named element retains its signing.
    let declarations = "typedef bit signed [3:0] Nibble; Nibble [1:0] a;";
    let (session, ty) = query(declarations, "a[0]");
    let TyData::Packed(packed) = session.types.data(ty) else {
        panic!()
    };
    assert_eq!(packed.bits(), 4);
    assert!(packed.signed);
    let (session, ty) = query(declarations, "a[0][1]");
    let TyData::Packed(packed) = session.types.data(ty) else {
        panic!()
    };
    assert_eq!(packed.bits(), 1);
    assert!(!packed.signed);
}

#[test]
fn function_result_types_are_available_without_binding_the_body() {
    let (session, ty) = query(
        "typedef logic[7:0] Bytes[2:4]; int calls; function automatic Bytes values(); calls++; return '{default:0}; endfunction",
        "values()[2]",
    );
    let TyData::Packed(packed) = session.types.data(ty) else {
        panic!()
    };
    assert_eq!(packed.bits(), 8);
    assert!(packed.four_state);
}

#[test]
fn memoized_types_follow_lexical_binding_snapshots() {
    let tree = crate::source_tree::parse_scope_fixture(
        "module M; logic[7:0] a; assign out=a; endmodule",
        Path::new("type-cache.sv"),
    )
    .unwrap();
    let tree = AnalysisContext::unspecialized(&tree);
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
    let original = type_of(&tree, &mut session, id, tree.declarations(), &dims).unwrap();
    assert_eq!(
        session
            .expression_types
            .get(&tree, &session, id, tree.declarations(), &dims),
        Some(original)
    );
    let outer = dims.clone();
    let mut selected_element = outer.clone();
    selected_element.element_select = Some((id, "temporary_element".into()));
    assert!(
        session
            .expression_types
            .get(&tree, &session, id, tree.declarations(), &selected_element)
            .is_none()
    );
    let mut shape = dims.get("a").unwrap().clone();
    shape.packed[0].left = ConstExpr::Literal("15".into());
    shape.packed[0].width = ConstExpr::Literal("16".into());
    dims.insert("a".into(), shape);
    assert!(
        session
            .expression_types
            .get(&tree, &session, id, tree.declarations(), &dims)
            .is_none()
    );
    let shadowed = type_of(&tree, &mut session, id, tree.declarations(), &dims).unwrap();
    assert!(!session.types.matches(original, shadowed));
    assert_eq!(
        type_of(&tree, &mut session, id, tree.declarations(), &outer).unwrap(),
        original
    );
}

#[test]
fn parenless_instance_and_static_methods_use_semantic_result_types() {
    // IEEE 1800-2023 13.5.5: omitting empty parentheses retains the result.
    let declarations = "class C; function string text(); return \"value\"; endfunction static function string static_text(); return \"value\"; endfunction endclass C c;";
    for expression in ["c.text", "C::static_text"] {
        let (session, ty) = query(declarations, expression);
        assert_eq!(session.types.data(ty), &TyData::String);
    }
    let (session, ty) = query(
        "typedef logic[7:0] Bytes[2]; class C; function Bytes values(); return '{default:0}; endfunction endclass C c;",
        "c.values[0]",
    );
    let TyData::Packed(packed) = session.types.data(ty) else {
        panic!("a selected method result lost its element type")
    };
    assert_eq!(packed.bits(), 8);
    assert!(packed.four_state);
}

#[test]
fn parenless_factory_results_can_be_receivers() {
    let declarations = "class C; int id; static function C latest(); return null; endfunction function C peer(); return this; endfunction endclass C c;";
    for expression in ["C::latest", "c.peer", "c.peer.peer"] {
        let (session, ty) = query(declarations, expression);
        assert!(matches!(session.types.data(ty), TyData::Class(_)));
    }
    let (session, ty) = query(declarations, "C::latest.id");
    let TyData::Packed(packed) = session.types.data(ty) else {
        panic!("a factory result lost its property type")
    };
    assert_eq!(packed.bits(), 32);
    assert!(packed.signed);
}

#[test]
fn unresolved_queries_report_unsupported_instead_of_inventing_a_type() {
    let tree = crate::source_tree::parse_scope_fixture(
        "module M; assign out=missing; endmodule",
        Path::new("unknown-type.sv"),
    )
    .unwrap();
    let tree = AnalysisContext::unspecialized(&tree);
    let id = tree.declarations().assign_statements[0].assignments[0].right;
    let error = type_of(
        &tree,
        &mut Session::default(),
        id,
        tree.declarations(),
        &PackedDimensions::default(),
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("semantic type of native expression")
    );
}

#[test]
fn constant_slice_widths_do_not_allocate_runtime_call_sites() {
    let (session, ty) = query_in(
        "function automatic int width(); return 2; endfunction logic[7:0] a[4];",
        "a[1+:width()]",
        true,
    );
    let TyData::Array {
        kind: ArrayKind::Fixed(range),
        ..
    } = session.types.data(ty)
    else {
        panic!()
    };
    assert_eq!(range.size(), 2);
}

#[test]
fn selections_of_container_builtin_results_keep_the_declared_element_type() {
    // IEEE 1800-2023 7.10.2.4: pop_front returns the queue's element type.
    let (session, ty) = query("class C; endclass C q[$][$];", "q.pop_front()[0]");
    assert!(matches!(session.types.data(ty), TyData::Class(_)));
}

#[test]
fn queue_removal_returns_the_whole_element_type() {
    // IEEE 1800-2023 7.10.2.4–5: query the call itself, before any select.
    for method in ["pop_front()", "pop_back()", "pop_front"] {
        let (session, ty) = query("class C; endclass C q[$][$];", &format!("q.{method}"));
        let TyData::Array {
            kind: ArrayKind::Queue { bound: None },
            element,
        } = session.types.data(ty)
        else {
            panic!("a popped queue lost its own type")
        };
        assert!(matches!(session.types.data(*element), TyData::Class(_)));
    }
}

#[test]
fn locators_keep_element_and_associative_index_types() {
    // IEEE 1800-2023 7.12.1: a locator's `with` expression does not change
    // its result element type; index locators retain associative key types.
    for (decl, expr, string) in [
        ("string a[3];", "a.find with (item.len() > 1)", true),
        ("string a[$];", "a.max with (item.len())", true),
        ("int a[string];", "a.find_index with (item == 1)", true),
        ("int a[string];", "a.unique_index()", true),
        ("string a[3];", "a.find_index with (item.len() > 1)", false),
    ] {
        let (session, ty) = query(decl, expr);
        let TyData::Array {
            kind: ArrayKind::Queue { bound: None },
            element,
        } = session.types.data(ty)
        else {
            panic!("a locator must return an unbounded queue")
        };
        if string {
            assert_eq!(session.types.data(*element), &TyData::String);
        } else {
            let TyData::Packed(packed) = session.types.data(*element) else {
                panic!("a non-associative index must be int")
            };
            assert_eq!(packed.keyword, Some("int"));
        }
    }
}

#[test]
fn builtin_results_can_be_selected_and_chained_without_binding() {
    let (session, ty) = query("string a[$];", "a.max()[0].toupper().len()");
    let TyData::Packed(packed) = session.types.data(ty) else {
        panic!("len must return int")
    };
    assert_eq!(packed.keyword, Some("int"));
    assert_eq!(packed.bits(), 32);
    assert!(packed.signed);
}

#[test]
fn builtin_size_and_character_results_keep_declared_integer_types() {
    for (decl, expr, keyword) in [
        ("int a[];", "a.size()", "int"),
        ("int a[$];", "a.size", "int"),
        ("int a[string];", "a.num()", "int"),
        ("string s;", "s.getc(0)", "byte"),
        ("string s;", "s.compare(\"other\")", "int"),
        ("string s;", "s.atoi()", "integer"),
    ] {
        let (session, ty) = query(decl, expr);
        let TyData::Packed(packed) = session.types.data(ty) else {
            panic!("expected a declared integer result")
        };
        assert_eq!(packed.keyword, Some(keyword));
        assert!(packed.signed);
    }
}

#[test]
fn builtin_spelling_does_not_override_a_class_method_signature() {
    let (session, ty) = query(
        "class C; function string len(); return \"value\"; endfunction endclass C c;",
        "c.len()",
    );
    assert_eq!(session.types.data(ty), &TyData::String);
}

#[test]
fn escaped_builtin_names_have_the_same_result_type() {
    let (session, ty) = query("string q[$];", "q.\\pop_front ()");
    assert_eq!(session.types.data(ty), &TyData::String);
}

#[test]
fn slice_constants_belong_to_the_supplied_native_arena() {
    let source = crate::source_tree::parse_scope_fixture(
        "module M; string a[8]; assign out=a[1:2]; endmodule",
        Path::new("local-arena.sv"),
    )
    .unwrap();
    let foreign = crate::source_tree::parse_scope_fixture(
        "module M; string a[8]; assign out=a[1:4]; endmodule",
        Path::new("foreign-arena.sv"),
    )
    .unwrap();
    let tree = AnalysisContext::unspecialized(&source);
    let foreign = AnalysisContext::unspecialized(&foreign);
    let id = foreign.declarations().assign_statements[0].assignments[0].right;
    let mut session = Session::default();
    let dims = dimensions::query_operand_dimensions_id(
        id,
        &foreign,
        &mut session,
        &PackedDimensions::default(),
    )
    .unwrap();
    let ty = type_of(&tree, &mut session, id, foreign.declarations(), &dims).unwrap();
    let TyData::Array {
        kind: ArrayKind::Fixed(range),
        element,
    } = session.types.data(ty)
    else {
        panic!("a slice lost its array type")
    };
    assert_eq!(range.size(), 4);
    assert_eq!(session.types.data(*element), &TyData::String);
}

#[test]
fn selecting_a_signed_named_packed_element_matches_its_declaration() {
    // IEEE 1800-2023 7.4.1: an element of a packed array of a signed named
    // type retains that type, not merely its bit width and signedness.
    let source = crate::source_tree::parse_scope_fixture(
        "module M; typedef bit signed[3:0] N; N[1:0] a; N n; assign out=a[0]; endmodule",
        Path::new("named-packed-element.sv"),
    )
    .unwrap();
    let tree = AnalysisContext::unspecialized(&source);
    let id = tree.declarations().assign_statements[0].assignments[0].right;
    let mut session = Session::default();
    let dims = dimensions::query_operand_dimensions_id(
        id,
        &tree,
        &mut session,
        &PackedDimensions::default(),
    )
    .unwrap();
    let selected = type_of(&tree, &mut session, id, tree.declarations(), &dims).unwrap();
    let declared = semantic_types::of_shape(
        &mut session.types,
        &tree.functions,
        dims.get("n").unwrap(),
        &dims.const_env,
    )
    .unwrap();
    assert!(session.types.matches(selected, declared));
}

#[test]
fn locator_selection_keeps_the_original_enum_declaration() {
    let (session, ty) = query(
        "typedef enum logic[1:0] {A,B} E; E a[3];",
        "a.find with (item == A)[0]",
    );
    let TyData::Packed(packed) = session.types.data(ty) else {
        panic!("a locator result lost its enum type")
    };
    assert!(matches!(
        packed.element,
        PackedElement::Enum { array: false, .. }
    ));
}
