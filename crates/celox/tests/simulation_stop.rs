#![cfg(feature = "systemverilog")]

use std::path::Path;

use celox::{SimBackend, Simulation};

macro_rules! check_backends {
    ($source:expr, $check:ident) => {
        for four_state in [false, true] {
            let build = || {
                Simulation::from_sv_sources(vec![($source, Path::new("stop.sv"))], "Top")
                    .four_state(four_state)
            };
            $check(build().build_native().unwrap());
            $check(build().build_cranelift().unwrap());
            $check(build().build_wasm().unwrap());
            $check(build().build_interpreter().unwrap());
            $check(
                build()
                    .threads(4)
                    .parallel_partition(celox::ParallelPartition::Always)
                    .build_native()
                    .unwrap(),
            );
            $check(
                build()
                    .threads(4)
                    .parallel_partition(celox::ParallelPartition::Always)
                    .build_cranelift()
                    .unwrap(),
            );
        }
    };
}

#[test]
fn resumed_process_reads_settled_raw_host_writes() {
    let source = r#"
        module Top(input int d, output int comb, seen);
          assign comb = d + 1;
          initial begin
            $stop;
            seen = comb;
            $stop;
            seen = comb;
          end
        endmodule
    "#;
    fn check<B: SimBackend>(mut sim: Simulation<B>) {
        let d = sim.signal("d");
        let seen = sim.signal("seen");
        sim.modify(|io| io.set(d, 0u32)).unwrap();
        assert_eq!(sim.step().unwrap(), Some(0));
        assert!(sim.is_stopped());
        for (value, stopped) in [(41u32, true), (9, false)] {
            let (memory, size) = sim.memory_as_mut_ptr();
            assert!(d.offset + 4 <= size);
            // Safety: d is a live 32-bit input within this simulator's memory.
            unsafe {
                std::ptr::copy_nonoverlapping(value.to_le_bytes().as_ptr(), memory.add(d.offset), 4)
            };
            sim.note_raw_writes();
            assert_eq!(sim.step().unwrap(), Some(0));
            assert_eq!(sim.is_stopped(), stopped);
            assert_eq!(sim.get(seen), (value + 1).into());
        }
        assert_eq!(sim.step().unwrap(), None);
    }
    check_backends!(source, check);
}

#[test]
fn resumed_slot_consumes_logged_host_pulses_and_clock_edges() {
    let source = r#"
        module Top(input logic clk, pulse, input int d, output int edges, waited, seen);
          always_ff @(posedge clk) edges <= edges + 1;
          initial begin
            edges = 0;
            @(posedge pulse);
            waited = d;
          end
          initial begin
            $stop;
            seen = d;
          end
        endmodule
    "#;
    fn check<B: SimBackend>(mut sim: Simulation<B>) {
        let clk = sim.signal("clk");
        let pulse = sim.signal("pulse");
        let d = sim.signal("d");
        let edges = sim.signal("edges");
        let waited = sim.signal("waited");
        let seen = sim.signal("seen");
        sim.modify(|io| {
            io.set(clk, 0u8);
            io.set(pulse, 0u8);
            io.set(d, 0u32);
        })
        .unwrap();
        assert_eq!(sim.step().unwrap(), Some(0));
        assert!(sim.is_stopped());
        sim.modify(|io| {
            io.set(d, 42u32);
            // A logged pulse wakes its waiter even though its final level
            // is unchanged. A separate clock edge must also run its domain.
            io.set(pulse, 1u8);
            io.set(pulse, 0u8);
            io.set(clk, 1u8);
        })
        .unwrap();
        assert_eq!(sim.step().unwrap(), Some(0));
        assert!(!sim.is_stopped());
        assert_eq!(sim.get(seen), 42u32.into());
        assert_eq!(sim.get(waited), 42u32.into());
        assert_eq!(sim.get(edges), 1u32.into());
        assert_eq!(sim.step().unwrap(), None);
        assert_eq!(sim.get(edges), 1u32.into());
    }
    check_backends!(source, check);
}

