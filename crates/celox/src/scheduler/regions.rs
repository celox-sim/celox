//! Regression coverage for portable entry into native scheduling regions.
#[cfg(all(
    test,
    feature = "systemverilog",
    target_arch = "x86_64",
    not(feature = "arm64-codegen")
))]
mod tests {
    use crate::{SchedulerMode, Simulation};
    use celox_runtime::{UnitExecutor, backend::SimBackend};

    #[test]
    fn restored_inactive_runs_after_active_and_before_nba() {
        let source = r#"`timescale 1ns/1ns
module Top;
 bit trigger = 0;
 int q = 0, seen = 99;
 initial forever begin @(trigger); #0; seen = q; end
 initial forever begin @(trigger); q = 2; q <= 7; end
endmodule"#;
        for four_state in [false, true] {
            for (hotspot, active) in [(false, false), (false, true), (true, false), (true, true)] {
                let make = |mode| {
                    Simulation::from_sv_sources(
                        vec![(source, std::path::Path::new("native-region-entry.sv"))],
                        "Top",
                    )
                    .four_state(four_state)
                    .hotspot_jit(hotspot)
                    .scheduler_mode(mode)
                    .build_scheduler_test()
                    .unwrap()
                };
                let mut reference = make(SchedulerMode::Reference);
                reference.run_until(1).unwrap();
                let initial = reference.save_state().unwrap();
                let waiters = &initial.schedule.as_ref().unwrap().processes.waiters;
                assert_eq!(waiters.len(), 2);
                let inactive = waiters[0].process.clone();
                let index = reference
                    .simulator
                    .program
                    .runtime_schema
                    .processes
                    .iter()
                    .position(|process| {
                        reference.simulator.program.get_path(&process.status)
                            == inactive.to_string()
                    })
                    .unwrap();
                // Execute the real kernel continuation to its #0 suspension,
                // then save the corresponding portable Inactive queue.
                UnitExecutor::run_process(&mut reference.simulator, index).unwrap();
                let status = reference.simulator.program.runtime_schema.processes[index].status;
                let status = reference.simulator.backend.resolve_signal(&status);
                assert_eq!(
                    reference.simulator.backend.get_as::<u8>(status),
                    celox_design::ProcessStatus::InactiveDelay.code()
                );
                let mut saved = reference.save_state().unwrap();
                let processes = &mut saved.schedule.as_mut().unwrap().processes;
                processes
                    .waiters
                    .retain(|waiter| waiter.process != inactive);
                processes.inactive.push(inactive);
                if active {
                    processes.waiters.clear();
                    processes.ready.push(waiters[1].process.clone());
                }
                reference.load_state(&saved).unwrap();
                let mut native = make(SchedulerMode::Compiled);
                native.load_state(&saved).unwrap();
                let before = native.scheduler_stats().reference_steps;
                reference.run_until(1).unwrap();
                native.run_until(1).unwrap();
                assert_eq!(
                    native.save_state().unwrap(),
                    reference.save_state().unwrap()
                );
                assert_eq!(native.scheduler_stats().reference_steps, before);
                assert_eq!(
                    native.get(native.signal("seen")),
                    u8::from(active).wrapping_mul(2).into()
                );
                assert_eq!(
                    native.get(native.signal("q")),
                    u8::from(active).wrapping_mul(7).into()
                );
            }
        }
    }
}

#[cfg(all(
    test,
    feature = "systemverilog",
    target_arch = "x86_64",
    not(feature = "arm64-codegen")
))]
mod assertions {
    use crate::{SchedulerMode, Simulation};
    use celox_runtime::{UnitExecutor, backend::SimBackend};

