//! Canonical event bindings and shared clock storage stay in generated code.
#[cfg(all(test, target_arch = "x86_64", not(feature = "arm64-codegen")))]
mod tests {
    use crate::{SchedulerMode, Simulation};

    #[test]
    fn hierarchical_clock_and_reset_aliases_remain_native() {
        for opposite in [false, true] {
            let other_edge = if opposite { "negedge" } else { "posedge" };
            let source = format!(
                r#"`timescale 1ns/1ns
module Leaf(input logic clk, rst_n, output int q);
 initial q = 0;
 always_ff @(posedge clk or negedge rst_n)
     if (!rst_n) q <= 0; else q <= q + 1;
endmodule
module Other(input logic clk, rst_n, output int q);
 initial q = 0;
 always_ff @({other_edge} clk or negedge rst_n)
     if (!rst_n) q <= 0; else q <= q + 1;
endmodule
module Top(input logic clk, rst_n);
 wire first = clk, second = first, third = second;
 wire reset_a = rst_n, reset_b = reset_a;
 int q0, q1, q2, q3;
 Leaf a(first, reset_a, q0);
 Leaf b(second, reset_b, q1);
 Other c(third, reset_b, q2);
 Other d(clk, rst_n, q3);
endmodule"#
            );
            for four_state in [false, true] {
                for hotspot in [false, true] {
                    let make = |mode| {
                        Simulation::from_sv_sources(
                            vec![(
                                source.as_str(),
                                std::path::Path::new("native-event-aliases.sv"),
                            )],
                            "Top",
                        )
                        .four_state(four_state)
                        .hotspot_jit(hotspot)
                        .scheduler_mode(mode)
                        .build_scheduler_test()
                        .unwrap()
                    };
                    let mut native = make(SchedulerMode::Compiled);
                    let mut reference = make(SchedulerMode::Reference);
                    for sim in [&mut native, &mut reference] {
                        let reset = sim.signal("rst_n");
                        sim.modify(|io| io.set(reset, 1u8)).unwrap();
                        sim.add_clock("clk", 4, 1);
                        sim.run_until(0).unwrap();
                    }
                    let before = native.scheduler_stats().reference_steps;
                    for end in [11, 19, 27] {
                        native.run_until(end).unwrap();
                        reference.run_until(end).unwrap();
                        assert_eq!(
                            native.save_state().unwrap(),
                            reference.save_state().unwrap(),
                            "opposite={opposite} at {end}"
                        );
                        for sim in [&mut native, &mut reference] {
                            let reset = sim.signal("rst_n");
                            sim.modify(|io| io.set(reset, 0u8)).unwrap();
                            sim.run_until(end).unwrap();
                            sim.modify(|io| io.set(reset, 1u8)).unwrap();
                            sim.run_until(end).unwrap();
                        }
                        assert_eq!(
                            native.save_state().unwrap(),
                            reference.save_state().unwrap()
                        );
                    }
                    assert_eq!(
                        native.scheduler_stats().reference_steps,
                        before,
                        "{:?}",
                        native.scheduler_stats()
                    );
                }
            }
        }
    }
}

#[cfg(all(test, target_arch = "x86_64", not(feature = "arm64-codegen")))]
mod batches {
    use crate::{SchedulerMode, Simulation};

    #[test]
    fn opposite_edges_of_host_alias_batches_keep_four_state_levels() {
        let ports = (0..5)
            .map(|lane| format!("input logic clk{lane}"))
            .collect::<Vec<_>>()
            .join(", ");
        let cells = (0..5).map(|lane| format!("wire alias{lane} = clk{lane}; int up{lane}, down{lane};\n Pos p{lane}(alias{lane}, up{lane}); Neg n{lane}(clk{lane}, down{lane});")).collect::<Vec<_>>().join("\n");
        let source = format!(
            r#"`timescale 1ns/1ns
module Pos(input logic clk, output int q);
 initial q = 0;
 always_ff @(posedge clk) q <= q + 1;
endmodule
module Neg(input logic clk, output int q);
 initial q = 0;
 always_ff @(negedge clk) q <= q + 1;
endmodule
module Top({ports});
 {cells}
endmodule"#
        );
        for four_state in [false, true] {
            for hotspot in [false, true] {
                let make = |mode| {
                    Simulation::from_sv_sources(
                        vec![(
                            source.as_str(),
                            std::path::Path::new("native-opposite-host-batch.sv"),
                        )],
                        "Top",
                    )
                    .four_state(four_state)
                    .hotspot_jit(hotspot)
                    .scheduler_mode(mode)
                    .build_scheduler_test()
                    .unwrap()
                };
                let mut native = make(SchedulerMode::Compiled);
                let mut reference = make(SchedulerMode::Reference);
                native.run_until(0).unwrap();
                reference.run_until(0).unwrap();
                let before = native.scheduler_stats().reference_steps;
                for (time, (value, mask)) in [(0u8, 0u8), (1, 0), (1, 1), (0, 1), (1, 0), (0, 0)]
                    .into_iter()
                    .enumerate()
                {
                    for sim in [&mut native, &mut reference] {
                        let signals = (0..5)
                            .map(|lane| sim.signal(&format!("clk{lane}")))
                            .collect::<Vec<_>>();
                        sim.modify(|io| {
                            for signal in &signals {
                                io.set_four_state(*signal, value.into(), mask.into());
                            }
                        })
                        .unwrap();
                        sim.run_until(time as u64).unwrap();
                    }
                    assert_eq!(
                        native.save_state().unwrap(),
                        reference.save_state().unwrap(),
                        "time={time}, four_state={four_state}"
                    );
                }
                assert_eq!(
                    native.scheduler_stats().reference_steps,
                    before,
                    "{:?}",
                    native.scheduler_stats()
                );
            }
        }
    }
}

