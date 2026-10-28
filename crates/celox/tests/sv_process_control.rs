#![cfg(feature = "systemverilog")]
use celox::{SimBackend, Simulation};
use std::path::Path;

fn replay<B: SimBackend>(mut sim: Simulation<B>) {
    sim.run_until(1).unwrap();
    assert_eq!(sim.get(sim.signal("checks")), 1u8.into());
    let checkpoint = sim.checkpoint().unwrap();
    assert!(
        sim.save_state()
            .unwrap_err()
            .to_string()
            .contains("process handles")
    );
    for _ in 0..3 {
        sim.restore(&checkpoint).unwrap();
        sim.run_until(8).unwrap();
        assert_eq!(sim.get(sim.signal("checks")), 7u8.into());
    }
}
#[test]
fn checkpoint_replays_suspended_process_handles() {
    let source = r#"`timescale 1ns/1ns
    module Top(output int checks);
      process p;
      initial begin
       fork begin p=process::self(); #4; end join_none
       #1; p.suspend(); checks=(p.status()==process::SUSPENDED);
       #5; p.resume(); p.await();
       checks+=(p.status()==process::FINISHED)*2;
       checks+=($time==6)*4;
      end
    endmodule"#;
    for four_state in [false, true] {
        replay(
            Simulation::from_sv_sources(vec![(source, Path::new("checkpoint.sv"))], "Top")
                .four_state(four_state)
                .build_native()
                .unwrap(),
        );
        replay(
            Simulation::from_sv_sources(vec![(source, Path::new("checkpoint.sv"))], "Top")
                .four_state(four_state)
                .build_interpreter()
                .unwrap(),
        );
    }
}
#[test]
fn invalid_process_operations_report_errors() {
    for (body, message) in [
        ("process p; initial p.status();", "null process handle"),
        (
            "process p; initial begin p=process::self(); p.await(); end",
            "cannot await itself",
        ),
        (
            "process p; function void f(); p.suspend(); endfunction initial begin p=process::self(); f(); end",
            "function cannot suspend",
        ),
        (
            "process p; initial begin p=process::self(); p.set_randstate(\"invalid\"); end",
            "invalid process random state",
        ),
    ] {
        let source = format!("module Top; {body} endmodule");
        let mut sim = Simulation::from_sv_sources(vec![(&source, Path::new("invalid.sv"))], "Top")
            .build_native()
            .unwrap();
        let error = sim.run_until(0).expect_err(message);
        assert!(error.to_string().contains(message), "{error}");
    }
}
#[test]
fn process_construction_and_function_await_are_rejected() {
    for body in [
        "process p; initial p=new();",
        "std::process p; initial p=new();",
        "process p,q; initial p=new q;",
        "class Child extends process; endclass",
        "process p; function void f(); p.await(); endfunction initial f();",
        "process p; initial p.kill(3);",
        "process p; int x; initial x=p.kill();",
        "typedef int process; int x; initial x=process::self();",
    ] {
        let source = format!("module Top; {body} endmodule");
        assert!(
            Simulation::from_sv_sources(vec![(&source, Path::new("illegal.sv"))], "Top")
                .build_native()
                .is_err(),
            "{source}"
        );
    }
}

