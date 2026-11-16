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
