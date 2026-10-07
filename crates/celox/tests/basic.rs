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
        @case "basic::test_comb_override_dependency";
    }

    fn test_always_comb_read_before_write_uses_previous_value(sim) {
        @case "basic::test_always_comb_read_before_write_uses_previous_value";
    }

    fn test_comb_function_call_early_return(sim) {
        @case "basic::test_comb_function_call_early_return";
    }

    fn test_comb_function_call_return_indexed_local_temp(sim) {
        @case "basic::test_comb_function_call_return_indexed_local_temp";
    }

    fn test_comb_function_call_partial_write_local_temp(sim) {
        @case "basic::test_comb_function_call_partial_write_local_temp";
    }

    fn test_comb_function_call_local_and_return_width_coercion(sim) {
        @case "basic::test_comb_function_call_local_and_return_width_coercion";
    }

    fn test_comb_function_call_constant_folded_if_return(sim) {
        @case "basic::test_comb_function_call_constant_folded_if_return";
    }

    fn test_comb_function_call_return_inside_for(sim) {
        @case "basic::test_comb_function_call_return_inside_for";
    }

    fn test_comb_function_call_break_inside_for(sim) {
        @case "basic::test_comb_function_call_break_inside_for";
    }

    fn test_comb_function_call_break_inside_dynamic_for(sim) {
        @build Simulator::builder(r#"
module Top (
    count: input logic<3>,
    d: input logic<4>,
    q: output logic<8>,
) {
    function f (
        n: input logic<3>,
        x: input logic<4>,
    ) -> logic<8> {
        var tmp: logic<8>;
        tmp = 8'd0;
        for i in 0..n {
            if x[i] {
                tmp = i + 8'd1;
                break;
            }
        }
        return tmp;
    }

    always_comb {
        q = f(count, d);
    }
}
"#, "Top");
        let count = sim.signal("count");
        let d = sim.signal("d");
        let q = sim.signal("q");

        for n in 0..=4u8 {
            for x in 0..16u8 {
                sim.modify(|io| {
                    io.set(count, n);
                    io.set(d, x);
                }).unwrap();
                let expected = (0..n).find(|i| x & (1 << i) != 0).map_or(0, |i| i + 1);
                assert_eq!(sim.get(q), expected.into(), "count={n}, d={x}");
            }
        }
    }

    fn test_comb_function_call_nested_break_inside_dynamic_for(sim) {
        @build Simulator::builder(r#"
module Top (
    count: input logic<3>,
    d: input logic<4>,
    q: output logic<8>,
) {
    function f (
        n: input logic<3>,
        x: input logic<4>,
    ) -> logic<8> {
        var tmp: logic<8>;
        tmp = 8'd0;
        for i in 0..n {
            for j in 0..4 {
                if x[j] {
                    break;
                }
                tmp = tmp + 8'd1;
            }
            tmp = tmp + 8'd1;
        }
        return tmp;
    }

    always_comb {
        q = f(count, d);
    }
}
"#, "Top");
        let count = sim.signal("count");
        let d = sim.signal("d");
        let q = sim.signal("q");

        for n in 0..=4u8 {
            for x in 0..16u8 {
                sim.modify(|io| {
                    io.set(count, n);
                    io.set(d, x);
                }).unwrap();
                let inner_count = (0..4u8).find(|j| x & (1 << j) != 0).unwrap_or(4);
                assert_eq!(sim.get(q), (n * (inner_count + 1)).into(), "count={n}, d={x}");
            }
        }
    }

    fn test_comb_function_call_nested_helper(sim) {
        @case "basic::test_comb_function_call_nested_helper";
    }

    fn test_comb_function_call_statement_with_output_argument(sim) {
        @case "basic::test_comb_function_call_statement_with_output_argument";
    }

    fn test_comb_function_call_expression_with_output_argument(sim) {
        @case "basic::test_comb_function_call_expression_with_output_argument";
    }

    fn test_comb_function_call_expression_output_is_visible_to_later_operand(sim) {
        @case "basic::test_comb_function_call_expression_output_is_visible_to_later_operand";
    }

    fn test_comb_function_call_expression_with_constant_input_keeps_output_write(sim) {
        @case "basic::test_comb_function_call_expression_with_constant_input_keeps_output_write";
    }

    fn test_comb_function_call_expression_output_survives_system_function_wrapper(sim) {
        @case "basic::test_comb_function_call_expression_output_survives_system_function_wrapper";
    }

    fn test_comb_function_call_with_output_argument_in_display_argument(sim) {
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
        @ignore_on(veryl);
        @case "basic::test_comb_function_call_expression_output_is_guarded_by_ternary";
    }

    fn test_comb_function_call_expression_output_respects_short_circuit(sim) {
        @ignore_on(veryl);
        @case "basic::test_comb_function_call_expression_output_respects_short_circuit";
    }

    fn test_comb_function_call_with_output_argument_in_if_condition(sim) {
        @case "basic::test_comb_function_call_with_output_argument_in_if_condition";
    }

    fn test_comb_function_call_with_output_argument_in_case_target(sim) {
        @case "basic::test_comb_function_call_with_output_argument_in_case_target";
    }

    fn test_comb_function_call_with_output_argument_in_loop_condition(sim) {
        @case "basic::test_comb_function_call_with_output_argument_in_loop_condition";
    }

    fn test_comb_nested_function_output_call_in_function_condition(sim) {
        @ignore_on(veryl);
        @case "basic::test_comb_nested_function_output_call_in_function_condition";
    }

    fn test_comb_case_target_output_call_is_evaluated_once(sim) {
        @ignore_on(veryl);
        @case "basic::test_comb_case_target_output_call_is_evaluated_once";
    }

    fn test_comb_loop_bound_output_call_writes_back_once(sim) {
        @case "basic::test_comb_loop_bound_output_call_writes_back_once";
    }

    fn test_comb_value_system_function_statement_applies_argument_outputs(sim) {
        @ignore_on(veryl);
        @case "basic::test_comb_value_system_function_statement_applies_argument_outputs";
    }

    fn test_comb_value_system_function_after_dynamic_break_stays_inactive(sim) {
        @ignore_on(veryl);
        @case "basic::test_comb_value_system_function_after_dynamic_break_stays_inactive";
    }

    fn test_comb_effectful_if_condition_after_dynamic_break_stays_inactive(sim) {
        @case "basic::test_comb_effectful_if_condition_after_dynamic_break_stays_inactive";
    }

    fn test_statement_call_inputs_follow_output_writeback_order(sim) {
        @case "basic::test_statement_call_inputs_follow_output_writeback_order";
    }

    fn test_comb_effectful_case_after_dynamic_break_stays_inactive(sim) {
        @ignore_on(veryl);
        @case "basic::test_comb_effectful_case_after_dynamic_break_stays_inactive";
    }

    fn test_comb_function_condition_output_is_guarded_after_early_return(sim) {
        @ignore_on(veryl);
        @case "basic::test_comb_function_condition_output_is_guarded_after_early_return";
    }

    fn test_comb_function_call_statement_ignores_return_value(sim) {
        @case "basic::test_comb_function_call_statement_ignores_return_value";
    }

    fn test_comb_returning_output_function_reads_current_caller_store(sim) {
        @case "basic::test_comb_returning_output_function_reads_current_caller_store";
    }

    fn test_comb_returning_output_function_allows_nested_output_call(sim) {
        @case "basic::test_comb_returning_output_function_allows_nested_output_call";
    }

    fn test_comb_function_call_statement_preserves_return_control_flow(sim) {
        @case "basic::test_comb_function_call_statement_preserves_return_control_flow";
    }

    fn test_comb_function_call_statement_preserves_conditional_return_control_flow(sim) {
        @case "basic::test_comb_function_call_statement_preserves_conditional_return_control_flow";
    }

    fn test_comb_nested_function_call_statement_with_output_argument(sim) {
        @case "basic::test_comb_nested_function_call_statement_with_output_argument";
    }

    fn test_comb_function_call_output_reads_current_caller_store(sim) {
        @case "basic::test_comb_function_call_output_reads_current_caller_store";
    }

    fn test_comb_function_call_statement_with_output_argument_in_loop(sim) {
        @case "basic::test_comb_function_call_statement_with_output_argument_in_loop";
    }

    fn test_comb_function_call_output_bit_select_preserves_unwritten_loop_bits(sim) {
        @case "basic::test_comb_function_call_output_bit_select_preserves_unwritten_loop_bits";
    }

    fn test_always_comb_blocking_assignment_chain(sim) {
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
fn test_dse_keeps_stores_read_through_identity_aliases() {
    // Each child reads its enable port only combinationally, so the port
    // shares the parent's wire as an identity alias and the parent never
    // loads that wire itself. Lane partitioning puts the parent's store and
    // the child's load into different units: dead-store elimination must
    // count the load of the alias as a read of the shared home.
    let code = r#"
module Counter (
    clk: input clock,
    i_en: input logic,
    i_mask: input logic,
    o_count: output logic<8>,
) {
    var count: logic<8>;
    let advance: logic = i_en & i_mask;
    always_ff {
        if advance {
            count = count + 8'd1;
        }
    }
    assign o_count = count;
}

module Top (
    clk: input clock,
    a: input logic<4>,
    b: input logic<4>,
    mask: input logic,
    c0: output logic<8>,
    c1: output logic<8>,
    c2: output logic<8>,
    c3: output logic<8>,
) {
    let en0: logic = a[0] ^ b[0];
    let en1: logic = a[1] ^ b[1];
    let en2: logic = a[2] ^ b[2];
    let en3: logic = a[3] ^ b[3];
    inst u0: Counter (clk, i_en: en0, i_mask: mask, o_count: c0);
    inst u1: Counter (clk, i_en: en1, i_mask: mask, o_count: c1);
    inst u2: Counter (clk, i_en: en2, i_mask: mask, o_count: c2);
    inst u3: Counter (clk, i_en: en3, i_mask: mask, o_count: c3);
}
"#;
    for threads in [1, 4] {
        let mut sim = Simulator::builder(code, "Top")
            .opt_level(OptLevel::O2)
            .threads(threads)
            .parallel_partition(celox::ParallelPartition::Always)
            .build()
            .unwrap();
        let clk = sim.event("clk");
        let a = sim.signal("a");
        let b = sim.signal("b");
        let mask = sim.signal("mask");
        let counts = ["c0", "c1", "c2", "c3"].map(|name| sim.signal(name));

        sim.modify(|io| {
            io.set(a, 0b1111u8);
            io.set(b, 0b0101u8);
            io.set(mask, 1u8);
        })
        .unwrap();
        for _ in 0..3 {
            sim.tick(clk).unwrap();
        }
        let observed = counts.map(|count| sim.get_as::<u8>(count));
        assert_eq!(observed, [0, 3, 0, 3], "threads={threads}");
    }
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
