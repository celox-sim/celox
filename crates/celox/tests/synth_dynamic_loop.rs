use celox::Simulator;

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
