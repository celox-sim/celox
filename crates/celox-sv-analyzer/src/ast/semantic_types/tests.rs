use super::*;
use super::{
    conversion::of_type,
    relations::{Assignable, Castable, Mismatch},
};
use crate::ast::Session;
use crate::ast::{AnalysisContext, types};
use crate::ast::{ConstEnv, TypeAliases};
use std::path::Path;

/// The semantic types of the typedefs of module `M` in `items`.
fn typedefs(items: &str) -> (TyTable, HashMap<String, TyId>) {
    let session = &mut Session::default();
    let source = format!("module M; {items} endmodule");
    let tree =
        crate::source_tree::parse_scope_fixture(&source, Path::new("semantic_types.sv")).unwrap();
    let tree = AnalysisContext::unspecialized(&tree);
    let env = ConstEnv::default();
    let aliases = types::type_aliases_from_scope_with_env(&tree, session, &env).unwrap();
    let mut table = TyTable::default();
    let ids = aliases
        .iter()
        .map(|(name, ty)| {
            let id = of_type(&mut table, &tree.functions, ty, &env)
                .unwrap_or_else(|| panic!("`{name}` has no semantic type"));
            (name.clone(), id)
        })
        .collect();
    (table, ids)
}

/// The typedefs of `items` in module `M`, bound by an analysis of
/// elaborated scope `scope`, as types of `table`.
fn typedefs_in(table: &mut TyTable, scope: &str, items: &str) -> HashMap<String, TyId> {
    let session = &mut Session {
        scope: Some(scope.into()),
        ..Session::default()
    };
    let source = format!("module M; {items} endmodule");
    let tree =
        crate::source_tree::parse_scope_fixture(&source, Path::new("semantic_types.sv")).unwrap();
    let tree = AnalysisContext::unspecialized(&tree);
    let env = ConstEnv::default();
    let aliases = types::type_aliases_from_scope_with_env(&tree, session, &env).unwrap();
    aliases
        .iter()
        .map(|(name, ty)| {
            let id = of_type(table, &tree.functions, ty, &env)
                .unwrap_or_else(|| panic!("`{name}` has no semantic type"));
            (name.clone(), id)
        })
        .collect()
}

