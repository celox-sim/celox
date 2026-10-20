#![cfg(feature = "systemverilog")]
use std::path::Path;

macro_rules! scheduled_startup {
    ($module:ident, $backend:ident) => {
        mod $module {
            use super::*;

            #[test]
            fn time_zero_clock_arms_ff_without_reordering_initials_or_nba() {
                let source = r#"module Top(input bit clk,
                    output bit regular, host, initial_wait, before_nba,
                    output int first, second);
                  always_ff @(posedge clk) regular <= 1;
                  always_ff @(posedge clk) host <= $test$plusargs("ON");
                  initial begin first = $test$plusargs("ON"); #0 first = first + 2; end
                  initial begin second = first; #0 before_nba = host; end
                  initial begin @(posedge clk); initial_wait = 1; end
                endmodule"#;
                for periodic in [false, true] {
                    let mut sim = celox::Simulation::from_sv_sources(
                        vec![(source, Path::new("startup_clock.sv"))],
                        "Top",
                    )
                    .plusargs(["+ON"])
                    .$backend()
                    .unwrap();
                    if periodic {
                        sim.add_clock("clk", 2, 0);
                    } else {
                        sim.schedule("clk", 0, 1).unwrap();
                    }
                    sim.run_until(0).unwrap();
                    for name in ["regular", "host"] {
                        assert_eq!(sim.get(sim.signal(name)), 1u8.into(), "{name}");
                    }
                    assert_eq!(sim.get(sim.signal("first")), 3u8.into());
                    assert_eq!(sim.get(sim.signal("second")), 1u8.into());
                    assert_eq!(sim.get(sim.signal("before_nba")), 0u8.into());
                    assert_eq!(sim.get(sim.signal("initial_wait")), 0u8.into());
                    if !periodic {
                        sim.schedule("clk", 1, 0).unwrap();
                        sim.schedule("clk", 2, 1).unwrap();
                    }
                    sim.run_until(2).unwrap();
                    assert_eq!(sim.get(sim.signal("initial_wait")), 1u8.into());
                }
            }

            #[test]
            fn time_zero_unknown_to_reset_edge_matches_regular_ff() {
                let source = r#"module Top(input bit clk, input logic rst_n,
                    output bit regular, host);
                  initial begin regular = 1; host = 1; end
                  always_ff @(posedge clk or negedge rst_n)
                    if (!rst_n) regular <= 0; else regular <= 1;
                  always_ff @(posedge clk or negedge rst_n)
                    if (!rst_n) host <= 0; else host <= $test$plusargs("ON");
                endmodule"#;
                let mut sim = celox::Simulation::from_sv_sources(
                    vec![(source, Path::new("startup_reset.sv"))],
                    "Top",
                )
                .four_state(true)
                .plusargs(["+ON"])
                .$backend()
                .unwrap();
                sim.schedule("rst_n", 0, 0).unwrap();
                sim.run_until(0).unwrap();
                for name in ["regular", "host"] {
                    assert_eq!(sim.get(sim.signal(name)), 0u8.into(), "{name}");
                }
                sim.schedule("rst_n", 1, 1).unwrap();
                sim.schedule("clk", 2, 1).unwrap();
                sim.run_until(2).unwrap();
                for name in ["regular", "host"] {
                    assert_eq!(sim.get(sim.signal(name)), 1u8.into(), "{name}");
                }
            }

            #[test]
            fn restored_time_zero_wait_does_not_restart_registration() {
                // A folded host call retains the scheduled FF marker but no
                // live host resource, so checkpoints/state files are supported.
                let source = r#"module Top(input bit clk, output int count);
                  always_ff @(posedge clk) begin
                    if (1'bx) $fwrite(1, "dead");
                    count <= count + 1;
                  end
                endmodule"#;
                let mut sim = celox::Simulation::from_sv_sources(
                    vec![(source, Path::new("startup_restore.sv"))],
                    "Top",
                )
                .$backend()
                .unwrap();
                let before = sim.checkpoint().unwrap();
                sim.schedule("clk", 0, 1).unwrap();
                sim.run_until(0).unwrap();
                assert_eq!(sim.get(sim.signal("count")), 1u8.into());
                sim.restore(&before).unwrap();
                sim.schedule("clk", 0, 1).unwrap();
                sim.run_until(0).unwrap();
                assert_eq!(sim.get(sim.signal("count")), 1u8.into());
                let after = sim.checkpoint().unwrap();
                let file = sim.save_state().unwrap();
                for import_file in [false, true] {
                    if import_file {
                        sim.load_state(&file).unwrap();
                    } else {
                        sim.restore(&after).unwrap();
                    }
                    sim.schedule("clk", 0, 0).unwrap();
                    sim.run_until(0).unwrap();
                    sim.schedule("clk", 0, 1).unwrap();
                    sim.run_until(0).unwrap();
                    assert_eq!(sim.get(sim.signal("count")), 2u8.into());
                }
            }
        }
    };
}
scheduled_startup!(interpreter, build_interpreter);
scheduled_startup!(cranelift, build);
#[cfg(target_arch = "x86_64")]
scheduled_startup!(native, build_native);
scheduled_startup!(wasm, build_wasm);
