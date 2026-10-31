use super::*;

sv_backends! {
    fn inherited_parameter_aliases_materialize_replication_masks(sim) {
        @case "types::inherited_parameter_aliases_materialize_replication_masks";
    }
    fn inherited_parameter_aliases_preserve_types_and_unknowns(sim) {
        @case "types::inherited_parameter_aliases_preserve_types_and_unknowns";
    }
}

sv_backends! {
    fn constant_functions_in_unpacked_port_dimensions(sim) {
        @case "types::constant_functions_in_unpacked_port_dimensions";
    }

    fn constant_functions_in_unpacked_dimensions(sim) {
        @case "types::constant_functions_in_unpacked_dimensions";
    }

    fn passes_unpacked_arrays_of_equivalent_element_types(sim) {
        @case "types::passes_unpacked_arrays_of_equivalent_element_types";
    }

    fn passes_unpacked_arguments_of_equivalent_element_types(sim) {
        @case "types::passes_unpacked_arguments_of_equivalent_element_types";
    }

    fn rejects_unpacked_argument_of_a_wider_element_type(sim) {
        @case "types::rejects_unpacked_argument_of_a_wider_element_type";
    }

    fn rejects_unpacked_assignment_of_a_different_state_count(sim) {
        @case "types::rejects_unpacked_assignment_of_a_different_state_count";
    }

    fn rejects_unpacked_assignment_of_a_different_element_count(sim) {
        @case "types::rejects_unpacked_assignment_of_a_different_element_count";
    }

    fn rejects_unpacked_port_connection_of_a_narrower_element_type(sim) {
        @case "types::rejects_unpacked_port_connection_of_a_narrower_element_type";
    }

    fn rejects_assignment_pattern_item_of_a_narrower_array_type(sim) {
        @case "types::rejects_assignment_pattern_item_of_a_narrower_array_type";
    }

    fn packed_arrays_of_signed_named_types_have_signed_elements(sim) {
        @case "types::packed_arrays_of_signed_named_types_have_signed_elements";
    }

    fn packed_parameters_of_signed_named_types_have_signed_elements(sim) {
        @case "types::packed_parameters_of_signed_named_types_have_signed_elements";
    }

    fn simulates_systemverilog_parameterized_port_widths(sim) {
        @case "types::simulates_systemverilog_parameterized_port_widths";
    }

    fn simulates_systemverilog_bit_ports_as_two_state(sim) {
        @case "types::simulates_systemverilog_bit_ports_as_two_state";
    }

    fn preserves_integer_atom_state_kinds(sim) {
        @case "types::preserves_integer_atom_state_kinds";
    }
}

sv_backends! {
    fn undriven_internal_nets_are_high_impedance(sim) {
        @case "types::undriven_internal_nets_are_high_impedance";
    }
    fn undriven_internal_nets_convert_to_two_state_zero(sim) {
        @case "types::undriven_internal_nets_convert_to_two_state_zero";
    }
}

sv_backends! {
    fn enum_methods_iterate_members(sim) {
        @case "types::enum_methods_iterate_members";
    }

    fn enum_methods_wrap_and_count(sim) {
        @case "types::enum_methods_wrap_and_count";
    }

    fn enum_methods_in_combinational_and_sequential_logic(sim) {
        @case "types::enum_methods_in_combinational_and_sequential_logic";
    }

    fn unpacked_array_assignments_through_selects_and_calls(sim) {
        @case "types::unpacked_array_assignments_through_selects_and_calls";
    }
}