    #[test]
    fn restored_assertion_regions_remain_ordered_in_native_code() {
        let source = r#"`timescale 1ns/1ns
module Top;
 bit trigger = 0;
 int hits_a = 0, hits_b = 0;
 assert property (@(posedge trigger) disable iff (hits_b != 0) 1) hits_a++;
 assert property (@(posedge trigger) 1) hits_b++;
endmodule"#;
        for four_state in [false, true] {
            for hotspot in [false, true] {
                let make = |mode| {
                    Simulation::from_sv_sources(
                        vec![(source, std::path::Path::new("native-assertion-entry.sv"))],
                        "Top",
                    )
                    .four_state(four_state)
                    .hotspot_jit(hotspot)
                    .scheduler_mode(mode)
                    .build_scheduler_test()
                    .unwrap()
                };
                let mut reference = make(SchedulerMode::Reference);
                reference.run_until(0).unwrap();
                let initial = reference.save_state().unwrap();
                let waiters = &initial.schedule.as_ref().unwrap().processes.waiters;
                assert_eq!(waiters.len(), 2);
                for (ordinal, waiter) in waiters.iter().enumerate() {
                    let index = reference
                        .simulator
                        .program
                        .runtime_schema
                        .processes
                        .iter()
                        .position(|process| {
                            reference.simulator.program.get_path(&process.status)
                                == waiter.process.to_string()
                        })
                        .unwrap();
                    UnitExecutor::run_process(&mut reference.simulator, index).unwrap();
                    if ordinal == 1 {
                        UnitExecutor::run_process(&mut reference.simulator, index).unwrap();
                    }
                    let status = reference.simulator.program.runtime_schema.processes[index].status;
                    let status = reference.simulator.backend.resolve_signal(&status);
                    let expected = if ordinal == 0 {
                        celox_design::ProcessStatus::AssertionObserved
                    } else {
                        celox_design::ProcessStatus::AssertionReactive
                    };
                    assert_eq!(
                        reference.simulator.backend.get_as::<u8>(status),
                        expected.code()
                    );
                }
                let mut saved = reference.save_state().unwrap();
                let processes = &mut saved.schedule.as_mut().unwrap().processes;
                processes.waiters.clear();
                processes.observed.push(waiters[0].process.clone());
                processes.reactive.push(waiters[1].process.clone());
                reference.load_state(&saved).unwrap();
                let mut native = make(SchedulerMode::Compiled);
                native.load_state(&saved).unwrap();
                let before = native.scheduler_stats().reference_steps;
                reference.run_until(0).unwrap();
                native.run_until(0).unwrap();
                assert_eq!(
                    native.save_state().unwrap(),
                    reference.save_state().unwrap()
                );
                assert_eq!(native.scheduler_stats().reference_steps, before);
                assert_eq!(native.get(native.signal("hits_a")), 1u8.into());
                assert_eq!(native.get(native.signal("hits_b")), 1u8.into());
            }
        }
    }
}

#[cfg(all(
    test,
    feature = "systemverilog",
    target_arch = "x86_64",
    not(feature = "arm64-codegen")
))]
mod deferred {
    use crate::{SchedulerMode, Simulation};
    use celox_runtime::{UnitExecutor, backend::SimBackend};

    #[test]
    fn restored_deferred_active_rearms_after_other_ready_work() {
        let source = r#"`timescale 1ns/1ns
module Top;
 reg [3:0] a = 4'b0100, b = 4'b0010;
 reg any_a, any_b;
 integer j;
 always @(*) begin
     any_a = 0;
     for (j = 0; j < 4; j = j + 1) any_a = any_a | a[j];
 end
 always @(*) begin
     any_b = 0;
     for (j = 0; j < 4; j = j + 1) any_b = any_b | b[j];
 end
endmodule"#;
        for four_state in [false, true] {
            for hotspot in [false, true] {
                let make = |mode| {
                    Simulation::from_sv_sources(
                        vec![(source, std::path::Path::new("native-deferred-entry.sv"))],
                        "Top",
                    )
                    .four_state(four_state)
                    .hotspot_jit(hotspot)
                    .scheduler_mode(mode)
                    .build_scheduler_test()
                    .unwrap()
                };
                let mut reference = make(SchedulerMode::Reference);
                reference.run_until(0).unwrap();
                let initial = reference.save_state().unwrap();
                let waiters = &initial.schedule.as_ref().unwrap().processes.waiters;
                assert_eq!(waiters.len(), 2);
                let process = waiters[0].process.clone();
                let index = reference
                    .simulator
                    .program
                    .runtime_schema
                    .processes
                    .iter()
                    .position(|slots| {
                        reference.simulator.program.get_path(&slots.status) == process.to_string()
                    })
                    .unwrap();
                UnitExecutor::run_process(&mut reference.simulator, index).unwrap();
                let status = reference.simulator.program.runtime_schema.processes[index].status;
                let status = reference.simulator.backend.resolve_signal(&status);
                assert_eq!(
                    reference.simulator.backend.get_as::<u8>(status),
                    celox_design::ProcessStatus::DeferActive.code()
                );
                let mut saved = reference.save_state().unwrap();
                let processes = &mut saved.schedule.as_mut().unwrap().processes;
                processes.waiters.retain(|waiter| waiter.process != process);
                processes.deferred_ready.push(process);
                reference.load_state(&saved).unwrap();
                let mut native = make(SchedulerMode::Compiled);
                native.load_state(&saved).unwrap();
                let before = native.scheduler_stats().reference_steps;
                reference.run_until(0).unwrap();
                native.run_until(0).unwrap();
                assert_eq!(
                    native.save_state().unwrap(),
                    reference.save_state().unwrap()
                );
                assert_eq!(native.scheduler_stats().reference_steps, before);
                assert_eq!(native.get(native.signal("any_a")), 1u8.into());
                assert_eq!(native.get(native.signal("any_b")), 1u8.into());
            }
        }
    }
}
