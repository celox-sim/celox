#![allow(
    clippy::disallowed_types,
    reason = "test-only: watchdog flags shared with the thread that runs the simulation"
)]
use std::path::Path;

use crate::{SchedulerMode, Simulation};

const SOURCE: &str = r#"
        `timescale 1ns/1ns
        module Top(output logic [7:0] count, output logic flag);
            logic clk = 1'b0;
            always #5 clk = ~clk;
            initial begin
                count = 8'd0;
                flag = $test$plusargs("fast");
                forever @(posedge clk) count <= count + (flag ? 8'd2 : 8'd1);
            end
        endmodule
    "#;

fn build(mode: SchedulerMode) -> Simulation {
    Simulation::from_sv_sources(vec![(SOURCE, Path::new("top.sv"))], "Top")
        .scheduler_mode(mode)
        .build()
        .unwrap()
}

#[test]
fn differential_ignores_storage_padding_but_checks_each_value_and_mask_plane() {
    use crate::SimBackend;
    let source = "module Top(output logic [12:0] scalar, output logic [12:0] words[3]); endmodule";
    for four_state in [false, true] {
        let make = || {
            Simulation::from_sv_sources(vec![(source, Path::new("differential-padding.sv"))], "Top")
                .four_state(four_state)
                .scheduler_mode(SchedulerMode::Reference)
                .build_interpreter()
                .unwrap()
        };
        let mut primary = make();
        let reference = make();
        for name in ["scalar", "words"] {
            let signal = primary.signal(name);
            let (elements, stride, plane_size) = signal.array_layout.map_or((1, 2, 2), |array| {
                (array.element_count, array.element_stride, array.plane_size)
            });
            for plane in 0..if four_state { 2 } else { 1 } {
                for element in 0..elements {
                    let offset = signal.offset + plane * plane_size + element * stride + 1;
                    let backend = &mut primary.simulator.backend;
                    let (pointer, len) = backend.memory_as_ptr();
                    assert!(offset < len);
                    // SAFETY: the allocation includes this element's upper byte.
                    let old = unsafe { *pointer.add(offset) };
                    backend.write_memory(offset, &[old ^ 0xe0]);
                    assert_eq!(
                        super::first_difference(backend, &reference.simulator.backend, &[]),
                        None,
                        "padding of {name}[{element}], plane {plane}"
                    );
                    backend.write_memory(offset, &[old ^ 0xe1]);
                    assert_eq!(
                        super::first_difference(backend, &reference.simulator.backend, &[]),
                        Some(offset),
                        "declared bit of {name}[{element}], plane {plane}"
                    );
                    backend.write_memory(offset, &[old]);
                }
            }
        }
    }
}

#[test]
fn differential_mode_checks_every_step_against_the_reference() {
    let mut sim = build(SchedulerMode::Differential);
    sim.run_until(100).unwrap();
    // A host write between runs reaches the shadow too.
    let count = sim.signal("count");
    sim.modify(|io| io.set(count, 100u8)).unwrap();
    sim.run_until(200).unwrap();
    let mut reference = build(SchedulerMode::Reference);
    reference.run_until(100).unwrap();
    reference.modify(|io| io.set(count, 100u8)).unwrap();
    reference.run_until(200).unwrap();
    assert_eq!(sim.get(count), reference.get(count));
    let stats = sim.scheduler_stats();
    assert_eq!(stats.mode, SchedulerMode::Differential);
    assert!(stats.checked_steps > 0);
    assert_eq!(reference.scheduler_stats().checked_steps, 0);
}

#[test]
fn differential_mode_mirrors_host_writes_in_a_stopped_slot() {
    let source = r#"
            `timescale 1ns/1ns
            module Top(input logic [7:0] a, output logic [7:0] seen);
                logic [7:0] y;
                assign y = a + 8'd1;
                initial begin
                    seen = 8'd0;
                    #5 $stop;
                    seen = y;
                end
            endmodule
        "#;
    let mut sim = Simulation::from_sv_sources(vec![(source, Path::new("top.sv"))], "Top")
        .scheduler_mode(SchedulerMode::Differential)
        .build()
        .unwrap();
    sim.run_until(10).unwrap();
    assert!(sim.is_stopped());
    let a = sim.signal("a");
    sim.modify(|io| io.set(a, 3u8)).unwrap();
    sim.run_until(10).unwrap();
    let seen = sim.signal("seen");
    assert_eq!(sim.get(seen), 4u8.into());
    assert!(sim.scheduler_stats().checked_steps >= 2);
}

#[cfg(any(
    all(target_arch = "x86_64", not(feature = "arm64-codegen")),
    all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
))]
#[test]
fn the_shadow_runs_its_own_compiled_events() {
    use crate::backend::{EventHandle as _, SimBackend as _};
    let source = r#"
            module Top(input logic clk, output logic [7:0] count);
                initial count = 8'd0;
                always_ff @(posedge clk) count <= count + 8'd1;
            endmodule
        "#;
    let mut sim = Simulation::from_sv_sources(vec![(source, Path::new("top.sv"))], "Top")
        .scheduler_mode(SchedulerMode::Differential)
        .build_native()
        .unwrap();
    sim.add_clock("clk", 10, 5);
    sim.run_until(100).unwrap();
    let shadow = sim.shadow.as_ref().unwrap();
    let own = shadow.simulator.backend.id_to_event_slice();
    let primary = sim.simulator.backend.id_to_event_slice();
    let events = shadow
        .state
        .export_schedule(&shadow.simulator.backend)
        .events;
    assert!(!events.is_empty());
    for event in events {
        let id = event.event_ref.id();
        assert_eq!(event.event_ref.func as usize, own[id].func as usize);
        assert_ne!(event.event_ref.func as usize, primary[id].func as usize);
    }
}

#[test]
fn the_compiled_scheduler_runs_delay_only_kernels() {
    let source = r#"
            `timescale 1ns/1ns
            module Top(output logic [7:0] count);
                logic clk = 1'b0;
                always #5 clk = ~clk;
                initial count = 8'd0;
                always_ff @(posedge clk) count <= count + 8'd1;
            endmodule
        "#;
    for mode in [
        SchedulerMode::Compiled,
        SchedulerMode::Differential,
        SchedulerMode::DifferentialRuns,
        SchedulerMode::Reference,
    ] {
        let mut sim = Simulation::from_sv_sources(vec![(source, Path::new("top.sv"))], "Top")
            .scheduler_mode(mode)
            .build()
            .unwrap();
        sim.run_until(1000).unwrap();
        let count = sim.signal("count");
        assert_eq!(sim.get(count), 100u8.into(), "{mode:?}");
        let stats = sim.scheduler_stats();
        if mode == SchedulerMode::Reference {
            assert_eq!(stats.compiled_slots, 0);
            assert_eq!(stats.compiled_runs, 0);
            assert!(stats.reference_steps > 100);
        } else {
            assert!(stats.compiled_slots > 0, "{mode:?}: {stats:?}");
            assert!(stats.compiled_runs > 0, "{stats:?}");
            assert!(stats.reference_steps < 10, "{stats:?}");
        }
        assert_eq!(stats.native_service_calls, 0, "{stats:?}");
        match mode {
            SchedulerMode::Differential => {
                assert!(stats.checked_steps >= stats.compiled_slots)
            }
            // One run takes all the slots up to the target.
            SchedulerMode::DifferentialRuns => {
                assert!(stats.checked_steps < stats.compiled_slots, "{stats:?}");
                assert_eq!(stats.compiled_runs, 1, "{stats:?}");
            }
            _ => {}
        }
    }
}

/// A kernel whose writes no event domain listens to: the scheduler
/// runs its slots without any domain to dispatch.
#[test]
fn the_compiled_scheduler_runs_kernels_without_event_writes() {
    let source = r#"
            `timescale 1ns/1ns
            module Top(output logic [7:0] seen);
                logic tick = 1'b0;
                logic [7:0] count = 8'd0;
                always #5 begin
                    tick = ~tick;
                    count = count + 8'd1;
                end
                assign seen = count + {7'd0, tick};
            endmodule
        "#;
    for mode in [SchedulerMode::Differential, SchedulerMode::DifferentialRuns] {
        let mut sim = Simulation::from_sv_sources(vec![(source, Path::new("top.sv"))], "Top")
            .scheduler_mode(mode)
            .build()
            .unwrap();
        sim.run_until(52).unwrap();
        let seen = sim.signal("seen");
        assert_eq!(sim.get(seen), 10u8.into(), "{mode:?}");
        assert!(sim.scheduler_stats().compiled_slots > 0, "{mode:?}");
    }
}

const MODES: [SchedulerMode; 3] = [
    SchedulerMode::Compiled,
    SchedulerMode::Differential,
    SchedulerMode::DifferentialRuns,
];

fn build_in(source: &str, mode: SchedulerMode) -> Simulation {
    Simulation::from_sv_sources(vec![(source, Path::new("top.sv"))], "Top")
        .scheduler_mode(mode)
        .build()
        .unwrap()
}

/// A `$finish` in a domain the scheduler inlines, with two sites.
#[test]
fn compiled_slots_end_at_a_finish_in_a_domain() {
    let source = r#"
            `timescale 1ns/1ns
            module Top(input logic err, output logic [7:0] n);
                logic clk = 1'b0;
                always #5 clk = ~clk;
                initial n = 8'd0;
                always_ff @(posedge clk) begin
                    n <= n + 8'd1;
                    if (n == 8'd3) $finish;
                    if (err) $finish;
                end
            endmodule
        "#;
    let mut reference = build_in(source, SchedulerMode::Reference);
    reference.run_until(100).unwrap();
    assert!(reference.is_finished());
    for mode in MODES {
        let mut sim = build_in(source, mode);
        sim.run_until(100).unwrap();
        assert!(sim.is_finished(), "{mode:?}");
        assert_eq!(sim.time(), reference.time(), "{mode:?}");
        let n = sim.signal("n");
        assert_eq!(sim.get(n), reference.get(n), "{mode:?}");
        assert!(sim.scheduler_stats().compiled_slots > 0, "{mode:?}");
    }
}

/// A compiled kernel's `$finish` leaves the state the reference leaves:
/// a kernel due in the same slot that did not run yet is still ready,
/// combinational logic holds the last slot's settle, and a state file
/// taken afterwards is the same.
#[test]
fn compiled_slots_end_at_a_kernel_finish_as_the_reference_does() {
    let clock = "always #5 clk = ~clk;";
    for finish in ["initial #27 $finish;", "initial #30 $finish;"] {
        for order in [[finish, clock], [clock, finish]] {
            let source = format!(
                "`timescale 1ns/1ns
                    module Top(output logic [7:0] count, output logic [7:0] twice);
                        logic clk = 1'b0;
                        {}
                        {}
                        initial count = 8'd0;
                        always_ff @(posedge clk) count <= count + 8'd1;
                        assign twice = count * 8'd2;
                    endmodule",
                order[0], order[1]
            );
            let mut reference = build_in(&source, SchedulerMode::Reference);
            reference.run_until(100).unwrap();
            let expected = reference.save_state().unwrap();
            for mode in MODES {
                let mut sim = build_in(&source, mode);
                sim.run_until(100).unwrap();
                assert!(sim.is_finished(), "{mode:?} {order:?}");
                assert!(sim.scheduler_stats().compiled_slots > 0, "{mode:?}");
                assert_eq!(sim.save_state().unwrap(), expected, "{mode:?} {order:?}");
                assert!(sim.checkpoint().is_ok());
            }
        }
    }
}

