#![cfg(feature = "systemverilog")]
use celox::{SchedulerMode, SimBackend, Simulation};
use std::path::Path;

fn replay<B: SimBackend>(mut sim: Simulation<B>) {
    sim.run_until(1).unwrap();
    let checkpoint = sim.checkpoint().unwrap();
    for _ in 0..3 {
        sim.restore(&checkpoint).unwrap();
        sim.run_until(4).unwrap();
        assert_eq!(sim.get(sim.signal("q")), 0u8.into());
        sim.run_until(7).unwrap();
        assert_eq!(sim.get(sim.signal("q")), 3u8.into());
    }
}

#[test]
fn checkpoint_replays_cancellation_join_and_id_reuse() {
    let source = r#"`timescale 1ns/1ns
module Top(output bit [1:0] q);
 initial begin
  fork begin #4; q=2; end join_none
  #2; disable fork;
  fork begin #5; q[0]=1; end join_none
  wait fork;
  q[1]=1;
 end
endmodule"#;
    replay(
        Simulation::from_sv_sources(vec![(source, Path::new("control.sv"))], "Top")
            .build_interpreter()
            .unwrap(),
    );
    replay(
        Simulation::from_sv_sources(vec![(source, Path::new("control.sv"))], "Top")
            .scheduler_mode(SchedulerMode::Differential)
            .build_native()
            .unwrap(),
    );
}

#[test]
fn checkpoint_preserves_wait_fork_membership() {
    let source = r#"`timescale 1ns/1ns
module Top(output bit [2:0] q);
 initial begin
  fork begin #2; q[0]=1; end begin #4; q[1]=1; end join_none
  wait fork; q[2]=1;
 end
endmodule"#;
    let mut sim = Simulation::from_sv_sources(vec![(source, Path::new("wait.sv"))], "Top")
        .scheduler_mode(SchedulerMode::Differential)
        .build_native()
        .unwrap();
    sim.run_until(1).unwrap();
    let checkpoint = sim.checkpoint().unwrap();
    for _ in 0..3 {
        sim.restore(&checkpoint).unwrap();
        sim.run_until(3).unwrap();
        assert_eq!(sim.get(sim.signal("q")), 1u8.into());
        sim.run_until(5).unwrap();
        assert_eq!(sim.get(sim.signal("q")), 7u8.into());
    }
}

#[test]
fn disable_without_children_in_final_is_a_zero_time_noop() {
    let source = r#"module Top(output int q);
 initial q=1;
 final begin disable fork; q=2; end
endmodule"#;
    let mut sim = Simulation::from_sv_sources(vec![(source, Path::new("final.sv"))], "Top")
        .build_native()
        .unwrap();
    sim.run_until(0).unwrap();
    sim.finish().unwrap();
    assert_eq!(sim.get(sim.signal("q")), 2u8.into());
    assert_eq!(sim.time(), 0);
}

#[test]
fn disable_without_activation_frames_preserves_other_process_wakeups() {
    let source = r#"`timescale 1ns/1ns
module Top(output bit [1:0] q);
 initial begin #1; disable fork; q[0]=1; end
 initial begin #2; q[1]=1; end
endmodule"#;
    let mut sim = Simulation::from_sv_sources(vec![(source, Path::new("empty.sv"))], "Top")
        .scheduler_mode(SchedulerMode::Differential)
        .build_native()
        .unwrap();
    sim.run_until(3).unwrap();
    assert_eq!(sim.get(sim.signal("q")), 3u8.into());
}
