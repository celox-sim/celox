use super::*;
use super::{
    conversion::of_type,
    relations::{Assignable, Mismatch},
};
use crate::ast::{AnalysisContext, types};
use std::path::Path;

/// The semantic types of the typedefs of module `M` in `items`.
fn typedefs(items: &str) -> (TyTable, HashMap<String, TyId>) {
    let source = format!("module M; {items} endmodule");
    let tree =
        crate::source_tree::parse_scope_fixture(&source, Path::new("semantic_types.sv")).unwrap();
    let tree = AnalysisContext::unspecialized(&tree);
    let env = HashMap::default();
    let aliases = types::type_aliases_from_scope_with_env(&tree, &env).unwrap();
    let table = TyTable::default();
    let ids = aliases
        .iter()
        .map(|(name, ty)| {
            let id = of_type(&table, &tree.state, ty, &env)
                .unwrap_or_else(|| panic!("`{name}` has no semantic type"));
            (name.clone(), id)
        })
        .collect();
    (table, ids)
}

/// Whether each pair of typedefs of `items` matches as expected.
fn assert_matching(items: &str, pairs: &[(&str, &str, bool)]) {
    let (table, ids) = typedefs(items);
    for &(a, b, expected) in pairs {
        assert_eq!(
            table.matches(ids[a], ids[b]),
            expected,
            "{a} and {b} in `{items}`"
        );
    }
}

#[test]
fn typedefs_of_built_in_types_match_the_type_they_rename() {
    // IEEE 1800-2023 6.22.1 a, b, g.
    assert_matching(
        "typedef bit node; typedef bit other; typedef byte b8; typedef byte signed MY_CHAR;
         typedef byte unsigned u8;",
        &[
            ("node", "other", true),
            ("b8", "MY_CHAR", true),
            ("b8", "u8", false),
            ("node", "b8", false),
        ],
    );
}

#[test]
fn integer_types_match_vectors_of_their_width_signing_and_states() {
    // IEEE 1800-2023 6.22.1 e: a range other than [width-1:0] does not match.
    assert_matching(
        "typedef byte b8; typedef bit signed [7:0] BYTE; typedef bit signed [0:7] ETYB;
         typedef int i32; typedef integer n32; typedef logic signed [31:0] l32;",
        &[
            ("b8", "BYTE", true),
            ("b8", "ETYB", false),
            ("i32", "n32", false),
            ("n32", "l32", true),
        ],
    );
    let (_, ids) = typedefs("typedef byte b8; typedef bit signed [7:0] BYTE;");
    // `$typename` still tells them apart (20.6.1).
    assert_ne!(ids["b8"], ids["BYTE"]);
}

#[test]
fn arrays_match_by_kind_bounds_and_element_type() {
    // IEEE 1800-2023 6.22.1 f.
    assert_matching(
        "typedef byte MEM_BYTES [256]; typedef bit signed [7:0] MY_MEM_BYTES [256];
         typedef byte OTHER_BYTES [1:256];
         typedef logic [1:0][3:0] NIBBLES; typedef logic [7:0] MY_BYTE;
         typedef int q_t[$]; typedef int q2_t[$]; typedef int d_t[]; typedef int m_t[string];",
        &[
            ("MEM_BYTES", "MY_MEM_BYTES", true),
            ("MEM_BYTES", "OTHER_BYTES", false),
            ("NIBBLES", "MY_BYTE", false),
            ("q_t", "q2_t", true),
            ("q_t", "d_t", false),
            ("d_t", "m_t", false),
        ],
    );
}

#[test]
fn named_aggregates_and_enumerations_match_only_their_own_declaration() {
    // IEEE 1800-2023 6.22.1 d.
    assert_matching(
        "typedef struct packed {int A; int B;} AB_t; typedef AB_t AB2_t;
         typedef struct packed {int A; int B;} otherAB_t;
         typedef enum {X, Y} e_t; typedef e_t e2_t; typedef enum {P, Q} f_t;
         typedef struct {int A;} u_t; typedef u_t u2_t; typedef struct {int A;} v_t;",
        &[
            ("AB_t", "AB2_t", true),
            ("AB_t", "otherAB_t", false),
            ("e_t", "e2_t", true),
            ("e_t", "f_t", false),
            ("u_t", "u2_t", true),
            ("u_t", "v_t", false),
        ],
    );
}

#[test]
fn interning_a_type_again_returns_its_id() {
    let table = TyTable::default();
    let packed = |keyword| {
        TyData::Packed(Packed {
            element: PackedElement::Bits,
            four_state: false,
            signed: true,
            signed_element_depth: None,
            dims: Box::new([Range { left: 31, right: 0 }]),
            keyword,
        })
    };
    let int = table.intern(packed(Some("int")));
    let vector = table.intern(packed(None));
    assert_eq!(table.len(), 2);
    assert_eq!(table.intern(packed(Some("int"))), int);
    assert_eq!(table.data(int), packed(Some("int")));
    assert_eq!(table.canonical(int), vector);
    let queue = |element| TyData::Array {
        kind: ArrayKind::Queue { bound: None },
        element,
    };
    let of_int = table.intern(queue(int));
    assert_eq!(table.canonical(of_int), table.intern(queue(vector)));
    assert_eq!(table.len(), 4);
}

