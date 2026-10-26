#![cfg(feature = "systemverilog")]
use celox::{SimBackend, Simulation};
use std::path::Path;

fn check<B: SimBackend>(mut sim: Simulation<B>) {
    sim.run_until(1).unwrap();
    assert_eq!(sim.get(sim.signal("q")), 1u8.into());
    sim.run_until(2).unwrap();
    assert_eq!(sim.get(sim.signal("q")), 7u8.into());
}
#[test]
fn fork_join_smoke() {
    let source = r#"`timescale 1ns/1ns
module Top(output bit [2:0] q);
 initial begin
   fork
     begin #1; q[0] = 1; end
     begin #2; q[1] = 1; end
   join
   q[2] = 1;
 end
endmodule"#;
    check(
        Simulation::from_sv_sources(vec![(source, Path::new("fork.sv"))], "Top")
            .build_interpreter()
            .unwrap(),
    );
    check(
        Simulation::from_sv_sources(vec![(source, Path::new("fork.sv"))], "Top")
            .build_native()
            .unwrap(),
    );
    let image = Simulation::from_sv_sources(vec![(source, Path::new("fork.sv"))], "Top")
        .compile_native()
        .unwrap()
        .into_program_image();
    let image =
        celox::NativeProgramImage::from_container_bytes(&image.to_container_bytes().unwrap())
            .unwrap();
    check(
        Simulation::from_sv_sources(Vec::new(), "Top")
            .build_native_from_image(image)
            .unwrap(),
    );
}

#[test]
fn checkpoint_replays_pending_children() {
    let source = r#"`timescale 1ns/1ns
module Top(output int q);
 initial begin q=0; fork begin #2; q=q+1; end begin #4; q=q+2; end join q=q+4; end
endmodule"#;
    let mut sim = Simulation::from_sv_sources(vec![(source, Path::new("fork.sv"))], "Top")
        .build_native()
        .unwrap();
    sim.run_until(1).unwrap();
    let saved = sim.checkpoint().unwrap();
    sim.run_until(5).unwrap();
    assert_eq!(sim.get(sim.signal("q")), 7u8.into());
    sim.restore(&saved).unwrap();
    sim.run_until(3).unwrap();
    assert_eq!(sim.get(sim.signal("q")), 1u8.into());
    sim.run_until(5).unwrap();
    assert_eq!(sim.get(sim.signal("q")), 7u8.into());
}

#[test]
fn checkpoint_before_spawn_can_be_restored_repeatedly() {
    let source = r#"`timescale 1ns/1ns
module Top(output int q);
 initial begin q=0; #1; fork begin #1; q=q+1; end begin #2; q=q+2; end join end
endmodule"#;
    let mut sim = Simulation::from_sv_sources(vec![(source, Path::new("fork.sv"))], "Top")
        .build_native()
        .unwrap();
    sim.run_until(0).unwrap();
    let saved = sim.checkpoint().unwrap();
    for _ in 0..3 {
        sim.restore(&saved).unwrap();
        sim.run_until(4).unwrap();
        assert_eq!(sim.get(sim.signal("q")), 3u8.into());
    }
    sim.save_state().unwrap();
}

#[test]
fn stop_resumes_the_same_child_before_other_ready_processes() {
    let source = r#"`timescale 1ns/1ns
module Top(output bit [2:0] q);
 initial begin
  fork begin q[0]=1; $stop; q[1]=1; end begin #2; q[2]=1; end join
 end
endmodule"#;
    let mut sim = Simulation::from_sv_sources(vec![(source, Path::new("fork.sv"))], "Top")
        .scheduler_mode(celox::SchedulerMode::Differential)
        .build_native()
        .unwrap();
    sim.run_until(5).unwrap();
    assert!(sim.is_stopped());
    assert_eq!(sim.get(sim.signal("q")), 1u8.into());
    sim.run_until(5).unwrap();
    assert_eq!(sim.get(sim.signal("q")), 7u8.into());
}
#[test]
fn child_finish_runs_final_once_without_resuming_siblings() {
    let source = r#"`timescale 1ns/1ns
module Top(output bit [2:0] q);
 initial fork begin #1; q[0]=1; $finish; end begin #2; q[1]=1; end join
 final q[2]=1;
endmodule"#;
    let mut sim = Simulation::from_sv_sources(vec![(source, Path::new("fork.sv"))], "Top")
        .build_native()
        .unwrap();
    sim.run_until(5).unwrap();
    assert!(sim.is_finished());
    assert_eq!(sim.get(sim.signal("q")), 5u8.into());
    assert_eq!(sim.time(), 1);
}

#[test]
fn fork_and_compiled_processes_share_scheduler_state() {
    let source = r#"`timescale 1ns/1ns
module Top(output bit [2:0] q);
 initial begin
  fork begin #1; q[0]=1; end begin #3; q[1]=1; end join
 end
 initial begin #2; q[2]=1; end
endmodule"#;
    let mut sim = Simulation::from_sv_sources(vec![(source, Path::new("fork.sv"))], "Top")
        .scheduler_mode(celox::SchedulerMode::Differential)
        .build_native()
        .unwrap();
    sim.run_until(5).unwrap();
    assert_eq!(sim.get(sim.signal("q")), 7u8.into());
}

#[test]
fn program_exit_discards_children_waiting_for_parent_suspension() {
    let source = r#"`timescale 1ns/1ns
module Top(output int q);
 Tb t(q);
 Other other();
endmodule
program Tb(output int q);
 initial begin
  q=0;
  fork begin #1; q=7; end join_none
  $exit;
 end
endprogram
program Other;
 initial #10;
endprogram"#;
    let mut sim = Simulation::from_sv_sources(vec![(source, Path::new("fork.sv"))], "Top")
        .scheduler_mode(celox::SchedulerMode::Differential)
        .build_native()
        .unwrap();
    sim.run_until(5).unwrap();
    assert_eq!(sim.get(sim.signal("q")), 0u8.into());
    assert!(!sim.is_finished());
}
