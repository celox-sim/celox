#![cfg(feature = "systemverilog")]

use celox::{SimBackend, Simulation};
use std::path::Path;

macro_rules! check_backends {
    ($source:expr, $check:ident) => {
        for four_state in [false, true] {
            let build = || {
                Simulation::from_sv_sources(vec![($source, Path::new("time.sv"))], "Top")
                    .four_state(four_state)
            };
            $check(build().build_native().unwrap(), four_state);
            $check(build().build_cranelift().unwrap(), four_state);
            $check(build().build_wasm().unwrap(), four_state);
            $check(build().build_interpreter().unwrap(), four_state);
            $check(
                build()
                    .threads(4)
                    .parallel_partition(celox::ParallelPartition::Always)
                    .build_native()
                    .unwrap(),
                four_state,
            );
            $check(
                build()
                    .threads(4)
                    .parallel_partition(celox::ParallelPartition::Always)
                    .build_cranelift()
                    .unwrap(),
                four_state,
            );
        }
    };
}

#[test]
fn state_file_time_is_published_before_combinational_settling() {
    let source = r#"
      module Top(input int a, output logic [63:0] y);
        timeunit 1ns / 1ps;
        assign y = a + $time;
      endmodule
    "#;
    fn check<B: SimBackend>(mut sim: Simulation<B>, _four_state: bool) {
        let a = sim.signal("a");
        let y = sim.signal("y");
        sim.run_until(5_000).unwrap();
        sim.modify(|io| io.set(a, 9u32)).unwrap();
        let mut saved = sim.save_state().unwrap();
        assert_eq!(sim.get(y), 14u32.into());
        // Edited state inputs must be recomputed, rather than retaining the
        // file's old combinational seed or evaluating with the current time.
        saved
            .objects
            .iter_mut()
            .find(|object| object.path.ends_with("a"))
            .unwrap()
            .value = 11u32.to_le_bytes().to_vec();
        sim.run_until(9_000).unwrap();
        sim.modify(|io| io.set(a, 3u32)).unwrap();
        assert_eq!(sim.get(y), 12u32.into());
        let mut invalid = saved.clone();
        invalid
            .objects
            .iter_mut()
            .find(|object| object.path.ends_with("a"))
            .unwrap()
            .width += 1;
        assert!(sim.load_state(&invalid).is_err());
        assert_eq!(sim.time(), 9_000);
        assert_eq!(sim.get(y), 12u32.into());
        sim.load_state(&saved).unwrap();
        assert_eq!(sim.time(), 5_000);
        let (memory, size) = sim.memory_as_ptr();
        assert!(y.offset + 8 <= size);
        // Safety: y is a live 64-bit output within the allocation.
        let raw = unsafe { std::slice::from_raw_parts(memory.add(y.offset), 8) };
        assert_eq!(u64::from_le_bytes(raw.try_into().unwrap()), 16);
        assert_eq!(sim.get(y), 16u32.into());
    }
    check_backends!(source, check);
}

#[test]
fn scaled_delays_reject_overflow_before_narrowing() {
    let source = r#"
      module Top(input logic [127:0] amount, output bit done);
        timeunit 1ns / 1ps;
        initial begin #(amount); done = 1; end
      endmodule
    "#;
    fn check<B: SimBackend>(mut sim: Simulation<B>, _four_state: bool) {
        let amount = sim.signal("amount");
        sim.modify(|io| io.set_wide(amount, (u128::from(u64::MAX / 1000) + 1).into()))
            .unwrap();
        let error = sim.step().unwrap_err().to_string();
        assert!(
            error.contains("delay") && error.contains("overflow"),
            "{error}"
        );
        assert_eq!(sim.time(), 0);
    }
    check_backends!(source, check);
}

#[test]
fn scaled_delay_at_the_last_representable_unit_succeeds() {
    let source = r#"
      module Top(input logic [63:0] amount, output bit done);
        timeunit 1ns / 1ps;
        initial begin #(amount); done = 1; end
      endmodule
    "#;
    fn check<B: SimBackend>(mut sim: Simulation<B>, _four_state: bool) {
        let amount = sim.signal("amount");
        let done = sim.signal("done");
        let units = u64::MAX / 1000;
        sim.modify(|io| io.set(amount, units)).unwrap();
        assert_eq!(sim.step().unwrap(), Some(0));
        assert_eq!(sim.next_event_time(), Some(units * 1000));
        sim.run_until(units * 1000).unwrap();
        assert_eq!(sim.get(done), 1u8.into());
    }
    check_backends!(source, check);
}

#[test]
fn unknown_scaled_delay_is_zero_before_the_overflow_check() {
    let source = r#"
      module Top(input logic [63:0] amount, output bit done);
        timeunit 1ns / 1ps;
        initial begin #(amount); done = 1; end
      endmodule
    "#;
    fn check<B: SimBackend>(mut sim: Simulation<B>, four_state: bool) {
        let amount = sim.signal("amount");
        let done = sim.signal("done");
        sim.modify(|io| {
            if four_state {
                io.set_four_state(amount, 1u8.into(), 1u8.into());
            } else {
                io.set(amount, 0u64);
            }
        })
        .unwrap();
        sim.run_until(0).unwrap();
        assert_eq!(sim.get(done), 1u8.into());
        assert_eq!(sim.time(), 0);
    }
    check_backends!(source, check);
}