/// A kernel may wake at the last representable time.
#[test]
fn compiled_kernels_may_wake_at_the_last_time() {
    let source = r#"
            `timescale 1fs/1fs
            module Top(output logic [7:0] x);
                initial begin
                    x = 8'd0;
                    #10 x = 8'd1;
                    #(64'd18446744073709551605) x = 8'd2;
                end
            endmodule
        "#;
    for mode in MODES {
        let mut sim = build_in(source, mode);
        sim.run_until(100).unwrap();
        let x = sim.signal("x");
        assert_eq!(sim.get(x), 1u8.into(), "{mode:?}");
        assert_eq!(sim.next_event_time(), Some(u64::MAX), "{mode:?}");
        sim.run_until(u64::MAX).unwrap();
        assert_eq!(sim.get(x), 2u8.into(), "{mode:?}");
    }
}

/// Run `source` to `end` in every mode and compare the state files with
/// the reference's; at least one slot must run compiled.
fn agrees_with_the_reference(source: &str, end: u64) {
    let mut reference = build_in(source, SchedulerMode::Reference);
    reference.run_until(end).unwrap();
    let expected = reference.save_state().unwrap();
    for mode in MODES {
        let mut sim = build_in(source, mode);
        sim.run_until(end).unwrap();
        assert_eq!(sim.save_state().unwrap(), expected, "{mode:?}");
        let stats = sim.scheduler_stats();
        assert!(stats.compiled_slots > 0, "{mode:?}: {stats:?}");
    }
}

/// A kernel waiting for a clock edge writes what the domain of that edge
/// samples, blocking and nonblocking.
#[test]
fn clock_kernels_run_in_compiled_slots() {
    for assign in ["=", "<="] {
        let source = format!(
            "`timescale 1ns/1ns
                module Top(output logic [7:0] sum);
                    logic clk = 1'b0;
                    logic [7:0] stim = 8'd0;
                    logic [7:0] r = 8'd0;
                    always #5 clk = ~clk;
                    initial forever @(posedge clk) stim {assign} stim + 8'd3;
                    always_ff @(posedge clk) r <= r + stim;
                    assign sum = r;
                endmodule"
        );
        agrees_with_the_reference(&source, 1000);
    }
}