#[test]
fn final_process_identity_and_finished_random_state() {
    let source = r#"module Top(output int checks);
      process p;
      string state;
      initial begin p=process::self(); p.srandom(42); end
      final begin
        process mine;
        mine=process::self();
        checks=(mine.status()==process::RUNNING && p.status()==process::FINISHED);
        state=p.get_randstate(); p.set_randstate(state); p.srandom(8);
        checks+=(state.len()>0 && p.status()==process::FINISHED)*2;
      end
    endmodule"#;
    let mut sim = Simulation::from_sv_sources(vec![(source, Path::new("final.sv"))], "Top")
        .build_native()
        .unwrap();
    sim.run_until(0).unwrap();
    sim.finish().unwrap();
    assert_eq!(sim.get(sim.signal("checks")), 3u8.into());
}
#[test]
fn ordinary_process_names_keep_their_scope() {
    for source in [
        "module Top(output int q); class C #(type T=std::process); T p; function int get(); p=std::process::self(); return p.status();endfunction endclass typedef C#(std::process) Alias; Alias x; initial begin x=new();q=x.get()+7;end endmodule",
        "module Top(output int q); std::process p; std::process::state s; initial begin p=std::process::self(); s=p.status(); q=(s==std::process::RUNNING)+7;end endmodule",
        "module Top(output int q); class process; endclass initial q=(std::process::self().status()==std::process::RUNNING)+7; endmodule",
        "module Top(output int q); typedef process P; initial q=((P::self()).status()==P::RUNNING)+7; endmodule",
        "package process; parameter int value=8; endpackage module Top(output int q); initial q=process::value; endmodule",
        "package process; typedef bit[63:0] state; endpackage module Top(output int q); std::process explicit_handle; process::state wide; std::process::state builtin_state; initial begin explicit_handle=std::process::self(); builtin_state=explicit_handle.status(); q=($bits(wide)==64 && $bits(builtin_state)==32 && builtin_state==std::process::RUNNING)+7;end endmodule",
        "package process; parameter int RUNNING=8; endpackage module Top(output int q); std::process p; initial begin p=std::process::self(); q=(p.status()==std::process::RUNNING)+7;end endmodule",
        "module Top(output int q); class process; function int status(); return 8; endfunction endclass process p; initial begin p=new();q=p.status();end endmodule",
    ] {
        let mut sim = Simulation::from_sv_sources(vec![(source, Path::new("names.sv"))], "Top")
            .build_native()
            .unwrap();
        sim.run_until(0).unwrap();
        assert_eq!(sim.get(sim.signal("q")), 8u8.into());
    }
}

#[test]
fn four_state_random_state_roundtrips() {
    fn verify<B: SimBackend>(mut sim: Simulation<B>) {
        sim.run_until(0).unwrap();
        assert_eq!(sim.get(sim.signal("checks")), 1u8.into());
    }
    let source = r#"module Top(output int checks);
      process p; string state; int unsigned a,b;
      initial begin p=process::self(); p.srandom(-1); state=p.get_randstate();
       a=$urandom; p.set_randstate(state); b=$urandom; checks=(a==b && state.len()>0);
      end
    endmodule"#;
    for four_state in [false, true] {
        verify(
            Simulation::from_sv_sources(vec![(source, Path::new("rng.sv"))], "Top")
                .four_state(four_state)
                .build_native()
                .unwrap(),
        );
        verify(
            Simulation::from_sv_sources(vec![(source, Path::new("rng.sv"))], "Top")
                .four_state(four_state)
                .build_interpreter()
                .unwrap(),
        );
    }
}
#[test]
fn process_calls_in_argument_defaults_get_an_activation() {
    let source = r#"module Top(output int checks);
      task automatic t(input process p=process::self()); checks=(p.status()==process::RUNNING); endtask
      initial t();
    endmodule"#;
    let mut sim = Simulation::from_sv_sources(vec![(source, Path::new("default.sv"))], "Top")
        .build_native()
        .unwrap();
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(sim.signal("checks")), 1u8.into());
}

