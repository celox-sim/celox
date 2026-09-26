use celox::{DeadStorePolicy, OptLevel, Simulator, SimulatorBuilder, TestResult};
use insta::assert_snapshot;

#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

fn setup_and_trace(code: &str, top: &str) -> celox::CompilationTrace {
    let result = SimulatorBuilder::new(code, top)
        .optimize(true)
        .trace_sim_modules()
        .trace_post_optimized_sir()
        .build_with_trace();

    result.trace
}

// ---------------------------------------------------------------------------
// Dead Store Elimination (DSE) tests
// ---------------------------------------------------------------------------

const DSE_HIERARCHY_SOURCE: &str = r#"
module Sub (
    i_data: input logic<8>,
    o_data: output logic<8>,
) {
    assign o_data = i_data;
}

module Top (
    clk: input clock,
    rst: input reset,
    top_in: input logic<8>,
    top_out: output logic<8>,
) {
    inst u_sub: Sub (
        i_data: top_in,
        o_data: top_out,
    );
}
"#;

all_backends! {

    fn test_simple_assignment(sim) {
        @case "basic::test_simple_assignment";
    }

    fn test_dependency_chain(sim) {
        @case "basic::test_dependency_chain";
    }

    fn test_mixed_selects_execution(sim) {
        @case "basic::test_mixed_selects_execution";
    }

    fn test_overlapping_override(sim) {
        @case "basic::test_overlapping_override";
    }

    fn test_comb_override_dependency(sim) {
        @ignore_on(sv);
        @case "basic::test_comb_override_dependency";
    }

    fn test_always_comb_read_before_write_uses_previous_value(sim) {
        @ignore_on(sv);
        @case "basic::test_always_comb_read_before_write_uses_previous_value";
    }

    fn test_comb_function_call_early_return(sim) {
        @ignore_on(veryl, sv);
        @case "basic::test_comb_function_call_early_return";
    }

    fn test_comb_function_call_return_indexed_local_temp(sim) {
        @case "basic::test_comb_function_call_return_indexed_local_temp";
    }

    fn test_comb_function_call_partial_write_local_temp(sim) {
        @ignore_on(sv);
        @case "basic::test_comb_function_call_partial_write_local_temp";
    }

    fn test_comb_function_call_local_and_return_width_coercion(sim) {
        @ignore_on(sv);
        @case "basic::test_comb_function_call_local_and_return_width_coercion";
    }

    fn test_comb_function_call_constant_folded_if_return(sim) {
        @ignore_on(sv);
        @case "basic::test_comb_function_call_constant_folded_if_return";
    }

    fn test_comb_function_call_return_inside_for(sim) {
        @ignore_on(veryl, sv);
        @case "basic::test_comb_function_call_return_inside_for";
    }

    fn test_comb_function_call_break_inside_for(sim) {
        @ignore_on(sv);
        @case "basic::test_comb_function_call_break_inside_for";
    }

    fn test_comb_function_call_nested_helper(sim) {
        @case "basic::test_comb_function_call_nested_helper";
    }

    fn test_comb_function_call_statement_with_output_argument(sim) {
        @ignore_on(sv);
        @case "basic::test_comb_function_call_statement_with_output_argument";
    }

    fn test_comb_function_call_expression_with_output_argument(sim) {
        @ignore_on(sv);
        @case "basic::test_comb_function_call_expression_with_output_argument";
    }

    fn test_comb_function_call_expression_output_is_visible_to_later_operand(sim) {
        @ignore_on(sv);
        @case "basic::test_comb_function_call_expression_output_is_visible_to_later_operand";
    }

    fn test_comb_function_call_expression_with_constant_input_keeps_output_write(sim) {
        @ignore_on(sv);
        @case "basic::test_comb_function_call_expression_with_constant_input_keeps_output_write";
    }

    fn test_comb_function_call_expression_output_survives_system_function_wrapper(sim) {
        @ignore_on(sv);
        @case "basic::test_comb_function_call_expression_output_survives_system_function_wrapper";
    }

    fn test_comb_function_call_with_output_argument_in_display_argument(sim) {
        @ignore_on(sv);
        @case "basic::test_comb_function_call_with_output_argument_in_display_argument";
    }

    fn test_comb_function_call_with_output_argument_in_index_expression(sim) {
        @ignore_on(sv);
        @case "basic::test_comb_function_call_with_output_argument_in_index_expression";
    }

    fn test_comb_function_call_with_output_argument_in_destination_index(sim) {
        @ignore_on(sv);
        @case "basic::test_comb_function_call_with_output_argument_in_destination_index";
    }

    fn test_comb_function_call_expression_output_is_guarded_by_ternary(sim) {
        @ignore_on(veryl, sv);
        @case "basic::test_comb_function_call_expression_output_is_guarded_by_ternary";
    }

    fn test_comb_function_call_expression_output_respects_short_circuit(sim) {
        @ignore_on(veryl, sv);
        @case "basic::test_comb_function_call_expression_output_respects_short_circuit";
    }

    fn test_comb_function_call_with_output_argument_in_if_condition(sim) {
        @ignore_on(sv);
        @case "basic::test_comb_function_call_with_output_argument_in_if_condition";
    }

    fn test_comb_function_call_with_output_argument_in_case_target(sim) {
        @ignore_on(sv);
        @case "basic::test_comb_function_call_with_output_argument_in_case_target";
    }

    fn test_comb_function_call_with_output_argument_in_loop_condition(sim) {
        @ignore_on(sv);
        @case "basic::test_comb_function_call_with_output_argument_in_loop_condition";
    }

    fn test_comb_nested_function_output_call_in_function_condition(sim) {
        @ignore_on(veryl, sv);
        @case "basic::test_comb_nested_function_output_call_in_function_condition";
    }

    fn test_comb_case_target_output_call_is_evaluated_once(sim) {
        @ignore_on(veryl, sv);
        @case "basic::test_comb_case_target_output_call_is_evaluated_once";
    }

    fn test_comb_loop_bound_output_call_writes_back_once(sim) {
        @ignore_on(sv);
        @case "basic::test_comb_loop_bound_output_call_writes_back_once";
    }

    fn test_comb_value_system_function_statement_applies_argument_outputs(sim) {
        @ignore_on(veryl, sv);
        @case "basic::test_comb_value_system_function_statement_applies_argument_outputs";
    }

    fn test_comb_value_system_function_after_dynamic_break_stays_inactive(sim) {
        @ignore_on(veryl, sv);
        @case "basic::test_comb_value_system_function_after_dynamic_break_stays_inactive";
    }

    fn test_comb_effectful_if_condition_after_dynamic_break_stays_inactive(sim) {
        @ignore_on(sv);
        @case "basic::test_comb_effectful_if_condition_after_dynamic_break_stays_inactive";
    }

    fn test_statement_call_inputs_follow_output_writeback_order(sim) {
        @ignore_on(sv);
        @case "basic::test_statement_call_inputs_follow_output_writeback_order";
    }

    fn test_comb_effectful_case_after_dynamic_break_stays_inactive(sim) {
        @ignore_on(veryl, sv);
        @case "basic::test_comb_effectful_case_after_dynamic_break_stays_inactive";
    }

    fn test_comb_function_condition_output_is_guarded_after_early_return(sim) {
        @ignore_on(veryl, sv);
        @case "basic::test_comb_function_condition_output_is_guarded_after_early_return";
    }

    fn test_comb_function_call_statement_ignores_return_value(sim) {
        @ignore_on(sv);
        @case "basic::test_comb_function_call_statement_ignores_return_value";
    }

    fn test_comb_returning_output_function_reads_current_caller_store(sim) {
        @ignore_on(sv);
        @case "basic::test_comb_returning_output_function_reads_current_caller_store";
    }

    fn test_comb_returning_output_function_allows_nested_output_call(sim) {
        @ignore_on(sv);
        @case "basic::test_comb_returning_output_function_allows_nested_output_call";
    }

    fn test_comb_function_call_statement_preserves_return_control_flow(sim) {
        @ignore_on(veryl, sv);
        @case "basic::test_comb_function_call_statement_preserves_return_control_flow";
    }

    fn test_comb_function_call_statement_preserves_conditional_return_control_flow(sim) {
        @ignore_on(veryl, sv);
        @case "basic::test_comb_function_call_statement_preserves_conditional_return_control_flow";
    }

    fn test_comb_nested_function_call_statement_with_output_argument(sim) {
        @ignore_on(sv);
        @case "basic::test_comb_nested_function_call_statement_with_output_argument";
    }

    fn test_comb_function_call_output_reads_current_caller_store(sim) {
        @ignore_on(sv);
        @case "basic::test_comb_function_call_output_reads_current_caller_store";
    }

    fn test_comb_function_call_statement_with_output_argument_in_loop(sim) {
        @ignore_on(sv);
        @case "basic::test_comb_function_call_statement_with_output_argument_in_loop";
    }

    fn test_comb_function_call_output_bit_select_preserves_unwritten_loop_bits(sim) {
        @ignore_on(sv);
        @case "basic::test_comb_function_call_output_bit_select_preserves_unwritten_loop_bits";
    }

    fn test_always_comb_blocking_assignment_chain(sim) {
        @ignore_on(sv);
        @case "basic::test_always_comb_blocking_assignment_chain";
    }
}