/// Kernels woken in the middle of a pass, declared before and after the
/// clock kernels, and waits for falling and any edges.
#[test]
fn clock_kernels_wake_in_the_reference_order() {
    let source = r#"
            `timescale 1ns/1ns
            module Top(output logic [7:0] seen);
                logic a = 1'b0;
                logic b = 1'b0;
                logic [7:0] log = 8'd0;
                logic [7:0] acc = 8'd0;
                initial forever @(posedge a) log = log * 8'd3 + 8'd1;
                always #5 a = ~a;
                initial forever @(negedge a) log = log * 8'd5 + 8'd2;
                always #5 b = ~b;
                initial forever @(b) log = log * 8'd7 + 8'd3;
                initial forever @(posedge b) acc <= acc + log;
                assign seen = log + acc;
            endmodule
        "#;
    agrees_with_the_reference(source, 200);
}

/// A clock kernel's `$finish` ends a compiled slot as the reference's.
#[test]
fn clock_kernels_finish_compiled_slots() {
    let source = r#"
            `timescale 1ns/1ns
            module Top(output logic [7:0] n);
                logic clk = 1'b0;
                always #5 clk = ~clk;
                initial begin
                    n = 8'd0;
                    forever @(posedge clk) begin
                        n = n + 8'd1;
                        if (n == 8'd7) $finish;
                    end
                end
            endmodule
        "#;
    agrees_with_the_reference(source, 1000);
}

/// A compiled kernel that displays at every edge. Its runs of the
/// compiled scheduler return when their events fill a batch.
const DISPLAYS: &str = r#"
        `timescale 1ns/1ns
        module Top(output logic [31:0] n);
            logic clk = 1'b0;
            always #5 clk = ~clk;
            initial begin
                n = 0;
                forever @(posedge clk) begin
                    n = n + 1;
                    $display("n=%0d", n);
                end
            end
        endmodule
    "#;

/// A drain on another thread that paces the simulation takes every
/// event of runs of the compiled scheduler longer than the ring buffer.
#[test]
fn a_paced_drain_takes_every_event_of_compiled_runs() {
    let mut sim = build_in(DISPLAYS, SchedulerMode::Compiled);
    let drain = sim.runtime_event_drain().expect("a ring buffer");
    drain.set_backpressure(true);
    let done = std::sync::atomic::AtomicBool::new(false);
    let displays = std::thread::scope(|scope| {
        let reader = scope.spawn(|| {
            let mut drain = drain;
            let mut displays = Vec::new();
            loop {
                let finished = done.load(std::sync::atomic::Ordering::Acquire);
                for event in drain.drain() {
                    match event {
                        crate::RuntimeEvent::Display { message } => displays.push(message),
                        other => panic!("unexpected event {other:?}"),
                    }
                }
                if finished {
                    return displays;
                }
                drain.wait(std::time::Duration::from_secs(1));
            }
        });
        sim.run_until(50_000).unwrap();
        done.store(true, std::sync::atomic::Ordering::Release);
        reader.thread().unpark();
        reader.join().unwrap()
    });
    let expected: Vec<Vec<u8>> = (1..=5000).map(|n| format!("n={n}").into_bytes()).collect();
    assert_eq!(displays, expected);
    assert!(sim.scheduler_stats().compiled_slots > 1000);
}

/// Stdout file requests bypass RuntimeEvent instructions but must obey
/// the same batch boundary and backpressure as ordinary displays.
#[test]
fn a_paced_drain_takes_every_native_stdout_file_record() {
    for operation in ["$fwrite(1,", "$fdisplay(1,"] {
        for four_state in [false, true] {
            let source = DISPLAYS.replace("$display(", operation);
            let mut sim =
                Simulation::from_sv_sources(vec![(source.as_str(), Path::new("stdout.sv"))], "Top")
                    .four_state(four_state)
                    .scheduler_mode(SchedulerMode::Compiled)
                    .build_native()
                    .unwrap();
            let mut drain = sim.runtime_event_drain().expect("a ring buffer");
            drain.set_backpressure(true);
            let done = std::sync::atomic::AtomicBool::new(false);
            let records = std::thread::scope(|scope| {
                let reader = scope.spawn(|| {
                    // Let an unbounded native run exceed the 1,024-record
                    // queue before the first read, then keep pacing it.
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    let mut records = Vec::new();
                    loop {
                        let finished = done.load(std::sync::atomic::Ordering::Acquire);
                        for event in drain.drain() {
                            match event {
                                crate::RuntimeEvent::Display { message }
                                | crate::RuntimeEvent::Write { message } => records.push(message),
                                other => panic!("unexpected event {other:?}"),
                            }
                        }
                        if finished {
                            return records;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                });
                let result = sim.run_until(50_000);
                done.store(true, std::sync::atomic::Ordering::Release);
                let records = reader.join().unwrap();
                result.unwrap();
                records
            });
            let expected: Vec<Vec<u8>> =
                (1..=5000).map(|n| format!("n={n}").into_bytes()).collect();
            assert_eq!(records, expected, "{operation} four_state={four_state}");
            assert!(sim.scheduler_stats().compiled_slots > 1000);
        }
    }
}

/// A paced drain also takes every event of single steps, however slowly
/// it reads.
#[test]
fn a_paced_drain_takes_every_event_of_single_steps() {
    let mut sim = build_in(DISPLAYS, SchedulerMode::Compiled);
    let drain = sim.runtime_event_drain().expect("a ring buffer");
    drain.set_backpressure(true);
    let done = std::sync::atomic::AtomicBool::new(false);
    let count = std::thread::scope(|scope| {
        let reader = scope.spawn(|| {
            let mut drain = drain;
            let mut count = 0;
            loop {
                let finished = done.load(std::sync::atomic::Ordering::Acquire);
                count += drain.drain().len();
                if finished {
                    return count;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        });
        while sim.time() < 25_000 {
            sim.step().unwrap();
        }
        done.store(true, std::sync::atomic::Ordering::Release);
        reader.join().unwrap()
    });
    assert_eq!(count, 2500);
}

/// A simulation waiting for a paced drain that does not drain goes on
/// once the drain stops pacing it.
#[test]
fn a_simulation_goes_on_when_its_drain_stops_pacing() {
    let mut sim = build_in(DISPLAYS, SchedulerMode::Compiled);
    let drain = sim.runtime_event_drain().expect("a ring buffer");
    drain.set_backpressure(true);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            std::thread::sleep(std::time::Duration::from_millis(100));
            drain.set_backpressure(false);
        });
        sim.run_until(25_000).unwrap();
    });
    assert_eq!(sim.time(), 25_000);
}

/// A paced drain also takes the events the host moves into its queue,
/// as it does around the processes of a design with deferred
/// assertions.
#[test]
fn a_paced_drain_takes_the_events_the_host_queues() {
    let source = r#"
            `timescale 1ns/1ns
            module Top(output logic [31:0] n);
                logic clk = 1'b0;
                always #5 clk = ~clk;
                initial begin
                    n = 0;
                    forever @(posedge clk) begin
                        n = n + 1;
                        $display("n=%0d", n);
                        assert #0 (n != 0);
                    end
                end
            endmodule
        "#;
    let mut sim = build_in(source, SchedulerMode::Compiled);
    let drain = sim.runtime_event_drain().expect("a ring buffer");
    drain.set_backpressure(true);
    let done = std::sync::atomic::AtomicBool::new(false);
    let displays = std::thread::scope(|scope| {
        let reader = scope.spawn(|| {
            let mut drain = drain;
            let mut displays = 0;
            loop {
                let finished = done.load(std::sync::atomic::Ordering::Acquire);
                for event in drain.drain() {
                    match event {
                        crate::RuntimeEvent::Display { .. } => displays += 1,
                        other => panic!("unexpected event {other:?}"),
                    }
                }
                if finished {
                    return displays;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        });
        sim.run_until(25_000).unwrap();
        done.store(true, std::sync::atomic::Ordering::Release);
        reader.join().unwrap()
    });
    assert_eq!(displays, 2500);
}

/// A paced drain gets the contents of the strings a design displays,
/// which the simulation's thread resolves from its heap.
#[test]
fn a_paced_drain_gets_displayed_strings() {
    let source = r#"
            `timescale 1ns/1ns
            module Top(output logic [31:0] n);
                logic clk = 1'b0;
                string s;
                always #5 clk = ~clk;
                initial begin
                    n = 0;
                    forever @(posedge clk) begin
                        n = n + 1;
                        s = $sformatf("n=%0d", n);
                        $display("%s", s);
                    end
                end
            endmodule
        "#;
    let mut sim = build_in(source, SchedulerMode::Compiled);
    let drain = sim.runtime_event_drain().expect("a ring buffer");
    drain.set_backpressure(true);
    let done = std::sync::atomic::AtomicBool::new(false);
    let displays = std::thread::scope(|scope| {
        let reader = scope.spawn(|| {
            let mut drain = drain;
            let mut displays = Vec::new();
            loop {
                let finished = done.load(std::sync::atomic::Ordering::Acquire);
                for event in drain.drain() {
                    match event {
                        crate::RuntimeEvent::Display { message } => displays.push(message),
                        other => panic!("unexpected event {other:?}"),
                    }
                }
                if finished {
                    return displays;
                }
                drain.wait(std::time::Duration::from_secs(1));
            }
        });
        sim.run_until(20_000).unwrap();
        done.store(true, std::sync::atomic::Ordering::Release);
        reader.thread().unpark();
        reader.join().unwrap()
    });
    // The records of the last step stay in the ring for the simulation.
    let mut displays = displays;
    for event in sim.drain_runtime_events() {
        if let crate::RuntimeEvent::Display { message } = event {
            displays.push(message);
        }
    }
    let expected: Vec<Vec<u8>> = (1..=2000).map(|n| format!("n={n}").into_bytes()).collect();
    assert_eq!(displays, expected);
}

/// A watched `run_slots` settles host writes that wake no process and
/// reports what they fail, as a watched run does.
#[test]
fn watched_run_slots_settle_idle_host_writes() {
    let source = r#"
            module Top(input logic [7:0] a);
                always_comb assert #0 (a != 8'd3) else $error("a is 3");
            endmodule
        "#;
    let mut sim = build_in(source, SchedulerMode::Compiled);
    sim.set_fail_on(Some(crate::Severity::Error));
    let a = sim.signal("a");
    sim.modify(|io| io.set(a, 3u8)).unwrap();
    sim.run_slots(10, || false).unwrap();
    assert!(sim.failure().is_some());
}

/// A slot whose runtime events fill more than half the ring buffer
/// loses none of them to a paced drain that reads slowly.
#[test]
fn a_paced_drain_takes_a_burst_of_most_of_the_buffer() {
    let source = r#"
            `timescale 1ns/1ns
            module Top;
                logic clk = 1'b0;
                always #5 clk = ~clk;
                initial forever @(posedge clk) for (int i = 0; i < 900; i++) $display("%0d", i);
            endmodule
        "#;
    let mut sim = build_in(source, SchedulerMode::Compiled);
    let drain = sim.runtime_event_drain().expect("a ring buffer");
    drain.set_backpressure(true);
    let done = std::sync::atomic::AtomicBool::new(false);
    let count = std::thread::scope(|scope| {
        let reader = scope.spawn(|| {
            let mut drain = drain;
            let mut count = 0;
            loop {
                let finished = done.load(std::sync::atomic::Ordering::Acquire);
                for event in drain.drain() {
                    match event {
                        crate::RuntimeEvent::Display { .. } => count += 1,
                        other => panic!("unexpected event {other:?}"),
                    }
                }
                if finished {
                    return count;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        });
        sim.run_until(100).unwrap();
        done.store(true, std::sync::atomic::Ordering::Release);
        reader.join().unwrap()
    });
    assert_eq!(count, 10 * 900);
}

/// A simulation that reads its events itself, to watch reports or to
/// compare them with a shadow, hands out no drain.
#[test]
fn simulations_that_read_their_events_hand_out_no_drain() {
    let mut watched = build_in(DISPLAYS, SchedulerMode::Compiled);
    watched.set_fail_on(Some(crate::Severity::Error));
    assert!(watched.runtime_event_drain().is_none());
    for mode in [SchedulerMode::Differential, SchedulerMode::DifferentialRuns] {
        assert!(
            build_in(DISPLAYS, mode).runtime_event_drain().is_none(),
            "{mode:?}"
        );
    }
}

#[test]
#[should_panic(expected = "cannot watch reports while a RuntimeEventDrain is active")]
fn reports_are_not_watched_while_a_drain_is_active() {
    let mut sim = build_in(DISPLAYS, SchedulerMode::Compiled);
    let _drain = sim.runtime_event_drain().expect("a ring buffer");
    sim.set_fail_on(Some(crate::Severity::Error));
}

/// A host write that fails an immediate assertion with `$fatal` ends
/// the simulation at the next step while a drain is active.
#[test]
fn host_writes_keep_fatal_assertions_with_a_drain() {
    let source = r#"
            module Top(input logic [7:0] a);
                always_comb assert (a != 8'd3) else $fatal(1, "a is 3");
            endmodule
        "#;
    let mut sim = build_in(source, SchedulerMode::Compiled);
    let _drain = sim.runtime_event_drain().expect("a ring buffer");
    let a = sim.signal("a");
    sim.modify(|io| io.set(a, 3u8)).unwrap();
    let result = sim.step();
    assert!(
        matches!(result, Err(crate::RuntimeErrorCode::Fatal { .. })),
        "{result:?}"
    );
    assert!(sim.is_finished());
}

/// A run of the compiled scheduler in `run_slots` returns now and then
/// to ask whether to go on, also without runtime events to drain.
#[test]
fn run_slots_asks_to_go_on_between_batches_of_slots() {
    let source = r#"
            `timescale 1ns/1ns
            module Top(output logic [31:0] n);
                logic clk = 1'b0;
                always #5 clk = ~clk;
                initial begin
                    n = 0;
                    forever @(posedge clk) n = n + 1;
                end
            endmodule
        "#;
    let mut sim = build_in(source, SchedulerMode::Compiled);
    let mut asked = 0;
    let steps = sim
        .run_slots(u64::MAX, || {
            asked += 1;
            asked > 3
        })
        .unwrap();
    assert!(steps > 0 && steps <= 3 << 16, "{steps}");
    assert!(sim.scheduler_stats().compiled_slots > 0);
}

/// `run_slots` takes as many steps as it may, compiled slots included,
/// and stays at the time of the last one.
#[test]
fn run_slots_counts_compiled_slots_as_steps() {
    let mut sim = build_in(DISPLAYS, SchedulerMode::Compiled);
    let mut steps = 0;
    while steps < 300 {
        let run = sim.run_slots(300 - steps, || false).unwrap();
        assert!(run > 0);
        steps += run;
        sim.drain_runtime_events();
    }
    assert_eq!(steps, 300);
    // A slot every 5ns: the run did not move on to the time of the next.
    let time = sim.time();
    assert_eq!(time % 5, 0);
    assert!((290 * 5..300 * 5).contains(&time), "{time}");
    assert!(sim.scheduler_stats().compiled_slots > 200);
    assert_eq!(sim.run_slots(10, || true).unwrap(), 0);
    assert_eq!(sim.time(), time);
}

/// `source`'s kernels all run in the compiled scheduler, and its slots
/// agree with the reference's in two and four states.
fn compiles_and_agrees(source: &str, end: u64) {
    for four_state in [false, true] {
        let build = |mode| {
            Simulation::from_sv_sources(vec![(source, Path::new("top.sv"))], "Top")
                .scheduler_mode(mode)
                .four_state(four_state)
                .build()
                .unwrap()
        };
        let kernels = build(SchedulerMode::Compiled).scheduler_stats().kernels;
        assert!(
            kernels
                .iter()
                .all(|kernel| *kernel == crate::KernelSchedule::Compiled),
            "{kernels:?}"
        );
        let mut reference = build(SchedulerMode::Reference);
        reference.run_until(end).unwrap();
        let expected = reference.save_state().unwrap();
        for mode in MODES {
            let mut sim = build(mode);
            sim.run_until(end).unwrap();
            assert_eq!(sim.save_state().unwrap(), expected, "{four_state} {mode:?}");
            let stats = sim.scheduler_stats();
            assert!(stats.compiled_slots > 0, "{four_state} {mode:?}: {stats:?}");
        }
    }
}

/// A testbench that waits for both edges of its clock and delays in
/// between, writing what the design samples and reading what it
/// computes.
#[test]
fn kernels_waiting_for_several_edges_and_delays_compile() {
    let source = r#"
            `timescale 1ns/1ns
            module Top(output logic [7:0] seen);
                logic clk = 1'b0;
                logic [7:0] d = 8'd0;
                logic [7:0] q = 8'd0;
                logic [7:0] acc = 8'd0;
                always #5 clk = ~clk;
                always_ff @(posedge clk) q <= q + d;
                initial begin
                    repeat (2) @(posedge clk);
                    forever begin
                        @(posedge clk);
                        #1;
                        acc = acc * 8'd3 ^ q;
                        d = d + 8'd1;
                        @(negedge clk);
                        acc = acc + q;
                    end
                end
                assign seen = acc;
            endmodule
        "#;
    compiles_and_agrees(source, 1000);
}

/// Waits for the edges of two clocks of different periods, and delays
/// that end in the slot of an edge: a kernel declared before the clocks
/// resumes from its delay, waits, and is woken again in that slot.
#[test]
fn kernels_waiting_for_several_signals_compile() {
    let source = r#"
            `timescale 1ns/1ns
            module Top(output logic [7:0] seen);
                logic a = 1'b0;
                logic b = 1'b0;
                logic [7:0] log = 8'd0;
                logic [7:0] n = 8'd0;
                initial forever begin
                    #5;
                    @(posedge a);
                    n = n + 8'd1;
                end
                always #5 a = ~a;
                always #7 b = ~b;
                initial forever begin
                    @(posedge a);
                    log = log * 8'd3 + 8'd1;
                    @(negedge b);
                    log = log * 8'd5 + 8'd2;
                    #3;
                    log = log + n;
                    @(b);
                    log = log ^ 8'h5a;
                end
                assign seen = log + n;
            endmodule
        "#;
    compiles_and_agrees(source, 1000);
}

/// A kernel that delays, or waits for edges of two signals, may run
/// twice in a slot: one that stages nonblocking assignments keeps the
/// reference loop.
#[test]
fn kernels_that_may_run_twice_in_a_slot_publish_native_queued_assignments() {
    for waits in ["@(posedge clk); #1;", "@(posedge clk); @(posedge other);"] {
        let source = format!(
            "`timescale 1ns/1ns
                module Top(output logic [7:0] sum);
                    logic clk = 1'b0;
                    logic other = 1'b0;
                    logic [7:0] stim = 8'd0;
                    always #5 clk = ~clk;
                    always #3 other = ~other;
                    initial forever begin {waits} stim <= stim + 8'd3; end
                    always_ff @(posedge clk) sum <= stim;
                endmodule"
        );
        let kernels = build_in(&source, SchedulerMode::Compiled)
            .scheduler_stats()
            .kernels;
        assert_eq!(kernels[2], crate::KernelSchedule::Compiled, "{waits}");
        let mut reference = build_in(&source, SchedulerMode::Reference);
        reference.run_until(200).unwrap();
        let expected = reference.save_state().unwrap();
        for mode in MODES {
            let mut sim = build_in(&source, mode);
            sim.run_until(200).unwrap();
            assert_eq!(sim.save_state().unwrap(), expected, "{waits} {mode:?}");
        }
    }
}

/// Compiled kernels declared after more than 64 others keep their own
/// bits in the ready mask.
#[test]
fn compiled_kernels_after_many_processes() {
    let mut source =
        String::from("`timescale 1ns/1ns\nmodule Top(output logic [7:0] n);\nlogic clk = 1'b0;\n");
    for index in 0..66 {
        source.push_str(&format!(
            "logic d{index} = 1'b0;\ninitial #0 d{index} = 1'b1;\n"
        ));
    }
    source.push_str(
        "always #5 clk = ~clk;\n\
             initial begin n = 8'd0; forever @(posedge clk) n = n + 8'd1; end\n\
             endmodule\n",
    );
    agrees_with_the_reference(&source, 200);
}

/// A kernel that loops through queued NBAs stays native and remains bounded.
#[test]
fn waiting_kernels_that_yield_remain_compiled() {
    #[cfg(all(target_arch = "x86_64", not(feature = "arm64-codegen")))]
    let reasons = |source: &str| -> Vec<String> {
        build_in(source, SchedulerMode::Compiled)
            .scheduler_stats()
            .kernels
            .into_iter()
            .filter_map(|kernel| match kernel {
                crate::KernelSchedule::Reference(reason) => Some(reason),
                crate::KernelSchedule::Compiled => None,
            })
            .collect()
    };
    // A loop that does not suspend yields now and then.
    let spinning = r#"
            `timescale 1ns/1ns
            module Top(output logic q);
                logic clk = 1'b0;
                always #5 clk = ~clk;
                initial begin q = 1'b0; @(posedge clk); forever q <= ~q; end
            endmodule
        "#;
    #[cfg(all(target_arch = "x86_64", not(feature = "arm64-codegen")))]
    assert!(reasons(spinning).is_empty());
    let mut sim = build_in(spinning, SchedulerMode::Compiled);
    sim.run_until(0).unwrap();
    sim.state.set_slot_budgets(32, 16);
    let _before = sim.scheduler_stats().reference_steps;
    let error = sim.run_until(10).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("does not suspend within one time slot"),
        "{error}"
    );
    #[cfg(all(target_arch = "x86_64", not(feature = "arm64-codegen")))]
    assert_eq!(sim.scheduler_stats().reference_steps, _before);
}

/// A nonblocking site in a loop runs several times in one run of its
/// kernel. Both retained and unrolled loops preserve every queued value
/// while the generated scheduler owns their steady execution.
#[test]
fn nonblocking_sites_in_loops_remain_compiled() {
    for bound in ["n", "2"] {
        for tail in ["", "$finish;"] {
            let source = format!(
                "`timescale 1ns/1ns
                    module Top(output logic [7:0] q);
                        logic clk = 1'b0;
                        int n = 2;
                        always #5 clk = ~clk;
                        initial begin
                            q = 8'd0;
                            forever begin
                                @(posedge clk);
                                for (int i = 0; i < {bound}; i++) q <= q + 8'(i) + 8'd1;
                                {tail}
                            end
                        end
                    endmodule"
            );
            let _kernels = build_in(&source, SchedulerMode::Compiled)
                .scheduler_stats()
                .kernels;
            #[cfg(all(target_arch = "x86_64", not(feature = "arm64-codegen")))]
            assert!(
                _kernels
                    .iter()
                    .all(|kernel| *kernel == crate::KernelSchedule::Compiled),
                "{bound} {tail}: {_kernels:?}"
            );
            let mut reference = build_in(&source, SchedulerMode::Reference);
            reference.run_until(100).unwrap();
            let expected = reference.save_state().unwrap();
            for mode in MODES {
                let mut sim = build_in(&source, mode);
                sim.run_until(0).unwrap();
                let _before = sim.scheduler_stats().reference_steps;
                sim.run_until(100).unwrap();
                if mode != SchedulerMode::Reference {
                    #[cfg(all(target_arch = "x86_64", not(feature = "arm64-codegen")))]
                    assert_eq!(sim.scheduler_stats().reference_steps, _before);
                }
                assert_eq!(
                    sim.save_state().unwrap(),
                    expected,
                    "{bound} {tail} {mode:?}"
                );
            }
        }
    }
}

