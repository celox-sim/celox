/// A ref actual of an enumerated type is equivalent only to a formal of the
/// same enumerated type (IEEE 1800-2023 6.22.2): not to its base type, nor
/// to another enumeration of the same base and values.
#[test]
fn ref_actuals_of_enumerated_types_match_only_their_enumeration() {
    let source = |formal: &str, actual: &str| {
        format!(
            "module Top(output logic [1:0] y);
               typedef enum bit [1:0] {{A, B, C}} E;
               typedef enum bit [1:0] {{P, Q, R}} F;
               typedef bit [1:0] U;
               {actual} v = {actual}'(1);
               function automatic void advance(ref {formal} x);
                 x = {formal}'(x + 1);
               endfunction
               initial begin advance(v); y = v; end
             endmodule"
        )
    };
    let build = |formal: &str, actual: &str| {
        let source = source(formal, actual);
        celox::Simulation::from_sv_sources(
            vec![(source.as_str(), std::path::Path::new("ref_enums.sv"))],
            "Top",
        )
        .build()
        .map_err(|error| format!("{error:?}"))
    };
    let mut sim = build("E", "E").unwrap();
    sim.run_until(0).unwrap();
    let y = sim.signal("y");
    assert_eq!(sim.get(y), num_bigint::BigUint::from(2u8));
    for (formal, actual) in [("E", "F"), ("F", "E"), ("E", "U"), ("U", "E")] {
        let error = build(formal, actual)
            .err()
            .unwrap_or_else(|| panic!("ref {formal} from {actual} must be rejected"));
        assert!(
            error.contains("the actual's type is not equivalent to the formal's"),
            "ref {formal} from {actual}: {error}"
        );
    }
}

/// An element of an unpacked array of a structure type is of that type
/// (IEEE 1800-2023 7.4), which a ref formal of it matches (6.22.1, 6.22.2),
/// from a one- or two-dimensional array: a ref binding to such an element
/// is not rejected as of another type. An element of another structure
/// type of the same members is still of that type.
#[test]
fn ref_actuals_selected_from_arrays_of_a_structure_are_of_its_type() {
    let source = |actual: &str| {
        format!(
            "module Top;
               typedef struct {{ int a; }} s_t;
               typedef struct {{ int a; }} t_t;
               s_t one [2];
               s_t two [2][2];
               t_t other [2];
               task automatic set(ref s_t x); x.a = 5; endtask
               initial set({actual});
             endmodule"
        )
    };
    let build = |actual: &str| {
        let source = source(actual);
        celox::Simulation::from_sv_sources(
            vec![(source.as_str(), std::path::Path::new("ref_structures.sv"))],
            "Top",
        )
        .build()
        .map(|_| ())
        .map_err(|error| format!("{error:?}"))
    };
    for actual in ["one[0]", "two[1][0]"] {
        // A ref binding to an element of an unpacked array of structures
        // is not supported yet; it is not of another type.
        let error = build(actual).expect_err("ref binding to an array element of a structure");
        assert!(
            !error.contains("not equivalent") && error.contains("ref argument `x` of `set`"),
            "ref s_t from {actual}: {error}"
        );
    }
    let error = build("other[0]").expect_err("ref s_t from a t_t");
    assert!(
        error.contains("the actual's type is not equivalent to the formal's"),
        "ref s_t from other[0]: {error}"
    );
}