#[test]
fn packed_random_state_arguments_are_converted_to_strings() {
    fn verify<B: SimBackend>(mut sim: Simulation<B>) {
        sim.run_until(0).unwrap();
        assert_eq!(sim.get(sim.signal("checks")), 31u8.into());
    }
    // RNG-state text is implementation-defined (18.13.4); this is a Celox API test.
    let source = r#"module Top(output int checks);
      localparam STATE="celox-process-rng:10000002a";
      process p;
      bit [$bits(STATE)-1:0] packed_state;
      string state;
      int unsigned expected;
      initial begin
       p=process::self(); p.srandom(42); state=p.get_randstate();
       packed_state=STATE; expected=$urandom;
       p.set_randstate("celox-process-rng:10000002a"); checks=($urandom==expected);
       p.set_randstate(STATE); checks+=($urandom==expected)*2;
       p.set_randstate(packed_state); checks+=($urandom==expected)*4;
       p.set_randstate({"celox-process-rng:","10000002a"}); checks+=($urandom==expected)*8;
       p.set_randstate(state); checks+=($urandom==expected)*16;
      end
    endmodule"#;
    for four_state in [false, true] {
        verify(
            Simulation::from_sv_sources(vec![(source, Path::new("rng_literals.sv"))], "Top")
                .four_state(four_state)
                .build_native()
                .unwrap(),
        );
        verify(
            Simulation::from_sv_sources(vec![(source, Path::new("rng_literals.sv"))], "Top")
                .four_state(four_state)
                .build_interpreter()
                .unwrap(),
        );
    }
}

#[test]
fn killing_program_processes_finishes_and_runs_finals() {
    fn verify<B: SimBackend>(mut sim: Simulation<B>, finish_time: u8) {
        sim.run_until(2).unwrap();
        assert!(!sim.is_finished());
        sim.run_until(10).unwrap();
        assert!(sim.is_finished());
        assert_eq!(sim.get(sim.signal("ticks")), (finish_time / 2).into());
        assert_eq!(sim.get(sim.signal("final_time")), finish_time.into());
        assert_eq!(sim.get(sim.signal("after_kill")), 0u8.into());
    }
    for (control, program, finish_time) in [
        ("", "p=process::self(); #3; p.kill(); after_kill=1;", 3),
        (
            "import handles::victim; initial begin #3; victim.kill(); end",
            "handles::victim=process::self(); #100;",
            3,
        ),
        (
            "",
            "p=process::self(); fork begin #100; end join_none #3; p.kill(); after_kill=1;",
            3,
        ),
        (
            "Other other();",
            "p=process::self(); #3; p.kill(); after_kill=1;",
            5,
        ),
    ] {
        let source = format!(
            r#"`timescale 1ns/1ns
          package handles; process victim; endpackage
          module Top(output int ticks, final_time, after_kill);
            Tb tb(after_kill);
            {control}
            initial begin ticks=0; forever #2 ticks++; end
            final final_time=$time;
          endmodule
          program Tb(output int after_kill);
            process p;
            initial begin after_kill=0; {program} end
          endprogram
          program Other;
            initial #5;
          endprogram"#
        );
        for four_state in [false, true] {
            let build = || {
                Simulation::from_sv_sources(vec![(&source, Path::new("program_kill.sv"))], "Top")
                    .four_state(four_state)
            };
            verify(build().build_native().unwrap(), finish_time);
            verify(build().build_interpreter().unwrap(), finish_time);
            #[cfg(feature = "cranelift")]
            verify(build().build_cranelift().unwrap(), finish_time);
            #[cfg(feature = "wasm-runtime")]
            verify(build().build_wasm().unwrap(), finish_time);
        }
    }
}