#[test]
fn test_shared_expression_hoisting() {
    let code = r#"
    module Top (
        a: input logic<32>,
        b: input logic<32>,
        x: output logic<32>,
        y: output logic<32>,
    ) {
        // (a + b) is shared
        assign x = (a + b) & 32'h1;
        assign y = (a + b) | 32'h2;
    }
    "#;

    let trace = setup_and_trace(code, "Top");
    let output = trace.format_program().unwrap();
    assert_snapshot!("shared_expression_sir", output);
}

#[test]
fn test_mux_safe_hoisting() {
    let code = r#"
    module Top (
        a: input logic<32>,
        b: input logic<32>,
        c: input logic,
        x: output logic<32>,
        y: output logic<32>,
    ) {
        var m: logic<32>;
        always_comb {
            if c {
                m = a;
            } else {
                m = b;
            }
        }
        
        // (m + 1) is shared but depends on Mux result (m)
        // It should NOT be hoisted to entry block.
        assign x = (m + 1) & 32'h1;
        assign y = (m + 1) | 32'h2;
    }
    "#;

    let trace = setup_and_trace(code, "Top");
    let output = trace.format_program().unwrap();
    assert_snapshot!("mux_safe_hoisting_sir", output);
}

#[test]
fn test_hash_consing_deduplication() {
    let code = r#"
    module Top (
        a: input logic<32>,
        b: input logic<32>,
        x: output logic<32>,
    ) {
        // Multiple identical additions
        assign x = (a + b) + (a + b);
    }
    "#;

    let trace = setup_and_trace(code, "Top");
    let output = trace.format_program().unwrap();
    assert_snapshot!("hash_consing_sir", output);
}