/// A nonblocking update into what a stateful combinational process, a
/// latch, is sensitive to bumps its occurrence counter when the
/// generated scheduler publishes it. Every mode leaves the reference's
/// state and records.
#[test]
fn nonblocking_updates_into_probed_signals_remain_compiled() {
    let source = r#"
            `timescale 1ns/1ns
            module Top(output logic [7:0] n);
                logic clk = 1'b0;
                logic [7:0] q = 8'd0;
                always #5 clk = ~clk;
                logic [7:0] l;
                initial forever @(posedge clk) q <= q + 8'd1;
                always_latch if (q[0]) l = q;
                assign n = l;
            endmodule
        "#;
    let _kernels = build_in(source, SchedulerMode::Compiled)
        .scheduler_stats()
        .kernels;
    #[cfg(all(target_arch = "x86_64", not(feature = "arm64-codegen")))]
    assert!(
        _kernels
            .iter()
            .all(|kernel| *kernel == crate::KernelSchedule::Compiled),
        "{_kernels:?}"
    );
    let displays = |sim: &mut Simulation| {
        sim.drain_runtime_events()
            .into_iter()
            .filter(|event| matches!(event, crate::RuntimeEvent::Display { .. }))
            .count()
    };
    let mut reference = build_in(source, SchedulerMode::Reference);
    reference.run_until(100).unwrap();
    let expected = (reference.save_state().unwrap(), displays(&mut reference));
    for mode in MODES {
        let mut sim = build_in(source, mode);
        sim.run_until(0).unwrap();
        let _before = sim.scheduler_stats().reference_steps;
        sim.run_until(100).unwrap();
        if mode != SchedulerMode::Reference {
            #[cfg(all(target_arch = "x86_64", not(feature = "arm64-codegen")))]
            assert_eq!(sim.scheduler_stats().reference_steps, _before);
        }
        let state = sim.save_state().unwrap();
        assert_eq!((state, displays(&mut sim)), expected, "{mode:?}");
    }
}

/// What the host does before a run.
#[derive(Clone, Copy)]
enum Host {
    Clock(&'static str, u64, u64),
    Set(&'static str, u8),
    Schedule(&'static str, u64, u64),
}

fn drive<B: crate::SimBackend>(sim: &mut Simulation<B>, actions: &[Host]) {
    for &action in actions {
        match action {
            Host::Clock(port, period, delay) => sim.add_clock(port, period, delay),
            Host::Set(port, value) => {
                let signal = sim.signal(port);
                sim.modify(|io| io.set(signal, value)).unwrap();
            }
            Host::Schedule(port, time, value) => sim.schedule(port, time, value).unwrap(),
        }
    }
}

/// Run `source`, whose kernels all compile, in every mode on the native
/// and Cranelift backends: before each run until `end`, the host does
/// `actions`, and afterwards the state file is the reference's. At least
/// one slot must run compiled.
fn host_driven_agrees(source: &str, runs: &[(u64, &[Host])]) {
    let _kernels = build_in(source, SchedulerMode::Compiled)
        .scheduler_stats()
        .kernels;
    #[cfg(all(target_arch = "x86_64", not(feature = "arm64-codegen")))]
    assert!(
        _kernels
            .iter()
            .all(|kernel| *kernel == crate::KernelSchedule::Compiled),
        "{_kernels:?}"
    );
    let mut reference = build_in(source, SchedulerMode::Reference);
    let mut expected = Vec::new();
    for &(end, actions) in runs {
        drive(&mut reference, actions);
        reference.run_until(end).unwrap();
        expected.push(reference.save_state().unwrap());
    }
    fn check<B: crate::SimBackend>(
        mut sim: Simulation<B>,
        runs: &[(u64, &[Host])],
        expected: &[crate::StateFile],
        label: &str,
    ) {
        for (run, &(end, actions)) in runs.iter().enumerate() {
            drive(&mut sim, actions);
            sim.run_until(end)
                .unwrap_or_else(|error| panic!("{label} run {run}: {error:?}"));
            assert_eq!(
                sim.save_state().unwrap(),
                expected[run],
                "{label} run {run}"
            );
        }
        let stats = sim.scheduler_stats();
        assert!(stats.compiled_slots > 0, "{label}: {stats:?}");
    }
    for mode in MODES {
        check(
            build_in(source, mode),
            runs,
            &expected,
            &format!("{mode:?}"),
        );
        #[cfg(feature = "cranelift")]
        let cranelift = Simulation::from_sv_sources(vec![(source, Path::new("top.sv"))], "Top")
            .scheduler_mode(mode)
            .build_cranelift()
            .unwrap();
        #[cfg(feature = "cranelift")]
        check(cranelift, runs, &expected, &format!("cranelift {mode:?}"));
    }
}

/// Kernels waiting for a host clock run in compiled slots with the
/// domain of their edge, blocking and nonblocking.
#[test]
fn host_clocks_run_in_compiled_slots() {
    for assign in ["=", "<="] {
        let source = format!(
            "`timescale 1ns/1ns
                module Top(input logic clk, output logic [7:0] sum);
                    logic [7:0] stim = 8'd0;
                    logic [7:0] r = 8'd0;
                    initial forever @(posedge clk) stim {assign} stim + 8'd3;
                    always_ff @(posedge clk) r <= r + stim;
                    assign sum = r;
                endmodule"
        );
        host_driven_agrees(&source, &[(1000, &[Host::Clock("clk", 10, 5)])]);
    }
}

