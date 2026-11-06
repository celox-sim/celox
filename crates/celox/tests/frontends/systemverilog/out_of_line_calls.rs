//! Subroutines lowered out of line into SIR function units, through the
//! test switch `force_out_of_line_calls`: each design runs on every backend
//! with the switch on, and gives the results it gives inlined, or the
//! results a recursive design must give.

use super::*;
use celox::{SimBackend, Simulation, SimulatorBuilder};

/// What a run gives: the four-state value of each output, and the runtime
/// events it raised.
type Run = (Vec<(BigUint, BigUint)>, Vec<celox::RuntimeEvent>);

/// The four-state value of each of `outputs` at time `until` and the
/// runtime events raised until then, or the runtime error that stopped the
/// run.
fn run<B: SimBackend>(mut sim: Simulation<B>, until: u64, outputs: &[&str]) -> Result<Run, String> {
    sim.run_until(until).map_err(|error| error.to_string())?;
    let values = outputs
        .iter()
        .map(|name| {
            let signal = sim.signal(name);
            sim.get_four_state(signal)
        })
        .collect();
    Ok((values, sim.drain_runtime_events()))
}

fn backends() -> Vec<&'static str> {
    let mut backends = vec!["interpreter", "cranelift", "wasm"];
    if cfg!(any(target_arch = "x86_64", target_arch = "aarch64")) {
        backends.push("native");
    }
    backends
}

/// Run `source` on `backend` with calls out of line when `forced`.
fn simulate(
    source: &str,
    four_state: bool,
    forced: bool,
    backend: &str,
    until: u64,
    names: &[&str],
) -> Result<Vec<(BigUint, BigUint)>, String> {
    simulate_run(source, four_state, forced, backend, until, names).map(|(values, _)| values)
}

/// [`simulate`], with the runtime events of the run.
fn simulate_run(
    source: &str,
    four_state: bool,
    forced: bool,
    backend: &str,
    until: u64,
    names: &[&str],
) -> Result<Run, String> {
    let builder = Simulation::from_sv_sources(vec![(source, Path::new("calls.sv"))], "Top")
        .four_state(four_state);
    let builder = if forced {
        builder.force_out_of_line_calls()
    } else {
        builder
    };
    let built = |error: celox::SimulatorError| format!("build: {error}");
    match backend {
        "interpreter" => run(builder.build_interpreter().map_err(built)?, until, names),
        "cranelift" => run(builder.build_cranelift().map_err(built)?, until, names),
        "wasm" => run(builder.build_wasm().map_err(built)?, until, names),
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        "native" => run(builder.build_native().map_err(built)?, until, names),
        _ => unreachable!("backend {backend}"),
    }
}

/// The number of function units `source` lowers to with calls forced out
/// of line, or not.
fn function_units(source: &str, forced: bool) -> usize {
    let builder = SimulatorBuilder::<Simulator>::from_sv_sources(
        vec![(source, Path::new("calls.sv"))],
        "Top",
    )
    .trace_pre_optimized_sir();
    let builder = if forced {
        builder.force_out_of_line_calls()
    } else {
        builder
    };
    let result = builder.build_with_trace();
    result
        .trace
        .pre_optimized_sir
        .as_ref()
        .unwrap_or_else(|| panic!("the design compiles: {:?}", result.res.err()))
        .sir
        .functions
        .len()
}

/// Check that `source` goes out of line into `units` function units and
/// gives on every backend, in both state modes, what it gives inlined: the
/// same outputs and the same runtime events.
fn same_as_inlined(source: &str, units: usize, until: u64, names: &[&str]) {
    assert_eq!(
        function_units(source, false),
        0,
        "inlined without the switch"
    );
    assert_eq!(function_units(source, true), units, "function units");
    for four_state in [false, true] {
        let inlined = simulate_run(source, four_state, false, "interpreter", until, names)
            .expect("the inlined design runs");
        for backend in backends() {
            assert_eq!(
                simulate_run(source, four_state, true, backend, until, names),
                Ok(inlined.clone()),
                "{backend}, four-state {four_state}"
            );
        }
    }
}

fn value(value: u64) -> (BigUint, BigUint) {
    (value.into(), 0u32.into())
}

#[test]
fn functions_called_from_always_ff_and_always_comb() {
    // The combinational process runs procedurally, since its symbolic form
    // would expand the call.
    let source = r#"`timescale 1ns/1ns
module Top(output logic [7:0] q, output logic [7:0] c);
    logic clk = 0;
    logic [7:0] a = 3;
    function automatic logic [7:0] twice(input logic [7:0] x);
        return x * 2 + 1;
    endfunction
    always_comb c = twice(a);
    always_ff @(posedge clk) q <= twice(a + 1);
    initial begin
        #1 clk = 1;
        #1 clk = 0;
        a = 5;
    end
endmodule
"#;
    same_as_inlined(source, 1, 5, &["q", "c"]);
    for backend in backends() {
        assert_eq!(
            simulate(source, false, true, backend, 5, &["q", "c"]),
            Ok(vec![value(9), value(11)]),
            "{backend}"
        );
    }
}

#[test]
fn recursive_functions() {
    let source = r#"module Top(output int f, output int g, output int h);
    function automatic int fact(int n);
        if (n <= 1) return 1;
        return n * fact(n - 1);
    endfunction
    function automatic int fib(int n);
        if (n < 2) return n;
        return fib(n - 1) + fib(n - 2);
    endfunction
    function automatic bit is_even(int n);
        if (n == 0) return 1;
        return is_odd(n - 1);
    endfunction
    function automatic bit is_odd(int n);
        if (n == 0) return 0;
        return is_even(n - 1);
    endfunction
    initial begin
        f = fact(10);
        g = fib(15);
        h = {is_even(10), is_odd(7), is_even(7)};
    end
endmodule
"#;
    // Inlining cannot expand recursion.
    let inlined = simulate(source, false, false, "interpreter", 0, &["f"]);
    assert!(
        inlined
            .as_ref()
            .is_err_and(|error| error.contains("recursive call")),
        "{inlined:?}"
    );
    assert_eq!(function_units(source, true), 4);
    for four_state in [false, true] {
        for backend in backends() {
            assert_eq!(
                simulate(source, four_state, true, backend, 0, &["f", "g", "h"]),
                Ok(vec![value(3_628_800), value(610), value(0b110)]),
                "{backend}, four-state {four_state}"
            );
        }
    }
}

