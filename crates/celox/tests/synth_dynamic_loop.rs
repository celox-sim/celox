use celox::{BigUint, RuntimeErrorCode, Simulator};

#[path = "test_utils/mod.rs"]
#[macro_use]
#[allow(unused_macros)]
mod test_utils;

all_backends! {

fn test_expression_bounds_in_synth_for_loops(sim) {
    @ignore_on(sv);
    @case "synth_dynamic_loop::test_expression_bounds_in_synth_for_loops";
}

fn test_constant_break_in_synth_comb_loop(sim) {
    @ignore_on(sv);
    @case "synth_dynamic_loop::test_constant_break_in_synth_comb_loop";
}


#[ignore]
fn test_constant_signed_bounds_in_unrolled_synth_loops(sim) {
    @case "synth_dynamic_loop::test_constant_signed_bounds_in_unrolled_synth_loops";
}

fn test_runtime_bounds_in_synth_for_loops(sim) {
    @ignore_on(sv);
    @case "synth_dynamic_loop::test_runtime_bounds_in_synth_for_loops";
}

fn test_runtime_bitwise_steps_in_synth_for_loops(sim) {
    @ignore_on(sv);
    @case "synth_dynamic_loop::test_runtime_bitwise_steps_in_synth_for_loops";
}

fn test_signed_xor_step_uses_loop_counter_width(sim) {
    @ignore_on(veryl, sv);
    @case "synth_dynamic_loop::test_signed_xor_step_uses_loop_counter_width";
}

fn test_i32_bitwise_steps_discard_bits_above_the_counter_width(sim) {
    @ignore_on(veryl, sv);
    @case "synth_dynamic_loop::test_i32_bitwise_steps_discard_bits_above_the_counter_width";
}

fn test_i32_xor_step_with_only_high_bits_reports_true_loop(sim) {
    @ignore_on(veryl, sv);
    @setup { let code = r#"
        module Top (
            start: input logic<32>,
            end_bound: input logic<32>,
            last: output logic<32>
        ) {
            always_comb {
                last = 0;
                for i in start..end_bound step ^= 4294967296 {
                    last = i;
                }
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");

    let start = sim.signal("start");
    let end_bound = sim.signal("end_bound");
    sim.set(start, 3u32);
    sim.set(end_bound, 4u32);
    assert_eq!(sim.eval_comb().unwrap_err(), RuntimeErrorCode::DetectedTrueLoop);
}

fn test_i32_or_step_with_only_existing_low_bits_reports_true_loop(sim) {
    @ignore_on(veryl, sv);
    @setup { let code = r#"
        module Top (
            start: input logic<32>,
            end_bound: input logic<32>,
            last: output logic<32>
        ) {
            always_comb {
                last = 0;
                for i in start..end_bound step |= 4294967299 {
                    last = i;
                }
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");

    let start = sim.signal("start");
    let end_bound = sim.signal("end_bound");
    sim.set(start, 3u32);
    sim.set(end_bound, 4u32);
    assert_eq!(sim.eval_comb().unwrap_err(), RuntimeErrorCode::DetectedTrueLoop);
}

fn test_i32_mul_step_overflow_reports_true_loop(sim) {
    @ignore_on(veryl, sv);
    @setup { let code = r#"
        module Top (
            start: input signed logic<32>,
            end_bound: input signed logic<64>,
            hits: output logic<32>
        ) {
            always_comb {
                hits = 0;
                for i in start..end_bound step *= 2 {
                    hits += 1;
                }
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");

    let start = sim.signal("start");
    let end_bound = sim.signal("end_bound");
    sim.set(start, 1_500_000_000u32);
    sim.set(end_bound, 3_100_000_000u64);
    assert_eq!(sim.eval_comb().unwrap_err(), RuntimeErrorCode::DetectedTrueLoop);
}

fn test_i32_shl_step_overflow_reports_true_loop(sim) {
    @ignore_on(veryl, sv);
    @setup { let code = r#"
        module Top (
            start: input signed logic<32>,
            end_bound: input signed logic<64>,
            hits: output logic<32>
        ) {
            always_comb {
                hits = 0;
                for i in start..end_bound step <<= 1 {
                    hits += 1;
                }
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");

    let start = sim.signal("start");
    let end_bound = sim.signal("end_bound");
    sim.set(start, 1_073_741_824u32);
    sim.set(end_bound, 2_147_483_649u64);
    assert_eq!(sim.eval_comb().unwrap_err(), RuntimeErrorCode::DetectedTrueLoop);
}

fn test_runtime_bounds_terminal_inclusive_mul_loop_reports_true_loop(sim) {
    @ignore_on(sv);
    @setup { let code = r#"
        module Top (
            count: input logic<32>,
            hits: output logic<32>
        ) {
            always_comb {
                hits = 0;
                for i in 0..=count step *= 2 {
                    hits += 1;
                }
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");

    let count = sim.signal("count");
    sim.set(count, 0u32);
    assert_eq!(sim.eval_comb().unwrap_err(), RuntimeErrorCode::DetectedTrueLoop);
}

fn test_runtime_reverse_step_matches_emitted_sv_order(sim) {
    @ignore_on(sv);
    @case "synth_dynamic_loop::test_runtime_reverse_step_matches_emitted_sv_order";
}

fn test_runtime_reverse_i32_step_truncation_reports_true_loop(sim) {
    @ignore_on(veryl, sv);
    @setup { let code = r#"
        module Top (
            start: input signed logic<64>,
            end_bound: input signed logic<64>,
            hits: output logic<32>
        ) {
            always_comb {
                hits = 0;
                for i in rev start..=end_bound step += 4294967296 {
                    hits += 1;
                }
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");

    let start = sim.signal("start");
    let end_bound = sim.signal("end_bound");
    sim.set(start, 0u64);
    sim.set(end_bound, 3u64);
    assert_eq!(sim.eval_comb().unwrap_err(), RuntimeErrorCode::DetectedTrueLoop);
}

fn test_runtime_break_in_synth_comb_loop(sim) {
    @ignore_on(sv);
    @case "synth_dynamic_loop::test_runtime_break_in_synth_comb_loop";
}

fn test_runtime_break_after_assign_in_synth_comb_loop(sim) {
    @ignore_on(sv);
    @case "synth_dynamic_loop::test_runtime_break_after_assign_in_synth_comb_loop";
}

fn test_runtime_if_without_break_in_synth_comb_loop(sim) {
    @ignore_on(sv);
    @case "synth_dynamic_loop::test_runtime_if_without_break_in_synth_comb_loop";
}

fn test_runtime_bounds_stalled_step_with_break_exits_cleanly(sim) {
    @ignore_on(sv);
    @case "synth_dynamic_loop::test_runtime_bounds_stalled_step_with_break_exits_cleanly";
}

fn test_runtime_bounds_stalled_step_with_break_guard_false_reports_true_loop(sim) {
    @ignore_on(sv);
    @setup { let code = r#"
        module Top (
            start: input logic<32>,
            count: input logic<32>,
            sel: input logic,
            out: output logic<32>
        ) {
            always_comb {
                out = 0;
                for i in start..count step *= 2 {
                    out += 1;
                    if sel {
                        break;
                    }
                }
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");

    let start = sim.signal("start");
    let count = sim.signal("count");
    let sel = sim.signal("sel");

    sim.set(start, 0u32);
    sim.set(count, 4u32);
    sim.set(sel, 0u8);
    assert_eq!(sim.eval_comb().unwrap_err(), RuntimeErrorCode::DetectedTrueLoop);
}

fn test_runtime_bounds_signed_inclusive_range_preserves_negative_bounds(sim) {
    @ignore_on(veryl, sv);
    @case "synth_dynamic_loop::test_runtime_bounds_signed_inclusive_range_preserves_negative_bounds";
}

fn test_runtime_bounds_truncate_loop_var_to_declared_width(sim) {
    @ignore_on(sv);
    @case "synth_dynamic_loop::test_runtime_bounds_truncate_loop_var_to_declared_width";
}

fn test_constant_bounds_preserve_wide_limit_above_loop_width(sim) {
    @ignore_on(sv);
    @case "synth_dynamic_loop::test_constant_bounds_preserve_wide_limit_above_loop_width";
}

fn test_runtime_bounds_track_initial_seed_dependency(sim) {
    @ignore_on(sv);
    @case "synth_dynamic_loop::test_runtime_bounds_track_initial_seed_dependency";
}

fn test_runtime_bounds_preserve_pre_loop_bits_for_partial_updates(sim) {
    @ignore_on(sv);
    @case "synth_dynamic_loop::test_runtime_bounds_preserve_pre_loop_bits_for_partial_updates";
}

fn test_runtime_bounds_reconstruct_wide_loop_carried_reads_from_partial_state(sim) {
    @ignore_on(sv);
    @case "synth_dynamic_loop::test_runtime_bounds_reconstruct_wide_loop_carried_reads_from_partial_state";
}

fn test_runtime_bounds_preserve_untouched_high_bits_for_dynamic_index_reads(sim) {
    @ignore_on(sv);
    @case "synth_dynamic_loop::test_runtime_bounds_preserve_untouched_high_bits_for_dynamic_index_reads";
}

fn test_runtime_bounds_reverse_singleton_exits_cleanly(sim) {
    @ignore_on(sv);
    @case "synth_dynamic_loop::test_runtime_bounds_reverse_singleton_exits_cleanly";
}

fn test_runtime_bounds_track_initial_seed_dependency_across_module_boundary(sim) {
    @ignore_on(sv);
    @case "synth_dynamic_loop::test_runtime_bounds_track_initial_seed_dependency_across_module_boundary";
}

fn test_runtime_break_condition_dependency_across_module_boundary(sim) {
    @ignore_on(sv);
    @case "synth_dynamic_loop::test_runtime_break_condition_dependency_across_module_boundary";
}

fn test_runtime_bounds_stalled_step_reports_true_loop(sim) {
    @ignore_on(sv);
    @setup { let code = r#"
        module Top (
            start: input logic<32>,
            count: input logic<32>,
            out: output logic<32>
        ) {
            always_comb {
                out = 0;
                for i in start..count step *= 2 {
                    out += 1;
                }
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");

    let start = sim.signal("start");
    let count = sim.signal("count");

    sim.set(start, 0u32);
    sim.set(count, 4u32);
    assert_eq!(sim.eval_comb().unwrap_err(), RuntimeErrorCode::DetectedTrueLoop);
}

fn test_runtime_bounds_preserve_loop_carried_state_for_indexed_reads(sim) {
    @ignore_on(sv);
    @case "synth_dynamic_loop::test_runtime_bounds_preserve_loop_carried_state_for_indexed_reads";
}

fn test_runtime_bounds_forward_overshoot_exits_without_wraparound(sim) {
    @ignore_on(sv);
    @case "synth_dynamic_loop::test_runtime_bounds_forward_overshoot_exits_without_wraparound";
}

fn test_runtime_bounds_large_additive_step_exits_without_wraparound(sim) {
    @ignore_on(sv);
    @case "synth_dynamic_loop::test_runtime_bounds_large_additive_step_exits_without_wraparound";
}

fn test_runtime_bounds_inclusive_max_bound_runs_full_range(sim) {
    @ignore_on(sv);
    @case "synth_dynamic_loop::test_runtime_bounds_inclusive_max_bound_runs_full_range";
}

}

#[test]
fn test_runtime_bounds_wide_dynamic_bound_is_still_allowed() {
    let code = r#"
        module Top (
            bound: input logic<128>,
            hits: output logic<32>,
            last: output logic<64>
        ) {
            always_comb {
                hits = 0;
                last = 64'hffff_ffff_ffff_ffff;
                for i in (bound - 1) .. bound {
                    hits += 1;
                    last = i;
                }
            }
        }
    "#;

    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let bound = sim.signal("bound");
    let hits = sim.signal("hits");
    let last = sim.signal("last");

    sim.modify(|io| io.set_wide(bound, BigUint::from(2u32)))
        .unwrap();
    sim.eval_comb().unwrap();
    assert_eq!(sim.get(hits), 1u32.into());
    assert_eq!(sim.get(last), 1u32.into());
}