#[test]
fn resumed_slot_settles_raw_derived_clocks_and_waits() {
    let source = r#"
        module Top(input logic clk, input int d, output logic derived,
                   output int edges, waited, seen, comb);
          assign derived = clk & d[0];
          assign comb = d + 1;
          always_ff @(posedge derived) edges <= edges + 1;
          initial begin
            edges = 0;
            @(posedge clk);
            waited = comb;
          end
          initial begin
            $stop;
            seen = comb;
          end
        endmodule
    "#;
    fn check<B: SimBackend>(mut sim: Simulation<B>) {
        let clk = sim.signal("clk");
        let d = sim.signal("d");
        let edges = sim.signal("edges");
        let waited = sim.signal("waited");
        let seen = sim.signal("seen");
        sim.modify(|io| {
            io.set(clk, 0u8);
            io.set(d, 0u32);
        })
        .unwrap();
        assert_eq!(sim.step().unwrap(), Some(0));
        assert!(sim.is_stopped());
        let (memory, size) = sim.memory_as_mut_ptr();
        assert!(d.offset + 4 <= size && clk.offset < size);
        // Safety: these are live inputs within the simulator's allocation.
        unsafe {
            std::ptr::copy_nonoverlapping(41u32.to_le_bytes().as_ptr(), memory.add(d.offset), 4);
            memory.add(clk.offset).write(1);
        }
        sim.note_raw_writes();
        assert_eq!(sim.step().unwrap(), Some(0));
        assert!(!sim.is_stopped());
        assert_eq!(sim.get(seen), 42u32.into());
        assert_eq!(sim.get(waited), 42u32.into());
        assert_eq!(sim.get(edges), 1u32.into());
        assert_eq!(sim.step().unwrap(), None);
        assert_eq!(sim.get(edges), 1u32.into());
    }
    check_backends!(source, check);
}

#[test]
fn resumed_slot_keeps_pre_stop_edges_when_host_writes_arrive() {
    let source = r#"
        module Top(input int d, output logic clk, output int edges, seen);
          always_ff @(posedge clk) edges <= edges + 1;
          initial begin
            edges = 0;
            clk = 1;
            $stop;
            seen = d;
          end
        endmodule
    "#;
    fn check<B: SimBackend>(mut sim: Simulation<B>) {
        let d = sim.signal("d");
        let edges = sim.signal("edges");
        let seen = sim.signal("seen");
        assert_eq!(sim.step().unwrap(), Some(0));
        assert!(sim.is_stopped());
        assert_eq!(sim.get(edges), 0u32.into());
        sim.modify(|io| io.set(d, 42u32)).unwrap();
        assert_eq!(sim.step().unwrap(), Some(0));
        assert!(!sim.is_stopped());
        assert_eq!(sim.get(seen), 42u32.into());
        assert_eq!(sim.get(edges), 1u32.into());
        assert_eq!(sim.step().unwrap(), None);
        assert_eq!(sim.get(edges), 1u32.into());
    }
    check_backends!(source, check);
}

#[test]
fn resumed_slot_merges_host_derived_edges_with_pending_edges() {
    let source = r#"
        module Top(input logic source_clk, enable, output logic pending_clk, gated,
                   output int source_edges, pending_edges, gated_edges);
          timeunit 1ns;
          timeprecision 1ns;
          assign gated = source_clk & enable;
          always_ff @(posedge source_clk) source_edges <= source_edges + 1;
          always_ff @(posedge pending_clk) pending_edges <= pending_edges + 1;
          always_ff @(posedge gated) gated_edges <= gated_edges + 1;
          initial begin
            pending_edges = 0;
            gated_edges = 0;
            source_edges = 0;
            // Establish the source clock's high baseline before the stop.
            #5;
            pending_clk = 1;
            $stop;
            // The inactive region advances edge baselines before NBA publication.
            #0;
          end
        endmodule
    "#;
    fn check<B: SimBackend>(mut sim: Simulation<B>) {
        let enable = sim.signal("enable");
        let source_clk = sim.signal("source_clk");
        let source_edges = sim.signal("source_edges");
        let pending_edges = sim.signal("pending_edges");
        let gated_edges = sim.signal("gated_edges");
        sim.modify(|io| {
            io.set(source_clk, 1u8);
            io.set(enable, 0u8);
        })
        .unwrap();
        sim.run_until(5).unwrap();
        assert_eq!(sim.time(), 5);
        assert!(sim.is_stopped());
        assert_eq!(sim.get(source_edges), 1u32.into());
        assert_eq!(sim.get(pending_edges), 0u32.into());
        assert_eq!(sim.get(gated_edges), 0u32.into());
        let (memory, size) = sim.memory_as_mut_ptr();
        assert!(enable.offset < size);
        // Safety: enable is a live input within the simulator's allocation.
        unsafe { memory.add(enable.offset).write(1) };
        sim.note_raw_writes();
        assert_eq!(sim.step().unwrap(), Some(5));
        assert!(!sim.is_stopped());
        assert_eq!(sim.get(source_edges), 1u32.into());
        assert_eq!(sim.get(pending_edges), 1u32.into());
        assert_eq!(sim.get(gated_edges), 1u32.into());
        assert_eq!(sim.step().unwrap(), None);
        assert_eq!(sim.get(source_edges), 1u32.into());
        assert_eq!(sim.get(pending_edges), 1u32.into());
        assert_eq!(sim.get(gated_edges), 1u32.into());
    }
    check_backends!(source, check);
}

