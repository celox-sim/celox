use celox::{BigUint, Simulator};

#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {
    fn indeterminate_short_circuit_lhs_executes_effectful_rhs(sim) {
        @ignore_on(sv);
        @case "four_state_expression_semantics::indeterminate_short_circuit_lhs_executes_effectful_rhs";
    }

    fn indeterminate_ternary_executes_effectful_arms_in_order(sim) {
        @ignore_on(veryl, sv);
        @case "four_state_expression_semantics::indeterminate_ternary_executes_effectful_arms_in_order";
    }

    fn logical_unknown_truth_table_matches_comb_and_ff(sim) {
        @ignore_on(sv);
        @case "four_state_expression_semantics::logical_unknown_truth_table_matches_comb_and_ff";
    }

    fn ternary_unknown_condition_merges_branch_bits(sim) {
        @omit_veryl;
        @ignore_on(sv);
        @case "four_state_expression_semantics::ternary_unknown_condition_merges_branch_bits";
    }

    fn wide_logical_unknown_truth_table_uses_dominant_values(sim) {
        @ignore_on(sv);
        @case "four_state_expression_semantics::wide_logical_unknown_truth_table_uses_dominant_values";
    }

    fn logical_not_known_one_dominates_unknown_bits(sim) {
        @ignore_on(sv);
        @case "four_state_expression_semantics::logical_not_known_one_dominates_unknown_bits";
    }

    fn effectful_ternary_takes_known_true_branch_despite_unknown_bits(sim) {
        @ignore_on(veryl, sv);
        @case "four_state_expression_semantics::effectful_ternary_takes_known_true_branch_despite_unknown_bits";
    }

    fn ff_procedural_control_uses_known_nonzero_truth(sim) {
        @omit_veryl;
        @ignore_on(sv);
        @case "four_state_expression_semantics::ff_procedural_control_uses_known_nonzero_truth";
    }

    fn ff_unknown_reset_is_not_active(sim) {
        @omit_veryl;
        @ignore_on(sv);
        @case "four_state_expression_semantics::ff_unknown_reset_is_not_active";
    }

    fn ff_assert_uses_procedural_four_state_truth(sim) {
        @omit_veryl;
        @ignore_on(sv);
        @build Simulator::builder(r#"
module Top (
    clk: input clock,
    cond: input logic<130>,
) {
    always_ff (clk) {
        $assert_continue(cond, "bad condition");
    }
}
"#, "Top").four_state(true);

        let clk = sim.event("clk");
        let cond = sim.signal("cond");
        let known_one = BigUint::from(1u8) << 100usize;
        let unknown = BigUint::from(1u8) << 2usize;

        sim.modify(|io| {
            io.set_four_state(cond, &known_one | &unknown, unknown.clone());
        })
        .unwrap();
        sim.tick(clk).unwrap();
        assert!(sim.drain_runtime_events().is_empty());

        for (value, mask, label) in [
            (BigUint::from(0u8), BigUint::from(0u8), "0"),
            (unknown.clone(), unknown.clone(), "X"),
            (BigUint::from(0u8), unknown.clone(), "Z"),
        ] {
            sim.modify(|io| {
                io.set_four_state(cond, value.clone(), mask.clone());
            })
            .unwrap();
            sim.tick(clk).unwrap();
            assert_eq!(
                sim.drain_runtime_events(),
                vec![celox::RuntimeEvent::AssertContinue {
                    message: "bad condition".to_string(),
                }],
                "cond={label}",
            );
        }
    }

    fn comb_assert_uses_procedural_four_state_truth(sim) {
        @omit_veryl;
        @ignore_on(sv);
        @build Simulator::builder(r#"
module Top (
    cond: input logic<130>,
) {
    always_comb {
        $assert_continue(cond, "bad condition");
    }
}
"#, "Top").four_state(true);

        let cond = sim.signal("cond");
        let known_one = BigUint::from(1u8) << 100usize;
        let unknown = BigUint::from(1u8) << 2usize;
        sim.drain_runtime_events();

        for (unknown_value, label) in [(1u8, "X"), (0u8, "Z")] {
            sim.modify(|io| {
                io.set_four_state(
                    cond,
                    &known_one | (&unknown * unknown_value),
                    unknown.clone(),
                );
            })
            .unwrap();
            assert!(
                sim.drain_runtime_events().is_empty(),
                "known one plus {label} must pass",
            );

            sim.modify(|io| {
                io.set_four_state(cond, &unknown * unknown_value, unknown.clone());
            })
            .unwrap();
            assert_eq!(
                sim.drain_runtime_events(),
                vec![celox::RuntimeEvent::AssertContinue {
                    message: "bad condition".to_string(),
                }],
                "only {label} must fail",
            );
        }
    }

    fn wide_ternary_condition_known_one_dominates_unknown_bits(sim) {
        @ignore_on(sv);
        @case "four_state_expression_semantics::wide_ternary_condition_known_one_dominates_unknown_bits";
    }

    fn wide_ternary_unknown_condition_merges_every_arm_chunk(sim) {
        @ignore_on(veryl, sv);
        @case "four_state_expression_semantics::wide_ternary_unknown_condition_merges_every_arm_chunk";
    }

    fn wide_unaligned_partial_store_and_slice_preserve_four_state_bits(sim) {
        @case "four_state_expression_semantics::wide_unaligned_partial_store_and_slice_preserve_four_state_bits";
    }
}