#[test]
fn each_module_analysis_declares_types_of_its_own() {
    // IEEE 1800-2023 6.22: the enumeration, structure and union types an
    // instance declares are its own. Instances with the same parameter
    // values share one analysis, and so its types.
    let items = "typedef enum logic [1:0] {X, Y} e_t; typedef struct packed {logic a;} p_t;
                 typedef struct {int a;} u_t; typedef bit [3:0] nibble;";
    let mut table = TyTable::default();
    let first = typedefs_in(&mut table, "M#1", items);
    let again = typedefs_in(&mut table, "M#1", items);
    let other = typedefs_in(&mut table, "M#2", items);
    for name in ["e_t", "p_t", "u_t"] {
        assert_eq!(first[name], again[name], "{name}");
        assert!(!table.matches(first[name], other[name]), "{name}");
        // Diagnostics write a declaration without its scope.
        assert_eq!(table.spell(first[name]), table.spell(other[name]), "{name}");
    }
    assert!(table.matches(first["nibble"], other["nibble"]));
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
fn enumerations_keep_their_members_named_or_anonymous() {
    // IEEE 1800-2023 6.19: the members are part of the type, which a named
    // enumeration's declaration names.
    let (table, ids) =
        typedefs("typedef enum {X, Y} e_t; typedef e_t e2_t; typedef e_t [1:0] ea_t;");
    for name in ["e_t", "e2_t"] {
        let domain = table
            .enum_domain(ids[name])
            .unwrap_or_else(|| panic!("{name}"));
        let members = domain
            .members
            .iter()
            .map(|member| member.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(members, ["X", "Y"], "{name}");
    }
    assert!(table.enum_domain(ids["ea_t"]).is_some());
}

#[test]
fn interning_a_type_again_returns_its_id() {
    let mut table = TyTable::default();
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
    assert_eq!(*table.data(int), packed(Some("int")));
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
    let session = &mut Session::default();
    // IEEE 1800-2023 27.4: a typedef in a generate scope declares a type of
    // its own in each iteration.
    let source = "module M; typedef struct packed {int A;} p_t; typedef struct {int A;} u_t;
                  endmodule";
    let tree =
        crate::source_tree::parse_scope_fixture(source, Path::new("semantic_types.sv")).unwrap();
    let tree = AnalysisContext::unspecialized(&tree);
    let items = tree.body_membership().unwrap();
    let env = ConstEnv::default();
    let mut table = TyTable::default();
    let mut scope = |path| {
        let aliases = types::type_aliases_of_generate_scope(
            &tree,
            session,
            &items,
            &env,
            &TypeAliases::default(),
            path,
        )
        .unwrap();
        ["p_t", "u_t"]
            .map(|name| of_type(&mut table, &tree.functions, &aliases[name], &env).unwrap())
    };
    let (first, second, again) = (scope("g[0]"), scope("g[1]"), scope("g[0]"));
    for index in 0..2 {
        assert!(!table.matches(first[index], second[index]));
        assert_eq!(first[index], again[index]);
    }
}

#[test]
fn enumeration_constants_of_each_generate_scope_have_its_type() {
    let session = &mut Session::default();
    // IEEE 1800-2023 27.4: an enumeration typedef in a generate scope
    // declares a type of its own in each iteration, which is the type of its
    // constants there (6.19.3), also for an enumeration declared after
    // another, whose typedefs are bound again with its constants.
    let source = "module M; typedef enum {A, B} e_t; typedef enum {C, D} f_t; endmodule";
    let tree =
        crate::source_tree::parse_scope_fixture(source, Path::new("semantic_types.sv")).unwrap();
    let tree = AnalysisContext::unspecialized(&tree);
    let items = tree.body_membership().unwrap();
    let env = ConstEnv::default();
    let mut scope = |path| {
        let aliases = types::type_aliases_of_generate_scope(
            &tree,
            session,
            &items,
            &env,
            &TypeAliases::default(),
            path,
        )
        .unwrap();
        let constants = crate::ast::parameters::enum_member_constants_from_items(
            &tree,
            session,
            &items,
            &env,
            &aliases,
            &HashMap::default(),
            Some(path),
        )
        .unwrap();
        [("e_t", "A"), ("f_t", "C")].map(|(typedef, constant)| {
            let typedef =
                of_type(&mut session.types, &tree.functions, &aliases[typedef], &env).unwrap();
            let constant = constants.declarations[constant]
                .type_id(&mut session.types)
                .unwrap();
            (typedef, constant)
        })
    };
    let (first, second) = (scope("g[0]"), scope("g[1]"));
    for index in 0..2 {
        assert_eq!(first[index].0, first[index].1);
        assert_eq!(second[index].0, second[index].1);
        assert!(!session.types.matches(first[index].1, second[index].0));
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

#[test]
fn diagnostics_spell_element_types_and_element_counts() {
    let (table, ids) = typedefs(
        "typedef logic signed [7:0] r_t [2][3]; typedef bit b_t [4]; typedef int q_t[$];
         typedef byte m_t[string]; typedef enum {X, Y} e_t; typedef e_t ea_t [2];
         typedef string s_t [1:0]; typedef struct packed {int A;} p_t; typedef p_t pa_t [2];",
    );
    for (name, spelling) in [
        ("r_t", "logic signed [7:0] [2][3]"),
        ("b_t", "bit [4]"),
        ("q_t", "bit signed [31:0] [$]"),
        ("m_t", "bit signed [7:0] [string]"),
        ("s_t", "string [2]"),
        ("pa_t", "bit [31:0] [2]"),
    ] {
        assert_eq!(table.spell(ids[name]), spelling, "{name}");
    }
    assert!(
        table.spell(ids["ea_t"]).starts_with("enum "),
        "{}",
        table.spell(ids["ea_t"])
    );
    assert!(table.spell(ids["ea_t"]).ends_with(" [2]"));
}

#[test]
fn virtual_interfaces_take_their_interface_without_or_with_their_modport() {
    // IEEE 1800-2023 25.9: a virtual interface is assigned one of its
    // interface with no modport or with its own modport.
    let mut table = TyTable::default();
    let mut vif = |interface: &str, modport: Option<&str>| {
        let interface = table.interface(interface);
        table.intern(TyData::VirtualInterface {
            interface,
            modport: modport.map(Into::into),
        })
    };
    let (bus, bus_m, bus_s, other) = (
        vif("bus", None),
        vif("bus", Some("m")),
        vif("bus", Some("s")),
        vif("other", None),
    );
    let null = table.intern(TyData::Null);
    for (source, target, expected) in [
        (bus, bus, Ok(())),
        (bus, bus_m, Ok(())),
        (bus_m, bus_m, Ok(())),
        (bus_m, bus, Err(Mismatch::VirtualInterface)),
        (bus_m, bus_s, Err(Mismatch::VirtualInterface)),
        (other, bus, Err(Mismatch::VirtualInterface)),
        (null, bus_m, Ok(())),
    ] {
        assert_eq!(
            table.assignable_handle(source, target),
            expected,
            "{} to {}",
            table.spell(source),
            table.spell(target)
        );
    }
}

#[test]
fn class_handles_assign_to_their_classes_bases_and_interfaces() {
    // IEEE 1800-2023 8.16: `class E extends C`; 8.26.5: `class Fifo extends
    // Base implements PutImp`; 6.11.1: `null` to any handle.
    let mut table = TyTable::default();
    let mut class = |name: &str| {
        let decl = table.class(name);
        table.intern(TyData::Class(decl))
    };
    let (c, e, base, fifo, put, other) = (
        class("C"),
        class("E"),
        class("Base"),
        class("Fifo"),
        class("PutImp"),
        class("Other"),
    );
    let null = table.intern(TyData::Null);
    let chandle = table.intern(TyData::Chandle);
    let int = table.intern(TyData::Packed(Packed {
        element: PackedElement::Bits,
        four_state: false,
        signed: true,
        signed_element_depth: None,
        dims: Box::new([Range { left: 31, right: 0 }]),
        keyword: Some("int"),
    }));
    let supertypes = |base: Option<TyId>, interfaces: &[TyId]| Supertypes {
        base,
        interfaces: interfaces.into(),
        interface: false,
    };
    table.record_supertypes(e, supertypes(Some(c), &[]));
    table.record_supertypes(c, supertypes(None, &[]));
    table.record_supertypes(fifo, supertypes(Some(base), &[put]));
    // `Base` is left unresolved: its supertypes are unknown.
    for (source, target, expected) in [
        (e, c, Ok(())),
        (e, e, Ok(())),
        (c, e, Err(Mismatch::ClassHandle)),
        (fifo, put, Ok(())),
        (fifo, base, Ok(())),
        (put, fifo, Err(Mismatch::ClassHandle)),
        (base, c, Err(Mismatch::ClassHandle)),
        (other, c, Err(Mismatch::ClassHandle)),
        (null, c, Ok(())),
        (null, chandle, Ok(())),
        (c, null, Err(Mismatch::NotHandles)),
        (int, c, Err(Mismatch::NotHandles)),
    ] {
        assert_eq!(
            table.assignable_handle(source, target),
            expected,
            "{} to {}",
            table.spell(source),
            table.spell(target)
        );
    }
    assert_eq!(table.lineage(fifo), vec![fifo, base, put]);
    assert_eq!(table.base_classes(fifo), vec![fifo, base]);
    assert_eq!(table.base_classes(e), vec![e, c]);
}

#[test]
fn casts_assign_convert_enums_and_reinterpret_bit_streams() {
    // IEEE 1800-2023 6.19.4, 6.24.1 and 6.24.3, with their examples.
    let (table, ids) = typedefs(
        "typedef enum {Red, Green, Blue} Colors; typedef int int_t; typedef real real_t;
         typedef shortint short_t; typedef string string_t; typedef byte bytes_t[$];
         typedef struct { shortint address; logic [3:0] code; byte command [2]; } Control;
         typedef bit Bits [36:1]; typedef bit Short [35:1]; typedef int map_t[int];",
    );
    let cast = |source: &str, target: &str| table.castable(ids[source], ids[target]);
    let assign = |source: &str, target: &str| table.assignable(ids[source], ids[target]);
    // 6.19.4: `C = I` needs a cast; `I = C` does not.
    assert_eq!(assign("int_t", "Colors"), Err(Mismatch::Enum));
    assert_eq!(cast("int_t", "Colors"), Ok(Castable::Enum));
    assert_eq!(assign("Colors", "int_t"), Ok(Assignable::Always));
    // 6.24.1: `int'(2.0 * 3.0)` and `shortint'({8'hFA, 8'hCE})` assign.
    assert_eq!(
        cast("real_t", "int_t"),
        Ok(Castable::Assign(Assignable::Always))
    );
    assert_eq!(
        cast("int_t", "short_t"),
        Ok(Castable::Assign(Assignable::Always))
    );
    // 6.24.3: `Control` and `Bits` are bit-stream types of 36 bits.
    assert_eq!(assign("Control", "Bits"), Err(Mismatch::NotAssignable));
    assert_eq!(
        cast("Control", "Bits"),
        Ok(Castable::BitStream {
            source_bits: Some(36),
            target_bits: Some(36)
        })
    );
    assert_eq!(
        cast("Control", "Short"),
        Err(Mismatch::Size {
            actual: 36,
            target: 35
        })
    );
    // A string is a dynamic array of bytes, whose size a fixed-size target
    // checks at run time; an integral value goes into a string by a cast
    // (6.16).
    assert_eq!(assign("short_t", "string_t"), Err(Mismatch::NotAssignable));
    assert_eq!(
        cast("string_t", "short_t"),
        Ok(Castable::BitStream {
            source_bits: None,
            target_bits: Some(16)
        })
    );
    assert_eq!(
        cast("bytes_t", "short_t"),
        Ok(Castable::BitStream {
            source_bits: None,
            target_bits: Some(16)
        })
    );
    assert_eq!(
        cast("short_t", "string_t"),
        Ok(Castable::BitStream {
            source_bits: Some(16),
            target_bits: None
        })
    );
    // An associative array is no destination of a bit-stream cast, and a
    // real is no bit-stream type.
    assert_eq!(cast("int_t", "map_t"), Err(Mismatch::NotCastable));
    assert_eq!(cast("real_t", "string_t"), Err(Mismatch::NotCastable));
    assert_eq!(cast("real_t", "Colors"), Ok(Castable::Enum));
}

#[test]
fn class_casts_check_derived_targets_at_run_time() {
    // IEEE 1800-2023 8.16: `$cast` to a derived class's handle checks the
    // object; to an unrelated class it cannot succeed.
    let mut table = TyTable::default();
    let mut class = |name: &str| {
        let decl = table.class(name);
        table.intern(TyData::Class(decl))
    };
    let (base, derived, other, interface) = (class("B"), class("D"), class("O"), class("I"));
    let supertypes = |base: Option<TyId>, interface: bool| Supertypes {
        base,
        interfaces: Arc::new([]),
        interface,
    };
    table.record_supertypes(base, supertypes(None, false));
    table.record_supertypes(derived, supertypes(Some(base), false));
    table.record_supertypes(other, supertypes(None, false));
    table.record_supertypes(interface, supertypes(None, true));
    assert_eq!(
        table.castable(derived, base),
        Ok(Castable::Assign(Assignable::Always))
    );
    assert_eq!(table.castable(base, derived), Ok(Castable::Dynamic));
    assert_eq!(table.castable(interface, other), Ok(Castable::Dynamic));
    assert_eq!(table.castable(other, derived), Err(Mismatch::ClassHandle));
}

#[test]
fn bit_stream_casts_reject_destinations_holding_associative_arrays() {
    // IEEE 1800-2023 6.24.3: an associative array is illegal as a
    // destination type, also as an element or a member of one.
    let (mut table, ids) = typedefs("typedef int map_t[int]; typedef string string_t;");
    let int = table.intern(TyData::Packed(Packed {
        element: PackedElement::Bits,
        four_state: false,
        signed: true,
        signed_element_depth: None,
        dims: Box::new([Range { left: 31, right: 0 }]),
        keyword: Some("int"),
    }));
    let maps = table.intern(TyData::Array {
        kind: ArrayKind::Fixed(Range { left: 0, right: 1 }),
        element: ids["map_t"],
    });
    let decl = table.declare("holder_t", None);
    let holder = table.intern(TyData::UnpackedStruct(Decl::Aggregate {
        decl,
        members: Arc::new([("i".into(), int), ("m".into(), ids["map_t"])]),
    }));
    for target in [ids["map_t"], maps, holder] {
        assert_eq!(
            table.castable(ids["string_t"], target),
            Err(Mismatch::NotCastable),
            "{}",
            table.spell(target)
        );
    }
}

#[test]
fn declarations_are_one_per_name_and_scope() {
    let mut table = TyTable::default();
    let scopes = (0..64)
        .map(|index| format!("M#{index}"))
        .collect::<Vec<_>>();
    let ids = scopes
        .iter()
        .map(|scope| table.declare("M::e_t", Some(scope)))
        .collect::<Vec<_>>();
    let unscoped = table.declare("M::e_t", None);
    let mut distinct = ids.clone();
    distinct.push(unscoped);
    distinct.sort();
    distinct.dedup();
    assert_eq!(distinct.len(), scopes.len() + 1);
    for (scope, id) in scopes.iter().zip(&ids) {
        assert_eq!(table.declare("M::e_t", Some(scope)), *id);
        assert_eq!(table.decl(*id).scope.as_deref(), Some(scope.as_str()));
    }
    assert_eq!(table.declare("M::e_t", None), unscoped);
    assert_ne!(table.declare("M::f_t", None), unscoped);
}

#[test]
fn variables_keep_the_semantic_types_of_their_declarations() {
    // IEEE 1800-2023 6.18: the names in a type resolve in the scope that
    // declares it. A class property or a module variable with unpacked
    // dimensions is bound to its semantic type where it is declared, and
    // keeps the binding when its constants are substituted, so a reader
    // neither resolves it again nor needs to see `W`.
    let session = &mut Session::default();
    let source = "module M;
        class C; bit d [W]; logic [W-1:0] q [$:W]; endclass
        logic [W-1:0] a [2][W];
        endmodule";
    let tree =
        crate::source_tree::parse_scope_fixture(source, Path::new("semantic_types.sv")).unwrap();
    let tree = AnalysisContext::unspecialized(&tree);
    let declaring = ConstEnv::from_iter([("W".into(), 4)]);
    let class =
        crate::ast::classes::class(&tree, session, "C", &declaring, &TypeAliases::default())
            .unwrap();
    for property in &class.properties {
        assert!(
            property.r#type.binding.get(&session.types).is_some(),
            "property `{}`",
            property.name
        );
    }
    let signals = crate::ast::declarations::signals_from_scope(
        &tree,
        session,
        &declaring,
        &TypeAliases::default(),
    )
    .unwrap();
    let signal = signals.iter().find(|signal| signal.name() == "a").unwrap();
    assert!(signal.r#type().binding.get(&session.types).is_some());
}

#[test]
fn types_keep_their_semantic_types_when_their_bounds_take_values() {
    // IEEE 1800-2023 6.18: the names in a type resolve in the scope that
    // declares it. Replacing its bounds by their values there, as freezing
    // them or resolving a type argument does, leaves its semantic type as it
    // was, so the type keeps its binding.
    let session = &mut Session::default();
    let source = "module M; typedef logic [W-1:0] t [W]; endmodule";
    let tree =
        crate::source_tree::parse_scope_fixture(source, Path::new("semantic_types.sv")).unwrap();
    let tree = AnalysisContext::unspecialized(&tree);
    let declaring = ConstEnv::from_iter([("W".into(), 4)]);
    let aliases = types::type_aliases_from_scope_with_env(&tree, session, &declaring).unwrap();
    let mut ty = aliases["t"].clone();
    bind_declared(&mut session.types, &tree.functions, &mut ty, &declaring);
    let declared = ty.binding.get(&session.types).unwrap();
    let mut frozen = ty.clone();
    types::freeze_bounds(&mut frozen, &tree.functions, &declaring);
    let resolved =
        crate::ast::type_parameters::resolve(&tree.functions, ty.clone(), &declaring).unwrap();
    for ty in [frozen, resolved] {
        assert_eq!(ty.binding.get(&session.types), Some(declared));
    }
}

#[test]
fn a_container_alone_has_the_type_of_its_declaration() {
    // IEEE 1800-2023 6.18, 7.10: a queue declared where `W` is 4 has
    // `logic [3:0]` elements. A reader that has only the container, not the
    // type that holds it, takes the type it was bound to instead of
    // resolving it again, also where `W` is not known.
    let session = &mut Session::default();
    let source = "module M; class C; logic [W-1:0] q [$]; endclass endmodule";
    let tree =
        crate::source_tree::parse_scope_fixture(source, Path::new("semantic_types.sv")).unwrap();
    let tree = AnalysisContext::unspecialized(&tree);
    let declaring = ConstEnv::from_iter([("W".into(), 4)]);
    let class =
        crate::ast::classes::class(&tree, session, "C", &declaring, &TypeAliases::default())
            .unwrap();
    let ty = &class.properties[0].r#type;
    let declared = of_type(&mut session.types, &tree.functions, ty, &declaring).unwrap();
    let container = ty.container().map(|c| &**c).unwrap().clone();
    let alone = crate::ast::containers::of(container);
    let unbound = session.types.unbound_reads;
    assert_eq!(
        of_type(
            &mut session.types,
            &tree.functions,
            &alone,
            &ConstEnv::default()
        ),
        Some(declared)
    );
    assert_eq!(session.types.unbound_reads, unbound);
}

#[test]
fn handles_and_fixed_size_arrays_of_them_name_their_class() {
    // IEEE 1800-2023 8.4: a handle names its class; an element of an
    // unpacked array of handles is one (7.4.2). A dynamic array is a
    // container, whose elements its type does not name.
    let mut table = TyTable::default();
    let decl = table.class("C");
    let class = table.intern(TyData::Class(decl));
    let fixed = |element| TyData::Array {
        kind: ArrayKind::Fixed(Range { left: 0, right: 1 }),
        element,
    };
    let array = table.intern(fixed(class));
    let nested = table.intern(fixed(array));
    let dynamic = table.intern(TyData::Array {
        kind: ArrayKind::Dynamic,
        element: class,
    });
    let string = table.intern(TyData::String);
    for (id, expected) in [
        (class, Some("C")),
        (array, Some("C")),
        (nested, Some("C")),
        (dynamic, None),
        (string, None),
    ] {
        assert_eq!(table.handle_class(id).map(|class| &**class), expected);
    }
}

#[test]
fn class_handle_types_are_bound_where_they_are_made() {
    let mut table = TyTable::default();
    let handle = crate::ast::classes::handle(&mut table, "C");
    assert_eq!(handle.class_in(&table).as_deref(), Some("C"));
    assert_eq!(table.unbound_reads, 0);
}

#[test]
fn enumerations_and_packed_arrays_of_them_are_told_by_their_types() {
    // IEEE 1800-2023 6.19: an enumerated type; 7.4.1: a packed array of
    // one; 7.4.2: an unpacked array of one, whose elements are of it.
    let (table, ids) = typedefs(
        "typedef enum logic [1:0] {A, B} e_t;
         typedef e_t [1:0] pa_t;
         typedef e_t ua_t [2];
         typedef logic [1:0] v_t;",
    );
    let enumeration = |name: &str| table.enumeration(ids[name]);
    assert_eq!(enumeration("e_t"), Some(false));
    assert_eq!(enumeration("pa_t"), Some(true));
    assert_eq!(enumeration("ua_t"), None);
    assert_eq!(
        table.enumeration(table.fixed_element(ids["ua_t"])),
        Some(false)
    );
    assert_eq!(enumeration("v_t"), None);
    let domain = table.enum_domain(ids["e_t"]).expect("the members of e_t");
    assert_eq!(
        domain
            .members
            .iter()
            .map(|member| &*member.name)
            .collect::<Vec<_>>(),
        ["A", "B"]
    );
}

#[test]
fn unpacked_structures_and_fixed_size_arrays_of_them_are_told_by_their_types() {
    // IEEE 1800-2023 7.2: an unpacked structure; 7.4.2: an element of a
    // fixed-size array of them is one; a queue of them is a container.
    let (table, ids) = typedefs(
        "typedef struct {int a;} s_t;
         typedef s_t sa_t [2];
         typedef s_t sq_t [$];
         typedef struct packed {int a;} p_t;",
    );
    let is_struct = |name: &str| table.is_unpacked_struct(table.fixed_element(ids[name]));
    assert!(is_struct("s_t"));
    assert!(is_struct("sa_t"));
    assert!(!is_struct("sq_t"));
    assert!(!is_struct("p_t"));
}

#[test]
fn unpacked_structure_types_name_their_declarations() {
    // IEEE 1800-2023 7.2: the members, defaults and layout of an unpacked
    // structure type; 7.4.2: those of the elements of a fixed-size array of
    // them. A queue of them is a container (whose elements take no member
    // defaults), and a packed structure has no
    // leaves of its own.
    let (table, ids) = typedefs(
        "typedef struct {int a; bit [3:0] b = 4'd5;} s_t;
         typedef s_t sa_t [2];
         typedef struct {int c;} t_t;
         typedef t_t tq_t [$];
         typedef struct packed {int a;} p_t;",
    );
    let declaration = |name: &str| table.unpacked_struct(ids[name]);
    let aggregate = declaration("s_t").expect("the declaration of `s_t`");
    assert_eq!(
        aggregate
            .members()
            .iter()
            .map(|member| (member.name.as_str(), member.width, member.default.is_some()))
            .collect::<Vec<_>>(),
        [("a", 32, false), ("b", 4, true)]
    );
    assert!(Arc::ptr_eq(
        declaration("sa_t").expect("the declaration of the elements of `sa_t`"),
        aggregate
    ));
    assert!(declaration("t_t").is_some());
    assert!(declaration("tq_t").is_none());
    assert!(declaration("p_t").is_none());
}

#[test]
fn classes_and_interfaces_are_declarations_of_their_own() {
    // IEEE 1800-2023 8.4, 25.9: a handle names its class and a virtual
    // interface its interface, each one declaration however often a type
    // names it, apart from a typedef or an interface of the same name.
    let mut table = TyTable::default();
    let class = table.class("C");
    assert_eq!(table.class("C"), class);
    let interface = table.interface("C");
    assert_ne!(interface, class);
    let typedef = table.declare("C", None);
    let (Decl::Class(class), Decl::Interface(interface)) = (class, interface) else {
        panic!("a class and an interface declaration");
    };
    assert!(typedef != class && typedef != interface);
    assert_eq!(table.decl(class).name.as_ref(), "C");
    // 8.25: the parameter values of a specialization, recorded once.
    let int = table.intern(TyData::Packed(Packed {
        element: PackedElement::Bits,
        four_state: false,
        signed: true,
        signed_element_depth: None,
        dims: Box::new([Range { left: 31, right: 0 }]),
        keyword: Some("int"),
    }));
    let arguments: Arc<[ClassArgument]> =
        Arc::from([ClassArgument::Type(int), ClassArgument::Value(Some(3))]);
    table.record_class_arguments(class, arguments.clone());
    assert_eq!(table.decl(class).arguments.as_ref(), Some(&arguments));
}

#[test]
fn typedefs_name_the_declarations_of_their_aggregates() {
    // IEEE 1800-2023 6.18: a typedef names a structure or union type, and
    // `typedef A B` names the type of `A` still (6.22.1). The declaration
    // of the type keeps the name: each member of a packed aggregate (7.2.1,
    // 7.3.1), and an unpacked structure (7.2), which keep it through the
    // copies of their type.
    let session = &mut Session::default();
    let source = "module M;
        typedef struct packed {logic a;} p_t;
        typedef p_t q_t;
        typedef union packed {logic a;} u_t;
        typedef struct {int a;} s_t;
        endmodule";
    let tree =
        crate::source_tree::parse_scope_fixture(source, Path::new("semantic_types.sv")).unwrap();
    let tree = AnalysisContext::unspecialized(&tree);
    let env = ConstEnv::default();
    let aliases = types::type_aliases_from_scope_with_env(&tree, session, &env).unwrap();
    let packed = |name: &str| {
        let ty = &aliases[name];
        let declaration = ty.members[0].declaration().clone();
        assert!(
            ty.members
                .iter()
                .all(|member| Arc::ptr_eq(member.declaration(), &declaration))
        );
        let rebuilt = crate::ast::Type::new(crate::ast::TypeKind::Logic).with_members_of(ty);
        assert_eq!(
            rebuilt.members[0].declaration().naming().declaration(),
            declaration.naming().declaration()
        );
        (
            declaration.naming().declaration().map(str::to_owned),
            declaration.keyword(),
            ty.declaration_name().map(str::to_owned),
        )
    };
    let (p_t, keyword, spelled) = packed("p_t");
    assert!(p_t.as_deref().is_some_and(|name| name.ends_with("p_t")));
    assert_eq!((keyword, &spelled), (Some("struct"), &p_t));
    assert_eq!(packed("q_t"), (p_t.clone(), Some("struct"), p_t));
    let (u_t, keyword, spelled) = packed("u_t");
    assert!(u_t.as_deref().is_some_and(|name| name.ends_with("u_t")));
    assert_eq!((keyword, spelled), (Some("union"), u_t));
    let s_t = &aliases["s_t"];
    let structure = s_t.unpacked_struct().expect("an unpacked structure");
    assert!(
        structure
            .naming()
            .declaration()
            .is_some_and(|name| name.ends_with("s_t"))
    );
    assert_eq!(structure.naming().declaration(), s_t.declaration_name());
}

#[test]
fn shapes_of_a_named_aggregate_are_of_its_typedef_without_a_declared_type() {
    // IEEE 1800-2023 6.22.1: a typedef of a structure is a type of its own,
    // which matches only itself; an element or member selected from a
    // variable of it (7.2, 7.4) is of that type, and `$typename` writes its
    // name (20.6.1). The shape of such a value, made without the declared
    // type of a variable, is of the typedef too, not of an anonymous
    // structure declared at the same place.
    let session = &mut Session::default();
    let source = "module M;
        typedef struct {int a;} s_t;
        typedef struct packed {logic [3:0] a;} p_t;
        endmodule";
    let tree =
        crate::source_tree::parse_scope_fixture(source, Path::new("semantic_types.sv")).unwrap();
    let tree = AnalysisContext::unspecialized(&tree);
    let env = ConstEnv::default();
    let aliases = types::type_aliases_from_scope_with_env(&tree, session, &env).unwrap();
    let mut table = TyTable::default();
    for name in ["s_t", "p_t"] {
        let ty = &aliases[name];
        let id = of_type(&mut table, &tree.functions, ty, &env).unwrap();
        let shape = crate::ast::procedural::dimensions_from_type(ty);
        let element = shape.derived(crate::ast::variable_dimensions::VariableShape {
            declared: crate::ast::type_names::Declared::default(),
            ..shape.clone_shape()
        });
        let element = element
            .type_id(&mut table, &tree.functions, &env)
            .unwrap_or_else(|| panic!("a shape of `{name}` has a semantic type"));
        assert_eq!(element, id, "a shape of `{name}`");
        assert!(table.matches(element, id));
    }
}