/// Host clocks with coinciding edges, odd and short periods, a kernel
/// clock in the same slots, and waits for falling and any edges.
#[test]
fn coincident_host_clocks_run_in_compiled_slots() {
    let source = r#"
            `timescale 1ns/1ns
            module Top(input logic a, input logic b, input logic c, output logic [7:0] seen);
                logic g = 1'b0;
                logic [7:0] log = 8'd0;
                logic [7:0] acc = 8'd0;
                logic [7:0] ra = 8'd0;
                logic [7:0] rb = 8'd0;
                logic [7:0] rg = 8'd0;
                logic [7:0] rc = 8'd0;
                initial forever @(posedge a) log = log * 8'd3 + 8'd1;
                always #3 g = ~g;
                initial forever @(negedge b) log = log * 8'd5 + 8'd2;
                initial forever @(c) acc <= acc + log;
                always_ff @(posedge a) ra <= ra + log;
                always_ff @(negedge b) rb <= rb + acc;
                always_ff @(posedge g) rg <= rg + ra + rb;
                always_ff @(posedge c) rc <= rc + log;
                assign seen = log + acc + rg + rc;
            endmodule
        "#;
    for (a, b, c) in [(10, 6, 15), (7, 3, 1), (2, 5, 9)] {
        host_driven_agrees(
            source,
            &[(
                300,
                &[
                    Host::Clock("a", a, 1),
                    Host::Clock("b", b, 3),
                    Host::Clock("c", c, 2),
                ],
            )],
        );
    }
}

/// Host writes, of a clocked signal too, a clock added later, a one-shot
/// event on a clocked signal and a clock added twice to one event
/// between runs.
#[test]
fn host_clocks_agree_across_host_changes() {
    let source = r#"
            `timescale 1ns/1ns
            module Top(input logic clk, input logic clk2, input logic go, input logic [7:0] k,
                       output logic [7:0] q);
                logic [7:0] n = 8'd0;
                logic [7:0] m = 8'd0;
                logic [7:0] r = 8'd0;
                logic [7:0] r2 = 8'd0;
                logic [7:0] boost = 8'd0;
                initial forever @(posedge clk) n = n + k + boost;
                initial @(posedge go) boost = 8'd5;
                initial forever @(posedge clk2) m <= m + n;
                always_ff @(posedge clk) r <= r + n + m;
                always_ff @(posedge clk2) r2 <= r2 ^ m;
                assign q = r + r2;
            endmodule
        "#;
    host_driven_agrees(
        source,
        &[
            (100, &[Host::Set("k", 1), Host::Clock("clk", 10, 5)]),
            (
                200,
                &[
                    Host::Set("k", 3),
                    Host::Set("go", 1),
                    Host::Clock("clk2", 7, 2),
                ],
            ),
            (300, &[Host::Schedule("clk", 253, 1)]),
            (400, &[Host::Clock("clk2", 4, 1)]),
            (500, &[Host::Set("clk", 1)]),
            (600, &[Host::Set("clk", 0), Host::Set("clk2", 1)]),
        ],
    );
}

/// A kernel's `$finish` in a slot with a host clock edge, and a state
/// file and a checkpoint taken between runs and restored.
#[test]
fn host_clocks_finish_and_restore_as_the_reference() {
    let source = r#"
            `timescale 1ns/1ns
            module Top(input logic clk, input logic clk2, output logic [7:0] n);
                logic [7:0] r = 8'd0;
                initial begin
                    n = 8'd0;
                    forever @(posedge clk) begin
                        n = n + 8'd1;
                        if (n == 8'd40) $finish;
                    end
                end
                logic [7:0] q = 8'd0;
                always_ff @(posedge clk2) r <= r + n;
                always_ff @(posedge clk) q <= q ^ r;
            endmodule
        "#;
    let clocks = [Host::Clock("clk", 10, 5), Host::Clock("clk2", 3, 1)];
    host_driven_agrees(source, &[(100, &clocks), (1000, &[])]);
    // A state file and a checkpoint taken in the middle continue as the
    // reference does.
    let mut reference = build_in(source, SchedulerMode::Reference);
    drive(&mut reference, &clocks);
    reference.run_until(1000).unwrap();
    assert!(reference.is_finished());
    let expected = reference.save_state().unwrap();
    for mode in MODES {
        let mut sim = build_in(source, mode);
        drive(&mut sim, &clocks);
        sim.run_until(133).unwrap();
        let state = sim.save_state().unwrap();
        let checkpoint = sim.checkpoint().unwrap();
        sim.run_until(1000).unwrap();
        assert_eq!(sim.save_state().unwrap(), expected, "{mode:?}");
        sim.restore(&checkpoint).unwrap();
        sim.run_until(1000).unwrap();
        assert_eq!(sim.save_state().unwrap(), expected, "{mode:?} checkpoint");
        let mut loaded = build_in(source, mode);
        loaded.load_state(&state).unwrap();
        loaded.run_until(1000).unwrap();
        assert_eq!(
            loaded.save_state().unwrap(),
            expected,
            "{mode:?} state file"
        );
    }
}

/// The next edge of a host clock that overflows the simulation time
/// fails the run as the reference's, after the clocks before it in the
/// slot re-armed and the slot settled.
#[test]
fn host_clock_overflow_fails_as_the_reference() {
    let source = r#"
            `timescale 1ns/1ns
            module Top(input logic a, input logic b, output logic [7:0] n, output logic [7:0] y);
                logic [7:0] ra = 8'd0;
                logic [7:0] rb = 8'd0;
                initial begin n = 8'd0; forever @(posedge b) n = n + 8'd1; end
                always_ff @(posedge a) ra <= ra + n;
                always_ff @(posedge b) rb <= rb + ra;
                assign y = ra ^ rb;
            endmodule
        "#;
    let run = |mode: SchedulerMode| {
        let mut sim = build_in(source, mode);
        sim.add_clock("a", 4, u64::MAX - 3);
        sim.add_clock("b", 10, u64::MAX - 3);
        let error = sim.run_until(u64::MAX).unwrap_err();
        (error.to_string(), sim.save_state().unwrap(), sim)
    };
    let (expected_error, expected, _) = run(SchedulerMode::Reference);
    assert!(
        expected_error.contains("overflows simulation time"),
        "{expected_error}"
    );
    for mode in MODES {
        let (error, state, sim) = run(mode);
        assert_eq!(error, expected_error, "{mode:?}");
        assert_eq!(state, expected, "{mode:?}");
        assert!(sim.scheduler_stats().compiled_slots > 0, "{mode:?}");
    }
}

/// Events only the host writes and no clock drives, a reset and a clock
/// input: runs skip the clock code, host writes of them between runs
/// reach their domains in the reference loop, and a `$finish` in the
/// middle of a slot leaves their baselines as the reference's.
#[test]
fn unclocked_host_events_agree() {
    for finish in ["", "initial #47 $finish;"] {
        let source = format!(
            "`timescale 1ns/1ns
                module Top(input logic rst_n, input logic clk2, output logic [7:0] q);
                    logic clk = 1'b0;
                    logic [7:0] q2 = 8'd0;
                    always #5 clk = ~clk;
                    {finish}
                    always_ff @(posedge clk or negedge rst_n)
                        if (!rst_n) q <= 8'd0; else q <= q + 8'd1;
                    always_ff @(posedge clk2) q2 <= q2 + q;
                endmodule"
        );
        host_driven_agrees(
            &source,
            &[
                (100, &[Host::Set("rst_n", 1), Host::Set("clk2", 1)]),
                (200, &[Host::Set("rst_n", 0), Host::Set("clk2", 0)]),
                (300, &[Host::Set("rst_n", 1), Host::Set("clk2", 1)]),
            ],
        );
    }
}

/// Combinational logic reads what each writer changes, on slots where
/// no domain triggers: a kernel's blocking write, a published
/// nonblocking value, a host clock's level and a host input written
/// between runs, besides a domain's registers. Each change reaches the
/// combinational outputs in the state file as in the reference.
#[test]
fn settles_follow_every_write() {
    let source = r#"
            `timescale 1ns/1ns
            module Top(input logic h, input logic [7:0] k, output logic [7:0] y);
                logic clk = 1'b0;
                logic [7:0] stim = 8'd0;
                logic [7:0] staged = 8'd0;
                logic [7:0] r = 8'd0;
                logic [7:0] rh = 8'd0;
                logic [7:0] w;
                always #5 clk = ~clk;
                initial forever @(negedge clk) stim = stim + 8'd1;
                initial forever @(negedge clk) staged <= staged + 8'd2;
                always_ff @(posedge clk) r <= r + w;
                always_ff @(posedge h) rh <= rh + 8'd1;
                assign w = stim * 8'd3 + staged + k + {7'd0, h};
                assign y = w ^ r ^ rh;
            endmodule
        "#;
    host_driven_agrees(
        source,
        &[
            (100, &[Host::Set("k", 1)]),
            (200, &[Host::Clock("h", 7, 3)]),
            (300, &[Host::Set("k", 9)]),
            (400, &[]),
        ],
    );
}

/// Settles wait for their readers, which see what they see in the
/// reference: a domain whose next state combinational logic computes,
/// a kernel that samples a combinational output after a delay, one that
/// samples it at an edge, in the pass after the clock's kernel changed
/// it, and the host between runs.
#[test]
fn deferred_settles_reach_their_readers() {
    for reader in [
        "",
        "always #3 seen = y;",
        "initial forever @(negedge clk) seen = y + stim;",
    ] {
        let source = format!(
            "`timescale 1ns/1ns
                module Top(input logic [7:0] k, output logic [7:0] y);
                    logic clk = 1'b0;
                    logic [7:0] stim = 8'd0;
                    logic [7:0] r = 8'd0;
                    logic [7:0] q = 8'd0;
                    logic [7:0] seen = 8'd0;
                    logic [7:0] t = 8'd0;
                    logic [7:0] nxt;
                    always #5 begin t = t + 8'd1; clk = ~clk; end
                    always #7 stim = stim + 8'd1;
                    {reader}
                    always_comb nxt = r + stim + k;
                    always_ff @(posedge clk) r <= nxt;
                    always_ff @(posedge clk) q <= q + stim;
                    assign y = r ^ q ^ seen ^ t;
                endmodule"
        );
        host_driven_agrees(
            &source,
            &[
                (100, &[Host::Set("k", 1)]),
                (200, &[Host::Set("k", 5)]),
                (300, &[]),
            ],
        );
    }
}

