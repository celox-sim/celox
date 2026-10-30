sv_backends! {
    fn ref_writes_are_seen_before_the_return(sim) {
        @case "references::ref_writes_are_seen_before_the_return";
    }
    fn ref_formals_of_one_actual_alias_each_other(sim) {
        @case "references::ref_formals_of_one_actual_alias_each_other";
    }
    fn ref_task_keeps_the_reference_while_suspended(sim) {
        @case "references::ref_task_keeps_the_reference_while_suspended";
    }
    fn ref_fills_caller_queues(sim) {
        @case "references::ref_fills_caller_queues";
    }
    fn ref_replaces_a_class_handle(sim) {
        @case "references::ref_replaces_a_class_handle";
    }
    fn const_ref_reads_the_current_value(sim) {
        @case "references::const_ref_reads_the_current_value";
    }
    fn const_ref_in_a_continuous_assignment(sim) {
        @case "references::const_ref_in_a_continuous_assignment";
    }
    fn ref_default_and_named_arguments(sim) {
        @case "references::ref_default_and_named_arguments";
    }
    fn ref_actual_of_an_equivalent_type(sim) {
        @case "references::ref_actual_of_an_equivalent_type";
    }
    fn ref_class_property_names_the_object_of_the_call(sim) {
        @case "references::ref_class_property_names_the_object_of_the_call";
    }
    fn ref_property_of_this(sim) {
        @case "references::ref_property_of_this";
    }
    fn ref_task_keeps_a_property_while_suspended(sim) {
        @case "references::ref_task_keeps_a_property_while_suspended";
    }
    fn ref_array_element_index_is_fixed_at_the_call(sim) {
        @case "references::ref_array_element_index_is_fixed_at_the_call";
    }
    fn ref_elements_of_concurrent_task_activations(sim) {
        @case "references::ref_elements_of_concurrent_task_activations";
    }
    fn ref_static_class_properties(sim) {
        @case "references::ref_static_class_properties";
    }
    fn ref_property_forwarded_to_output_and_inout(sim) {
        @case "references::ref_property_forwarded_to_output_and_inout";
    }
    fn ref_element_select_past_the_element(sim) {
        @case "references::ref_element_select_past_the_element";
    }
    fn ref_element_after_an_input_in_forked_activations(sim) {
        @case "references::ref_element_after_an_input_in_forked_activations";
    }
}

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
               {actual} v = {{1'b0, 1'b1}};
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