#[test]
fn resumed_slot_keeps_opposite_edges_after_host_settle() {
    for inactive in ["", "#0;"] {
        let source = format!(
            r#"
            module Top(input logic enable, output logic source_clk, gated,
                       output int rises, falls);
              timeunit 1ns;
              timeprecision 1ns;
              assign gated = source_clk & enable;
              always_ff @(posedge gated) rises <= rises + 1;
              always_ff @(negedge gated) falls <= falls + 1;
              initial begin
                rises = 0;
                falls = 0;
                source_clk = 1;
                #5;
                $stop;
                source_clk = 0;
                {inactive}
              end
            endmodule
            "#
        );
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            let enable = sim.signal("enable");
            let gated = sim.signal("gated");
            let rises = sim.signal("rises");
            let falls = sim.signal("falls");
            sim.modify(|io| io.set(enable, 0u8)).unwrap();
            sim.run_until(5).unwrap();
            assert!(sim.is_stopped());
            assert_eq!(sim.get(gated), 0u8.into());
            let initial_falls = sim.get(falls);
            let (memory, size) = sim.memory_as_mut_ptr();
            assert!(enable.offset < size);
            // Safety: enable is a live input within the simulator's allocation.
            unsafe { memory.add(enable.offset).write(1) };
            sim.note_raw_writes();
            assert_eq!(sim.step().unwrap(), Some(5));
            assert!(!sim.is_stopped());
            assert_eq!(sim.get(gated), 0u8.into());
            assert_eq!(sim.get(rises), 1u32.into());
            assert_eq!(sim.get(falls), initial_falls.clone() + 1u32);
            assert_eq!(sim.step().unwrap(), None);
            assert_eq!(sim.get(rises), 1u32.into());
            assert_eq!(sim.get(falls), initial_falls + 1u32);
        }
        check_backends!(&source, check);
    }
}

#[test]
fn host_reversal_uses_the_level_at_the_stop() {
    let source = r#"
        module Top(input logic enable, output logic gated,
                   output int rises, falls);
          timeunit 1ns;
          timeprecision 1ns;
          logic local_enable;
          assign gated = local_enable & enable;
          always_ff @(posedge gated) rises <= rises + 1;
          always_ff @(negedge gated) falls <= falls + 1;
          initial begin
            rises = 0;
            falls = 0;
            local_enable = 0;
            #5;
            local_enable = 1;
            $stop;
          end
        endmodule
    "#;
    fn check<B: SimBackend>(mut sim: Simulation<B>) {
        let enable = sim.signal("enable");
        let gated = sim.signal("gated");
        let rises = sim.signal("rises");
        let falls = sim.signal("falls");
        sim.modify(|io| io.set(enable, 1u8)).unwrap();
        sim.run_until(0).unwrap();
        let initial_falls = sim.get(falls);
        sim.run_until(5).unwrap();
        assert!(sim.is_stopped());
        assert_eq!(sim.get(gated), 1u8.into());
        let (memory, size) = sim.memory_as_mut_ptr();
        assert!(enable.offset < size);
        // Safety: enable is a live input within the simulator's allocation.
        unsafe { memory.add(enable.offset).write(0) };
        sim.note_raw_writes();
        assert_eq!(sim.step().unwrap(), Some(5));
        assert_eq!(sim.get(gated), 0u8.into());
        assert_eq!(sim.get(rises), 1u32.into());
        assert_eq!(sim.get(falls), initial_falls.clone() + 1u32);
        assert_eq!(sim.step().unwrap(), None);
        assert_eq!(sim.get(rises), 1u32.into());
        assert_eq!(sim.get(falls), initial_falls + 1u32);
    }
    check_backends!(source, check);
}