#[cfg(all(test, target_arch = "x86_64", not(feature = "arm64-codegen")))]
#[allow(
    clippy::disallowed_types,
    reason = "test-only: the tests read back what the observers they hand to the simulation record"
)]
mod unknown_kinds {
    use crate::{
        NativeBackend, SchedulerMode, SignalRef, SimBackend, Simulation, Simulator,
        SimulatorOptions, SlotIo, SlotObserver,
    };
    use std::sync::{Arc, Mutex};

    type Pending = Arc<Mutex<Option<(u8, u8)>>>;
    struct Drive {
        inputs: Vec<SignalRef>,
        pending: Pending,
    }
    impl<B: SimBackend> SlotObserver<B> for Drive {
        fn before_domains(&mut self, io: &mut SlotIo<'_, B>, _: u64) {
            if let Some((value, mask)) = self.pending.lock().unwrap().take() {
                for input in &self.inputs {
                    io.set_four_state(*input, value.into(), mask.into());
                }
            }
        }
        fn end_of_slot(&mut self, _: &mut SlotIo<'_, B>, _: u64) {}
    }

    // A frontend can leave a canonical event's kind unspecified. Exercise
    // that IR contract directly, including the batched edge comparison.
    #[test]
    fn unspecified_domains_observe_every_settled_four_state_change() {
        for observed in [false, true] {
            for lanes in [1, 5] {
                let ports = (0..lanes)
                    .map(|i| format!("input logic clk{i}"))
                    .collect::<Vec<_>>()
                    .join(",");
                let cells = (0..lanes)
                    .map(|i| format!("int q{i}=0; always_ff @(posedge clk{i}) q{i} <= q{i}+1;"))
                    .collect::<Vec<_>>()
                    .join("\n");
                let timing = if observed {
                    "initial #100 $finish;"
                } else {
                    ""
                };
                let source = format!(
                    "module Top(input bit trigger, known, {ports});\n{timing}\nint ticks=0, other=0; always_ff @(posedge trigger) ticks <= other+1; always_ff @(posedge known) other <= other+1;\n{cells}\nendmodule"
                );
                for four_state in [false, true] {
                    let make = |mode| {
                        let options = SimulatorOptions {
                            four_state,
                            emit_triggers: true,
                            scheduler: mode,
                            ..Default::default()
                        };
                        let (mut program, warnings) = crate::compile_sv_to_sir(
                            &[(
                                source.as_str(),
                                std::path::Path::new("unspecified-domain.sv"),
                            )],
                            "Top",
                            &[],
                            &[],
                            four_state,
                            &Default::default(),
                            None,
                            None,
                            None,
                            None,
                            &[],
                            &options.optimize_options,
                        )
                        .unwrap();
                        let known = program.runtime.get_addr(&[], &["known"]).unwrap();
                        let events: Vec<_> = program
                            .runtime
                            .design
                            .state_objects
                            .iter()
                            .filter_map(|(&address, metadata)| {
                                (metadata.kind == celox_design::DomainKind::ClockPosedge
                                    && address != known)
                                    .then_some(address)
                            })
                            .collect();
                        for event in events {
                            program
                                .runtime
                                .design
                                .set_event_kind_for_test(event, celox_design::DomainKind::Other);
                        }
                        let mut program = program.into_laid_out(four_state);
                        if mode != SchedulerMode::Reference {
                            super::super::enable_native_services(&mut program);
                            super::super::attach(&mut program, true);
                            assert!(
                                program
                                    .runtime
                                    .runtime_schema
                                    .scheduler
                                    .as_ref()
                                    .unwrap()
                                    .unit
                                    .is_some()
                            );
                        }
                        let backend = NativeBackend::new(&program, &options).unwrap();
                        let mut sim = Simulator::with_backend_and_program_for_simulation(
                            backend,
                            program.into_runtime(),
                            warnings,
                        );
                        sim.apply_initial_values();
                        let mut sim = Simulation::new(sim);
                        sim.state.set_scheduler_mode(mode);
                        sim
                    };
                    let mut native = make(SchedulerMode::Compiled);
                    let mut reference = make(SchedulerMode::Reference);
                    let pending: [Pending; 2] = std::array::from_fn(|_| Arc::new(Mutex::new(None)));
                    for (sim, pending) in [&mut native, &mut reference].into_iter().zip(&pending) {
                        sim.add_clock("trigger", 2, 1);
                        sim.add_clock("known", 4, 2);
                        sim.run_until(1).unwrap();
                        if observed {
                            let inputs: Vec<_> =
                                (0..lanes).map(|i| sim.signal(&format!("clk{i}"))).collect();
                            sim.observe_slots(
                                Box::new(Drive {
                                    inputs: inputs.clone(),
                                    pending: pending.clone(),
                                }),
                                &inputs,
                            );
                        }
                    }
                    let before = native.scheduler_stats().reference_steps;
                    for (time, (value, mask)) in
                        [(0u8, 0u8), (1, 0), (1, 1), (0, 1), (1, 0), (0, 0)]
                            .into_iter()
                            .enumerate()
                    {
                        for (sim, pending) in
                            [&mut native, &mut reference].into_iter().zip(&pending)
                        {
                            let inputs = (0..lanes)
                                .map(|i| sim.signal(&format!("clk{i}")))
                                .collect::<Vec<_>>();
                            if observed {
                                *pending.lock().unwrap() = Some((value, mask));
                            } else {
                                sim.modify(|io| {
                                    for input in inputs {
                                        io.set_four_state(input, value.into(), mask.into());
                                    }
                                })
                                .unwrap();
                            }
                            sim.run_until(time as u64 + 2).unwrap();
                            assert!(
                                pending.lock().unwrap().is_none(),
                                "observed={observed}, lanes={lanes}, four_state={four_state}, time={time}, stats={:?}",
                                sim.scheduler_stats()
                            );
                        }
                        assert_eq!(
                            native.save_state().unwrap(),
                            reference.save_state().unwrap(),
                            "observed={observed}, lanes={lanes}, four_state={four_state}, time={time}"
                        );
                    }
                    assert_eq!(native.scheduler_stats().reference_steps, before);
                }
            }
        }
    }
}
#[cfg(all(test, target_arch = "x86_64", not(feature = "arm64-codegen")))]
mod shared_storage {
    use crate::{
        NativeBackend, SchedulerMode, SimBackend, Simulation, Simulator, SimulatorOptions,
    };