#[test]
fn program_exit_notifies_observers_after_checkpoint_restore() {
    fn verify<B: SimBackend>(mut sim: Simulation<B>) {
        sim.run_until(2).unwrap();
        assert!(!sim.is_finished());
        let checkpoint = sim.checkpoint().unwrap();
        for _ in 0..2 {
            sim.restore(&checkpoint).unwrap();
            sim.run_until(10).unwrap();
            assert!(sim.is_finished());
            for name in ["awaited_at", "observed_at"] {
                assert_eq!(sim.get(sim.signal(name)), 3u8.into(), "{name}");
            }
            assert_eq!(sim.get(sim.signal("final_time")), 4u8.into());
            assert_eq!(sim.get(sim.signal("late")), 0u8.into());
        }
    }
    for descendant in [false, true] {
        for suspended in [false, true] {
            let capture = "handles::victim=process::self(); #30; late=1;";
            let producer = if descendant {
                format!("fork begin {capture} end join_none #30;")
            } else {
                capture.to_owned()
            };
            let suspend = if suspended {
                "#2; victim.suspend();"
            } else {
                ""
            };
            let source = format!(
                r#"`timescale 1ns/1ns
              package handles; process victim; endpackage
              module Top(output int awaited_at, observed_at, late, final_time);
                Producer producer(late);
                Consumer consumer(awaited_at, observed_at);
                final final_time=$time;
              endmodule
              program Producer(output int late);
                import handles::victim;
                initial begin late=0; {producer} end
                initial begin {suspend} #({end_delay}); $exit; end
              endprogram
              program Consumer(output int awaited_at, observed_at);
                import handles::victim;
                initial begin awaited_at=0; #1; victim.await(); awaited_at=$time; #1; end
                initial begin observed_at=0; #1; wait(victim.status()==process::FINISHED); observed_at=$time; #1; end
              endprogram"#,
                end_delay = if suspended { 1 } else { 3 }
            );
            for four_state in [false, true] {
                let build = || {
                    Simulation::from_sv_sources(
                        vec![(&source, Path::new("exit_observers.sv"))],
                        "Top",
                    )
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
    }
}

#[test]
fn final_kill_keeps_running_cleanup_on_host_and_hdl_finish() {
    fn verify<B: SimBackend>(mut sim: Simulation<B>, host_finish: bool) {
        if host_finish {
            sim.run_until(3).unwrap();
            sim.finish().unwrap();
        } else {
            sim.run_until(5).unwrap();
        }
        assert!(sim.is_finished());
        assert_eq!(sim.get(sim.signal("first")), 1u8.into());
        assert_eq!(sim.get(sim.signal("second")), 1u8.into());
        let output = sim
            .drain_runtime_events()
            .into_iter()
            .filter_map(|event| {
                if let celox::RuntimeEvent::Write { message } = event {
                    Some(message)
                } else {
                    None
                }
            })
            .flatten()
            .collect::<Vec<_>>();
        assert_eq!(output, b"cleanup;cleanup;");
    }
    for host_finish in [false, true] {
        let finish = if host_finish {
            ""
        } else {
            "initial #3 $finish;"
        };
        let source = format!(
            r#"`timescale 1ns/1ns
          package handles; process victim; endpackage
          module Top(output bit first, second);
            import handles::victim;
            Tb tb();
            {finish}
            final begin victim.kill(); first=1; $write("cleanup;"); end
            final begin second=1; $write("cleanup;"); end
          endmodule
          program Tb;
            initial begin handles::victim=process::self(); #30; end
          endprogram"#
        );
        for four_state in [false, true] {
            let build = || {
                Simulation::from_sv_sources(vec![(&source, Path::new("final_kill.sv"))], "Top")
                    .four_state(four_state)
            };
            verify(build().build_native().unwrap(), host_finish);
            verify(build().build_interpreter().unwrap(), host_finish);
            #[cfg(feature = "cranelift")]
            verify(build().build_cranelift().unwrap(), host_finish);
            #[cfg(feature = "wasm-runtime")]
            verify(build().build_wasm().unwrap(), host_finish);
        }
    }
}

#[test]
fn final_phase_shares_rng_with_a_draw_only_final() {
    fn verify<B: SimBackend>(mut sim: Simulation<B>, pause: bool) {
        sim.run_until(2).unwrap();
        if pause {
            assert!(sim.is_stopped());
            assert!(!sim.is_finished());
            sim.step().unwrap();
        }
        assert!(sim.is_finished());
        let count = sim.get(sim.signal("count"));
        assert!(count == 2u8.into() || count == 3u8.into());
        let expected = sim.get(sim.signal("expected"));
        let expected = if count == 2u8.into() {
            expected >> 32usize
        } else {
            expected
        };
        assert_eq!(sim.get(sim.signal("samples")), expected);
        assert_eq!(sim.get(sim.signal("identity_checks")), 2u8.into());
    }
    // The draw-only final may run anywhere. It skips before the first visit,
    // otherwise it must continue the same stream; recorded samples retain order.
    let source = r#"`timescale 1ns/1ns
      module Top(output bit [95:0] samples, expected, output int count, identity_checks);
       process shared;
       function automatic void visit();
        process p;
        p=process::self();
        if(shared==null) begin shared=p; p.srandom(42); end
        identity_checks+=(p==shared && shared.status()==process::RUNNING);
        samples=(samples<<32)|96'($urandom); count++;
       endfunction
       initial begin
        process reference_process;
        samples=0; count=0; identity_checks=0;
        reference_process=process::self(); reference_process.srandom(42);
        expected=0;
        repeat(3) expected=(expected<<32)|96'($urandom);
        #1; $finish;
       end
       final begin visit(); __STOP__ end
       final if(shared!=null) begin samples=(samples<<32)|96'($urandom); count++; end
       final visit();
      endmodule"#;
    for pause in [false, true] {
        let source = source.replace("__STOP__", if pause { "$stop;" } else { "" });
        for four_state in [false, true] {
            let build = || {
                Simulation::from_sv_sources(vec![(&source, Path::new("final_stream.sv"))], "Top")
                    .four_state(four_state)
            };
            verify(build().build_native().unwrap(), pause);
            verify(build().build_interpreter().unwrap(), pause);
            #[cfg(feature = "cranelift")]
            verify(build().build_cranelift().unwrap(), pause);
            #[cfg(feature = "wasm-runtime")]
            verify(build().build_wasm().unwrap(), pause);
        }
    }
}

#[test]
fn suspended_iff_waits_discard_host_pulses_before_rearming() {
    let source = r#"`timescale 1ns/1ns
    module Top(input bit clk, en, output int seen);
      process global_waiter, local_waiter;
      initial begin
        automatic bit enabled = 1;
        fork
          begin global_waiter=process::self(); @(posedge clk iff en); seen |= 1; end
          begin local_waiter=process::self(); @(posedge clk iff enabled); seen |= 2; end
        join_none
        #1; global_waiter.suspend(); local_waiter.suspend();
        #2; global_waiter.resume(); local_waiter.resume();
      end
    endmodule"#;
    fn verify<B: SimBackend>(mut sim: Simulation<B>) {
        sim.run_until(1).unwrap();
        let checkpoint = sim.checkpoint().unwrap();
        for _ in 0..2 {
            sim.restore(&checkpoint).unwrap();
            let clk = sim.signal("clk");
            let en = sim.signal("en");
            let seen = sim.signal("seen");
            sim.modify(|io| {
                io.set(en, 1u8);
                io.set(clk, 1u8);
                io.set(clk, 0u8);
            })
            .unwrap();
            sim.run_until(2).unwrap();
            assert_eq!(sim.get(seen), 0u8.into());
            sim.run_until(3).unwrap();
            assert_eq!(sim.get(seen), 0u8.into(), "resume must discard old pulses");
            sim.modify(|io| {
                io.set(en, 0u8);
                io.set(clk, 1u8);
                io.set(clk, 0u8);
            })
            .unwrap();
            sim.run_until(3).unwrap();
            assert_eq!(sim.get(seen), 2u8.into(), "use the activation-local gate");
            sim.modify(|io| {
                io.set(en, 1u8);
                io.set(clk, 1u8);
                io.set(clk, 0u8);
            })
            .unwrap();
            sim.run_until(4).unwrap();
            assert_eq!(sim.get(seen), 3u8.into(), "rearm the module gate counter");
        }
    }
    for four_state in [false, true] {
        let build = || {
            Simulation::from_sv_sources(vec![(source, Path::new("suspended_iff.sv"))], "Top")
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

fn resume_review_source(name: &str) -> String {
    let case = celox_test_suite::sv::case(&format!("fork_control::{name}")).unwrap();
    let design = case.design();
    assert_eq!(design.sources.len(), 1);
    format!("`timescale 1ns/1ns\n{}", design.sources[0].text)
}

fn checkpoint_replays_resume_source(source: &str, warmup: u64, steps: &[(u64, &[(&str, u64)])]) {
    fn verify<B: SimBackend>(mut sim: Simulation<B>, warmup: u64, steps: &[(u64, &[(&str, u64)])]) {
        sim.run_until(warmup).unwrap();
        let checkpoint = sim.checkpoint().unwrap();
        for _ in 0..2 {
            sim.restore(&checkpoint).unwrap();
            for &(time, signals) in steps {
                sim.run_until(time).unwrap();
                for &(name, expected) in signals {
                    assert_eq!(
                        sim.get(sim.signal(name)),
                        expected.into(),
                        "{name} at {time}"
                    );
                }
            }
        }
    }
    for four_state in [false, true] {
        let build = || {
            Simulation::from_sv_sources(vec![(source, Path::new("resume_review.sv"))], "Top")
                .four_state(four_state)
        };
        verify(build().build_native().unwrap(), warmup, steps);
        verify(build().build_interpreter().unwrap(), warmup, steps);
        #[cfg(feature = "cranelift")]
        verify(build().build_cranelift().unwrap(), warmup, steps);
        #[cfg(feature = "wasm-runtime")]
        verify(build().build_wasm().unwrap(), warmup, steps);
    }
}

#[test]
fn checkpoint_replays_nested_level_wait_resume() {
    let source = resume_review_source("process_nested_calls_preserve_level_wait_resume");
    for defaults in [false, true] {
        let source = if defaults {
            source.replace("process worker;", "process worker; function void capture(process p=process::self()); worker=p; endfunction")
                .replace("worker=process::self();", "capture();")
        } else {
            source.clone()
        };
        checkpoint_replays_resume_source(
            &source,
            2,
            &[(6, &[("count", 2), ("resumed_at", 4), ("checks", 1)])],
        );
    }
}

#[test]
fn checkpoint_replays_resampled_automatic_event_resume() {
    let source = resume_review_source("process_resume_resamples_automatic_event_expression");
    checkpoint_replays_resume_source(
        &source,
        1,
        &[
            (5, &[("hits", 0), ("event_at", 0)]),
            (7, &[("hits", 1), ("event_at", 6)]),
        ],
    );
}

#[test]
fn checkpoint_keeps_program_exit_processes_finished_after_resume() {
    let source = resume_review_source("process_resume_after_program_exit_is_noop");
    checkpoint_replays_resume_source(
        &source,
        2,
        &[(9, &[("late", 0), ("checks", 3), ("ended_state", 0)])],
    );
}

#[test]
fn class_process_handle_stays_two_state_beside_four_state_properties() {
    let source = r#"`timescale 1ns/1ns
module Top(output int checks);
  class Holder;
    logic [3:0] before_handle;
    process p;
    logic [3:0] after_handle;
  endclass
  Holder h;
  initial begin
    h = new;
    checks = (h.p == null);
    h.before_handle = 4'bx1z0;
    h.after_handle = 4'bz0x1;
    h.p = process::self();
    checks += (h.p.status() == process::RUNNING) * 2;
    checks += (h.before_handle === 4'bx1z0) * 4;
    checks += (h.after_handle === 4'bz0x1) * 8;
    #1;
    checks += (h.p.status() == process::RUNNING) * 16;
  end
endmodule"#;
    fn check<B: SimBackend>(mut sim: Simulation<B>) {
        sim.run_until(0).unwrap();
        assert_eq!(sim.get(sim.signal("checks")), 15u32.into());
        let checkpoint = sim.checkpoint().unwrap();
        for _ in 0..2 {
            sim.restore(&checkpoint).unwrap();
            sim.run_until(1).unwrap();
            assert_eq!(sim.get(sim.signal("checks")), 31u32.into());
        }
    }
    let build = || {
        Simulation::from_sv_sources(vec![(source, Path::new("class_process.sv"))], "Top")
            .four_state(true)
    };
    check(build().build_native().unwrap());
    check(build().build_interpreter().unwrap());
}