#[test]
fn unrelated_host_write_keeps_pre_stop_transients_filtered() {
    let source = r#"
        module Top(input logic clk, hold, unrelated, output logic gated,
                   output int edges);
          timeunit 1ns;
          timeprecision 1ns;
          logic enable;
          assign gated = hold & enable;
          always_ff @(posedge clk) enable <= 0;
          always_ff @(posedge gated) edges <= edges + 1;
          initial begin
            enable = 0;
            edges = 0;
            #5;
            enable = 1;
            $stop;
          end
        endmodule
    "#;
    fn check<B: SimBackend>(mut sim: Simulation<B>) {
        let unrelated = sim.signal("unrelated");
        let gated = sim.signal("gated");
        let edges = sim.signal("edges");
        let hold = sim.signal("hold");
        sim.modify(|io| io.set(hold, 1u8)).unwrap();
        sim.schedule("clk", 0, 0).unwrap();
        sim.schedule("clk", 5, 1).unwrap();
        sim.run_until(5).unwrap();
        assert!(sim.is_stopped());
        assert_eq!(sim.get(gated), 1u8.into());
        sim.modify(|io| io.set(unrelated, 1u8)).unwrap();
        assert_eq!(sim.step().unwrap(), Some(5));
        assert_eq!(sim.get(gated), 0u8.into());
        assert_eq!(sim.get(edges), 0u32.into());
        assert_eq!(sim.step().unwrap(), None);
        assert_eq!(sim.get(edges), 0u32.into());
    }
    check_backends!(source, check);
}

#[test]
fn stopped_process_resumes_before_host_woken_waiters() {
    let source = r#"
        module Top(input logic logged_wake, raw_wake, input int d,
                   output int shared, waiter_runs, comb, first_seen, second_seen);
          assign comb = d + 1;
          initial begin
            shared = 0;
            waiter_runs = 0;
            @(posedge logged_wake);
            shared = 99;
            waiter_runs = waiter_runs + 1;
          end
          initial begin
            @(posedge raw_wake);
            shared = 77;
            waiter_runs = waiter_runs + 1;
          end
          initial begin
            $stop;
            first_seen = shared + comb;
            $stop;
            second_seen = shared + comb;
          end
        endmodule
    "#;
    fn check<B: SimBackend>(mut sim: Simulation<B>) {
        let d = sim.signal("d");
        let logged_wake = sim.signal("logged_wake");
        let raw_wake = sim.signal("raw_wake");
        let shared = sim.signal("shared");
        let waiter_runs = sim.signal("waiter_runs");
        let first_seen = sim.signal("first_seen");
        let second_seen = sim.signal("second_seen");
        sim.modify(|io| {
            io.set(d, 0u32);
            io.set(logged_wake, 0u8);
            io.set(raw_wake, 0u8);
        })
        .unwrap();
        assert_eq!(sim.step().unwrap(), Some(0));
        assert!(sim.is_stopped());
        sim.modify(|io| {
            io.set(d, 41u32);
            io.set(logged_wake, 1u8);
        })
        .unwrap();
        assert_eq!(sim.step().unwrap(), Some(0));
        assert!(sim.is_stopped());
        assert_eq!(sim.get(first_seen), 42u32.into());
        assert_eq!(sim.get(waiter_runs), 0u32.into());
        assert_eq!(sim.get(shared), 0u32.into());
        let (memory, size) = sim.memory_as_mut_ptr();
        assert!(d.offset + 4 <= size && raw_wake.offset < size);
        // Safety: both live inputs lie within the simulator's allocation.
        unsafe {
            std::ptr::copy_nonoverlapping(9u32.to_le_bytes().as_ptr(), memory.add(d.offset), 4);
            memory.add(raw_wake.offset).write(1);
        }
        sim.note_raw_writes();
        assert_eq!(sim.step().unwrap(), Some(0));
        assert!(!sim.is_stopped());
        assert_eq!(sim.get(second_seen), 10u32.into());
        assert_eq!(sim.get(waiter_runs), 2u32.into());
        assert_eq!(sim.get(shared), 77u32.into());
        assert_eq!(sim.step().unwrap(), None);
        assert_eq!(sim.get(waiter_runs), 2u32.into());
    }
    check_backends!(source, check);
}