    #[test]
    fn shared_clock_storage_is_sampled_before_any_alias_write() {
        let source = "module Top(input bit a, b); int qa=0, qb=0; always_ff @(posedge a) qa<=qa+1; always_ff @(posedge b) qb<=qb+1; endmodule";
        for four_state in [false, true] {
            let options = SimulatorOptions {
                four_state,
                emit_triggers: true,
                scheduler: SchedulerMode::Compiled,
                ..Default::default()
            };
            let (program, warnings) = crate::compile_sv_to_sir(
                &[(source, std::path::Path::new("shared-clock-storage.sv"))],
                "Top",
                &[],
                &[],
                four_state,
                &Default::default(),
                None,
                None,
                None,
                None,
                &[],
                &options.optimize_options,
            )
            .unwrap();
            let mut program = program.into_laid_out(four_state);
            super::super::enable_native_services(&mut program);
            super::super::attach(&mut program, true);
            let schema = program.runtime.runtime_schema.scheduler.clone().unwrap();
            let mut plan = super::super::Plan::new(&program, &schema.slots, true).unwrap();
            assert_eq!(plan.clocks.len(), 2);
            let first = plan.clocks[0];
            let second = plan.clocks[1];
            let source = plan.events[first].signal;
            // Exercise the generator's physical-alias contract independently
            // of whether a frontend chooses to retain two separate wires.
            // Both domains now read the first clock, with opposite polarities.
            plan.events[second].signal = source;
            plan.events[second].active = 0;
            plan.lazy = None;
            plan.incremental = None;
            program.sir.processes[schema.unit.unwrap()] = plan.generate(&program, &schema.slots);
            let backend = NativeBackend::new(&program, &options).unwrap();
            let event = backend.resolve_event(&source);
            let signal = backend.resolve_signal(&source);
            let mut sim = Simulator::with_backend_and_program_for_simulation(
                backend,
                program.into_runtime(),
                warnings,
            );
            sim.apply_initial_values();
            let mut sim = Simulation::new(sim);
            sim.state.set_scheduler_mode(SchedulerMode::Compiled);
            sim.run_until(0).unwrap();
            let before = sim.scheduler_stats().reference_steps;
            sim.state.add_clock(event, signal, 4, 1);
            sim.run_until(11).unwrap();
            for name in ["qa", "qb"] {
                assert_eq!(
                    sim.get(sim.signal(name)),
                    3u8.into(),
                    "{name}, four_state={four_state}"
                );
            }
            assert_eq!(sim.scheduler_stats().reference_steps, before);
        }
    }
}