// Receivers, counts and contexts that the external simulators of the shared
// suite do not accept: Verilator rejects non-constant counts and methods of
// casts, constants and call results; Icarus rejects methods of call results
// and of class properties, and methods in continuous assignments; xezim calls a method
// of an array element as one of a class handle and returns 0 instead of the
// 4-state base's 'x for a value that is no member (IEEE 1800-2023 6.19.5.3,
// 6.19.5.4, Table 6-7).
sv_backends! {
    fn enum_methods_of_any_enum_expression(sim) {
        @setup {
            let source = r#"
package p;
    typedef enum {PA, PB, PC} P;
    typedef enum {SHARED, OTHER} Q;
    P states[2] = '{PB, PC};
    localparam P START = PB;
endpackage
package r;
    typedef enum {SHARED, MIDDLE, LAST} R;
endpackage
module Top(output bit ok, output logic [1:0] unknown_next, output logic [1:0] unknown_prev,
           output bit unnamed, output bit [1:0] continuous, output bit [1:0] gap_next,
           output bit [3:0] chained);
    typedef enum bit [3:0] {W0, W1, W2, W3, W4, W5, W6, W7, W8, W9, W10, W11, W12, W13, W14, W15} W;
    W wide;
    // Each nested lookup lowers its selector once, not once per member.
    assign chained = wide.next().next().next().next().next().next().next().next();
    typedef enum logic [1:0] {IDLE, RUN, DONE} S;
    typedef enum bit [1:0] {K0, K1, K2} K;
    typedef enum bit [2:0] {A = 3'd1, B = 3'd4, C = 3'd6} E;
    S unknown;
    S queue[$];
    S states[3];
    E e;
    K current, gap;
    bit [2:0] steps;
    logic signed [2:0] signed_count;
    typedef enum logic [1:0] {LA, LB, LC} L;
    L le;
    // Parameters of an enumerated type.
    localparam S INITIAL = RUN;
    localparam E LAST = C;
    logic [1:0] unknown_count;
    int unsigned n;
    int calls;
    class Machine;
        static S initial_state = DONE;
        K state;
        function K current();
            return state;
        endfunction
        function string describe();
            return state.name();
        endfunction
    endclass
    function automatic S advance();
        calls++;
        return RUN;
    endfunction
    assign continuous = current.next(steps);
    assign gap_next = gap.next(2);
    initial begin
        Machine m;
        process running;
        ok = 1;
        e = B;
        ok &= e.next(2).name() == "A" && e.next.next.name == "A" && e.prev.name == "A";
        ok &= S'(0).name() == "IDLE" && E'(3'd6).prev().name() == "B";
        ok &= DONE.prev().name() == "RUN" && RUN.num() == 3 && C.next() == A;
        states[1] = DONE;
        ok &= states[1].name() == "DONE" && states[1].next.name == "IDLE";
        ok &= Machine::initial_state.name() == "DONE";
        queue.push_back(RUN);
        ok &= queue[0].prev().name() == "IDLE";
        ok &= advance().next(2).name() == "IDLE" && calls == 1;
        // Results that do not depend on the receiver still evaluate it once.
        ok &= advance().first() == IDLE && advance().last() == DONE && advance().num() == 3;
        ok &= calls == 4;
        ok &= p::PA.next().name() == "PB" && p::PC.prev(2).name() == "PA" && p::PB.num() == 3;
        // Each package's enumeration, though both declare SHARED = 0.
        ok &= p::states[0].name() == "PB" && p::states[1].next() == p::PA;
        ok &= p::SHARED.num() == 2 && r::SHARED.num() == 3 && r::SHARED.next().name() == "MIDDLE";
        queue = '{RUN, DONE};
        ok &= queue.pop_front().name() == "RUN" && queue.pop_back().next() == IDLE;
        ok &= DONE.name().len() == 4 && DONE.name().tolower() == "done";
        ok &= RUN.name.toupper() == "RUN" && RUN.next.name.tolower() == "done";
        m = new;
        m.state = K1;
        ok &= m.current().next().name() == "K2" && m.state.next() == K2;
        ok &= m.state.name() == "K1" && m.describe() == "K1";
        n = 5;
        ok &= e.next(n) == A && e.prev(n) == C;
        n = 32'hffff_ffff;
        ok &= e.next(n) == B && e.prev(n) == B;
        // A signed count converts to `int unsigned` with sign extension, at
        // run time as for a constant: -1 is a multiple of 3 and -2 is 2 more.
        signed_count = -1;
        ok &= e.next(signed_count) == B && e.next(3'sb111) == B;
        signed_count = -2;
        ok &= e.prev(signed_count) == C && e.prev(3'sb110) == C;
        // `int unsigned` is 2-state: an unknown count converts to 0.
        le = LB;
        unknown_count = 'x;
        ok &= le.next(unknown_count) === LB && le.prev(unknown_count) === LB;
        unknown_count = 2'bz1;
        ok &= le.next(unknown_count) === LC;
        ok &= INITIAL.next().name() == "DONE" && INITIAL.name() == "RUN";
        ok &= LAST.next() == A && LAST.num() == 3 && p::START.next() == p::PC;
        // The status of a process and the constants of its `state` type (9.7).
        running = process::self();
        ok &= running.status().name() == "RUNNING" && running.status.next() == process::WAITING;
        ok &= process::KILLED.next().name() == "FINISHED" && std::process::RUNNING.num() == 5;
        e = E'(3'd5);
        ok &= e.prev(n) == 0 && e.next(n) == 0;
        unknown_next = unknown.next();
        unknown_prev = unknown.prev(2);
        unnamed = unknown.name() == "";
        current = K1;
        wide = W3;
        gap = K'(3);
        steps = 4;
    end
endmodule
"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("enum_methods.sv"))], "Top")
            .four_state(true);
        sim.run_until(0).unwrap();
        let ok = sim.signal("ok");
        assert_eq!(sim.get(ok), BigUint::from(1u8));
        for name in ["unknown_next", "unknown_prev"] {
            let signal = sim.signal(name);
            assert_eq!(
                sim.get_four_state(signal),
                (BigUint::from(3u8), BigUint::from(3u8)),
                "{name}"
            );
        }
        let unnamed = sim.signal("unnamed");
        assert_eq!(sim.get(unnamed), BigUint::from(1u8));
        let continuous = sim.signal("continuous");
        assert_eq!(sim.get(continuous), BigUint::from(2u8));
        let chained = sim.signal("chained");
        assert_eq!(sim.get(chained), BigUint::from(11u8));
        let gap_next = sim.signal("gap_next");
        assert_eq!(sim.get(gap_next), BigUint::from(0u8));
    }
}

/// Methods of an element of a packed array of enums are not supported yet;
/// they are rejected, not given the members of another type.
#[test]
fn enum_methods_of_packed_array_elements_are_rejected() {
    for (declaration, call) in [
        (
            "typedef enum bit [1:0] {A, B, C} E; E [1:0] states;",
            "states[0].next()",
        ),
        (
            "typedef enum bit [1:0] {A, B, C} E; E [1:0] states;",
            "states[1].name() == \"B\"",
        ),
        ("enum bit [1:0] {X, Y} [1:0] states;", "states[0].first()"),
    ] {
        let source =
            format!("module Top(output bit [1:0] y); {declaration} initial y = {call}; endmodule");
        let error = celox::Simulation::from_sv_sources(
            vec![(source.as_str(), Path::new("packed_enum_methods.sv"))],
            "Top",
        )
        .build()
        .err()
        .unwrap_or_else(|| panic!("{source} must be rejected"))
        .to_string();
        assert!(
            error.contains("method of an element of a packed array of enums"),
            "{error}"
        );
    }
}