#[test]
fn test_rle_comb() {
    let trace = setup_and_trace(
        r#"
module ModuleA (
    x: input logic<32>,
    y: input logic<32>,
    z: output logic<32>
) {
    var temp: logic<32>;

    always_comb {
        temp = x + y;
        z = temp;
    }
}
"#,
        "ModuleA",
    );
    let output = trace.format_program().unwrap();
    assert_snapshot!("rle_comb", output);
}

#[test]
fn test_dse_preserve_top_ports() {
    // With PreserveTopPorts, top-level ports (top_in, top_out) survive DSE.
    let mut sim = Simulator::builder(DSE_HIERARCHY_SOURCE, "Top")
        .dead_store_policy(DeadStorePolicy::PreserveTopPorts)
        .build()
        .unwrap();
    let top_in = sim.signal("top_in");
    let top_out = sim.signal("top_out");

    sim.modify(|io| io.set(top_in, 0xABu8)).unwrap();
    assert_eq!(sim.get(top_out), 0xABu64.into());
}

#[test]
fn test_o2_dse_preserves_signals_read_by_native_testbench() {
    let result = Simulator::builder(
        r#"
#[test(test_o2_dse_preserves_signals_read_by_native_testbench)]
module test_o2_dse_preserves_signals_read_by_native_testbench {
    var source: logic;
    var observed: logic;

    assign observed = ~source;

    initial {
        source = 1'b0;
        $assert(observed == 1'b1, "DSE removed a signal read by the testbench");
        $finish();
    }
}
"#,
        "test_o2_dse_preserves_signals_read_by_native_testbench",
    )
    .opt_level(OptLevel::O2)
    .run_test()
    .unwrap();

    assert_eq!(result, TestResult::Pass);
}

#[test]
fn test_dse_preserve_all_ports() {
    // With PreserveAllPorts, both top-level AND sub-instance ports survive DSE.
    let mut sim = Simulator::builder(DSE_HIERARCHY_SOURCE, "Top")
        .dead_store_policy(DeadStorePolicy::PreserveAllPorts)
        .build()
        .unwrap();
    let top_in = sim.signal("top_in");
    let top_out = sim.signal("top_out");

    sim.modify(|io| io.set(top_in, 0x42u8)).unwrap();
    assert_eq!(sim.get(top_out), 0x42u64.into());

    // Sub-instance ports should also be accessible and correct
    let sub_signals = sim.instance_signals(&[("u_sub", 0)]);
    let sub_o_data = sub_signals.iter().find(|s| s.name == "o_data").unwrap();
    assert_eq!(sim.get(sub_o_data.signal), 0x42u64.into());
}