#[test]
fn negative_scaled_delay_is_checked_after_unsigned_time_conversion() {
    let source = r#"
      module Top(input int amount, output bit done);
        timeunit 1ns / 1ps;
        initial begin #(amount); done = 1; end
      endmodule
    "#;
    fn check<B: SimBackend>(mut sim: Simulation<B>, _four_state: bool) {
        let amount = sim.signal("amount");
        sim.modify(|io| io.set(amount, u32::MAX)).unwrap();
        let error = sim.step().unwrap_err().to_string();
        assert!(
            error.contains("delay") && error.contains("overflow"),
            "{error}"
        );
    }
    check_backends!(source, check);
}

#[test]
fn child_port_time_calls_use_parent_scope_without_time_sensitivity() {
    let source = r#"
      module Child(input logic [63:0] x, output logic [63:0] y);
        timeunit 10ps / 1ps;
        assign y = x;
      endmodule
      module Top(input logic [63:0] a, output logic [63:0] t, s, pure_time);
        timeunit 1ns / 1ps;
        Child time_child(.x(a + $time), .y(t));
        Child stime_child(.x(a + $stime), .y(s));
        Child pure_child(.x($time), .y(pure_time));
      endmodule
    "#;
    fn check<B: SimBackend>(mut sim: Simulation<B>, _four_state: bool) {
        let a = sim.signal("a");
        let t = sim.signal("t");
        let s = sim.signal("s");
        let pure = sim.signal("pure_time");
        sim.modify(|io| io.set(a, 1u64)).unwrap();
        assert_eq!(sim.get(t), 1u64.into());
        assert_eq!(sim.get(pure), 0u64.into());
        sim.run_until(1_499).unwrap();
        sim.modify(|io| io.set(a, 2u64)).unwrap();
        assert_eq!(sim.get(t), 3u64.into());
        assert_eq!(sim.get(s), 3u64.into());
        sim.run_until(1_500).unwrap();
        assert_eq!(sim.get(t), 3u64.into());
        sim.modify(|io| io.set(a, 3u64)).unwrap();
        assert_eq!(sim.get(t), 5u64.into());
        assert_eq!(sim.get(pure), 0u64.into());
        sim.run_until(((1u64 << 32) + 3) * 1000).unwrap();
        sim.modify(|io| io.set(a, 4u64)).unwrap();
        assert_eq!(sim.get(t), ((1u64 << 32) + 7).into());
        assert_eq!(sim.get(s), 7u64.into());
        assert_eq!(sim.get(pure), 0u64.into());
    }
    check_backends!(source, check);
}

#[test]
fn child_port_subroutines_read_time_and_omitted_defaults() {
    let source = r#"
      package P;
        timeunit 100ps / 1ps;
        function automatic logic [63:0] now(); return $time; endfunction
      endpackage
      module Child(input logic [63:0] x, output logic [63:0] y);
        timeunit 10ps / 1ps;
        assign y = x;
      endmodule
      module Top(input logic [63:0] a, output logic [63:0] t, s, p, pure_time);
        timeunit 1ns / 1ps;
        function automatic logic [63:0] leaf(); return $time; endfunction
        function automatic logic [63:0] now(input logic [63:0] bias); return leaf() + bias; endfunction
        function automatic logic [63:0] with_default(input logic [63:0] stamp = $stime);
          return stamp;
        endfunction
        Child time_child(.x(now(a)), .y(t));
        Child stime_child(.x(with_default() + a), .y(s));
        Child package_child(.x(P::now() + a), .y(p));
        Child pure_child(.x(leaf()), .y(pure_time));
      endmodule
    "#;
    fn check<B: SimBackend>(mut sim: Simulation<B>, _four_state: bool) {
        let a = sim.signal("a");
        let t = sim.signal("t");
        let s = sim.signal("s");
        let p = sim.signal("p");
        let pure = sim.signal("pure_time");
        sim.modify(|io| io.set(a, 1u64)).unwrap();
        assert_eq!(sim.get(t), 1u64.into());
        assert_eq!(sim.get(s), 1u64.into());
        sim.run_until(1_500).unwrap();
        assert_eq!(sim.get(t), 1u64.into());
        sim.modify(|io| io.set(a, 2u64)).unwrap();
        assert_eq!(sim.get(t), 4u64.into());
        assert_eq!(sim.get(s), 4u64.into());
        assert_eq!(sim.get(p), 17u64.into());
        assert_eq!(sim.get(pure), 0u64.into());
        sim.run_until(((1u64 << 32) + 3) * 1000).unwrap();
        sim.modify(|io| io.set(a, 4u64)).unwrap();
        assert_eq!(sim.get(t), ((1u64 << 32) + 7).into());
        assert_eq!(sim.get(s), 7u64.into());
        assert_eq!(sim.get(pure), 0u64.into());
    }
    check_backends!(source, check);
}