#[test]
fn wide_and_four_state_arguments_and_results() {
    let source = r#"`timescale 1ns/1ns
module Top(output logic [199:0] w, output logic [7:0] x, output logic [7:0] y);
    logic [199:0] wide = {4{50'h3_ffff_0000_1234}};
    logic [7:0] unknown = 8'bxxxx_0000;
    logic [7:0] mixed = 8'bzz01_xxxx;
    function automatic logic [199:0] mix(input logic [199:0] a, input logic [7:0] b);
        return {a[191:0], b} ^ (a >> 3);
    endfunction
    function automatic logic [7:0] fill(input logic [7:0] v);
        return v | 8'h0f;
    endfunction
    initial begin
        #1;
        w = mix(wide, mixed);
        x = fill(unknown);
        y = fill(mixed);
    end
endmodule
"#;
    same_as_inlined(source, 2, 1, &["w", "x", "y"]);
}

#[test]
fn output_and_inout_formals_and_defaults() {
    let source = r#"`timescale 1ns/1ns
module Top(output int a, output int b, output int c, output int d);
    task automatic split(input int v, output int hi, inout int acc, input int step = 3);
        hi = v >> 16;
        acc = acc + v + step;
    endtask
    function automatic int scale(int v, int by = 4);
        return v * by;
    endfunction
    initial begin
        #1 c = 10;
        split(32'h0005_0007, a, c);
        split(32'h0001_0001, b, c, 100);
        d = scale(c) + scale(c, 2);
    end
endmodule
"#;
    same_as_inlined(source, 2, 1, &["a", "b", "c", "d"]);
}

#[test]
fn class_methods_and_virtual_dispatch() {
    let source = r#"module Top(output int a, output int b, output int c);
    class Shape;
        int side;
        function new(int side);
            this.side = side;
        endfunction
        virtual function int area();
            return side * side;
        endfunction
        function int twice_area();
            return 2 * area();
        endfunction
    endclass
    class Triangle extends Shape;
        function new(int side);
            super.new(side);
        endfunction
        virtual function int area();
            return side * side / 2;
        endfunction
    endclass
    Shape square;
    Shape triangle;
    Triangle t;
    initial begin
        square = new(6);
        t = new(6);
        triangle = t;
        a = square.area();
        b = triangle.area();
        c = triangle.twice_area();
    end
endmodule
"#;
    // The constructors draw on the process RNG and stay inlined; the
    // methods and the dispatcher go out of line.
    let units = function_units(source, true);
    assert!(units >= 3, "{units} function units");
    same_as_inlined(source, units, 0, &["a", "b", "c"]);
}

#[test]
fn static_locals_are_shared_by_every_call() {
    // `count` keeps its value from one call to the next (IEEE 1800-2023
    // 13.4.2), whichever unit makes the call.
    let source = r#"`timescale 1ns/1ns
module Top(output int first, output int second, output int third);
    function int next();
        static int count = 0;
        count = count + 1;
        return count;
    endfunction
    initial begin
        first = next();
        #1 second = next();
    end
    initial #2 third = next();
endmodule
"#;
    same_as_inlined(source, 1, 5, &["first", "second", "third"]);
    assert_eq!(
        simulate(
            source,
            false,
            true,
            "interpreter",
            5,
            &["first", "second", "third"]
        ),
        Ok(vec![value(1), value(2), value(3)])
    );
}

#[test]
fn calls_inside_loops() {
    let source = r#"`timescale 1ns/1ns
module Top(output int sum, output int last);
    function automatic int square(int v);
        return v * v;
    endfunction
    initial begin
        #1 sum = 0;
        for (int i = 0; i < 10; i++) begin
            sum += square(i);
            last = square(sum % 7);
        end
    end
endmodule
"#;
    same_as_inlined(source, 1, 1, &["sum", "last"]);
}

#[test]
fn deep_recursion_raises_call_depth_and_stack_errors() {
    // Each activation of `down` takes a few bytes, so the depth limit trips
    // first; each of `wide` takes over 2 KiB, so the stack runs out first.
    let source = r#"module Top(output int deep, output int wide_result);
    function automatic int down(int n);
        if (n == 0) return 0;
        return 1 + down(n - 1);
    endfunction
    function automatic int wide(int n);
        logic [16383:0] keep;
        keep = {512{n}};
        if (n == 0) return 0;
        return wide(n - 1) + keep[31:0] - n + 1;
    endfunction
    initial begin
        deep = down(100);
        wide_result = wide(100);
    end
endmodule
"#;
    for backend in backends() {
        assert_eq!(
            simulate(source, false, true, backend, 0, &["deep", "wide_result"]),
            Ok(vec![value(100), value(100)]),
            "{backend}"
        );
    }
    let too_deep = source.replace("down(100)", "down(5000)");
    let too_wide = source.replace("wide(100)", "wide(1000)");
    for backend in backends() {
        let deep = simulate(&too_deep, false, true, backend, 0, &["deep"]);
        let wide = simulate(&too_wide, false, true, backend, 0, &["wide_result"]);
        // Wasmtime's own stack runs out before 4096 activations (the host
        // stack depth item of #2988): the run still fails, as a trap.
        if backend == "wasm" {
            assert!(deep.is_err(), "{backend}: {deep:?}");
            assert!(wide.is_err(), "{backend}: {wide:?}");
            continue;
        }
        assert!(
            deep.as_ref()
                .is_err_and(|error| error.contains("call depth exceeds 4096")),
            "{backend}: {deep:?}"
        );
        assert!(
            wide.as_ref()
                .is_err_and(|error| error.contains("stack overflow in a call of `wide`")),
            "{backend}: {wide:?}"
        );
    }
}

#[test]
fn ineligible_subroutines_stay_inlined() {
    // A host request that a unit cannot make (a mailbox method) keeps a
    // subroutine inlined with every subroutine that calls it; `plain` and `bump`, whose `ref` formal
    // names `a`, still go out of line.
    let source = r#"module Top(output int a, output int b, output int c);
    function automatic void bump(ref int v);
        v = v + 1;
    endfunction
    function automatic int shout(int v);
        mailbox #(int) box = new();
        return v + box.num();
    endfunction
    function automatic int via_shout(int v);
        return shout(v) * 2;
    endfunction
    function automatic int plain(int v);
        return v - 1;
    endfunction
    initial begin
        a = 1;
        bump(a);
        b = via_shout(3);
        c = plain(a);
    end
endmodule
"#;
    same_as_inlined(source, 2, 0, &["a", "b", "c"]);
}

#[test]
fn static_subroutines_share_their_storage_in_recursion() {
    // A static function's formals and locals belong to the function, shared
    // by every activation (IEEE 1800-2023 13.4.2): after the recursion
    // `keep` holds the innermost value. An automatic one keeps its own.
    // Icarus and Verilator give s = 0 and a = 3 too; inlining cannot
    // expand the recursion, so it is no shared suite case.
    let source = r#"`timescale 1ns/1ns
module Top(output int s, output int a);
    int r;
    function int shared_depth(int n);
        int keep;
        int ignored;
        keep = n;
        if (n > 0) ignored = shared_depth(n - 1);
        return keep;
    endfunction
    function automatic int own_depth(int n);
        int keep;
        int ignored;
        keep = n;
        if (n > 0) ignored = own_depth(n - 1);
        return keep;
    endfunction
    initial begin
        #1 r = 3;
        s = shared_depth(r);
        a = own_depth(r);
    end
endmodule
"#;
    assert_eq!(function_units(source, true), 2);
    for four_state in [false, true] {
        for backend in backends() {
            assert_eq!(
                simulate(source, four_state, true, backend, 2, &["s", "a"]),
                Ok(vec![value(0), value(3)]),
                "{backend}, four-state {four_state}"
            );
        }
    }
}

#[test]
fn functions_no_call_reaches_are_dropped() {
    // `peek` reads `m`, which its always_ff process writes first, so its
    // call stays inlined after its unit was lowered: the unit is dropped,
    // and `bump`, lowered after it, is renumbered.
    let source = r#"`timescale 1ns/1ns
module Top(output logic [7:0] q, output logic [7:0] r);
    logic clk = 0;
    logic [7:0] a = 5;
    logic [7:0] m;
    function automatic logic [7:0] peek(input logic [7:0] x);
        return x + m;
    endfunction
    function automatic logic [7:0] bump(input logic [7:0] x);
        return x + 1;
    endfunction
    always_ff @(posedge clk) begin
        m = a;
        q <= peek(a);
    end
    initial begin
        #1 clk = 1;
        r = bump(a);
    end
endmodule
"#;
    same_as_inlined(source, 1, 3, &["q", "r"]);
    assert_eq!(
        simulate(source, false, true, "interpreter", 3, &["q", "r"]),
        Ok(vec![value(10), value(6)])
    );
}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[test]
fn lane_units_that_call_are_ordered_and_others_stay_parallel() {
    // Calls share the call stack, so the lane footprint orders the units
    // that make them; the others are partitioned as without calls.
    let source = r#"module Top(input logic clk, input logic [31:0] a, input logic [31:0] b,
           output logic [31:0] q0, output logic [31:0] q1, output logic [31:0] q2,
           output logic [31:0] q3, output logic [31:0] c0, output logic [31:0] c1);
    function automatic logic [31:0] mix(input logic [31:0] x, input logic [31:0] y);
        logic [31:0] t;
        t = x ^ (y << 3);
        return t + (x >> 2);
    endfunction
    always_ff @(posedge clk) q0 <= a + b;
    always_ff @(posedge clk) q1 <= a * 3;
    always_ff @(posedge clk) q2 <= b - a;
    always_ff @(posedge clk) q3 <= a ^ b;
    always_ff @(posedge clk) c0 <= mix(a, b);
    always_ff @(posedge clk) c1 <= mix(b, a);
endmodule
"#;
    let parallel = || {
        SimulatorBuilder::<Simulator>::from_sv_sources(vec![(source, Path::new("lanes.sv"))], "Top")
            .force_out_of_line_calls()
            .threads(4)
            .parallel_partition(celox::ParallelPartition::Always)
    };
    let result = parallel().trace_post_optimized_sir().build_with_trace();
    let sir = &result
        .trace
        .post_optimized_sir
        .as_ref()
        .expect("compiles")
        .sir;
    assert_eq!(sir.functions.len(), 1);
    let kernels = &sir.parallel.as_ref().expect("partitioned").eval_apply_ffs;
    let calls = |unit: &celox_sir::ExecutionUnit<_>| {
        unit.blocks.values().any(|block| {
            block
                .instructions
                .iter()
                .any(|instruction| matches!(instruction, celox_sir::SIRInstruction::Call { .. }))
        })
    };
    let units: Vec<_> = kernels.values().flat_map(|kernel| kernel.units()).collect();
    let lanes: std::collections::BTreeSet<u32> = units
        .iter()
        .filter(|unit| !calls(&unit.unit))
        .map(|unit| unit.lane)
        .collect();
    assert!(lanes.len() > 1, "call-free units on lanes {lanes:?}");
    let calling: Vec<u32> = units
        .iter()
        .filter(|unit| calls(&unit.unit))
        .map(|unit| unit.lane)
        .collect();
    assert!(!calling.is_empty(), "no lane unit calls");
    let expected = |a: u32, b: u32| {
        let mix = |x: u32, y: u32| (x ^ (y << 3)).wrapping_add(x >> 2);
        [
            a.wrapping_add(b),
            a.wrapping_mul(3),
            b.wrapping_sub(a),
            a ^ b,
            mix(a, b),
            mix(b, a),
        ]
    };
    fn run<B: SimBackend>(mut sim: Simulator<B>, expected: impl Fn(u32, u32) -> [u32; 6]) {
        let clk = sim.event("clk");
        let (a, b) = (sim.signal("a"), sim.signal("b"));
        let outputs = ["q0", "q1", "q2", "q3", "c0", "c1"].map(|name| sim.signal(name));
        for (x, y) in [(1u32, 2u32), (0xdead_beef, 0x1234_5678), (7, 0xffff_ffff)] {
            sim.modify(|io| {
                io.set(a, x);
                io.set(b, y);
            })
            .unwrap();
            sim.tick(clk).unwrap();
            let got = outputs.map(|signal| sim.get_as::<u32>(signal));
            assert_eq!(got, expected(x, y), "a {x:#x} b {y:#x}");
        }
    }
    run(result.res.expect("builds natively"), expected);
    run(
        parallel().build_cranelift().expect("builds with Cranelift"),
        expected,
    );
}

#[test]
fn functions_first_lowered_in_a_forked_activation_serve_every_call() {
    // Each call with constant arguments is inlined, in an activation of
    // the forked branch that copies `n`; the recursive call in it goes out
    // of line. The function unit is built from the declared `n` and
    // result, not the copies of the first activation, so the second call
    // passes its argument and gets its result through the frame too.
    let source = r#"`timescale 1ns/1ns
module Top(output int a, output int b, output int c);
    function automatic int fact(int n);
        if (n <= 1) return 1;
        return n * fact(n - 1);
    endfunction
    initial begin
        fork
            begin
                a = fact(5);
                b = fact(6);
                #1 c = fact(a - 115);
            end
        join
    end
endmodule
"#;
    assert_eq!(function_units(source, true), 1);
    for four_state in [false, true] {
        for backend in backends() {
            assert_eq!(
                simulate(source, four_state, true, backend, 2, &["a", "b", "c"]),
                Ok(vec![value(120), value(720), value(120)]),
                "{backend}, four-state {four_state}"
            );
        }
    }
}

#[test]
fn stores_a_wait_observes_stay_inlined() {
    // A level wait reads the static `count`, so its stores are instrumented:
    // `counter`, which stores it, stays inlined, and `twice`, which stores
    // nothing a wait observes, still goes out of line.
    let source = r#"`timescale 1ns/1ns
module Top(output int done_at, output int r, output int d);
    function int counter(bit advance);
        static int count = 0;
        if (advance) count = count + 1;
        return count;
    endfunction
    function automatic int twice(int v);
        return v * 2;
    endfunction
    bit advance = 0;
    initial begin
        #1 advance = 1;
        r = counter(advance);
        d = twice(r);
    end
    initial begin
        wait (counter(0) == 1);
        done_at = $time;
    end
endmodule
"#;
    same_as_inlined(source, 1, 3, &["done_at", "r", "d"]);
    assert_eq!(
        simulate(
            source,
            false,
            true,
            "interpreter",
            3,
            &["done_at", "r", "d"]
        ),
        Ok(vec![value(1), value(1), value(2)])
    );
}

#[test]
fn callers_of_subroutines_with_ref_formals_go_out_of_line() {
    // `bumped_twice` goes out of line, and so does `bump`, bound to the
    // frame slot of its local, which each call copies in and back.
    let source = r#"module Top(input int i, output int a, output int b);
    function automatic void bump(ref int v);
        v = v + 1;
    endfunction
    function automatic int bumped_twice(int v);
        int t = v;
        bump(t);
        bump(t);
        return t * 2;
    endfunction
    initial begin
        a = bumped_twice(i);
        b = bumped_twice(a);
    end
endmodule
"#;
    same_as_inlined(source, 2, 0, &["a", "b"]);
    assert_eq!(
        simulate(source, false, true, "interpreter", 0, &["a", "b"]),
        Ok(vec![value(4), value(12)])
    );
}

#[test]
fn calls_kept_only_for_sensitivity_count_as_observing() {
    // The branch that never runs still makes `always_comb` sensitive to
    // what `counter` reads (IEEE 1800-2023 9.2.2.2.1), the static `total`
    // among it, so `counter`, which stores `total`, stays inlined.
    let source = r#"module Top(input int i, output int y, output int r);
    function int counter(int v);
        static int total = 0;
        total = total + v;
        return total;
    endfunction
    function automatic int pick(int v);
        if (0) return counter(v);
        return v + 1;
    endfunction
    always_comb y = pick(i);
    initial r = counter(i + 2);
endmodule
"#;
    same_as_inlined(source, 1, 0, &["y", "r"]);
    assert_eq!(
        simulate(source, false, true, "interpreter", 0, &["y", "r"]),
        Ok(vec![value(1), value(2)])
    );
}

#[test]
fn defaults_a_level_wait_watches_count_as_observing() {
    // A level wait watches what the defaults of the formals of the
    // functions it calls read, supplied or not, the static `total` of
    // `counter` among it, so `counter`, which stores `total`, stays inlined.
    // `ready`, whose call supplies `v`, still goes out of line.
    let source = r#"`timescale 1ns/1ns
module Top(input int i, output int done_at, output int r);
    function int counter(int v);
        static int total = 0;
        total = total + v;
        return total;
    endfunction
    function automatic bit ready(int a, int v = counter(0));
        return a + v > 1;
    endfunction
    initial begin
        wait (ready(i, 2));
        done_at = $time + 1;
    end
    initial #1 r = counter(i + 1);
endmodule
"#;
    same_as_inlined(source, 1, 2, &["done_at", "r"]);
    assert_eq!(
        simulate(source, false, true, "interpreter", 2, &["done_at", "r"]),
        Ok(vec![value(1), value(1)])
    );
}

#[test]
fn ref_formals_pass_their_places_out_of_line() {
    // A `ref` formal's place selects the function and passes the rest in
    // the frame: a module queue the unit names itself (`push(q, ..)`), a
    // class property through the handle the call evaluated, an array
    // element at the index the call fixed (`i` changes after it), and an
    // automatic of the calling unit, copied in and back (`twice`'s `t`,
    // `local_queue`'s queue, also passed on to a second unit).
    let source = r#"module Top(output int a, output int b, output int c, output int d, output int e);
    class Node;
        int v;
    endclass
    int q[$];
    int arr[4];
    function automatic void push(ref int target[$], input int n);
        target.push_back(n);
        target.push_back(target.size() + n);
    endfunction
    function automatic void bump(ref int v, input int by);
        v = v + by;
    endfunction
    function automatic void swap_ends(ref int target[$]);
        int t = target[0];
        target[0] = target[target.size() - 1];
        target[target.size() - 1] = t;
    endfunction
    function automatic int local_queue(int n);
        int mine[$];
        push(mine, n);
        swap_ends(mine);
        return mine[0] * 100 + mine[1];
    endfunction
    function automatic int twice(int start);
        int t = start;
        bump(t, start);
        bump(t, 1);
        return t;
    endfunction
    int one = 1;
    initial begin
        Node n = new;
        int i = one;
        push(q, one + 2);
        bump(n.v, one + 4);
        bump(n.v, one);
        bump(arr[i], one + 6);
        i = 2;
        bump(arr[i], one + 8);
        a = q[0] * 100 + q[1];
        b = n.v;
        c = arr[1] * 100 + arr[2];
        d = twice(one + 3);
        e = local_queue(one + 4);
    end
endmodule
"#;
    same_as_inlined(source, 9, 0, &["a", "b", "c", "d", "e"]);
}

#[test]
fn recursive_calls_pass_their_ref_formals_on() {
    // `collect` passes its own `ref` formal to itself: the place is the
    // same, so the call enters the same function, for a module queue and
    // for an automatic queue of `gather`.
    let source = r#"module Top(output int a, output int b);
    int q[$];
    function automatic void collect(ref int out[$], input int depth);
        out.push_back(depth);
        if (depth > 0) collect(out, depth - 1);
    endfunction
    function automatic int gather(int depth);
        int mine[$];
        collect(mine, depth);
        return mine.size() * 100 + mine[0];
    endfunction
    int one = 1;
    initial begin
        collect(q, one + 2);
        a = q.size() * 100 + q[0] * 10 + q[3];
        b = gather(one + 3);
    end
endmodule
"#;
    assert_eq!(function_units(source, true), 3);
    for four_state in [false, true] {
        for backend in backends() {
            assert_eq!(
                simulate(source, four_state, true, backend, 0, &["a", "b"]),
                Ok(vec![value(430), value(504)]),
                "{backend}, four-state {four_state}"
            );
        }
    }
}

#[test]
fn automatics_passed_by_ref_keep_their_copy_across_suspensions() {
    // `slow_bump` waits twice with the copy of `run`'s automatic in its
    // frame; meanwhile the sibling fork branch runs its own call of `run`,
    // whose `mine` is another automatic with another copy, and the copies
    // come back into each `mine` when each call returns.
    let source = r#"`timescale 1ns/1ns
module Top(output int a, output int b, output int c);
    task automatic slow_bump(ref int v, input int by);
        #1 v = v + by;
        #1 v = v + by;
    endtask
    task automatic run(input int start, output int result);
        int mine = start;
        slow_bump(mine, start);
        result = mine;
    endtask
    int one = 1;
    initial begin
        fork
            run(one, a);
            run(one + 9, b);
        join
        c = a * 100 + b;
    end
endmodule
"#;
    same_as_inlined(source, 2, 5, &["a", "b", "c"]);
    assert_eq!(
        simulate(source, false, true, "interpreter", 5, &["a", "b", "c"]),
        Ok(vec![value(3), value(30), value(330)])
    );
}

#[test]
fn ref_places_a_call_cannot_pass_stay_inlined() {
    // One automatic bound to two `ref` formals would be two copies, so
    // `add_both` stays inlined into `aliased`; `bump(watched, ..)` stores
    // into what a continuous assignment reads, which assembly instruments,
    // so that call stays inlined too.
    let source = r#"module Top(output int a, output int b, output int w);
    function automatic void add_both(ref int x, ref int y);
        x = x + 1;
        y = y + 10;
    endfunction
    function automatic int aliased(int start);
        int t = start;
        add_both(t, t);
        return t;
    endfunction
    function automatic void bump(ref int v, input int by);
        v = v + by;
    endfunction
    int watched;
    assign w = watched + 1;
    int one = 1;
    initial begin
        a = aliased(one);
        bump(watched, one + 1);
        b = watched;
    end
endmodule
"#;
    same_as_inlined(source, 1, 0, &["a", "b", "w"]);
}

#[test]
fn automatic_arrays_of_units_with_one_live_activation() {
    // `tabulate` and `fresh` keep their local arrays as module variables: no
    // call cycle reaches them and they do not wait, so one activation at a
    // time is live. Two processes call `tabulate` in the same time slot, each
    // across a host request between writing and reading its array; the
    // host serves the request before the other process runs, so neither
    // sees the other's elements. Each activation of `fresh` starts with its
    // array cleared. `slow` waits with its array live, so a second process
    // could enter it meanwhile: it stays inlined, each call with its own.
    let source = r#"`timescale 1ns/1ns
module Top(output int a, output int b, output int c, output int d, output int e, output int f);
    function automatic int tabulate(int n);
        int q[4];
        string text;
        foreach (q[k]) q[k] = n * 10 + k;
        text = $sformatf("%0d", q[n % 4]);
        return q[0] + q[3] + text.len() * 1000;
    endfunction
    function automatic int fresh(int n);
        int q[3];
        q[n % 3] = n;
        return q[0] + q[1] * 10 + q[2] * 100;
    endfunction
    task automatic slow(input int n, output int r);
        int q[2];
        q[0] = n;
        #1 q[1] = n + 1;
        r = q[0] * 100 + q[1];
    endtask
    int one = 1;
    initial begin
        a = tabulate(one + 1);
        #1 c = fresh(one);
        slow(one + 2, e);
    end
    initial begin
        b = tabulate(one + 4);
        #1 d = fresh(one + 1);
        slow(one + 6, f);
    end
endmodule
"#;
    same_as_inlined(source, 2, 4, &["a", "b", "c", "d", "e", "f"]);
}

#[test]
fn event_triggers_in_function_units() {
    // Units trigger events, blocking and nonblocking, and read whether one
    // was triggered: each is a host request that wakes the waiting
    // processes and lets the caller continue at once. A class's event is
    // triggered through its object, as UVM's objections trigger theirs.
    let source = r#"`timescale 1ns/1ns
module Top(output int a, output int b, output int c, output int d, output int e);
    class Box;
        event dropped;
        function void drop(int n);
            if (n > 0) -> dropped;
        endfunction
    endclass
    event done, late;
    function automatic void fire(int n);
        if (n > 0) -> done;
        ->> late;
    endfunction
    function automatic bit seen();
        return done.triggered;
    endfunction
    function automatic int check(int n);
        fire(n);
        return seen() + 10;
    endfunction
    Box box = new;
    int one = 1;
    initial begin
        #1 a = check(one);
        #1 box.drop(one);
    end
    initial begin
        @(done) b = $time;
        @(late) c = $time;
    end
    initial begin
        wait (done.triggered) d = 5;
    end
    initial begin
        @(box.dropped) e = $time;
    end
endmodule
"#;
    same_as_inlined(source, 5, 3, &["a", "b", "c", "d", "e"]);
    assert_eq!(
        simulate(
            source,
            false,
            true,
            "interpreter",
            3,
            &["a", "b", "c", "d", "e"]
        ),
        Ok(vec![value(11), value(1), value(1), value(5), value(2)])
    );
    // A level wait's predicate that reads `triggered` through a call
    // observes the event, which a unit cannot tell the host: the call stays
    // inlined, where it is rejected as before.
    let predicate = source.replace("wait (done.triggered)", "wait (seen())");
    for forced in [false, true] {
        let built = simulate(&predicate, false, forced, "interpreter", 3, &["d"]);
        assert!(
            built
                .as_ref()
                .is_err_and(|error| error.contains("read by a function called in a wait condition")),
            "forced {forced}: {built:?}"
        );
    }
}

#[test]
fn process_control_in_function_units() {
    // Units read a process's status, kill another process, resume one and
    // put and take semaphore keys without waiting: host requests after
    // which the caller continues. `nap` suspends its own process inside the
    // unit, with its frames kept until `wake` resumes it. UVM's objections
    // kill their drain processes this way.
    let source = r#"`timescale 1ns/1ns
module Top(output int a, output int b, output int c, output int d, output int e);
    process worker, sleeper;
    semaphore keys = new(0);
    function automatic int state_of(process p);
        return p == null ? -1 : int'(p.status());
    endfunction
    function automatic void stop(process p);
        if (p != null) p.kill();
    endfunction
    function automatic void wake(process p);
        if (p != null) p.resume();
    endfunction
    task automatic nap(int n);
        int start = n;
        process::self().suspend();
        b = start * 100 + $time;
    endtask
    function automatic void give(int n);
        keys.put(n);
    endfunction
    function automatic bit take(int n);
        return keys.try_get(n);
    endfunction
    int one = 1;
    initial begin
        worker = process::self();
        #1;
        forever begin
            a = a + 1;
            #2;
        end
    end
    initial begin
        sleeper = process::self();
        nap(one + 4);
    end
    initial begin
        #3 wake(sleeper);
        #1 c = state_of(worker) * 10;
        stop(worker);
        c = c + state_of(worker);
        give(one + 1);
        e = take(one) + take(one) * 2 + take(one) * 4;
        #3 d = a;
    end
endmodule
"#;
    same_as_inlined(source, 6, 8, &["a", "b", "c", "d", "e"]);
    assert_eq!(
        simulate(
            source,
            false,
            true,
            "interpreter",
            8,
            &["a", "b", "c", "d", "e"]
        ),
        Ok(vec![value(2), value(503), value(24), value(2), value(3)])
    );
}

#[test]
fn heap_operations_go_out_of_line() {
    // Queue concatenation, array locators, streaming of dynamically sized
    // data, new events, the random and constraint modes and the constraint
    // program a constructor registers are heap calls, which a function unit
    // makes as an inlined body does. The iterator of a locator is a local of
    // the function, in its frame.
    let source = r#"module Top(input int i, output int a, output int b, output int c, output int d);
    class K;
        rand int x;
        constraint lo { x < 10; }
    endclass
    function automatic int queues(int v);
        int q[$] = '{1, 2, 3};
        int found[$];
        q = {q, v, v + 1};
        found = q.find with (item > 2);
        return found.size() * 100 + found[0];
    endfunction
    function automatic int streams(int v);
        byte bytes[];
        int r;
        bytes = {>>8{v}};
        r = {<<8{bytes}};
        return r + bytes.size();
    endfunction
    function automatic int modes(K k, int v);
        int was = k.lo.constraint_mode();
        k.lo.constraint_mode(v[0]);
        k.x.rand_mode(0);
        return was * 100 + k.lo.constraint_mode() * 10 + k.x.rand_mode();
    endfunction
    function automatic int events(int v);
        event ev;
        event other;
        other = ev;
        return (ev != null) + 2 * (other == ev) + v;
    endfunction
    initial begin
        K k;
        k = new;
        a = queues(i);
        b = streams(i);
        c = modes(k, i);
        d = events(i);
    end
endmodule
"#;
    same_as_inlined(source, 5, 0, &["a", "b", "c", "d"]);
}

#[test]
fn recursive_methods() {
    // Inside a method, a call of its own name calls the method; the name
    // alone is its result variable (IEEE 1800-2023 13.4.1).
    let source = r#"module Top(input int i, output int f, output int g, output int s);
    class C;
        int base;
        extern virtual function int fact(int n);
        function int fib(int n);
            if (n < 2) return n + base;
            return fib(n - 1) + fib(n - 2) - base;
        endfunction
        static function int sum(int n);
            sum = 0;
            for (int k = 1; k <= n; k++) sum = sum + k;
        endfunction
    endclass
    function int C::fact(int n);
        if (n <= 1) return 1;
        return n * fact(n - 1);
    endfunction
    initial begin
        C c;
        c = new;
        f = c.fact(i);
        g = c.fib(i + 5);
        s = C::sum(i);
    end
endmodule
"#;
    let inlined = simulate(source, false, false, "interpreter", 0, &["f"]);
    assert!(
        inlined
            .as_ref()
            .is_err_and(|error| error.contains("recursive call")),
        "{inlined:?}"
    );
    // With `i` 0: 0! = 1, fib(5) = 5, and the empty sum.
    for four_state in [false, true] {
        for backend in backends() {
            assert_eq!(
                simulate(source, four_state, true, backend, 0, &["f", "g", "s"]),
                Ok(vec![value(1), value(5), value(0)]),
                "{backend}, four-state {four_state}"
            );
        }
    }
}

#[test]
fn recursive_methods_of_every_result_type() {
    // A recursive call types its result by the method it calls, not by the
    // result variable its name also is (IEEE 1800-2023 13.4.1), for every
    // kind of result.
    let source = r#"interface Bus;
    int v;
endinterface
module Top(input int i, output int a, output int b, output int c, output int d,
           output int e, output int f);
    Bus bus();
    typedef enum int { ZERO, ONE, TWO } level_t;
    typedef struct { int x; int y; } pair_t;
    typedef int queue_t[$];
    class Node;
        int depth;
        virtual Bus port;
        function string text(int n);
            if (n == 0) return "x";
            return {text(n - 1), "y"};
        endfunction
        function queue_t items(int n);
            if (n == 0) return '{};
            return {items(n - 1), n};
        endfunction
        function Node chain(int n);
            if (n == 0) return this;
            return chain(n - 1);
        endfunction
        function level_t level(int n);
            if (n == 0) return ZERO;
            return level_t'(level(n - 1) + 1);
        endfunction
        function pair_t pair(int n);
            pair_t p;
            if (n == 0) return '{0, 1};
            p = pair(n - 1);
            return '{p.x + 1, p.y * 2};
        endfunction
        function virtual Bus vif(int n);
            if (n == 0) return port;
            return vif(n - 1);
        endfunction
    endclass
    initial begin
        Node node;
        Node last;
        int q[$];
        pair_t p;
        string t;
        virtual Bus port;
        node = new;
        node.port = bus;
        node.depth = 7;
        bus.v = 5;
        t = node.text(i + 2);
        a = t.len();
        q = node.items(i + 3);
        b = q.size() * 10 + q[2];
        last = node.chain(i + 2);
        c = last.depth;
        d = node.level(i + 2);
        p = node.pair(i + 3);
        e = p.x * 100 + p.y;
        port = node.vif(i + 2);
        f = port.v;
    end
endmodule
"#;
    let inlined = simulate(source, false, false, "interpreter", 0, &["a"]);
    assert!(
        inlined
            .as_ref()
            .is_err_and(|error| error.contains("recursive call")),
        "{inlined:?}"
    );
    for four_state in [false, true] {
        for backend in backends() {
            assert_eq!(
                simulate(
                    source,
                    four_state,
                    true,
                    backend,
                    0,
                    &["a", "b", "c", "d", "e", "f"]
                ),
                Ok(vec![
                    value(3),
                    value(33),
                    value(7),
                    value(2),
                    value(308),
                    value(5)
                ]),
                "{backend}, four-state {four_state}"
            );
        }
    }
}

#[test]
fn process_random_state_in_function_units() {
    // A unit draws on the RNG of the process that calls it, which the call
    // passes in its frame and takes back: object seeding, `shuffle`,
    // `$urandom`, `std::randomize` and `$random` give each process the
    // sequence they give it inlined, through a unit calling a unit and two
    // processes calling the same units in turn. `$time` is the simulation
    // time.
    let source = r#"`timescale 1ns/1ns
module Top(output int a, output int b, output int c, output int d, output int e,
           output longint t);
    class Obj;
        rand int x;
        constraint bounded { x inside {[0:99]}; }
    endclass
    function automatic int one();
        return $urandom % 1000;
    endfunction
    function automatic int draw(int n);
        int sum = 0;
        for (int k = 0; k < n; k++) sum = sum * 3 + one();
        return sum;
    endfunction
    function automatic int ranged();
        return $urandom_range(10, 20) + draw(1);
    endfunction
    function automatic int made();
        Obj o = new;
        void'(o.randomize());
        return o.x;
    endfunction
    function automatic int shuffled();
        int q[$] = '{1, 2, 3, 4, 5};
        q.shuffle();
        return q[0] * 10000 + q[1] * 1000 + q[2] * 100 + q[3] * 10 + q[4];
    endfunction
    function automatic int scoped();
        int v;
        void'(std::randomize(v));
        return v;
    endfunction
    function automatic int legacy();
        return $random;
    endfunction
    function automatic longint now();
        return $time;
    endfunction
    initial begin
        a = draw(3);
        #1 b = ranged() ^ made();
        #2 a = a ^ draw(2);
    end
    initial begin
        c = draw(2);
        #1 d = shuffled() ^ scoped();
        #2 c = c ^ ranged() ^ $urandom;
    end
    initial begin
        e = legacy();
        #3 e = e ^ legacy() ^ $random;
        t = now();
    end
endmodule
"#;
    same_as_inlined(source, 9, 4, &["a", "b", "c", "d", "e", "t"]);
}

#[test]
fn recursive_functions_draw_on_the_process_rng() {
    // A recursive function passes the RNG down its activations and back:
    // two processes calling it in turn get the sequences a loop gives them
    // inlined.
    let design = |draw: &str| {
        format!(
            r#"`timescale 1ns/1ns
module Top(output int a, output int b);
{draw}
    initial begin
        a = draw(3);
        #2 a = a ^ draw(2);
    end
    initial begin
        #1 b = draw(4);
        #2 b = b ^ draw(1) ^ $urandom;
    end
endmodule
"#
        )
    };
    let recursive = design(
        "    function automatic int draw(int n);
        if (n == 0) return 0;
        return $urandom % 1000 + draw(n - 1) * 3;
    endfunction",
    );
    // The same draws, in the order the recursion makes them.
    let looped = design(
        "    function automatic int draw(int n);
        int parts[8];
        int sum = 0;
        for (int k = 0; k < n; k++) parts[k] = $urandom % 1000;
        for (int k = n - 1; k >= 0; k--) sum = parts[k] + sum * 3;
        return sum;
    endfunction",
    );
    assert_eq!(function_units(&recursive, true), 1, "function units");
    for four_state in [false, true] {
        let expected = simulate(&looped, four_state, false, "interpreter", 4, &["a", "b"])
            .expect("the looped design runs");
        for backend in backends() {
            assert_eq!(
                simulate(&recursive, four_state, true, backend, 4, &["a", "b"]),
                Ok(expected.clone()),
                "{backend}, four-state {four_state}"
            );
        }
    }
}

#[test]
fn always_ff_processes_pass_their_rng() {
    // An always_ff process keeps its RNG state among its writes, which the
    // call passes and takes back as it does any other process's.
    let source = r#"`timescale 1ns/1ns
module Top(output int q, output int r);
    logic clk = 0;
    function automatic int one(int bias);
        return $urandom % 1000 + bias;
    endfunction
    always_ff @(posedge clk) begin
        q <= one(q) ^ $urandom;
        r <= one(r + 1);
    end
    initial begin
        #1 clk = 1;
        #1 clk = 0;
        #1 clk = 1;
    end
endmodule
"#;
    same_as_inlined(source, 1, 4, &["q", "r"]);
}

#[test]
fn reports_and_messages_go_out_of_line() {
    // Every print and message of a unit, also of one a unit calls, reports
    // what it reports inlined: its text, its time and its scope. `$fatal`
    // ends the run from inside the call.
    let source = r#"`timescale 1ns/1ns
module Top(output int r, output int c);
    function automatic int checked(int x);
        $display("checked %0d at %0t in %m", x, $time);
        if (x > 2) $error("big %0d", x);
        else $info("small %0d", x);
        $write("[%0d]", x);
        return twice(x);
    endfunction
    function automatic int twice(int x);
        $display("twice %h", x);
        if (x == 0) $warning("zero");
        return 2 * x;
    endfunction
    function automatic void die(int x);
        if (x == 7) $fatal(1, "dead %0d", x);
    endfunction
    int k = 1;
    always_comb c = twice(r);
    initial begin
        r = checked(k);
        #1 r = checked(k + 4) + checked(k - 1);
        #1 die(k + 5);
        #1 die(k + 6);
        r = 99;
    end
endmodule
"#;
    same_as_inlined(source, 3, 2, &["r", "c"]);
    for four_state in [false, true] {
        let inlined = simulate_run(source, four_state, false, "interpreter", 5, &["r"]);
        assert_eq!(inlined, Err("dead 7".to_string()));
        for backend in backends() {
            assert_eq!(
                simulate_run(source, four_state, true, backend, 5, &["r"]),
                inlined,
                "{backend}, four-state {four_state}"
            );
        }
    }
}

#[test]
fn recursive_functions_report_each_activation() {
    // Inlined, recursion is rejected: the same reports in a loop are the
    // reference.
    let recursive = r#"`timescale 1ns/1ns
module Top(output int r);
    function automatic int count(int n);
        $display("count %0d at %0t", n, $time);
        if (n == 0) begin
            $warning("bottom");
            return 0;
        end
        $write("[%0d]", n);
        return count(n - 1) + 1;
    endfunction
    int k = 3;
    initial begin
        r = count(k);
        #1 r = count(k - 1);
    end
endmodule
"#;
    let looped = recursive.replace(
        "        $display(\"count %0d at %0t\", n, $time);
        if (n == 0) begin
            $warning(\"bottom\");
            return 0;
        end
        $write(\"[%0d]\", n);
        return count(n - 1) + 1;",
        // The warning keeps its line.
        "        for (int m = n; m > 0; m--) begin $display(\"count %0d at %0t\", m, $time); $write(\"[%0d]\", m); end
        $display(\"count %0d at %0t\", 0, $time);
        $warning(\"bottom\");
        return n;",
    );
    assert_ne!(looped, recursive);
    assert_eq!(function_units(recursive, true), 1, "function units");
    for four_state in [false, true] {
        let expected = simulate_run(&looped, four_state, false, "interpreter", 2, &["r"])
            .expect("the looped design runs");
        for backend in backends() {
            assert_eq!(
                simulate_run(recursive, four_state, true, backend, 2, &["r"]),
                Ok(expected.clone()),
                "{backend}, four-state {four_state}"
            );
        }
    }
}

#[test]
fn host_requests_suspend_through_function_units() {
    // Each unit of the chain makes host requests before and after its call,
    // drawing on the process RNG around them; two processes call it in
    // turn. Each request suspends the process inside the calls, which
    // continue where they stopped.
    let source = r#"`timescale 1ns/1ns
module Top(output int a, output int b, output int c, output int d);
    function automatic string inner(int n);
        string text = $sformatf("inner %0d %0d", n, $urandom % 100);
        int parsed;
        void'($sscanf(text, "inner %d", parsed));
        return $sformatf("%s/%0d", text, parsed + 1);
    endfunction
    function automatic int middle(int n);
        string first = $sformatf("m%0d", n);
        string got = inner(n + 1);
        int x = $urandom_range(9);
        $display("%s %s %0d", first, got, x);
        return got.len() + x;
    endfunction
    function automatic int outer(int n);
        int sum = 0;
        for (int k = 0; k < n; k++) begin
            sum += middle(k);
            if ($test$plusargs("never")) sum = 0;
        end
        $display("outer %0d", $sformatf("%0d", sum).len());
        return sum;
    endfunction
    int one = 1;
    initial begin
        a = outer(one + 2);
        #2 c = outer(one);
        fork
            d = outer(one + 1);
        join
    end
    initial begin
        #1 b = outer(one + 1);
    end
endmodule
"#;
    same_as_inlined(source, 3, 4, &["a", "b", "c", "d"]);
}

#[test]
fn process_methods_suspend_through_function_units() {
    // `process::self()`, the RNG state of a process and a new semaphore are
    // host requests a unit makes by suspending. `keep_rng` saves the RNG
    // state of its process around a construction that draws on it, as UVM
    // does, and draws in between; `reseed` seeds the process; `same` gets
    // a handle passed in; `make` returns a semaphore the process then
    // takes keys of. Two processes and a fork call them, so each request
    // reads the RNG of the process that made it, which a caller holds
    // while its call is suspended.
    let source = r#"`timescale 1ns/1ns
module Top(output int a, output int b, output int c, output int d, output int e);
    class Item;
        int v;
        function new();
            v = $urandom % 1000;
        endfunction
    endclass
    function automatic int keep_rng(int n);
        process p = process::self();
        string state = p.get_randstate();
        Item item = new;
        int first = $urandom % 1000;
        p.set_randstate(state);
        item = new;
        return n + (first == $urandom % 1000 ? item.v : -1);
    endfunction
    function automatic int reseed(int seed);
        process p = process::self();
        p.srandom(seed);
        return $urandom % 1000;
    endfunction
    function automatic bit same(process q);
        return process::self() == q;
    endfunction
    function automatic semaphore make(int keys);
        semaphore s = new(keys);
        return s;
    endfunction
    function automatic int nested(int n);
        process p = process::self();
        string state = p.get_randstate();
        int x = keep_rng(n);
        p.set_randstate(state);
        return x + keep_rng(n + 1) + $urandom % 10;
    endfunction
    int one = 1;
    initial begin
        process p = process::self();
        semaphore s = make(one + 1);
        a = keep_rng(one) + reseed(one + 4);
        b = same(p) + 2 * s.try_get(2) + 4 * s.try_get(1);
        #1 c = nested(one + 1);
        fork
            d = keep_rng(one + 2) + reseed(one + 8);
        join
    end
    initial begin
        #2 e = nested(one) + reseed(one + 4);
    end
endmodule
"#;
    same_as_inlined(source, 6, 3, &["a", "b", "c", "d", "e"]);
}

#[test]
fn functions_write_static_class_properties_and_module_variables() {
    // UVM's constructors count their instances in a static property and
    // its singletons keep theirs in one: writes no other process observes
    // go out of line. `touch` writes what a continuous assignment reads,
    // which assembly instruments, and stays inlined; `logged` writes a
    // variable that the `always_comb` process calling it owns as an output,
    // so it stays inlined too.
    let source = r#"module Top(output int a, output int b, output int c, output int d, output int f);
    class Counter;
        static int count = 0;
        static Counter inst;
        int id;
        function new();
            id = count++;
        endfunction
        static function void set(Counter c);
            inst = c;
        endfunction
        static function Counter get();
            if (inst == null) begin
                Counter made = new;
                set(made);
            end
            return inst;
        endfunction
    endclass
    int log_count;
    function automatic void note(int n);
        log_count += n;
    endfunction
    int seen;
    function automatic void touch(int n);
        seen = n;
    endfunction
    assign d = seen + 1;
    int comb_log;
    function automatic int logged(int n);
        comb_log = n;
        return n + 1;
    endfunction
    always_comb f = logged(a);
    initial begin
        Counter x = Counter::get();
        Counter y = new;
        Counter z = Counter::get();
        note(y.id + 2);
        note(y.id + 3);
        touch(y.id + 5);
        a = y.id;
        b = x == z ? Counter::count : -1;
        c = log_count;
    end
endmodule
"#;
    same_as_inlined(source, 4, 0, &["a", "b", "c", "d", "f"]);
}

#[test]
fn recursive_functions_make_host_requests() {
    // A format result made before a recursive call is kept across it: the
    // call reaches the same request site again.
    let recursive = r#"`timescale 1ns/1ns
module Top(output int a, output int b);
    function automatic string path(int n);
        string head;
        if (n == 0) return $sformatf("[%0d]", $urandom % 10);
        head = $sformatf("<%0d", n);
        return {head, path(n - 1), $sformatf("%0d>", n)};
    endfunction
    int k = 3;
    initial begin
        $display("%s", path(k));
        a = path(k - 1).len();
    end
    initial begin
        #1 $display("%s", path(k + 1));
        b = $urandom % 1000;
    end
endmodule
"#;
    let looped = recursive.replace(
        "        string head;
        if (n == 0) return $sformatf(\"[%0d]\", $urandom % 10);
        head = $sformatf(\"<%0d\", n);
        return {head, path(n - 1), $sformatf(\"%0d>\", n)};",
        "        string head = \"\";
        string tail = \"\";
        for (int m = n; m > 0; m--) begin
            head = {head, $sformatf(\"<%0d\", m)};
            tail = {$sformatf(\"%0d>\", m), tail};
        end
        return {head, $sformatf(\"[%0d]\", $urandom % 10), tail};",
    );
    assert_ne!(looped, recursive);
    assert_eq!(function_units(recursive, true), 1, "function units");
    for four_state in [false, true] {
        let expected = simulate_run(&looped, four_state, false, "interpreter", 2, &["a", "b"])
            .expect("the looped design runs");
        for backend in backends() {
            assert_eq!(
                simulate_run(recursive, four_state, true, backend, 2, &["a", "b"]),
                Ok(expected.clone()),
                "{backend}, four-state {four_state}"
            );
        }
    }
    // Every request inside the calls kept the live frames, and gave them back.
    let mut sim = Simulation::from_sv_sources(vec![(recursive, Path::new("calls.sv"))], "Top")
        .force_out_of_line_calls()
        .build_interpreter()
        .expect("the design builds");
    sim.run_until(2).expect("the design runs");
    let stats = sim.scheduler_stats();
    assert!(stats.call_stack_saves > 0);
    assert!(stats.call_stack_saved_bytes > 0);
    assert_eq!(
        stats.call_stack_saved_bytes,
        stats.call_stack_restored_bytes
    );
}

#[test]
fn errors_after_a_host_request_in_a_function_unit() {
    // The callee raises an error once it continues after its request.
    let source = r#"`timescale 1ns/1ns
module Top(output int r);
    function automatic int check(int x);
        string text = $sformatf("x=%0d", x);
        if (x == 3) $fatal(1, "%s", text);
        return text.len();
    endfunction
    function automatic int twice(int x);
        return check(x) + check(x + 1);
    endfunction
    int k = 1;
    initial begin
        r = twice(k);
        #1 r = twice(k + 1);
    end
endmodule
"#;
    same_as_inlined(source, 2, 0, &["r"]);
    for four_state in [false, true] {
        let inlined = simulate_run(source, four_state, false, "interpreter", 2, &["r"]);
        assert!(inlined.is_err(), "{inlined:?}");
        for backend in backends() {
            assert_eq!(
                simulate_run(source, four_state, true, backend, 2, &["r"]),
                inlined,
                "{backend}, four-state {four_state}"
            );
        }
    }
}

#[test]
fn file_output_in_function_units_with_native_services() {
    // The native backend serves file output of compiled kernels itself:
    // the edge process's `$fwrite` goes there, while the initial process's
    // calls into units that write files suspend through the reference loop.
    let source = r#"`timescale 1ns/1ns
module Top(output int r);
    bit clk = 0;
    int count = 0;
    function automatic int report(int n);
        $fdisplay(1, "report %0d", n);
        if (n > 0) $fwrite(1, "[%0d]", nested(n - 1));
        return n * 2;
    endfunction
    function automatic int nested(int n);
        $fwrite(1, "nested %0d;", n);
        return n + 1;
    endfunction
    always #1 clk = ~clk;
    always @(posedge clk) begin
        count <= count + 1;
        $fwrite(1, "edge %0d;", count);
    end
    initial begin
        r = report(count + 2);
        #3 r = report(count);
    end
endmodule
"#;
    same_as_inlined(source, 2, 6, &["r"]);
}

#[test]
fn delays_suspend_through_task_units() {
    // Each task of the chain waits before and after its call, so the
    // process sits inside the calls across time slots while the other
    // process runs the same tasks. `#0` resumes in the inactive region,
    // before the nonblocking assignment of the time is published.
    let source = r#"`timescale 1ns/1ns
module Top(output int a, output int b, output int c);
    int seen = 0;
    task automatic inner(input int n, output int r);
        int x = n * 3;
        #(n) x += $urandom % 7;
        $display("inner %0d at %0t", n, $time);
        #0 r = x + seen;
    endtask
    task automatic middle(input int n, output int r);
        int got;
        #1 inner(n + 1, got);
        $display("middle %0d got %0d at %0t", n, got, $time);
        r = got + 1;
    endtask
    task automatic outer(input int n, output int r);
        int sum = 0;
        for (int k = 0; k < n; k++) begin
            int part;
            middle(k, part);
            sum += part;
        end
        #2 r = sum;
    endtask
    int two = 2;
    initial begin
        outer(two + 1, a);
        seen <= 100;
        #0 c = seen;
    end
    initial begin
        #1 outer(two, b);
    end
endmodule
"#;
    same_as_inlined(source, 3, 40, &["a", "b", "c"]);
}

#[test]
fn saved_frames_keep_their_heap_handles_across_collections() {
    // Only the frames of the suspended tasks hold the objects and strings
    // they made. While they wait, the other process allocates enough for
    // the heap to collect between the time slots: the collector finds the
    // handles in the saved frames, walking the chain from the kernel's call.
    let source = r#"`timescale 1ns/1ns
module Top(output int a, output int n);
    class Box;
        int v;
        function new(int x);
            v = x;
        endfunction
    endclass
    task automatic hold(input int k, output int r);
        Box box = new(k);
        string s = "held";
        s = {s, "-", s};
        #5;
        r = box.v * 100 + s.len();
    endtask
    task automatic deep(input int k, output int r);
        Box mine = new(k + 1);
        string tag = "d";
        int got;
        tag = {tag, tag, tag};
        #1 hold(k, got);
        r = got + mine.v * 10000 + tag.len() * 1000000;
    endtask
    int seven = 7;
    int count = 2000;
    initial deep(seven, a);
    initial begin
        Box t;
        string u;
        #2;
        for (int i = 0; i < count; i++) begin
            t = new(i);
            u = {u, "x"};
            if (u.len() > 8) u = "";
            n += t.v % 2;
        end
    end
endmodule
"#;
    same_as_inlined(source, 3, 10, &["a", "n"]);
}

#[test]
fn state_files_keep_the_frames_of_suspended_calls() {
    // A state file saved while both processes wait inside their calls holds
    // the saved frames, strings by contents; a load on any backend resumes
    // them where they stopped. A frame holding a class object cannot be
    // saved, as a state object holding one cannot.
    let source = r#"`timescale 1ns/1ns
module Top(output int a, output int b);
    task automatic step(input int n, output int r);
        string s = "s";
        int x = n;
        repeat (n) s = {s, "+"};
        #(n) x += s.len();
        s = {s, "!"};
        #1 r = x * 10 + s.len();
    endtask
    task automatic chain(input int n, output int r);
        int first;
        int second;
        step(n, first);
        step(n + 1, second);
        r = first * 1000 + second;
    endtask
    int two = 2;
    initial chain(two, a);
    initial #1 chain(two + 1, b);
endmodule
"#;
    let build = |forced: bool| {
        let builder = Simulation::from_sv_sources(vec![(source, Path::new("calls.sv"))], "Top");
        if forced {
            builder.force_out_of_line_calls()
        } else {
            builder
        }
    };
    fn outputs<B: SimBackend>(sim: &mut Simulation<B>) -> (BigUint, BigUint) {
        sim.run_until(20).expect("the design runs");
        (sim.get(sim.signal("a")), sim.get(sim.signal("b")))
    }
    let expected = outputs(&mut build(false).build_interpreter().expect("builds"));
    let mut origin = build(true).build_interpreter().expect("builds");
    origin.run_until(3).expect("the design runs");
    assert!(origin.scheduler_stats().call_stack_saves > 0);
    let mut bytes = Vec::new();
    origin
        .save_state()
        .expect("the frames save")
        .write_to(&mut bytes)
        .expect("the file writes");
    let file = celox::StateFile::read_from(bytes.as_slice()).expect("the file reads");
    assert_eq!(outputs(&mut origin), expected, "the saved run continues");
    macro_rules! resumes {
        ($build:ident) => {{
            let mut target = build(true).$build().expect("builds");
            target.load_state(&file).expect("the frames load");
            assert_eq!(outputs(&mut target), expected, stringify!($build));
        }};
    }
    resumes!(build_interpreter);
    resumes!(build_cranelift);
    resumes!(build_wasm);
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    resumes!(build_native);

    let objects = r#"`timescale 1ns/1ns
module Top(output int a);
    class Box;
        int v;
    endclass
    task automatic hold(output int r);
        Box box = new;
        #5 r = box.v;
    endtask
    initial hold(a);
endmodule
"#;
    let mut sim = Simulation::from_sv_sources(vec![(objects, Path::new("calls.sv"))], "Top")
        .force_out_of_line_calls()
        .build_interpreter()
        .expect("builds");
    sim.run_until(1).expect("the design runs");
    assert!(sim.save_state().is_err(), "a class object in a frame");
}

#[test]
fn recursive_tasks_wait_at_every_level() {
    // Each activation waits before and after its recursive call, while a
    // second process runs the same task: both chains stay suspended inside
    // their calls across many time slots.
    let recursive = r#"`timescale 1ns/1ns
module Top(output int a, output int b);
    task automatic down(input int n, output int r);
        int below;
        int mine = n * 3;
        if (n == 0) begin
            #1 r = 1;
            return;
        end
        #(n) down(n - 1, below);
        #1 r = below * 2 + mine;
    endtask
    int k = 3;
    initial down(k, a);
    initial #2 down(k - 1, b);
endmodule
"#;
    let looped = recursive.replace(
        "        int below;
        int mine = n * 3;
        if (n == 0) begin
            #1 r = 1;
            return;
        end
        #(n) down(n - 1, below);
        #1 r = below * 2 + mine;",
        "        int below;
        for (int m = n; m > 0; m--) #(m);
        #1 below = 1;
        for (int m = 1; m <= n; m++) #1 below = below * 2 + m * 3;
        r = below;",
    );
    assert_ne!(looped, recursive);
    assert_eq!(function_units(recursive, true), 1, "function units");
    for four_state in [false, true] {
        let expected = simulate_run(&looped, four_state, false, "interpreter", 30, &["a", "b"])
            .expect("the looped design runs");
        for backend in backends() {
            assert_eq!(
                simulate_run(recursive, four_state, true, backend, 30, &["a", "b"]),
                Ok(expected.clone()),
                "{backend}, four-state {four_state}"
            );
        }
    }
}

#[test]
fn timed_tasks_in_fork_children() {
    // A task waits a real amount in a scope whose precision is finer than
    // its unit; fork children and their parent call it and a task that
    // calls it at once.
    let source = r#"`timescale 1ns/10ps
module Top(output int a, output int b, output int c, output int d);
    task automatic settle(input real d, output int r);
        #(d) r = int'($realtime * 100);
    endtask
    task automatic twice(input int n, output int r);
        int got;
        settle(n * 0.75, got);
        #1 r = got + n;
    endtask
    int one = 1;
    initial begin
        fork
            begin
                twice(one, a);
            end
            begin
                settle(one * 1.5, b);
            end
        join_none
        twice(one + 2, c);
        wait fork;
        settle(0.25, d);
    end
endmodule
"#;
    same_as_inlined(source, 2, 20, &["a", "b", "c", "d"]);
}

#[test]
fn stops_inside_task_units() {
    // `$stop` inside a task suspends the process with its frames; the run
    // continues from there, and `$finish` inside a task ends it.
    let source = r#"`timescale 1ns/1ns
module Top(output int a, output int b);
    task automatic halt(input int n, output int r);
        int x = n * 2;
        #1 $stop;
        r = x + 1;
        if (n > 2) begin
            #1 $finish;
            r = 0;
        end
    endtask
    int two = 2;
    initial begin
        halt(two, a);
        halt(two + 1, b);
    end
endmodule
"#;
    fn stops<B: SimBackend>(mut sim: Simulation<B>) -> Vec<(bool, u64, BigUint, BigUint)> {
        let mut seen = Vec::new();
        for _ in 0..3 {
            sim.run_until(10).expect("the design runs");
            seen.push((
                sim.is_stopped(),
                sim.time(),
                sim.get(sim.signal("a")),
                sim.get(sim.signal("b")),
            ));
        }
        seen
    }
    assert_eq!(function_units(source, true), 1, "function units");
    for four_state in [false, true] {
        let build = |forced: bool| {
            let builder = Simulation::from_sv_sources(vec![(source, Path::new("calls.sv"))], "Top")
                .four_state(four_state);
            if forced {
                builder.force_out_of_line_calls()
            } else {
                builder
            }
        };
        let expected = stops(build(false).build_interpreter().expect("builds"));
        assert!(expected[0].0, "the run stops: {expected:?}");
        assert_eq!(
            stops(build(true).build_interpreter().expect("builds")),
            expected
        );
        assert_eq!(
            stops(build(true).build_cranelift().expect("builds")),
            expected
        );
        assert_eq!(stops(build(true).build_wasm().expect("builds")), expected);
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        assert_eq!(stops(build(true).build_native().expect("builds")), expected);
    }
}

#[test]
fn final_procedures_keep_their_frames_across_a_stop() {
    // A final procedure stops inside its function; while it is stopped,
    // the combinational process calls a unit at the bottom of the stack.
    // The function continues with its own frame.
    let source = r#"`timescale 1ns/1ns
module Top(input int x, output int c, output int got);
    function automatic int scramble(int v);
        int t = v * 7;
        int u = t + 3;
        int w = u ^ 5;
        return w + t + u;
    endfunction
    always_comb c = scramble(x);
    function automatic void fin(input int n, output int r);
        int keep = n * 11;
        int more = keep + 2;
        $stop;
        r = keep * 100 + more;
    endfunction
    int seed = 4;
    final fin(seed, got);
endmodule
"#;
    fn stopped<B: SimBackend>(mut sim: Simulation<B>) -> (bool, BigUint, BigUint) {
        sim.run_until(0).expect("the design runs");
        sim.finish().expect("finalization starts");
        let stopped = sim.is_stopped();
        let x = sim.signal("x");
        sim.modify(|io| io.set(x, 9u32)).expect("the write settles");
        let c = sim.get(sim.signal("c"));
        sim.step().expect("finalization continues");
        (stopped, c, sim.get(sim.signal("got")))
    }
    assert_eq!(function_units(source, true), 2, "function units");
    for four_state in [false, true] {
        let build = |forced: bool| {
            let builder = Simulation::from_sv_sources(vec![(source, Path::new("calls.sv"))], "Top")
                .four_state(four_state);
            if forced {
                builder.force_out_of_line_calls()
            } else {
                builder
            }
        };
        let expected = stopped(build(false).build_interpreter().expect("builds"));
        assert!(expected.0, "the final procedure stops: {expected:?}");
        assert_eq!(
            stopped(build(true).build_interpreter().expect("builds")),
            expected
        );
        assert_eq!(
            stopped(build(true).build_cranelift().expect("builds")),
            expected
        );
        assert_eq!(stopped(build(true).build_wasm().expect("builds")), expected);
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        assert_eq!(
            stopped(build(true).build_native().expect("builds")),
            expected
        );
    }
}

#[test]
fn finishing_inside_a_task_leaves_no_frames() {
    // The process never resumes after `$finish`, so the frame holding a
    // class object is not kept, and the finished simulation saves. The
    // output formal is never copied back: the task does not return.
    let source = r#"`timescale 1ns/1ns
module Top(output int a);
    class Box;
        int v = 3;
    endclass
    task automatic end_with(output int r);
        Box box = new;
        r = box.v;
        #1 $finish;
    endtask
    initial end_with(a);
endmodule
"#;
    let mut sim = Simulation::from_sv_sources(vec![(source, Path::new("calls.sv"))], "Top")
        .force_out_of_line_calls()
        .build_interpreter()
        .expect("builds");
    sim.run_until(5).expect("the design runs");
    assert_eq!(sim.get(sim.signal("a")), 0u8.into());
    assert!(sim.save_state().is_ok(), "{:?}", sim.save_state().err());
}

#[test]
fn loads_refuse_class_handles_in_saved_frames() {
    // A null handle in a saved frame saves; a file that puts any other
    // value in that slot does not load, as a state object holding a class
    // handle does not.
    let source = r#"`timescale 1ns/1ns
module Top(output int a);
    class Box;
        int v;
    endclass
    task automatic hold(output int r);
        Box box;
        #5 r = box == null ? 7 : box.v;
    endtask
    initial hold(a);
endmodule
"#;
    let build = || {
        Simulation::from_sv_sources(vec![(source, Path::new("calls.sv"))], "Top")
            .force_out_of_line_calls()
            .build_interpreter()
            .expect("builds")
    };
    let mut origin = build();
    origin.run_until(1).expect("the design runs");
    let file = origin.save_state().expect("a null handle saves");
    let stack = &file
        .schedule
        .as_ref()
        .expect("a schedule")
        .processes
        .call_stacks[0]
        .stack;
    // Set each zero word of the stack in turn. The load refuses the
    // handle's; any other word the task overwrites or does not read.
    let mut refused = 0;
    for word in (0..stack.len() / 4).filter(|&w| stack[w * 4..w * 4 + 4] == [0; 4]) {
        let mut changed = file.clone();
        changed
            .schedule
            .as_mut()
            .expect("a schedule")
            .processes
            .call_stacks[0]
            .stack[word * 4] = 1;
        let mut target = build();
        match target.load_state(&changed) {
            Err(_) => refused += 1,
            Ok(()) => {
                target.run_until(10).expect("the loaded design runs");
                assert_eq!(target.get(target.signal("a")), 7u8.into(), "word {word}");
            }
        }
    }
    assert_eq!(refused, 1, "only the handle's word is refused");
}

#[test]
fn loads_refuse_saved_frames_that_do_not_fit() {
    // A saved stack holds the whole chain, its strings are the file's, and
    // it continues where its process does: a stack cut anywhere, a string
    // ID past the file's strings, a second stack for the process or another
    // resume point does not load.
    let source = r#"`timescale 1ns/1ns
module Top(output int a);
    task automatic hold(output int r);
        string s = "abc";
        #5 r = s.len();
    endtask
    task automatic outer(output int r);
        int got;
        hold(got);
        r = got + 10;
    endtask
    initial outer(a);
endmodule
"#;
    let build = || {
        Simulation::from_sv_sources(vec![(source, Path::new("calls.sv"))], "Top")
            .force_out_of_line_calls()
            .build_interpreter()
            .expect("builds")
    };
    let mut origin = build();
    origin.run_until(1).expect("the design runs");
    let file = origin.save_state().expect("the frames save");
    fn stack(file: &mut celox::StateFile) -> &mut Vec<u8> {
        &mut file
            .schedule
            .as_mut()
            .expect("a schedule")
            .processes
            .call_stacks[0]
            .stack
    }
    let len = stack(&mut file.clone()).len();
    for cut in 0..len {
        let mut changed = file.clone();
        stack(&mut changed).truncate(cut);
        assert!(build().load_state(&changed).is_err(), "cut at {cut}");
    }
    // The string's ID is 1; so may other words be: ones the task
    // overwrites or does not read, resume points of call sites, whose chain
    // no longer fits, and the innermost resume point, which the unit checks
    // when it continues.
    let mut refused = 0;
    let words = stack(&mut file.clone()).clone();
    for word in (0..len / 4).filter(|&w| words[w * 4..w * 4 + 4] == 1u32.to_le_bytes()) {
        let mut changed = file.clone();
        stack(&mut changed)[word * 4..word * 4 + 4].copy_from_slice(&99u32.to_le_bytes());
        let mut target = build();
        match target.load_state(&changed) {
            Err(_) => refused += 1,
            Ok(()) => match target.run_until(10) {
                Ok(()) => {
                    assert_eq!(target.get(target.signal("a")), 13u8.into(), "word {word}");
                }
                Err(error) => assert!(
                    error.to_string().contains("invalid resume point"),
                    "word {word}: {error}"
                ),
            },
        }
    }
    // The string's word is among them, with the call sites' resume points,
    // which no longer name the chain.
    assert!(refused >= 1, "the string's word is refused");
    // One stack per process, at the resume point the process continues at.
    let mut changed = file.clone();
    let processes = &mut changed.schedule.as_mut().expect("a schedule").processes;
    processes.call_stacks.push(processes.call_stacks[0].clone());
    assert!(build().load_state(&changed).is_err(), "a second stack");
    let resume = file
        .schedule
        .as_ref()
        .expect("a schedule")
        .processes
        .call_stacks[0]
        .resume;
    for other in (0..8).filter(|&point| point != resume) {
        let mut changed = file.clone();
        changed
            .schedule
            .as_mut()
            .expect("a schedule")
            .processes
            .call_stacks[0]
            .resume = other;
        assert!(
            build().load_state(&changed).is_err(),
            "resume point {other}"
        );
    }
}

#[test]
fn finishing_drops_the_frames_of_every_suspended_call() {
    // Another process, or the host, ends the simulation while a task waits
    // with a class object in its frame: nothing resumes, so the finished
    // simulation saves.
    let source = r#"`timescale 1ns/1ns
module Top(output int a);
    class Box;
        int v = 3;
    endclass
    task automatic hold(output int r);
        Box box = new;
        #10 r = box.v;
    endtask
    initial hold(a);
    initial #2 $finish;
endmodule
"#;
    let build = |source: &str| {
        Simulation::from_sv_sources(vec![(source, Path::new("calls.sv"))], "Top")
            .force_out_of_line_calls()
            .build_interpreter()
            .expect("builds")
    };
    let mut sim = build(source);
    sim.run_until(5).expect("the design runs");
    assert!(sim.save_state().is_ok(), "{:?}", sim.save_state().err());
    let hosted = source.replace("    initial #2 $finish;\n", "");
    let mut sim = build(&hosted);
    sim.run_until(2).expect("the design runs");
    sim.finish().expect("the host finishes");
    assert!(sim.save_state().is_ok(), "{:?}", sim.save_state().err());
}
