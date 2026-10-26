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

#[test]
fn checkpoint_replays_compound_event_baselines() {
    fn replay<B: SimBackend>(mut sim: Simulation<B>) {
        sim.run_until(1).unwrap();
        let saved = sim.checkpoint().unwrap();
        for _ in 0..2 {
            sim.restore(&saved).unwrap();
            sim.run_until(2).unwrap();
            assert_eq!(sim.get(sim.signal("q")), 0u8.into());
            sim.run_until(4).unwrap();
            assert_eq!(sim.get(sim.signal("q")), 3u8.into());
        }
    }
    let source = r#"`timescale 1ns/1ns
        module Top(output int q);
            task automatic start(input int n);
                int value=0;
                fork
                    begin @(value+n==4); q=q+n; end
                    begin #2; value=1; #1; value=4-n; end
                join_none
            endtask
            initial begin q=0; start(1); start(2); #5; end
        endmodule
    "#;
    replay(
        Simulation::from_sv_sources(vec![(source, Path::new("compound_checkpoint.sv"))], "Top")
            .build_interpreter()
            .unwrap(),
    );
    replay(
        Simulation::from_sv_sources(vec![(source, Path::new("compound_checkpoint.sv"))], "Top")
            .build_native()
            .unwrap(),
    );
}

#[test]
fn compound_event_probe_does_not_switch_the_writer() {
    fn check<B: SimBackend>(mut sim: Simulation<B>) {
        sim.run_until(2).unwrap();
        assert_eq!(sim.get(sim.signal("q")), 3u8.into());
    }
    // A probe is an implementation boundary. The selected writer continues
    // before its newly ready waiter, preserving the scheduler's execution order.
    let source = r#"`timescale 1ns/1ns
        module Top(output int q);
            initial begin
                automatic int value=0;
                q=0;
                fork
                    begin @(value==1); q=q+1; end
                    begin #1; value=1; value=0; if(q!=0) q=100; end
                join
                q=q+2;
            end
        endmodule
    "#;
    check(
        Simulation::from_sv_sources(vec![(source, Path::new("probe_order.sv"))], "Top")
            .build_interpreter()
            .unwrap(),
    );
    check(
        Simulation::from_sv_sources(vec![(source, Path::new("probe_order.sv"))], "Top")
            .build_native()
            .unwrap(),
    );
}

/// Celox chooses per-write observation for activation-local expressions.
/// IEEE 1800-2023 4.7 also permits other simulators to coalesce intermediate
/// compound results, so these guarantees belong to the Celox API suite.
#[test]
fn compound_event_observes_each_supported_write() {
    fn check<B: SimBackend>(mut sim: Simulation<B>, expected: u8) {
        sim.run_until(10).unwrap();
        assert_eq!(sim.get(sim.signal("q")), expected.into());
    }
    let cases = [
        (
            "compound_event_observes_transient_result.sv",
            r#"module Top(output int q);
 initial begin
  automatic int value=0;
  q=0;
  fork
   begin @(value==1); q=q+1; end
   begin #1; value=1; value=0; end
  join
  q=q+2;
 end
endmodule"#,
            3u8,
        ),
        (
            "compound_event_observes_subroutine_copyout.sv",
            r#"module Top(output int q);
 task automatic set_task(output int dst,input int x); dst=x; endtask
 function automatic void set_function(output int dst,input int x); dst=x; endfunction
 initial begin
  automatic int value=0;
  q=0;
  fork
   begin repeat(2) begin @(posedge (value==1)); q=q+1; end end
   begin #1; set_task(value,1); value=0;
         #1; set_function(value,1); set_function(value,0); end
  join
  q=q+4;
 end
endmodule"#,
            6u8,
        ),
        (
            "compound_event_observes_loop_variable_steps.sv",
            r#"module Top(output int q);
 initial begin
  automatic int i=0;
  q=0;
  fork
   begin @(i==1); q=q+1; end
   begin #1; for(i=0;i<2;i++) begin end end
  join
  q=q+2;
 end
endmodule"#,
            3u8,
        ),
        (
            "compound_event_copyout_tracks_distinct_operands.sv",
            r#"module Top(output int q);
 function automatic void set_pair(output int x,output int y); x=1; y=1; endfunction
 initial begin
  automatic int a=0;
  automatic int b=0;
  q=0;
  fork
   begin @(a!=b); q=q+1; end
   begin #1; set_pair(a,b); end
  join
  q=q+2;
 end
endmodule"#,
            3u8,
        ),
    ];
    for (name, body, expected) in cases {
        let source = format!("`timescale 1ns/1ns\n{body}");
        let build = || Simulation::from_sv_sources(vec![(source.as_str(), Path::new(name))], "Top");
        check(build().build_interpreter().unwrap(), expected);
        check(build().build_native().unwrap(), expected);
        check(
            build()
                .threads(4)
                .parallel_partition(celox::ParallelPartition::Always)
                .build_native()
                .unwrap(),
            expected,
        );
        let image = build().compile_native().unwrap().into_program_image();
        let image =
            celox::NativeProgramImage::from_container_bytes(&image.to_container_bytes().unwrap())
                .unwrap();
        check(
            Simulation::from_sv_sources(Vec::new(), "Top")
                .build_native_from_image(image)
                .unwrap(),
            expected,
        );
        #[cfg(feature = "cranelift")]
        {
            check(build().build_cranelift().unwrap(), expected);
            check(
                build()
                    .threads(4)
                    .parallel_partition(celox::ParallelPartition::Always)
                    .build_cranelift()
                    .unwrap(),
                expected,
            );
        }
        #[cfg(feature = "wasm-runtime")]
        check(build().build_wasm().unwrap(), expected);
    }
}
