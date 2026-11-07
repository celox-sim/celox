sv_backends! {
fn overlapped_boolean_implication(sim) { @case "concurrent_assertions::overlapped_boolean_implication"; }
fn overlapped_unknown_antecedent(sim) { @case "concurrent_assertions::overlapped_unknown_antecedent"; }
fn sampled_history_while_disabled(sim) { @case "concurrent_assertions::sampled_history_while_disabled"; }
fn sampled_history_predicates(sim) { @case "concurrent_assertions::sampled_history_predicates"; }
fn sampled_history_gates_and_indices(sim) { @case "concurrent_assertions::sampled_history_gates_and_indices"; }
fn sampled_history_unknown_transitions(sim) { @case "concurrent_assertions::sampled_history_unknown_transitions"; }
fn sampled_history_explicit_nesting(sim) { @case "concurrent_assertions::sampled_history_explicit_nesting"; }
fn sampled_predicate_live_action(_sim) { @case "concurrent_assertions::sampled_predicate_live_action"; }
fn disable_iff_after_nba(_sim) { @case "concurrent_assertions::disable_iff_after_nba"; }
fn predicates_before_all_actions(_sim) { @case "concurrent_assertions::predicates_before_all_actions"; }
fn assume_cover_and_generate(_sim) { @case "concurrent_assertions::assume_cover_and_generate"; }
fn subroutine_calls_in_actions(_sim) { @case "concurrent_assertions::subroutine_calls_in_actions"; }
fn named_sequences_and_properties(_sim) { @case "concurrent_assertions::named_sequences_and_properties"; }
fn named_sequence_leading_clock(_sim) { @case "concurrent_assertions::named_sequence_leading_clock"; }
fn sequences_over_cycles(_sim) { @case "concurrent_assertions::sequences_over_cycles"; }
fn sequence_values_over_sampled_ticks(_sim) { @case "concurrent_assertions::sequence_values_over_sampled_ticks"; }
fn boolean_iff_properties(_sim) { @case "concurrent_assertions::boolean_iff_properties"; }
fn expect_statements(_sim) { @case "concurrent_assertions::expect_statements"; }
}
sv_backends! {
fn unknown_predicates_and_disable_conditions(_sim) { @case "concurrent_assertions::unknown_predicates_and_disable_conditions"; }
}

#[test]
fn native_images_preserve_assertion_regions() {
    crate::suite::run_case(
        "concurrent_assertions::sampled_predicate_live_action",
        "native-image",
    );
}

#[test]
fn native_images_preserve_sampled_history_monitors() {
    for case in [
        "concurrent_assertions::sampled_history_gates_and_indices",
        "concurrent_assertions::sampled_history_unknown_transitions",
    ] {
        crate::suite::run_case(case, "native-image");
    }
}

/// A name in an `expect` resolves in the expect's scope (IEEE 1800-2023
/// 16.17): a task formal `p` is the formal, not the module's property `p`,
/// which an `expect (p)` outside the task instantiates. The body of `p`
/// binds where `p` is declared (16.8): with a task formal `a`, `p` still
/// reads the module's `a`.
#[test]
fn expect_names_resolve_in_their_scope() {
    let source = "`timescale 1ns/1ns
        module Top(output int r1, r2, r3);
            bit clk, a;
            property p;
                @(posedge clk) a;
            endproperty
            task automatic t(bit p, output int r);
                expect (@(posedge clk) p) r = 1; else r = 2;
            endtask
            task automatic u(bit a, output int r);
                expect (p) r = 1; else r = 2;
            endtask
            initial begin
                t(1, r1);
                expect (p) r2 = 1; else r2 = 2;
                u(1, r3);
            end
            initial repeat (4) begin #5 clk = 1; #5 clk = 0; end
        endmodule";
    for four_state in [false, true] {
        let mut sim = celox::Simulation::from_sv_sources(
            vec![(source, std::path::Path::new("expect.sv"))],
            "Top",
        )
        .four_state(four_state)
        .build()
        .unwrap();
        sim.run_until(50).unwrap();
        let (r1, r2, r3) = (sim.signal("r1"), sim.signal("r2"), sim.signal("r3"));
        assert_eq!(
            sim.get(r1),
            num_bigint::BigUint::from(1u8),
            "four-state {four_state}"
        );
        assert_eq!(
            sim.get(r3),
            num_bigint::BigUint::from(2u8),
            "four-state {four_state}"
        );
        assert_eq!(
            sim.get(r2),
            num_bigint::BigUint::from(2u8),
            "four-state {four_state}"
        );
    }
}