/// Combinational logic that keeps state between settles (a latch, the
/// snapshots an `always @*` waits on) settles where the reference
/// does: every activation in between counts, though no reader observes
/// it before the run ends.
#[test]
fn stateful_combinational_logic_settles_every_activation() {
    let opened = r#"
            `timescale 1ns/1ns
            module Top(output logic [7:0] n);
                logic [7:0] d = 8'd0;
                logic en = 1'b0;
                logic [7:0] q = 8'd0;
                initial begin
                    #5 d = 8'd7; en = 1'b1;
                    #5 en = 1'b0;
                    #5 d = 8'd9;
                end
                always_latch if (en) q = d;
                assign n = q;
            endmodule
        "#;
    agrees_with_the_reference(opened, 30);
    for process in [
        "always_latch if (g) l = d;",
        "always @* if (g) l = d;",
        "always @* l = d + 8'd1;",
    ] {
        let source = format!(
            "`timescale 1ns/1ns
                module Top(output logic [7:0] n);
                    logic clk = 1'b0;
                    logic [7:0] d = 8'd0;
                    logic g = 1'b1;
                    logic [7:0] l;
                    always #5 begin clk = ~clk; d = d + 8'd1; end
                    always_ff @(posedge clk) g <= ~g;
                    {process}
                    assign n = l;
                endmodule"
        );
        agrees_with_the_reference(&source, 30);
        agrees_with_the_reference(&source, 300);
    }
}

/// Clocks derived from the clock a kernel drives run in compiled slots:
/// gated by a kernel's enable, gated by an enable the clock's own domain
/// raises, inverted, moved back by the commit after the clock's edge
/// moved it, divided by a register, a divided clock gated by a kernel's
/// enable that changes with the clock's edge (it runs after the commit),
/// and divided twice.
#[test]
fn derived_clocks_run_in_compiled_slots() {
    for derived in [
        "assign dclk = clk & en;",
        "assign dclk = clk & ren;",
        "assign dclk = ~clk;",
        "logic tog = 1'b1; always_ff @(posedge clk) tog <= ~tog; assign dclk = clk ^ tog;",
        "always_ff @(posedge clk) dclk <= ~dclk;",
        "logic div = 1'b0; always_ff @(posedge clk) div <= ~div; assign dclk = div & en;",
        "logic half = 1'b0; always_ff @(posedge clk) half <= ~half; \
             always_ff @(posedge half) dclk <= ~dclk;",
    ] {
        let source = format!(
            "`timescale 1ns/1ns
                module Top(output logic [7:0] y);
                    logic clk = 1'b0;
                    logic en = 1'b0;
                    logic ren = 1'b0;
                    logic dclk;
                    logic [7:0] a = 8'd0;
                    logic [7:0] b = 8'd0;
                    always #5 clk = ~clk;
                    initial forever begin #17 en = ~en; end
                    always_ff @(posedge clk) begin a <= a + 8'd1; ren <= a[1]; end
                    {derived}
                    always_ff @(posedge dclk) b <= b + a;
                    assign y = a ^ b;
                endmodule"
        );
        host_driven_agrees(&source, &[(100, &[]), (333, &[])]);
    }
}

/// A host clock gated by a kernel's enable, or divided, in a design
/// with and without kernels.
#[test]
fn derived_host_clocks_run_in_compiled_slots() {
    for (kernel, derived) in [
        (
            "initial forever begin #17 en = ~en; end",
            "assign dclk = clk & en;",
        ),
        ("", "always_ff @(posedge clk) dclk <= ~dclk;"),
        ("", "assign dclk = clk & k[0];"),
    ] {
        let source = format!(
            "`timescale 1ns/1ns
                module Top(input logic clk, input logic [7:0] k, output logic [7:0] y);
                    logic en = 1'b0;
                    logic dclk;
                    logic [7:0] a = 8'd0;
                    logic [7:0] b = 8'd0;
                    {kernel}
                    always_ff @(posedge clk) a <= a + k;
                    {derived}
                    always_ff @(posedge dclk) b <= b + a;
                    assign y = a ^ b;
                endmodule"
        );
        host_driven_agrees(
            &source,
            &[
                (100, &[Host::Set("k", 3), Host::Clock("clk", 10, 5)]),
                (200, &[Host::Set("k", 2)]),
                (300, &[]),
            ],
        );
    }
}

/// A host clock input the host set once and no clock drives keeps its
/// level through compiled slots: a gate on it judges from that level,
/// not from an unrecorded one.
#[test]
fn derived_clocks_of_an_unclocked_host_input_keep_its_level() {
    let source = r#"
            `timescale 1ns/1ns
            module Top(input logic clk, output logic [7:0] y);
                logic en = 1'b0;
                logic dclk;
                logic [7:0] a = 8'd0;
                logic [7:0] b = 8'd0;
                initial forever #17 en = ~en;
                assign dclk = clk & en;
                always_ff @(posedge clk) a <= a + 8'd1;
                always_ff @(posedge dclk) b <= b + 8'd1;
                assign y = a ^ b;
            endmodule
        "#;
    host_driven_agrees(source, &[(100, &[Host::Set("clk", 1)]), (200, &[])]);
}

/// A gate a waiting kernel writes changes after the pass that judged
/// its clock: preserve that edge until the source domain commits.
#[test]
fn gates_written_by_waiting_kernels_preserve_the_first_settle_edges() {
    let source = r#"
            `timescale 1ns/1ns
            module Top(output logic [7:0] y);
                logic clk = 1'b0;
                logic en = 1'b0;
                logic gclk;
                logic [7:0] a = 8'd0;
                logic [7:0] b = 8'd0;
                always #5 clk = ~clk;
                initial forever @(posedge clk) en = ~en;
                assign gclk = clk & en;
                always_ff @(posedge clk) a <= a + 8'd1;
                always_ff @(posedge gclk) b <= a;
                assign y = a ^ b;
            endmodule
        "#;
    for four_state in [false, true] {
        let build = |mode| {
            Simulation::from_sv_sources(vec![(source, Path::new("top.sv"))], "Top")
                .scheduler_mode(mode)
                .four_state(four_state)
                .build()
                .unwrap()
        };
        let mut reference = build(SchedulerMode::Reference);
        reference.run_until(200).unwrap();
        let expected = reference.save_state().unwrap();
        for mode in MODES {
            let mut sim = build(mode);
            sim.run_until(200).unwrap();
            assert_eq!(
                sim.save_state().unwrap(),
                expected,
                "{mode:?}, {four_state}"
            );
            assert!(
                sim.scheduler_stats().compiled_slots > 0,
                "{mode:?}, {four_state}: {:?}",
                sim.scheduler_stats()
            );
        }
    }
}

/// A domain whose next state combinational logic computes runs its
/// fused units with a settle pending: they compute what it reads.
#[test]
fn fused_domains_compute_what_they_read() {
    let source = r#"
            module Top (
                clk: input clock,
                k  : input logic<8>,
                y  : output logic<8>,
            ) {
                var r  : logic<8>;
                var nxt: logic<8>;
                always_comb {
                    nxt = r + k;
                }
                always_ff (clk) {
                    r = nxt;
                }
                assign y = r ^ k;
            }
        "#;
    let run = |mode: SchedulerMode| {
        let mut sim = Simulation::builder(source, "Top")
            .scheduler_mode(mode)
            .build()
            .unwrap();
        sim.add_clock("clk", 10, 5);
        let k = sim.signal("k");
        let mut states = Vec::new();
        for (end, value) in [(100, 1u8), (200, 5), (300, 9)] {
            sim.modify(|io| io.set(k, value)).unwrap();
            sim.run_until(end).unwrap();
            states.push(sim.save_state().unwrap());
        }
        (states, sim.scheduler_stats().compiled_slots)
    };
    let (expected, _) = run(SchedulerMode::Reference);
    for mode in MODES {
        let (states, compiled) = run(mode);
        assert_eq!(states, expected, "{mode:?}");
        assert!(compiled > 0, "{mode:?}");
    }
}

