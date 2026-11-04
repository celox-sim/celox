//! Single-step entry uses the same generated scheduler as timed runs.
#[cfg(all(
    test,
    feature = "systemverilog",
    target_arch = "x86_64",
    not(feature = "arm64-codegen")
))]
mod tests {
    use crate::{SchedulerMode, Simulation};

    #[test]
    fn public_steps_remain_native_across_host_changes_and_restore() {
        let source = r#"`timescale 1ns/1ns
module Top(input bit clk, input logic [72:0] pulse);
 int q = 0, wakes = 0;
 always_ff @(posedge clk) q <= q + 1;
 initial forever begin @(pulse); wakes++; end
endmodule"#;
        for four_state in [false, true] {
            for hotspot in [false, true] {
                for mode in [SchedulerMode::Compiled, SchedulerMode::Differential] {
                    let make = |mode| {
                        Simulation::from_sv_sources(
                            vec![(source, std::path::Path::new("native-step.sv"))],
                            "Top",
                        )
                        .four_state(four_state)
                        .hotspot_jit(hotspot)
                        .scheduler_mode(mode)
                        .build_scheduler_test()
                        .unwrap()
                    };
                    let mut native = make(mode);
                    let mut reference = make(SchedulerMode::Reference);
                    for sim in [&mut native, &mut reference] {
                        sim.add_clock("clk", 6, 3);
                        sim.run_until(0).unwrap();
                    }
                    let before = native.scheduler_stats().reference_steps;
                    for step in 1..=12 {
                        for sim in [&mut native, &mut reference] {
                            let pulse = sim.signal("pulse");
                            sim.modify(|io| {
                                io.set(pulse, 1u8);
                                io.set(pulse, 0u8);
                            })
                            .unwrap();
                        }
                        assert_eq!(native.step().unwrap(), Some(step * 3));
                        assert_eq!(reference.step().unwrap(), Some(step * 3));
                        let saved = native.save_state().unwrap();
                        assert_eq!(saved, reference.save_state().unwrap());
                        if step == 5 {
                            native.load_state(&saved).unwrap();
                        }
                    }
                    assert_eq!(native.scheduler_stats().reference_steps, before);
                    assert_eq!(native.get(native.signal("q")), 6u8.into());
                    assert_eq!(native.get(native.signal("wakes")), 12u8.into());
                }
            }
        }
    }

    #[test]
    fn host_only_and_empty_steps_need_no_reference_scheduler() {
        let source = r#"`timescale 1ns/1ns
module Top(input bit pulse);
 int wakes = 0;
 initial forever begin @(pulse); wakes++; end
endmodule"#;
        for hotspot in [false, true] {
            let make = |mode| {
                Simulation::from_sv_sources(
                    vec![(source, std::path::Path::new("native-host-only-step.sv"))],
                    "Top",
                )
                .hotspot_jit(hotspot)
                .scheduler_mode(mode)
                .build_scheduler_test()
                .unwrap()
            };
            let mut native = make(SchedulerMode::Compiled);
            let mut reference = make(SchedulerMode::Reference);
            for sim in [&mut native, &mut reference] {
                sim.run_until(7).unwrap();
            }
            let before = native.scheduler_stats().reference_steps;
            for sim in [&mut native, &mut reference] {
                let pulse = sim.signal("pulse");
                sim.modify(|io| {
                    io.set(pulse, 1u8);
                    io.set(pulse, 0u8);
                })
                .unwrap();
                assert_eq!(sim.step().unwrap(), Some(7));
                assert_eq!(sim.step().unwrap(), None);
            }
            assert_eq!(
                native.save_state().unwrap(),
                reference.save_state().unwrap()
            );
            assert_eq!(native.scheduler_stats().reference_steps, before);
            assert_eq!(native.get(native.signal("wakes")), 1u8.into());
        }
    }
}
