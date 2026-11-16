use celox::{BigUint, RuntimeErrorCode, Simulator};

#[path = "test_utils/mod.rs"]
#[macro_use]
#[allow(unused_macros)]
mod test_utils;

all_backends! {

fn test_i32_xor_step_with_only_high_bits_reports_true_loop(sim) {
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

fn test_runtime_reverse_i32_step_truncation_reports_true_loop(sim) {
    // Veryl does not reject a constant step that is 0 in the 32-bit counter, and its simulator
    // hangs.
    @ignore_on(veryl);
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

fn test_runtime_bounds_stalled_step_with_break_guard_false_reports_true_loop(sim) {
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

fn test_runtime_bounds_stalled_step_reports_true_loop(sim) {
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