/// A `$fatal` in a domain ends the run with combinational logic as the
/// slot's settle before the domain left it.
#[test]
fn compiled_slots_fail_in_a_domain_as_the_reference() {
    let source = r#"
            `timescale 1ns/1ns
            module Top(output logic [7:0] y);
                logic clk = 1'b0;
                logic [7:0] r = 8'd0;
                always #5 clk = ~clk;
                always_ff @(posedge clk) begin
                    r <= r + 8'd1;
                    if (r == 8'd6) $fatal(1, "stop");
                end
                assign y = r * 8'd3;
            endmodule
        "#;
    let run = |mode: SchedulerMode| {
        let mut sim = build_in(source, mode);
        let result = sim.run_until(1000).map_err(|error| error.to_string());
        (result, sim.time(), sim.save_state().unwrap(), sim)
    };
    let (result, time, state, _) = run(SchedulerMode::Reference);
    assert!(result.is_err(), "{result:?}");
    for mode in MODES {
        let (mode_result, mode_time, mode_state, sim) = run(mode);
        assert_eq!(mode_result, result, "{mode:?}");
        assert_eq!(mode_time, time, "{mode:?}");
        assert_eq!(mode_state, state, "{mode:?}");
        assert!(sim.scheduler_stats().compiled_slots > 0, "{mode:?}");
    }
}

/// A design without process kernels runs its host clocks in compiled
/// slots: one domain, which has no units for several domains, coinciding
/// domains and a domain-less edge, with host writes between runs and a
/// one-shot event.
#[test]
fn kernel_less_designs_run_host_clocks_compiled() {
    let single = r#"
            module Top(input logic clk, input logic [7:0] k, output logic [7:0] y);
                logic [7:0] r = 8'd0;
                always_ff @(posedge clk) r <= r + k;
                assign y = r ^ k;
            endmodule
        "#;
    host_driven_agrees(
        single,
        &[
            (100, &[Host::Set("k", 3), Host::Clock("clk", 10, 5)]),
            (200, &[Host::Set("k", 5)]),
            (300, &[Host::Schedule("clk", 253, 1)]),
        ],
    );
    let source = r#"
            module Top(input logic a, input logic b, input logic [7:0] k, output logic [7:0] y);
                logic [7:0] ra = 8'd0;
                logic [7:0] rb = 8'd0;
                logic [7:0] w;
                always_ff @(posedge a) ra <= ra + w;
                always_ff @(negedge b) rb <= rb ^ ra;
                assign w = ra + rb + k;
                assign y = w ^ {7'd0, a};
            endmodule
        "#;
    host_driven_agrees(
        source,
        &[
            (100, &[Host::Set("k", 3), Host::Clock("a", 10, 5)]),
            (200, &[Host::Clock("b", 6, 1)]),
            (300, &[Host::Set("k", 7), Host::Schedule("a", 251, 1)]),
            (400, &[]),
        ],
    );
}

/// A design without kernels takes the `$finish` exit through a domain:
/// the run ends in the middle of the slot as the reference's does.
#[test]
fn kernel_less_designs_finish_in_a_domain() {
    let source = r#"
            module Top(input logic clk, output logic [7:0] n);
                logic [7:0] r = 8'd0;
                always_ff @(posedge clk) begin
                    r <= r + 8'd1;
                    if (r == 8'd6) $finish;
                end
                assign n = r;
            endmodule
        "#;
    let mut reference = build_in(source, SchedulerMode::Reference);
    reference.add_clock("clk", 10, 5);
    reference.run_until(1000).unwrap();
    assert!(reference.is_finished());
    let expected = reference.save_state().unwrap();
    for mode in MODES {
        let mut sim = build_in(source, mode);
        sim.add_clock("clk", 10, 5);
        sim.run_until(1000).unwrap();
        assert!(sim.is_finished(), "{mode:?}");
        assert_eq!(sim.save_state().unwrap(), expected, "{mode:?}");
        assert!(sim.scheduler_stats().compiled_slots > 0, "{mode:?}");
    }
}

/// A clock on an event a kernel drives, or added twice to one event,
/// stays with the reference loop and bounds the compiled runs.
#[test]
fn other_host_clocks_bound_compiled_runs() {
    let source = r#"
            `timescale 1ns/1ns
            module Top(input logic h, output logic [7:0] n);
                logic clk = 1'b0;
                logic [7:0] r = 8'd0;
                logic [7:0] r2 = 8'd0;
                always #5 clk = ~clk;
                initial begin n = 8'd0; forever @(posedge clk) n = n + 8'd1; end
                always_ff @(posedge clk) r <= r + n;
                always_ff @(posedge h) r2 <= r2 ^ n;
            endmodule
        "#;
    host_driven_agrees(source, &[(300, &[Host::Clock("clk", 6, 2)])]);
    host_driven_agrees(
        source,
        &[(300, &[Host::Clock("h", 20, 2), Host::Clock("h", 20, 7)])],
    );
}

/// A compiled run of several slots that fails in a later one: the
/// shadow catches up to that slot and fails the same way, so every mode
/// reports the reference's error and leaves its state, combinational
/// outputs as the last slot settled them.
#[test]
fn compiled_runs_that_fail_after_several_slots_agree() {
    let source = r#"
            `timescale 1fs/1fs
            module Top(output logic [7:0] x, output logic [7:0] y);
                logic clk = 1'b0;
                logic [7:0] r = 8'd0;
                always #5 clk = ~clk;
                always_ff @(posedge clk) r <= r + 8'd1;
                assign y = r + x;
                initial begin
                    x = 8'd0;
                    #30 x = 8'd1;
                    #(64'd18446744073709551605) x = 8'd2;
                end
            endmodule
        "#;
    let run = |mode: SchedulerMode| {
        let mut sim = build_in(source, mode);
        let error = sim.run_until(100).unwrap_err().to_string();
        (error, sim.time(), sim.save_state().unwrap(), sim)
    };
    let (error, time, state, _) = run(SchedulerMode::Reference);
    assert!(error.contains("delay overflows simulation time"), "{error}");
    assert_eq!(time, 30);
    for mode in MODES {
        let (mode_error, mode_time, mode_state, sim) = run(mode);
        assert_eq!(mode_error, error, "{mode:?}");
        assert_eq!(mode_time, time, "{mode:?}");
        assert_eq!(mode_state, state, "{mode:?}");
        assert!(sim.scheduler_stats().compiled_slots > 1, "{mode:?}");
    }
}

#[test]
fn constant_rtl_clock_and_reset_cones_allow_compiled_slots() {
    let source = r#"`timescale 1ns/1ns
            module Top(output bit [7:0] n, output bit dormant, output bit dormant_pos);
                bit clk;
                wire tied_clock = 1'b0;
                wire power_on_n = 1'b1;
                wire power_on = ~power_on_n;
                always #5 clk = ~clk;
                always_ff @(posedge clk or posedge power_on)
                    if (power_on) n <= 0; else n <= n + 1;
                always @(negedge tied_clock) dormant <= ~dormant;
                always @(posedge tied_clock) dormant_pos <= ~dormant_pos;
            endmodule"#;
    for partition in [
        crate::SettlePartition::Estimated,
        crate::SettlePartition::Always,
    ] {
        for four_state in [false, true] {
            for mode in MODES {
                let mut sim = Simulation::from_sv_sources(
                    vec![(source, Path::new("constant-clocks.sv"))],
                    "Top",
                )
                .four_state(four_state)
                .settle_partition(partition)
                .scheduler_mode(mode)
                .build()
                .unwrap();
                assert_eq!(
                    sim.program().runtime_schema.settle_partitions.is_some(),
                    partition == crate::SettlePartition::Always
                );
                sim.run_until(100).unwrap();
                let n = sim.signal("n");
                assert_eq!(sim.get(n), 10u8.into());
                assert!(
                    sim.scheduler_stats().compiled_slots > 0,
                    "{mode:?} {partition:?} four_state={four_state}: {:?}",
                    sim.scheduler_stats()
                );
            }
        }
    }
}

#[test]
fn counted_domain_pulses_compile_and_preserve_handover() {
    let source = "`timescale 1ns/1ns\nmodule Top(output bit [7:0] n); bit clk; always #5 begin clk = 1; clk = 0; end always_ff @(posedge clk) n <= n + 1; endmodule";
    let mut reference = build_in(source, SchedulerMode::Reference);
    reference.run_until(100).unwrap();
    let expected = reference.save_state().unwrap();
    for mode in MODES {
        let mut sim = build_in(source, mode);
        assert!(
            sim.program()
                .runtime_schema
                .scheduler
                .as_ref()
                .unwrap()
                .unit
                .is_some()
        );
        sim.run_until(45).unwrap();
        let checkpoint = sim.save_state().unwrap();
        sim.load_state(&checkpoint).unwrap();
        sim.run_until(100).unwrap();
        let n = sim.signal("n");
        assert_eq!(sim.get(n), 20u8.into(), "{mode:?}");
        assert_eq!(sim.save_state().unwrap(), expected, "{mode:?}");
        assert!(sim.scheduler_stats().compiled_slots > 0, "{mode:?}");
    }
}

#[test]
fn counted_wide_wait_snapshots_preserve_partial_words() {
    let source = r#"`timescale 1ns/1ns
            module Top(output bit [7:0] n);
                logic [64:0] a = 0;
                logic [8:0] b = 0;
                bit select;
                always #5 begin
                    if (select) begin a[64] = 1; a[64] = 0; end
                    else begin b = 1; b = 0; end
                    select = ~select;
                end
                initial forever begin @(a or b); n++; end
            endmodule"#;
    for four_state in [false, true] {
        let build = |mode| {
            Simulation::from_sv_sources(vec![(source, Path::new("wide-counted.sv"))], "Top")
                .four_state(four_state)
                .scheduler_mode(mode)
                .build()
                .unwrap()
        };
        let mut reference = build(SchedulerMode::Reference);
        reference.run_until(100).unwrap();
        let expected = reference.save_state().unwrap();
        for mode in MODES {
            let mut sim = build(mode);
            sim.run_until(45).unwrap();
            let checkpoint = sim.save_state().unwrap();
            sim.load_state(&checkpoint).unwrap();
            sim.run_until(100).unwrap();
            assert_eq!(
                sim.get(sim.signal("n")),
                20u8.into(),
                "{mode:?} four_state={four_state}"
            );
            assert_eq!(
                sim.save_state().unwrap(),
                expected,
                "{mode:?} four_state={four_state}"
            );
            assert!(
                sim.scheduler_stats().compiled_slots > 0,
                "{mode:?} four_state={four_state}"
            );
        }
    }
}

#[test]
fn compiled_host_clock_masks_span_multiple_words() {
    let mut source = String::from("`timescale 1ns/1ns\nmodule Top(");
    for index in 0..70 {
        if index != 0 {
            source.push(',');
        }
        source.push_str(&format!("input bit c{index}, output bit [7:0] q{index}"));
    }
    source.push_str(");\n");
    for index in 0..70 {
        source.push_str(&format!(
            "always_ff @(posedge c{index}) q{index} <= q{index} + 1;\n"
        ));
    }
    source.push_str("endmodule");
    for mode in MODES {
        let mut sim = build_in(&source, mode);
        for index in 0..70 {
            sim.add_clock(&format!("c{index}"), 4 + 2 * (index % 3), 1 + index % 3);
        }
        for limit in [1, 41, 81] {
            sim.run_until(limit).unwrap();
            for index in [0, 63, 64, 69] {
                let signal = sim.signal(&format!("q{index}"));
                let delay = 1 + index % 3;
                let expected = if limit < delay {
                    0
                } else {
                    1 + (limit - delay) / (4 + 2 * (index % 3))
                };
                assert_eq!(
                    sim.get(signal),
                    expected.into(),
                    "{mode:?} clock {index} at {limit}"
                );
            }
        }
        assert!(sim.scheduler_stats().compiled_slots > 0, "{mode:?}");
    }
}

#[test]
fn wide_ready_mask_survives_an_exit_mid_slot() {
    let mut source = String::from("`timescale 1ns/1ns\nmodule Top;\n");
    for index in 0..130 {
        let finish = if index == 65 {
            "if (!v65) $finish;"
        } else {
            ""
        };
        source.push_str(&format!("bit v{index}; initial begin v{index} = 0; forever begin #5 v{index} = ~v{index}; {finish} end end\n"));
    }
    source.push_str("endmodule");
    let mut reference = build_in(&source, SchedulerMode::Reference);
    reference.run_until(20).unwrap();
    let expected = reference.save_state().unwrap();
    for mode in MODES {
        let mut sim = build_in(&source, mode);
        sim.run_until(20).unwrap();
        assert!(sim.is_finished());
        assert!(sim.scheduler_stats().compiled_slots >= 2, "{mode:?}");
        assert_eq!(sim.save_state().unwrap(), expected, "{mode:?}");
    }
}