#[test]
fn aggregates_of_each_generate_scope_are_types_of_their_own() {
    // IEEE 1800-2023 27.4: a typedef in a generate scope declares a type of
    // its own in each iteration.
    let source = "module M; typedef struct packed {int A;} p_t; typedef struct {int A;} u_t;
                  endmodule";
    let tree =
        crate::source_tree::parse_scope_fixture(source, Path::new("semantic_types.sv")).unwrap();
    let tree = AnalysisContext::unspecialized(&tree);
    let items = tree.body_membership().unwrap();
    let env = HashMap::default();
    let table = TyTable::default();
    let scope = |path| {
        let aliases =
            types::type_aliases_of_generate_scope(&tree, &items, &env, &HashMap::default(), path)
                .unwrap();
        ["p_t", "u_t"].map(|name| of_type(&table, &tree.state, &aliases[name], &env).unwrap())
    };
    let (first, second, again) = (scope("g[0]"), scope("g[1]"), scope("g[0]"));
    for index in 0..2 {
        assert!(!table.matches(first[index], second[index]));
        assert_eq!(first[index], again[index]);
    }
}
#[test]
fn packed_types_of_the_same_bits_states_and_signing_are_equivalent() {
    // IEEE 1800-2023 6.22.2 c; an enumeration is equivalent only to a type it
    // matches.
    let (table, ids) = typedefs(
        "typedef byte b8; typedef struct packed signed {bit [3:0] a, b;} uint8;
         typedef bit [7:0] u8; typedef logic signed [7:0] l8; typedef bit signed [1:0][3:0] n8;
         typedef enum bit signed [7:0] {A, B} e_t; typedef e_t e2_t;
         typedef struct packed {int A; int B;} AB_t; typedef bit [63:0] u64;
         typedef e_t [1:0] ea_t; typedef bit [15:0] u16;",
    );
    for (a, b, expected) in [
        ("b8", "uint8", true),
        ("b8", "n8", true),
        ("b8", "u8", false),
        ("b8", "l8", false),
        ("b8", "e_t", false),
        ("e_t", "e2_t", true),
        ("AB_t", "u64", true),
        ("ea_t", "u16", true),
    ] {
        assert_eq!(table.equivalent(ids[a], ids[b]), expected, "{a} and {b}");
    }
}

#[test]
fn unpacked_arrays_of_equivalent_elements_and_equal_size_are_equivalent() {
    // IEEE 1800-2023 6.22.2 d, e.
    let (table, ids) = typedefs(
        "typedef bit [9:0] A_t [0:5]; typedef bit [1:10] B_t [6]; typedef bit [10:1] uint10;
         typedef uint10 C_t [6:1]; typedef int anint [0:0]; typedef int i_t;
         typedef int q_t[$]; typedef bit signed [31:0] q2_t[$:3]; typedef int d_t[];
         typedef int m_t[byte]; typedef int m2_t[bit signed [7:0]]; typedef int m3_t[string];",
    );
    for (a, b, expected) in [
        ("A_t", "B_t", true),
        ("A_t", "C_t", true),
        ("anint", "i_t", false),
        ("q_t", "q2_t", true),
        ("q_t", "d_t", false),
        ("m_t", "m2_t", true),
        ("m_t", "m3_t", false),
    ] {
        assert_eq!(table.equivalent(ids[a], ids[b]), expected, "{a} and {b}");
    }
}

#[test]
fn unpacked_arrays_assign_by_element_type_and_size() {
    // IEEE 1800-2023 7.6, 7.9.9.
    let (table, ids) = typedefs(
        "typedef int A_t [10:1]; typedef int B_t [0:9]; typedef int C_t [24:1];
         typedef int d_t[]; typedef int q_t[$]; typedef byte b_t[$]; typedef int m_t[string];
         typedef int m2_t[int]; typedef int i_t;",
    );
    let assign = |source: &str, target: &str| table.assignable_array(ids[source], ids[target]);
    assert_eq!(assign("B_t", "A_t"), Ok(Assignable::Always));
    assert_eq!(
        assign("C_t", "A_t"),
        Err(Mismatch::Size {
            actual: 24,
            target: 10
        })
    );
    assert_eq!(assign("d_t", "A_t"), Ok(Assignable::WhenSized(10)));
    assert_eq!(assign("A_t", "q_t"), Ok(Assignable::Always));
    assert_eq!(assign("b_t", "q_t"), Err(Mismatch::ElementType));
    assert_eq!(assign("m_t", "d_t"), Err(Mismatch::Associative));
    assert_eq!(assign("m_t", "m2_t"), Err(Mismatch::IndexType));
    assert_eq!(assign("m_t", "m_t"), Ok(Assignable::Always));
    assert_eq!(assign("i_t", "q_t"), Err(Mismatch::NotArrays));
}
