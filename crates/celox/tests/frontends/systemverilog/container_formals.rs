use super::*;

sv_backends! {
    fn input_container_changes_are_not_seen_by_the_caller(sim) {
        @case "container_formals::input_container_changes_are_not_seen_by_the_caller";
    }
    fn input_container_from_container_values(sim) {
        @case "container_formals::input_container_from_container_values";
    }
    fn input_container_from_an_empty_concatenation(sim) {
        @case "container_formals::input_container_from_an_empty_concatenation";
    }
    fn output_container_starts_empty_and_is_copied_back(sim) {
        @case "container_formals::output_container_starts_empty_and_is_copied_back";
    }
    fn inout_container_is_copied_in_and_back(sim) {
        @case "container_formals::inout_container_is_copied_in_and_back";
    }
    fn task_copies_containers_back_when_it_returns(sim) {
        @case "container_formals::task_copies_containers_back_when_it_returns";
    }
    fn container_formals_of_class_handles_copy_the_container(sim) {
        @case "container_formals::container_formals_of_class_handles_copy_the_container";
    }
    fn container_formals_of_class_methods(sim) {
        @case "container_formals::container_formals_of_class_methods";
    }
    fn static_subroutine_container_formals(sim) {
        @case "container_formals::static_subroutine_container_formals";
    }
    fn container_formals_forwarded_and_read_from_properties(sim) {
        @case "container_formals::container_formals_forwarded_and_read_from_properties";
    }
    fn selects_of_reference_container_elements(sim) {
        @case "container_formals::selects_of_reference_container_elements";
    }
    fn selects_of_four_state_container_elements(sim) {
        @case "container_formals::selects_of_four_state_container_elements";
    }
}

/// The index of a select of a dynamic element assigned a value with a call
/// is evaluated before the call and keeps its width and unknown bits: out
/// of range or unknown, it writes no element (IEEE 1800-2023 7.10.1,
/// 11.5.1).
#[test]
fn element_select_index_before_a_call_keeps_its_value() {
    let source = r#"
    module Top(output int k);
        bit [7:0] r[$];
        logic [63:0] x;
        function automatic bit [3:0] one();
            return 1;
        endfunction
        initial begin
            r = '{8'h00};
            r[64'h1_0000_0000][0] = one();
            x = 'x;
            r[x][1] = one();
            k = r[0];
        end
    endmodule"#;
    // A two-state `x` holds 0, which selects the first element.
    for (four_state, expected) in [(true, 0u8), (false, 2)] {
        let mut sim = celox::Simulation::from_sv_sources(
            vec![(source, Path::new("element-select-index.sv"))],
            "Top",
        )
        .four_state(four_state)
        .build()
        .unwrap();
        let k = sim.signal("k");
        sim.run_until(0).unwrap();
        assert_eq!(
            sim.get(k),
            BigUint::from(expected),
            "four_state={four_state}"
        );
    }
}

/// A select of a dynamic element at a position that changes the container
/// is rejected: the element would be read before the position is
/// evaluated (IEEE 1800-2023 11.5.1).
#[test]
fn element_select_at_a_position_with_effects_is_rejected() {
    let source = "module Top; bit [7:0] q[$]; \
        initial begin q = '{8'h00, 8'h10}; q[0][q.pop_front()] = 1'b1; end endmodule";
    let Err(error) =
        Simulator::from_sv_sources(vec![(source, Path::new("select.sv"))], "Top").build_cranelift()
    else {
        panic!("a select at a position with effects is unsupported");
    };
    let error = error.to_string();
    assert!(
        error.contains("select of a dynamic element at a position with effects"),
        "{error}"
    );
}
