//! Private scheduler storage and progress contracts for process controls.
use super::Simulation;
use crate::SimBackend;
use std::path::Path;

fn sources(case: &str) -> String {
    let design = celox_test_suite::sv::case(case).unwrap().design();
    assert_eq!(design.sources.len(), 1);
    format!("`timescale 1ns/1ns\n{}", design.sources[0].text)
}

#[test]
fn mutual_status_waits_do_not_consume_the_slot_budget() {
    fn verify<B: SimBackend>(mut sim: Simulation<B>) {
        sim.state.set_slot_budgets(128, 4096);
        sim.run_until(5).unwrap();
        assert_eq!(sim.get(sim.signal("checks")), 2u8.into());
        assert_eq!(sim.get(sim.signal("observed_at")), 3u8.into());
    }
    let source = sources("fork_control::process_mutual_status_waits_advance_to_cancellation");
    for four_state in [false, true] {
        let build = || {
            Simulation::from_sv_sources(vec![(&source, Path::new("status_cycle.sv"))], "Top")
                .four_state(four_state)
        };
        verify(build().build_native().unwrap());
        verify(build().build_interpreter().unwrap());
        #[cfg(feature = "cranelift")]
        verify(build().build_cranelift().unwrap());
        #[cfg(feature = "wasm-runtime")]
        verify(build().build_wasm().unwrap());
    }
}

#[test]
fn externally_killed_frames_release_storage_and_preserve_rng() {
    fn verify<B: SimBackend>(mut sim: Simulation<B>) {
        for time in 1..=16u64 {
            sim.run_until(time).unwrap();
            assert_eq!(sim.get(sim.signal("checks")), time.into());
            let state = sim.state.schedule_key();
            let killed: Vec<_> = state
                .activations
                .iter()
                .filter(|a| a.control.exposed && a.control.killed)
                .collect();
            assert_eq!(killed.len(), time as usize);
            for target in killed {
                assert!(!target.live);
                assert!(
                    target.private.is_empty(),
                    "killed worker retains its private frame"
                );
                assert!(
                    target.locals.is_empty(),
                    "killed worker retains lexical cells"
                );
                assert!(
                    target.parent.is_none(),
                    "retired leaf retains its parent link"
                );
                assert!(
                    target.control.random.is_some(),
                    "retirement lost the RNG state"
                );
            }
        }
        assert_eq!(sim.get(sim.signal("late")), 0u8.into());
    }
    let source = r#"`timescale 1ns/1ns
      module Top(output int checks, late);
       process worker;
       string state;
       initial begin
        checks=0; late=0;
        for (int i=0; i<16; i++) begin
         fork begin
          automatic bit [4095:0] payload='1;
          worker=process::self(); worker.srandom(17+i); #1000; late=payload[0];
         end join_none
         #1; state=worker.get_randstate(); worker.kill();
         checks+=(worker.status()==process::KILLED && state==worker.get_randstate());
        end
       end
      endmodule"#;
    for four_state in [false, true] {
        let build = || {
            Simulation::from_sv_sources(vec![(source, Path::new("kill_storage.sv"))], "Top")
                .four_state(four_state)
        };
        verify(build().build_native().unwrap());
        verify(build().build_interpreter().unwrap());
        #[cfg(feature = "cranelift")]
        verify(build().build_cranelift().unwrap());
        #[cfg(feature = "wasm-runtime")]
        verify(build().build_wasm().unwrap());
    }
}

#[test]
fn program_exit_discards_suspension_and_dead_wakeups() {
    fn verify<B: SimBackend>(mut sim: Simulation<B>) {
        for time in [2, 3] {
            sim.run_until(time).unwrap();
            let state = sim.state.schedule_key();
            let ended = state
                .activations
                .iter()
                .filter(|activation| activation.control.exposed)
                .collect::<Vec<_>>();
            assert_eq!(ended.len(), 1);
            assert!(!ended[0].live);
            assert!(ended[0].control.suspended.is_none());
            assert!(ended[0].control.awaiting.is_none());
            assert!(ended[0].control.observes.is_empty());
            assert!(
                state
                    .process_wakeups
                    .iter()
                    .all(|(_, id)| state.activations[*id].live)
            );
        }
        sim.run_until(9).unwrap();
        assert_eq!(sim.get(sim.signal("late")), 0u8.into());
    }
    let source = sources("fork_control::process_resume_after_program_exit_is_noop");
    for four_state in [false, true] {
        let build = || {
            Simulation::from_sv_sources(vec![(&source, Path::new("exit_resume.sv"))], "Top")
                .four_state(four_state)
        };
        verify(build().build_native().unwrap());
        verify(build().build_interpreter().unwrap());
        #[cfg(feature = "cranelift")]
        verify(build().build_cranelift().unwrap());
        #[cfg(feature = "wasm-runtime")]
        verify(build().build_wasm().unwrap());
    }
}
