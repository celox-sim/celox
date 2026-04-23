use celox::{RuntimeErrorCode, Simulator};

#[path = "test_utils/mod.rs"]
#[macro_use]
#[allow(unused_macros)]
mod test_utils;

all_backends! {

fn test_expression_bounds_in_synth_for_loops(sim) {
    @setup { let code = r#"
        module Top #(
            param LIMIT: u32 = 4,
        ) (
            sum_fwd: output logic<32>,
            sum_rev: output logic<32>,
            sum_inc: output logic<32>,
            sum_step: output logic<32>,
        ) {
            always_comb {
                sum_fwd = 0;
                for i: u32 in 0..(LIMIT + 1) {
                    sum_fwd += i;
                }

                sum_rev = 0;
                for i: i32 in rev 0..LIMIT {
                    sum_rev = sum_rev * 10 + i as 32;
                }

                sum_inc = 0;
                for i: u32 in 0..=LIMIT {
                    sum_inc += i;
                }

                sum_step = 0;
                for i: u32 in 1..(LIMIT + 4) step *= 2 {
                    sum_step += i;
                }
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    sim.eval_comb().unwrap();

    let sum_fwd = sim.signal("sum_fwd");
    let sum_rev = sim.signal("sum_rev");
    let sum_inc = sim.signal("sum_inc");
    let sum_step = sim.signal("sum_step");

    assert_eq!(sim.get(sum_fwd), 10u32.into());
    assert_eq!(sim.get(sum_rev), 3210u32.into());
    assert_eq!(sim.get(sum_inc), 10u32.into());
    assert_eq!(sim.get(sum_step), 7u32.into());
}

fn test_constant_break_in_synth_comb_loop(sim) {
    @setup { let code = r#"
        module Top (
            sum: output logic<32>,
        ) {
            always_comb {
                sum = 0;
                for i: u32 in 0..8 {
                    if i == 3 {
                        break;
                    }
                    sum += i;
                }
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");

    let sum = sim.signal("sum");
    sim.eval_comb().unwrap();
    assert_eq!(sim.get(sum), 3u32.into());
}

#[ignore]
fn test_constant_signed_bounds_in_unrolled_synth_loops(sim) {
    // Constant signed reverse bounds are currently broken in the upstream
    // Veryl analyzer unroller, so this regression is parked until deps/veryl
    // is fixed.
    @setup { let code = r#"
        module Top (
            sum_fwd: output logic<32>,
            sum_rev: output logic<32>
        ) {
            always_comb {
                sum_fwd = 0;
                for i: i32 in (0 - 1)..=1 {
                    sum_fwd += i as 32;
                }

                sum_rev = 0;
                for i: i32 in rev (0 - 1)..=1 {
                    sum_rev = sum_rev * 10 + (i + 1) as 32;
                }
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let sum_fwd = sim.signal("sum_fwd");
    let sum_rev = sim.signal("sum_rev");

    sim.eval_comb().unwrap();
    assert_eq!(sim.get(sum_fwd), 0u32.into());
    assert_eq!(sim.get(sum_rev), 210u32.into());
}

fn test_runtime_bounds_in_synth_for_loops(sim) {
    @setup { let code = r#"
        module Top (
            count: input logic<32>,
            sum_fwd: output logic<32>,
            sum_rev: output logic<32>,
            sum_inc: output logic<32>,
            sum_step: output logic<32>,
        ) {
            always_comb {
                sum_fwd = 0;
                for i: u32 in 0..count {
                    sum_fwd += i;
                }

                sum_rev = 0;
                for i: i32 in rev 0..count {
                    sum_rev = sum_rev * 10 + i as 32;
                }

                sum_inc = 0;
                for i: u32 in 0..=count {
                    sum_inc += i;
                }

                sum_step = 0;
                for i: u32 in 1..(count + 4) step *= 2 {
                    sum_step += i;
                }
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");

    let count = sim.signal("count");
    let sum_fwd = sim.signal("sum_fwd");
    let sum_rev = sim.signal("sum_rev");
    let sum_inc = sim.signal("sum_inc");
    let sum_step = sim.signal("sum_step");

    sim.set(count, 4u32);
    sim.eval_comb().unwrap();
    assert_eq!(sim.get(sum_fwd), 6u32.into());
    assert_eq!(sim.get(sum_rev), 3210u32.into());
    assert_eq!(sim.get(sum_inc), 10u32.into());
    assert_eq!(sim.get(sum_step), 7u32.into());

    sim.set(count, 5u32);
    sim.eval_comb().unwrap();
    assert_eq!(sim.get(sum_fwd), 10u32.into());
    assert_eq!(sim.get(sum_rev), 43210u32.into());
    assert_eq!(sim.get(sum_inc), 15u32.into());
    assert_eq!(sim.get(sum_step), 15u32.into());
}

fn test_runtime_bounds_terminal_inclusive_mul_loop_exits_cleanly(sim) {
    @ignore_on(veryl);
    @setup { let code = r#"
        module Top (
            count: input logic<32>,
            hits: output logic<32>
        ) {
            always_comb {
                hits = 0;
                for i: u32 in 0..=count step *= 2 {
                    hits += 1;
                }
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");

    let count = sim.signal("count");
    let hits = sim.signal("hits");

    sim.set(count, 0u32);
    sim.eval_comb().unwrap();
    assert_eq!(sim.get(hits), 1u32.into());
}

fn test_runtime_break_in_synth_comb_loop(sim) {
    @setup { let code = r#"
        module Top (
            count: input logic<32>,
            sum: output logic<32>
        ) {
            always_comb {
                sum = 0;
                for i: u32 in 0..count {
                    if i == 3 {
                        break;
                    }
                    sum += i;
                }
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");

    let count = sim.signal("count");
    let sum = sim.signal("sum");

    sim.set(count, 8u32);
    sim.eval_comb().unwrap();
    assert_eq!(sim.get(sum), 3u32.into());
}

fn test_runtime_break_after_assign_in_synth_comb_loop(sim) {
    @setup { let code = r#"
        module Top (
            count: input logic<32>,
            sum: output logic<32>
        ) {
            always_comb {
                sum = 0;
                for i: u32 in 0..count {
                    if i == 2 {
                        sum += 10;
                        break;
                    }
                    sum += 1;
                }
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");

    let count = sim.signal("count");
    let sum = sim.signal("sum");

    sim.set(count, 8u32);
    sim.eval_comb().unwrap();
    assert_eq!(sim.get(sum), 12u32.into());
}

fn test_runtime_if_without_break_in_synth_comb_loop(sim) {
    @setup { let code = r#"
        module Top (
            count: input logic<32>,
            sel: input logic,
            o: output logic<32>
        ) {
            always_comb {
                o = 0;
                for i: u32 in 0..count {
                    if sel {
                        o += 1;
                    }
                }
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");

    let count = sim.signal("count");
    let sel = sim.signal("sel");
    let o = sim.signal("o");

    sim.set(count, 5u32);
    sim.set(sel, 1u8);
    sim.eval_comb().unwrap();
    assert_eq!(sim.get(o), 5u32.into());
}

fn test_runtime_bounds_stalled_step_with_break_exits_cleanly(sim) {
    @ignore_on(veryl);
    @setup { let code = r#"
        module Top (
            start: input logic<32>,
            count: input logic<32>,
            out: output logic<32>
        ) {
            always_comb {
                out = 0;
                for i: u32 in start..count step *= 2 {
                    out += 1;
                    if out == 3 {
                        break;
                    }
                }
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");

    let start = sim.signal("start");
    let count = sim.signal("count");
    let out = sim.signal("out");

    sim.set(start, 0u32);
    sim.set(count, 4u32);
    sim.eval_comb().unwrap();
    assert_eq!(sim.get(out), 1u32.into());
}

fn test_runtime_bounds_reverse_stalled_step_with_break_exits_cleanly(sim) {
    @ignore_on(veryl);
    @setup { let code = r#"
        module Top (
            start: input logic<32>,
            out: output logic<32>
        ) {
            always_comb {
                out = 0;
                for i: u32 in rev start..4 step += 0 {
                    out += 1;
                    if out == 2 {
                        break;
                    }
                }
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");

    let start = sim.signal("start");
    let out = sim.signal("out");

    sim.set(start, 0u32);
    sim.eval_comb().unwrap();
    assert_eq!(sim.get(out), 1u32.into());
}

fn test_runtime_bounds_signed_inclusive_range_preserves_negative_bounds(sim) {
    @ignore_on(veryl);
    @setup { let code = r#"
        module Top (
            start: input logic<32>,
            count: input logic<32>,
            hits: output logic<32>,
            sum: output logic<32>
        ) {
            always_comb {
                hits = 0;
                sum = 0;
                for i: i32 in start..=count {
                    hits += 1;
                    sum += i as 32;
                }
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");

    let start = sim.signal("start");
    let count = sim.signal("count");
    let hits = sim.signal("hits");
    let sum = sim.signal("sum");

    sim.set(start, 0xffff_ffffu32);
    sim.set(count, 1u32);
    sim.eval_comb().unwrap();
    assert_eq!(sim.get(hits), 3u32.into());
    assert_eq!(sim.get(sum), 0u32.into());
}

fn test_runtime_bounds_truncate_loop_var_to_declared_width(sim) {
    @ignore_on(veryl);
    @setup { let code = r#"
        module Top (
            count: input logic<32>,
            wrapped_hits: output logic<32>
        ) {
            always_comb {
                wrapped_hits = 0;
                for i: u8 in 254..count {
                    if i <: 8'd4 {
                        wrapped_hits += 1;
                    }
                }
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");

    let count = sim.signal("count");
    let wrapped_hits = sim.signal("wrapped_hits");

    sim.set(count, 260u32);
    sim.eval_comb().unwrap();
    assert_eq!(sim.get(wrapped_hits), 4u32.into());
}

fn test_constant_bounds_preserve_wide_limit_above_loop_width(sim) {
    @ignore_on(veryl);
    @setup { let code = r#"
        module Top (
            start: input logic<32>,
            wrapped_hits: output logic<32>
        ) {
            always_comb {
                wrapped_hits = 0;
                for i: u8 in start..260 {
                    if i <: 8'd4 {
                        wrapped_hits += 1;
                    }
                }
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");

    let start = sim.signal("start");
    let wrapped_hits = sim.signal("wrapped_hits");

    sim.set(start, 254u32);
    sim.eval_comb().unwrap();
    assert_eq!(sim.get(wrapped_hits), 4u32.into());
}

fn test_runtime_bounds_track_initial_seed_dependency(sim) {
    @setup { let code = r#"
        module Top (
            seed: input logic<32>,
            count: input logic<32>,
            out: output logic<32>
        ) {
            var acc: logic<32>;
            always_comb {
                acc = seed;
                for i: u32 in 0..count {
                    acc += 1;
                }
                out = acc;
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");

    let seed = sim.signal("seed");
    let count = sim.signal("count");
    let out = sim.signal("out");

    sim.set(seed, 10u32);
    sim.set(count, 3u32);
    sim.eval_comb().unwrap();
    assert_eq!(sim.get(out), 13u32.into());

    sim.set(seed, 20u32);
    sim.eval_comb().unwrap();
    assert_eq!(sim.get(out), 23u32.into());
}

fn test_runtime_bounds_preserve_pre_loop_bits_for_partial_updates(sim) {
    @ignore_on(veryl);
    @setup { let code = r#"
        module Top (
            seed: input logic<2>,
            count: input logic<32>,
            out: output logic<2>
        ) {
            var x: logic<2>;
            always_comb {
                x = seed;
                for i: u32 in 0..count {
                    x[0] = x[1];
                }
                out = x;
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");

    let seed = sim.signal("seed");
    let count = sim.signal("count");
    let out = sim.signal("out");

    sim.set(seed, 2u8);
    sim.set(count, 1u32);
    sim.eval_comb().unwrap();
    assert_eq!(sim.get(out), 3u32.into());
}

fn test_runtime_bounds_reconstruct_wide_loop_carried_reads_from_partial_state(sim) {
    @ignore_on(veryl);
    @setup { let code = r#"
        module Top (
            seed: input logic<2>,
            count: input logic<32>,
            out: output logic<2>
        ) {
            var x: logic<2>;
            always_comb {
                x = seed;
                for i: u32 in 0..count {
                    x[0] = x == 2'b10;
                }
                out = x;
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");

    let seed = sim.signal("seed");
    let count = sim.signal("count");
    let out = sim.signal("out");

    sim.set(seed, 2u8);
    sim.set(count, 2u32);
    sim.eval_comb().unwrap();
    assert_eq!(sim.get(out), 2u32.into());
}

fn test_runtime_bounds_preserve_untouched_high_bits_for_dynamic_index_reads(sim) {
    @ignore_on(veryl);
    @setup { let code = r#"
        module Top (
            seed: input logic<2>,
            idx: input logic<32>,
            count: input logic<32>,
            out: output logic
        ) {
            var x: logic<2>;
            var y: logic;
            always_comb {
                x = seed;
                y = 0;
                for i: u32 in 0..count {
                    x[0] = 0;
                    y = x[idx];
                }
                out = y;
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");

    let seed = sim.signal("seed");
    let idx = sim.signal("idx");
    let count = sim.signal("count");
    let out = sim.signal("out");

    sim.set(seed, 2u8);
    sim.set(idx, 1u32);
    sim.set(count, 1u32);
    sim.eval_comb().unwrap();
    assert_eq!(sim.get(out), 1u32.into());
}

fn test_runtime_bounds_reverse_zero_step_singleton_exits_cleanly(sim) {
    @ignore_on(veryl);
    @setup { let code = r#"
        module Top (
            start: input logic<32>,
            count: input logic<32>,
            out: output logic<32>
        ) {
            always_comb {
                out = 0;
                for i: u32 in rev start..=count step += 0 {
                    out = i;
                }
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");

    let start = sim.signal("start");
    let count = sim.signal("count");
    let out = sim.signal("out");

    sim.set(start, 4u32);
    sim.set(count, 4u32);
    sim.eval_comb().unwrap();
    assert_eq!(sim.get(out), 4u32.into());
}

fn test_runtime_bounds_track_initial_seed_dependency_across_module_boundary(sim) {
    @setup { let code = r#"
        module Child (
            seed: input logic<32>,
            count: input logic<32>,
            out: output logic<32>
        ) {
            var acc: logic<32>;
            always_comb {
                acc = seed;
                for i: u32 in 0..count {
                    acc += 1;
                }
                out = acc;
            }
        }

        module Top (
            seed: input logic<32>,
            count: input logic<32>,
            out: output logic<32>
        ) {
            var child_out: logic<32>;
            inst u_child: Child (
                seed: seed,
                count: count,
                out: child_out
            );
            assign out = child_out;
        }
    "#; }
    @build Simulator::builder(code, "Top");

    let seed = sim.signal("seed");
    let count = sim.signal("count");
    let out = sim.signal("out");

    sim.set(seed, 10u32);
    sim.set(count, 3u32);
    sim.eval_comb().unwrap();
    assert_eq!(sim.get(out), 13u32.into());

    sim.set(seed, 20u32);
    sim.eval_comb().unwrap();
    assert_eq!(sim.get(out), 23u32.into());
}

fn test_runtime_bounds_stalled_step_reports_true_loop(sim) {
    @ignore_on(veryl);
    @setup { let code = r#"
        module Top (
            start: input logic<32>,
            count: input logic<32>,
            out: output logic<32>
        ) {
            always_comb {
                out = 0;
                for i: u32 in start..count step *= 2 {
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

fn test_runtime_bounds_reverse_stalled_step_reports_true_loop(sim) {
    @ignore_on(veryl);
    @setup { let code = r#"
        module Top (
            start: input logic<32>,
            out: output logic<32>
        ) {
            always_comb {
                out = 0;
                for i: u32 in rev start..4 step += 0 {
                    out += 1;
                }
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");

    let start = sim.signal("start");
    sim.set(start, 0u32);
    assert_eq!(sim.eval_comb().unwrap_err(), RuntimeErrorCode::DetectedTrueLoop);
}

fn test_runtime_bounds_preserve_loop_carried_state_for_indexed_reads(sim) {
    @ignore_on(veryl);
    @setup { let code = r#"
        module Top (
            count: input logic<32>,
            out: output logic<3>
        ) {
            var x: logic<3>;
            always_comb {
                x = 3'b100;
                for i: u32 in 0..count {
                    x[i + 1] = x[i];
                }
                out = x;
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");

    let count = sim.signal("count");
    let out = sim.signal("out");

    sim.set(count, 2u32);
    sim.eval_comb().unwrap();
    assert_eq!(sim.get(out), 0u32.into());
}

fn test_runtime_bounds_forward_overshoot_exits_without_wraparound(sim) {
    @ignore_on(veryl);
    @setup { let code = r#"
        module Top (
            start: input logic<32>,
            hits: output logic<32>,
            last: output logic<8>
        ) {
            always_comb {
                hits = 0;
                last = 8'hee;
                for i: u8 in start..255 step += 10 {
                    hits += 1;
                    last = i;
                }
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");

    let start = sim.signal("start");
    let hits = sim.signal("hits");
    let last = sim.signal("last");

    sim.set(start, 250u32);
    sim.eval_comb().unwrap();
    assert_eq!(sim.get(hits), 1u32.into());
    assert_eq!(sim.get(last), 250u32.into());
}

fn test_runtime_bounds_large_additive_step_exits_without_wraparound(sim) {
    @ignore_on(veryl);
    @setup { let code = r#"
        module Top (
            start: input logic<32>,
            hits: output logic<32>,
            last: output logic<8>
        ) {
            always_comb {
                hits = 0;
                last = 8'hee;
                for i: u8 in start..255 step += 300 {
                    hits += 1;
                    last = i;
                }
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");

    let start = sim.signal("start");
    let hits = sim.signal("hits");
    let last = sim.signal("last");

    sim.set(start, 250u32);
    sim.eval_comb().unwrap();
    assert_eq!(sim.get(hits), 1u32.into());
    assert_eq!(sim.get(last), 250u32.into());
}

fn test_runtime_bounds_inclusive_max_bound_runs_full_range(sim) {
    @ignore_on(veryl);
    @setup { let code = r#"
        module Top (
            count: input logic<8>,
            hits: output logic<32>
        ) {
            always_comb {
                hits = 0;
                for i: u8 in 0..=count {
                    hits += 1;
                }
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");

    let count = sim.signal("count");
    let hits = sim.signal("hits");

    sim.set(count, 255u8);
    sim.eval_comb().unwrap();
    assert_eq!(sim.get(hits), 256u32.into());
}

}