#[test]
fn ready_masks_span_multiple_words() {
    let mut source = String::from("`timescale 1ns/1ns\nmodule Top;\n");
    for index in 0..130 {
        source.push_str(&format!(
            "bit v{index}; initial begin v{index} = 0; forever #5 v{index} = ~v{index}; end\n"
        ));
    }
    source.push_str("endmodule");
    for mode in MODES {
        let mut sim = build_in(&source, mode);
        assert!(
            sim.scheduler_stats()
                .kernels
                .iter()
                .all(|kernel| *kernel == crate::KernelSchedule::Compiled)
        );
        for limit in [5, 10, 15] {
            sim.run_until(limit).unwrap();
            for index in [0, 63, 64, 127, 128, 129] {
                let signal = sim.signal(&format!("v{index}"));
                assert_eq!(sim.get(signal), (limit / 5 % 2).into(), "{mode:?} {index}");
            }
        }
        assert!(sim.scheduler_stats().compiled_slots >= 3, "{mode:?}");
    }
}

#[test]
fn eager_comb_slices_preserve_counted_pulses_in_compiled_slots() {
    let design =
        celox_test_suite::sv::case("timing::intermediate_combinational_writes_wake_waiters")
            .unwrap()
            .design();
    for four_state in [false, true] {
        for mode in MODES {
            let sources = design
                .sources
                .iter()
                .map(|source| (source.text.as_str(), source.path.as_path()))
                .collect();
            let mut sim = Simulation::from_sv_sources(sources, &design.top)
                .four_state(four_state)
                .scheduler_mode(mode)
                .build()
                .unwrap();
            sim.run_until(5000).unwrap();
            assert!(sim.is_finished());
            for name in ["n1", "n2"] {
                let signal = sim.signal(name);
                assert_eq!(
                    sim.get(signal),
                    3u8.into(),
                    "{mode:?} four_state={four_state} {name}"
                );
            }
            assert!(
                sim.scheduler_stats().compiled_slots > 0,
                "{mode:?} four_state={four_state}"
            );
        }
    }
}

/// A delay-only kernel pulses the signal another kernel waits on within
/// one activation, with two stores or in a loop: the frontend counts the
/// signal's occurrences and the compiled scheduler wakes the waiting
/// kernel even when the final signal level is unchanged.
#[test]
fn pulses_within_one_activation_wake_as_the_reference() {
    for pulse in [
        "a = 1'b1; a = 1'b0;",
        "for (int i = 0; i < 2; i++) a = ~a;",
        "for (int i = 0; i < k; i++) a = ~a;",
    ] {
        let source = format!(
            "`timescale 1ns/1ns
                module Top(output logic [7:0] n);
                    logic a = 1'b0;
                    int k = 2;
                    always #5 begin {pulse} end
                    initial begin n = 8'd0; forever @(posedge a) n = n + 8'd1; end
                endmodule"
        );
        let kernels = build_in(&source, SchedulerMode::Compiled)
            .scheduler_stats()
            .kernels;
        assert!(
            kernels
                .iter()
                .all(|kernel| *kernel == crate::KernelSchedule::Compiled),
            "{pulse}: {kernels:?}"
        );
        let mut reference = build_in(&source, SchedulerMode::Reference);
        reference.run_until(100).unwrap();
        let n = reference.signal("n");
        assert_eq!(reference.get(n), 20u8.into(), "{pulse}");
        let expected = reference.save_state().unwrap();
        for mode in MODES {
            let mut sim = build_in(&source, mode);
            sim.run_until(100).unwrap();
            assert_eq!(sim.save_state().unwrap(), expected, "{pulse} {mode:?}");
            assert!(sim.scheduler_stats().compiled_slots > 0, "{pulse} {mode:?}");
        }
    }
}

#[test]
fn compiled_assertions_drain_observed_before_reactive_actions() {
    for name in [
        "sampled_predicate_live_action",
        "disable_iff_after_nba",
        "predicates_before_all_actions",
        "sampled_history_predicates",
    ] {
        let case = format!("concurrent_assertions::{name}");
        let design = celox_test_suite::sv::case(&case).unwrap().design();
        for four_state in [false, true] {
            let build = |mode| {
                let sources = design
                    .sources
                    .iter()
                    .map(|source| (source.text.as_str(), source.path.as_path()))
                    .collect();
                Simulation::from_sv_sources(sources, &design.top)
                    .four_state(four_state)
                    .scheduler_mode(mode)
                    .build()
                    .unwrap()
            };
            let mut reference = build(SchedulerMode::Reference);
            reference.run_until(10_000).unwrap();
            let expected = reference.save_state().unwrap();
            for mode in MODES {
                let mut sim = build(mode);
                sim.run_until(1_000).unwrap();
                let checkpoint = sim.checkpoint().unwrap();
                sim.run_until(10_000).unwrap();
                assert_eq!(
                    sim.save_state().unwrap(),
                    expected,
                    "{case} {mode:?} four_state={four_state}"
                );
                assert!(
                    sim.scheduler_stats().compiled_slots > 0,
                    "{case} {mode:?} four_state={four_state}: {:?}",
                    sim.scheduler_stats()
                );
                sim.restore(&checkpoint).unwrap();
                sim.run_until(10_000).unwrap();
                assert_eq!(
                    sim.save_state().unwrap(),
                    expected,
                    "restored {case} {mode:?}"
                );
            }
        }
    }
}

#[test]
fn packed_array_waits_have_complete_compiled_snapshots() {
    let source = r#"`timescale 1ns/1ns
            module Top(output int n);
                logic [4:0] a [3];
                wire [4:0] b [3];
                assign b = a;
                initial begin
                    a = '{0, 0, 0};
                    #5 a[0] = 1;
                    #5 a[1] = 2;
                    #5 a[2] = 3;
                end
                initial forever @(b) n++;
            endmodule"#;
    for four_state in [false, true] {
        let build = |mode| {
            Simulation::from_sv_sources(vec![(source, Path::new("array.sv"))], "Top")
                .scheduler_mode(mode)
                .four_state(four_state)
                .build()
                .unwrap()
        };
        let mut reference = build(SchedulerMode::Reference);
        reference.run_until(20).unwrap();
        let expected = reference.save_state().unwrap();
        for mode in MODES {
            let mut sim = build(mode);
            sim.run_until(20).unwrap();
            assert_eq!(
                sim.save_state().unwrap(),
                expected,
                "{mode:?} four_state={four_state}"
            );
            assert!(
                sim.scheduler_stats().compiled_slots > 0,
                "{mode:?} four_state={four_state}: {:?}",
                sim.scheduler_stats()
            );
        }
    }
}

#[test]
fn compiled_assertion_exits_preserve_pending_regions() {
    for action in ["$finish;", "$fatal(1, \"reactive failure\");"] {
        let source = format!(
            r#"`timescale 1ns/1ns
                module Top(output int first, second);
                    bit clk;
                    always #5 clk = ~clk;
                    p: assert property (@(posedge clk) 1) begin first++; {action} end
                    q: assert property (@(posedge clk) 1) second++;
                endmodule"#
        );
        let mut reference = build_in(&source, SchedulerMode::Reference);
        let expected = reference
            .run_until(100)
            .map(|_| ())
            .map_err(|e| e.to_string());
        for mode in MODES {
            let mut sim = build_in(&source, mode);
            let result = sim.run_until(100).map(|_| ()).map_err(|e| e.to_string());
            assert_eq!(result, expected, "{mode:?} {action}");
            for name in ["first", "second"] {
                assert_eq!(
                    sim.get(sim.signal(name)),
                    reference.get(reference.signal(name))
                );
            }
            assert!(
                sim.scheduler_stats().compiled_slots > 0,
                "{mode:?} {action}"
            );
        }
    }
}

#[test]
fn differential_heap_check_ignores_dead_ff_working_handles() {
    let design = celox_test_suite::sv::case("string::strings_survive_heap_collections")
        .unwrap()
        .design();
    for mode in [SchedulerMode::Differential, SchedulerMode::DifferentialRuns] {
        let sources = design
            .sources
            .iter()
            .map(|source| (source.text.as_str(), source.path.as_path()))
            .collect();
        let mut sim = Simulation::from_sv_sources(sources, &design.top)
            .scheduler_mode(mode)
            .build()
            .unwrap();
        sim.run_until(6_000_000).unwrap();
        assert_eq!(sim.get(sim.signal("n")), 3000u32.into());
        assert!(sim.scheduler_stats().compiled_slots > 0, "{mode:?}");
    }
}

#[test]
fn differential_mode_detects_object_rng_divergence_before_randomize() {
    use crate::backend::SimBackend as _;
    let source = r#"
            module Top(output bit done);
                timeunit 1ps / 1ps;
                class C; rand int value; endclass
                C c;
                initial begin
                    c = new;
                    #5 done = 1;
                    #5 void'(c.randomize());
                end
            endmodule
        "#;
    let mut sim = Simulation::from_sv_sources(vec![(source, Path::new("rng.sv"))], "Top")
        .scheduler_mode(SchedulerMode::Differential)
        .build()
        .unwrap();
    sim.run_until(0).unwrap();
    let object = sim.get(sim.signal("c")).to_u32_digits()[0];
    use celox_runtime::StepObserver as _;
    let mut check =
        super::ShadowCheck::new(sim.shadow.as_mut().unwrap(), SchedulerMode::Differential);
    check.before_step(&sim.state, &mut sim.simulator);
    let result = sim.state.step_until(&mut sim.simulator, 5);
    // Inject an extra draw after the primary step: before_step mirrors the
    // heap, so a mutation between run_until calls would be overwritten.
    sim.simulator.backend.heap_mut().random_object(object);
    let error = check
        .after_step(&sim.state, &mut sim.simulator, 5, &result)
        .unwrap_err()
        .to_string();
    assert!(error.contains("runtime heap"), "{error}");
}

#[test]
fn differential_mode_reports_a_diverging_step() {
    let mut sim = build(SchedulerMode::Differential);
    // The shadow sees other plusargs, which a copy of the state does not
    // carry, so its first step computes another flag.
    sim.shadow.as_mut().unwrap().simulator.plusargs =
        celox_runtime::plusargs::Plusargs(vec!["+fast".to_string()]);
    let error = sim.run_until(100).unwrap_err().to_string();
    assert!(
        error.contains("diverged from the reference in the step from time 0"),
        "{error}"
    );
}
