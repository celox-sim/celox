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
    // Inlining cannot expand recursion: without the switch the policy takes
    // the recursive subroutines out of line, with the same results.
    let by_default = simulate(source, false, false, "interpreter", 0, &["f"]);
    assert_eq!(
        by_default,
        simulate(source, false, true, "interpreter", 0, &["f"])
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
    // A host request that a unit cannot make (a VCD task) keeps a
    // subroutine inlined with every subroutine that calls it; `plain` and `bump`, whose `ref` formal
    // names `a`, still go out of line.
    let source = r#"module Top(output int a, output int b, output int c);
    function automatic void bump(ref int v);
        v = v + 1;
    endfunction
    function automatic int shout(int v);
        $dumpflush;
        return v + 1;
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
fn stores_a_wait_observes_are_counted_in_units() {
    // A level wait reads the static `count`, so its stores are instrumented:
    // `counter`, which stores it, goes out of line with its stores counted
    // as a process's, and the wait sees the store that `advance` makes.
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
    same_as_inlined(source, 2, 3, &["done_at", "r", "d"]);
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
    // among it, so the combinational process keeps its call of `counter`,
    // which stores `total`, inlined; the initial process calls it out of
    // line.
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
    same_as_inlined(source, 2, 0, &["y", "r"]);
    assert_eq!(
        simulate(source, false, true, "interpreter", 0, &["y", "r"]),
        Ok(vec![value(1), value(2)])
    );
}

#[test]
fn defaults_a_level_wait_watches_count_as_observing() {
    // A level wait watches what the defaults of the formals of the
    // functions it calls read, supplied or not, the static `total` of
    // `counter` among it, so the stores of `counter` into `total` are
    // counted, also out of line. `ready`, whose call supplies `v`, goes out
    // of line too.
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
    same_as_inlined(source, 2, 2, &["done_at", "r"]);
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
    // `add_both` stays inlined into `aliased`; `bump(watched, ..)`, which
    // stores into what a continuous assignment reads, goes out of line.
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
    same_as_inlined(source, 2, 0, &["a", "b", "w"]);
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
    // enters it meanwhile: its array is a slot of its frame, one for each
    // call.
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
    same_as_inlined(source, 3, 4, &["a", "b", "c", "d", "e", "f"]);
}

#[test]
fn recursive_functions_keep_their_arrays_in_their_frames() {
    // Each activation of `digits` has its own arrays, slots of its frame,
    // which it writes and reads at positions computed at run time, also
    // after the recursive call returns.
    let source = r#"module Top(output int a, output int b);
    function automatic int digits(int n, int depth);
        int q[4];
        logic [7:0] w[3];
        int k;
        foreach (q[i]) q[i] = n * 10 + i + depth;
        w[depth % 3] = 8'(n);
        if (depth < 3) k = digits(n + 1, depth + 1);
        else k = 0;
        return q[n % 4] + q[3] + k + w[depth % 3];
    endfunction
    int one = 1;
    initial begin
        a = digits(one, 0);
        b = digits(one + 4, 1);
    end
endmodule
"#;
    assert_eq!(function_units(source, true), 1);
    for four_state in [false, true] {
        for backend in backends() {
            assert_eq!(
                simulate(source, four_state, true, backend, 0, &["a", "b"]),
                Ok(vec![value(240), value(405)]),
                "{backend}, four-state {four_state}"
            );
        }
    }
}

#[test]
fn wide_elements_of_frame_arrays() {
    // Elements of 65 and 100 bits, at positions computed at run time, start
    // at bit offsets that are not byte aligned (the second element of a
    // 65-bit array starts at bit 65): every backend reads and writes them
    // in place, and the neighbouring elements keep their bits. `wide` waits
    // with its arrays live, so they are slots of its frame, one per call.
    let source = r#"`timescale 1ns/1ns
module Top(output logic [64:0] a, output logic [99:0] b, output logic [64:0] c, output logic [99:0] d);
    task automatic wide(input int n, output logic [64:0] x, output logic [99:0] y);
        logic [64:0] q[3];
        logic [99:0] r[3];
        foreach (q[i]) begin
            q[i] = {1'b1, 64'(n) << (i * 7)} ^ 65'(i);
            r[i] = (100'(n) << (30 + i * 11)) | 100'(i + 1);
        end
        #1;
        x = q[n % 3] ^ q[(n + 1) % 3];
        y = r[n % 3] + r[(n + 2) % 3];
        q[(n + 1) % 3] = x;
        r[n % 3] = y;
        x = x ^ q[(n + 1) % 3] ^ q[(n + 2) % 3] ^ q[n % 3];
        y = y ^ r[n % 3] ^ r[(n + 1) % 3] ^ r[(n + 2) % 3];
    endtask
    int one = 1;
    initial wide(one, a, b);
    initial wide(one + 4, c, d);
endmodule
"#;
    same_as_inlined(source, 1, 2, &["a", "b", "c", "d"]);
}

#[test]
fn frame_arrays_passed_by_ref_stay_in_their_frames() {
    // `walk` recurses, so its array is a slot of each activation's frame.
    // Passed by `ref`, it has no place a call can pass, so the calls of
    // `fill` and `total`, which only reads it, stay inlined into `walk`'s
    // unit and name the slot: each activation keeps its own elements across
    // the recursive call.
    let source = r#"module Top(output int a, output int b);
    function automatic void fill(ref int target[3], input int n);
        foreach (target[i]) target[i] = n * 10 + i;
    endfunction
    function automatic int total(ref int target[3]);
        return target[0] + target[1] + target[2];
    endfunction
    function automatic int walk(int n);
        int q[3];
        int k;
        fill(q, n);
        k = n > 0 ? walk(n - 1) : 0;
        return total(q) + k;
    endfunction
    int one = 1;
    initial begin
        a = walk(one + 1);
        b = walk(one + 2);
    end
endmodule
"#;
    assert_eq!(function_units(source, true), 1);
    for four_state in [false, true] {
        for backend in backends() {
            assert_eq!(
                simulate(source, four_state, true, backend, 0, &["a", "b"]),
                Ok(vec![value(99), value(192)]),
                "{backend}, four-state {four_state}"
            );
        }
    }
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
fn units_store_into_what_waits_and_logic_observe() {
    // As UVM's objections push to a static queue that a task waits on,
    // `schedule` pushes to `Sched::pending`, which `drain` waits for with a
    // level wait at a function wait site; `note` stores `last`, which an event control waits on and
    // a continuous assignment reads. `pulse` changes `blip` and changes it
    // back, which an event control on it sees only as the stores' own
    // changes (IEEE 1800-2023 9.4.2). Assembly counts these stores in the
    // units as a process's, so every waiter wakes. `mirror` stores what the
    // `always_comb` process that calls it observes, so that call stays
    // inlined.
    let source = r#"`timescale 1ns/1ns
module Top(output int got, output int seen_at, output int follow, output int y, output int z,
           output int pulsed_at);
    class Sched;
        static int pending[$];
        static function void schedule(int n);
            pending.push_back(n);
        endfunction
        static task drain(output int v);
            wait (pending.size() != 0);
            v = pending.pop_front();
        endtask
    endclass
    int last;
    function automatic void note(int n);
        last = n;
    endfunction
    int blip;
    function automatic void pulse();
        blip = blip + 1;
        blip = blip - 1;
    endfunction
    assign follow = last + 1;
    int shadow;
    function automatic int mirror(int n);
        shadow = n;
        return n + 1;
    endfunction
    int one = 1;
    always_comb y = mirror(one) + shadow;
    initial Sched::drain(got);
    initial begin
        @(last);
        seen_at = $time;
    end
    initial begin
        @(blip);
        pulsed_at = $time;
    end
    initial begin
        #2 Sched::schedule(one + 6);
        #1 note(one + 2);
        #1 z = mirror(one + 3);
        pulse();
    end
endmodule
"#;
    let names = ["got", "seen_at", "follow", "y", "z", "pulsed_at"];
    same_as_inlined(source, 5, 5, &names);
    assert_eq!(
        simulate(source, false, true, "interpreter", 5, &names),
        Ok(vec![
            value(7),
            value(3),
            value(4),
            value(3),
            value(5),
            value(4)
        ])
    );
}

#[test]
fn mailboxes_and_blocking_methods_in_function_units() {
    // Units call the methods of a bounded mailbox, as UVM's TLM FIFOs do:
    // `send` blocks on the third `put` until `take` gets a message, and a
    // blocking method retries when its process is woken. `wait_keys`
    // blocks in a semaphore's `get` until another process puts the keys,
    // and `join_child` in `await` until a process ends. Each parks its
    // process with the unit's frames kept.
    let source = r#"`timescale 1ns/1ns
module Top(output int a, output int b, output int c, output int d, output int e, output int f);
    mailbox #(int) box = new(2);
    semaphore keys = new(0);
    process child;
    task automatic send(int n);
        box.put(n);
    endtask
    task automatic take(output int v);
        box.get(v);
    endtask
    function automatic int count();
        return box.num();
    endfunction
    function automatic bit try_send(int n);
        return box.try_put(n);
    endfunction
    task automatic wait_keys(int n);
        keys.get(n);
    endtask
    task automatic join_child();
        child.await();
    endtask
    int one = 1;
    initial begin
        send(one);
        send(one + 1);
        send(one + 2);
        a = $time;
        b = count();
    end
    initial begin
        #2 take(c);
        #1 e = try_send(one + 10);
    end
    initial begin
        wait_keys(one + 1);
        d = $time;
    end
    initial #4 keys.put(2);
    initial begin
        child = process::self();
        #5;
    end
    initial begin
        #1 join_child();
        f = $time;
    end
endmodule
"#;
    let names = ["a", "b", "c", "d", "e", "f"];
    same_as_inlined(source, 6, 7, &names);
    assert_eq!(
        simulate(source, false, true, "interpreter", 7, &names),
        Ok(vec![
            value(2),
            value(2),
            value(1),
            value(4),
            value(0),
            value(5)
        ])
    );
}

#[test]
fn large_subroutines_go_out_of_line_by_default() {
    // Without the switch a subroutine goes out of line when its fully
    // inlined size exceeds the policy's threshold: `big` has more
    // statements than that, `near` exactly as many, and `twice` calls it
    // twice, so it is over the threshold through its callee. The calls take
    // an input port: an initial process whose calls fold to constants is
    // lowered symbolically, where every call is inlined.
    let steps = |name: &str, count: usize| {
        let body: String = (0..count).map(|_| "        t = t + 1;\n").collect();
        format!(
            "    function automatic int {name}(int v);\n        int t = v;\n{body}        return t;\n    endfunction\n"
        )
    };
    let limit = 256;
    // The declaration of `t` and the return are statements too.
    let source = format!(
        "module Top(input int i, output int a, output int b, output int c);\n{}{}    function automatic int twice(int v);\n        return near(v) + near(v);\n    endfunction\n    initial begin\n        a = big(i);\n        b = near(i);\n        c = twice(i);\n    end\nendmodule\n",
        steps("big", limit),
        steps("near", limit - 2),
    );
    assert_eq!(function_units(&source, false), 2, "big and twice");
    assert_eq!(
        simulate(&source, false, false, "interpreter", 0, &["a", "b", "c"]),
        Ok(vec![
            value(limit as u64),
            value(limit as u64 - 2),
            value(2 * (limit as u64 - 2))
        ])
    );
}

#[test]
fn units_store_the_events_of_flip_flops_as_processes() {
    // `tick` drives the clock of an `always_ff` process. A process's store
    // into the clock is counted as the domain's occurrence, and so is the
    // store of a unit it calls: the task goes out of line.
    let source = r#"`timescale 1ns/1ns
module Top(output int count);
    logic clk = 0;
    task automatic tick(int n);
        clk = 1;
        #1 clk = 0;
        #1;
    endtask
    always_ff @(posedge clk) count <= count + 1;
    int one = 1;
    initial repeat (3) tick(one);
endmodule
"#;
    same_as_inlined(source, 1, 7, &["count"]);
    assert_eq!(
        simulate(source, false, true, "interpreter", 7, &["count"]),
        Ok(vec![value(3)])
    );
}

#[test]
fn defaults_count_at_every_call_of_the_size_policy() {
    // The default of `pick` calls `wide`, which runs at each of the three
    // calls of `caller`: inlined, `caller` is three times `wide`, over the
    // threshold, though `pick` and `wide` are under it.
    let body: String = (0..200).map(|_| "        t = t + 1;\n").collect();
    let source = format!(
        "module Top(input int i, output int c);\n    function automatic int wide();\n        int t = 0;\n{body}        return t;\n    endfunction\n    function automatic int pick(int v, int d = wide());\n        return v + d;\n    endfunction\n    function automatic int caller(int v);\n        return pick(v) + pick(v + 1) + pick(v + 2);\n    endfunction\n    initial c = caller(i);\nendmodule\n"
    );
    assert_eq!(function_units(&source, false), 1, "caller");
    assert_eq!(
        simulate(&source, false, false, "interpreter", 0, &["c"]),
        Ok(vec![value(3 + 3 * 200)])
    );
}

#[test]
fn constant_arguments_do_not_keep_large_calls_inlined() {
    // `big(1)` has constant arguments, but its body reads the input `i`, so
    // it does not fold: over the threshold, it goes out of line.
    let body: String = (0..300).map(|_| "        t = t + i;\n").collect();
    let source = format!(
        "`timescale 1ns/1ns\nmodule Top(input int i, output int a);\n    function automatic int big(int v);\n        int t = v;\n{body}        return t;\n    endfunction\n    initial #1 a = big(1);\nendmodule\n"
    );
    assert_eq!(function_units(&source, false), 1, "big");
    assert_eq!(
        simulate(&source, false, false, "interpreter", 2, &["a"]),
        Ok(vec![value(1)])
    );
}

#[test]
fn defaults_that_call_each_other_recurse_at_run_time() {
    // `f()` takes the default `g()`, which takes the default `f()`. The units
    // evaluate the defaults of the arguments their calls omit, so this is a
    // recursion like any other: the call of the `always_comb` process never
    // ends, and the call depth guard stops the run (IEEE 1800-2023 13.5.3).
    let source = r#"module Top(input int i, output int r);
    function automatic int f(int a = g());
        return a + i;
    endfunction
    function automatic int g(int b = f());
        return b + 1;
    endfunction
    always_comb r = f();
endmodule
"#;
    for forced in [false, true] {
        let run = simulate(source, false, forced, "interpreter", 0, &["r"]);
        assert!(
            run.as_ref()
                .is_err_and(|error| error.contains("call depth exceeds")),
            "forced {forced}: {run:?}"
        );
    }
}

#[test]
fn defaults_of_static_subroutines_that_call_each_other_recurse_at_run_time() {
    // As `defaults_that_call_each_other_recurse_at_run_time`, with static
    // subroutines, which go out of line from a procedural process (a
    // combinational one cannot share their static formals with a unit):
    // their units evaluate the defaults too.
    let source = r#"module Top(input int i, output int r);
    function int f(int a = g());
        return a + i;
    endfunction
    function int g(int b = f());
        return b + 1;
    endfunction
    initial r = f();
endmodule
"#;
    for forced in [false, true] {
        let run = simulate(source, false, forced, "interpreter", 0, &["r"]);
        assert!(
            run.as_ref()
                .is_err_and(|error| error.contains("call depth exceeds")),
            "forced {forced}: {run:?}"
        );
    }
}

#[test]
fn doubling_default_chains_build_in_linear_time() {
    // The default of each `fN` calls `fN-1` twice: 2^30 call paths, but 31
    // pairs of a subroutine and the arguments its calls omit, which the
    // policy sizes once each. They go out of line, and each unit evaluates
    // its default with two calls of the next.
    let depth = 30;
    let mut source = String::from(
        "module Top(input int i, output int r);\n    function automatic int f0(int a = 0);\n        return a + i;\n    endfunction\n",
    );
    for n in 1..=depth {
        let p = n - 1;
        source += &format!(
            "    function automatic int f{n}(int a = f{p}() + f{p}());\n        return a;\n    endfunction\n"
        );
    }
    source += &format!("    always_comb r = f5() + f{depth}(1);\nendmodule\n");
    let start = std::time::Instant::now();
    assert_eq!(
        simulate(&source, false, false, "interpreter", 0, &["r"]),
        Ok(vec![value(1)])
    );
    assert!(
        start.elapsed() < std::time::Duration::from_secs(30),
        "{:?}",
        start.elapsed()
    );
}

#[test]
fn defaults_that_may_call_each_other_end_when_a_branch_does() {
    // The default of `f` calls `g` only when `c` is clear, and that of `g`
    // calls `f`: a cycle through the defaults that ends when `c` is set.
    let source = r#"module Top(input bit c, output int r);
    function automatic int f(int a = c ? 0 : g());
        return a + 1;
    endfunction
    function automatic int g(int b = f());
        return b + 10;
    endfunction
    always_comb r = g();
endmodule
"#;
    for forced in [false, true] {
        let mut sim = Simulation::from_sv_sources(vec![(source, Path::new("calls.sv"))], "Top");
        if forced {
            sim = sim.force_out_of_line_calls();
        }
        let mut sim = sim.build_interpreter().expect("the design builds");
        let c = sim.signal("c");
        sim.modify(|io| io.set(c, 1u8)).unwrap();
        sim.run_until(0).unwrap();
        let r = sim.signal("r");
        assert_eq!(sim.get(r), BigUint::from(11u8), "forced {forced}");
    }
}

#[test]
fn recursive_calls_with_constant_arguments_go_out_of_line() {
    // `down(3)` has constant arguments, but `down` recurses, which inlining
    // cannot expand nor fold: the continuous assignment, like the
    // `always_comb` process, calls it out of line.
    let source = r#"module Top(input int i, output int r, output int s);
    function automatic int down(int n);
        return n <= 0 ? 0 : down(n - 1) + 1;
    endfunction
    assign r = down(3);
    always_comb s = down(i);
endmodule
"#;
    assert_eq!(function_units(source, false), 1);
    for forced in [false, true] {
        assert_eq!(
            simulate(source, false, forced, "interpreter", 0, &["r", "s"]),
            Ok(vec![value(3), value(0)]),
            "forced {forced}"
        );
    }
}

#[test]
fn calls_in_timing_controls_count_for_the_size_policy() {
    // The delays of `pause` call `cost`, whose 200 statements run at each:
    // inlined, `pause` is twice `cost`, over the threshold.
    let body: String = (0..200).map(|_| "        t = t ^ 1;\n").collect();
    let source = format!(
        "`timescale 1ns/1ns\nmodule Top(input int i, output int a);\n    function automatic int cost(int v);\n        int t = v;\n{body}        return t;\n    endfunction\n    task automatic pause(int v);\n        #(cost(v));\n        #(cost(v + 1));\n    endtask\n    initial begin\n        pause(i + 1);\n        a = $time;\n    end\nendmodule\n"
    );
    assert_eq!(function_units(&source, false), 1, "pause");
    assert_eq!(
        simulate(&source, false, false, "interpreter", 5, &["a"]),
        Ok(vec![value(3)])
    );
}

#[test]
fn cycles_through_defaults_go_out_of_line() {
    // The default of `f` calls `g`, whose body calls `f` again with the
    // default: a cycle only through the default, which the call of the
    // `always_comb` process starts. Both go out of line by default; the
    // default's call `g(0)` returns at once.
    let source = r#"module Top(input int i, output int r);
    function automatic int f(int n, int a = g(0));
        return a + n;
    endfunction
    function automatic int g(int n);
        return n > 0 ? f(n - 1) : 0;
    endfunction
    always_comb r = f(i + 3) + g(i + 2);
endmodule
"#;
    assert_eq!(function_units(source, false), 2);
    for forced in [false, true] {
        assert_eq!(
            simulate(source, false, forced, "interpreter", 0, &["r"]),
            Ok(vec![value(4)]),
            "forced {forced}"
        );
    }
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
    // Inlining cannot expand recursion: without the switch the policy takes
    // the recursive subroutines out of line, with the same results.
    let by_default = simulate(source, false, false, "interpreter", 0, &["f"]);
    assert_eq!(
        by_default,
        simulate(source, false, true, "interpreter", 0, &["f"])
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
    // Inlining cannot expand recursion: without the switch the policy takes
    // the recursive subroutines out of line, with the same results.
    let by_default = simulate(source, false, false, "interpreter", 0, &["a"]);
    assert_eq!(
        by_default,
        simulate(source, false, true, "interpreter", 0, &["a"])
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
    // go out of line, and so does `touch`, which writes what a continuous
    // assignment reads; `logged` writes a variable that the `always_comb`
    // process calling it owns as an output, so it stays inlined.
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
    same_as_inlined(source, 5, 0, &["a", "b", "c", "d", "f"]);
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

/// The classes of the tests of `ref` formals bound to the locals of the
/// caller: `recurse` collects the nodes of a tree into the queue its `ref`
/// formal names, recursively, so it goes out of line by default, and its
/// unit writes the caller's variable.
const TREE: &str = r#"`timescale 1ns/1ns
class node;
    node kids[$];
endclass
class root;
    int offs[$];
    function void find_all(node n, ref node out[$]);
        recurse(out, n);
    endfunction
    function void recurse(ref node out[$], input node n = null);
        out.push_back(n);
        foreach (n.kids[i]) this.recurse(out, n.kids[i]);
    endfunction
    function void depth(node n, ref int d);
        d = d + 1;
        foreach (n.kids[i]) this.depth(n.kids[i], d);
    endfunction
"#;

#[test]
fn ref_formals_bind_the_locals_of_a_process() {
    // An automatic queue of an `initial` process, and an automatic `int`,
    // which the recursive units write through their `ref` formals.
    let source = format!(
        "{TREE}endclass\nmodule Top(output int a, output int b, output int d);\n    initial begin\n        automatic root r = new;\n        automatic node x = new, y = new;\n        automatic node q[$];\n        automatic int n = 0;\n        x.kids.push_back(y);\n        r.find_all(x, q);\n        a = q.size();\n        r.find_all(x, q);\n        b = q.size();\n        r.depth(x, n);\n        d = n;\n    end\nendmodule\n"
    );
    for four_state in [false, true] {
        for forced in [false, true] {
            assert_eq!(
                simulate(
                    &source,
                    four_state,
                    forced,
                    "interpreter",
                    0,
                    &["a", "b", "d"]
                ),
                Ok(vec![value(2), value(4), value(2)]),
                "four_state {four_state} forced {forced}"
            );
        }
    }
}

#[test]
fn ref_formals_bind_the_locals_of_subroutines() {
    // The shapes of UVM's `uvm_root::find` (an automatic local of a
    // function), `uvm_root::m_do_timeout_settings` (a local of a `foreach`
    // body after a delay, in a task) and `uvm_utils::find_all` (a local of
    // a static method), and a local of static lifetime.
    let source = format!(
        "{TREE}    function int find(node a);\n        node list[$];\n        find_all(a, list);\n        return list.size();\n    endfunction\n    task timeouts(node a, output int sizes[$]);\n        foreach (offs[i]) begin\n            node comps[$];\n            #(offs[i]);\n            find_all(a, comps);\n            sizes.push_back(comps.size());\n        end\n    endtask\n    static function int count(node a);\n        node list[$];\n        root r = new;\n        r.find_all(a, list);\n        return list.size();\n    endfunction\n    function int kept(node a);\n        static node list[$];\n        find_all(a, list);\n        return list.size();\n    endfunction\nendclass\nmodule Top(output int a, output int b, output int c, output int d, output int e);\n    initial begin\n        automatic root r = new;\n        automatic node x = new, y = new, z = new;\n        automatic int sizes[$];\n        x.kids.push_back(y);\n        y.kids.push_back(z);\n        r.offs.push_back(1);\n        r.offs.push_back(2);\n        a = r.find(x);\n        b = root::count(y);\n        void'(r.kept(z));\n        e = r.kept(y);\n        r.timeouts(x, sizes);\n        c = sizes[0];\n        d = sizes[1];\n    end\nendmodule\n"
    );
    // The delays of the class's task are in the default time unit: a
    // `timescale` does not set that of the compilation unit (IEEE 1800-2023
    // 3.14.2.3).
    let names = ["a", "b", "c", "d", "e"];
    for four_state in [false, true] {
        for forced in [false, true] {
            assert_eq!(
                simulate(&source, four_state, forced, "interpreter", 10_000, &names),
                Ok(vec![value(3), value(2), value(3), value(3), value(3)]),
                "four_state {four_state} forced {forced}"
            );
        }
    }
}

#[test]
fn ref_formals_bind_the_locals_of_forked_processes() {
    // Each forked process has its own automatic queue, which the units fill
    // while the other process is suspended.
    let source = format!(
        "{TREE}endclass\nmodule Top(output int a, output int b);\n    root r = new;\n    node x = new, y = new, z = new;\n    initial begin\n        x.kids.push_back(y);\n        y.kids.push_back(z);\n        for (int k = 0; k < 2; k++) begin\n            fork\n                automatic int kk = k;\n                begin\n                    automatic node q[$];\n                    #(kk + 1);\n                    r.find_all(kk == 0 ? x : y, q);\n                    #1;\n                    if (kk == 0) a = q.size(); else b = q.size();\n                end\n            join_none\n        end\n    end\nendmodule\n"
    );
    for four_state in [false, true] {
        for forced in [false, true] {
            assert_eq!(
                simulate(&source, four_state, forced, "interpreter", 5, &["a", "b"]),
                Ok(vec![value(3), value(2)]),
                "four_state {four_state} forced {forced}"
            );
        }
    }
}

#[test]
fn forks_in_functions_start_processes_that_outlive_the_call() {
    // `spawn` forks a call of `work` and returns at once; the children
    // run in copies of its frame after it returned (IEEE 1800-2023 9.3.2).
    let source = r#"`timescale 1ns/1ns
module Top(output int a, output int b, output int c, output int d);
    int hits[$];
    task automatic work(int n);
        #(n);
        hits.push_back(n);
    endtask
    function automatic void spawn(int n);
        fork
            work(n);
        join_none
    endfunction
    int three = 3, one = 1;
    initial begin
        spawn(three);
        spawn(one);
        a = hits.size();
        #5;
        b = hits.size();
        c = hits[0];
        d = hits[1];
    end
endmodule
"#;
    same_as_inlined(source, 2, 6, &["a", "b", "c", "d"]);
    assert_eq!(
        simulate(source, false, true, "interpreter", 6, &["a", "b", "c", "d"]),
        Ok(vec![value(0), value(2), value(1), value(3)])
    );
}

#[test]
fn forks_after_the_writes_of_an_array_go_out_of_line() {
    // `fill` writes its array `items` only before the loop whose fork reads
    // it, so each child, which starts in a copy of the frame (IEEE 1800-2023
    // 9.3.2), reads what the parent does: the task goes out of line as
    // uvm-1.2's mbus_pipelined_check_seq::body must, which is recursive
    // through `uvm_sequence_base::start` and cannot be inlined. `refill`
    // writes its array in the loop of its fork, so the children would have
    // to share it: it stays inlined. Both pass an element of a class
    // handle array to a task, as the UVM sequence does.
    let source = r#"`timescale 1ns/1ns
module Top(output int a, output int b, output int c);
    class item;
        int v;
    endclass
    int sum, pending;
    task automatic check(item it, int i);
        #1;
        sum += it.v * 10 + i;
        pending--;
    endtask
    task automatic fill();
        item items[4];
        for (int i = 0; i < 4; i++) begin
            items[i] = new;
            items[i].v = i + 1;
        end
        pending = 4;
        for (int i = 0; i < 4; i++) begin
            items[i].v = items[i].v * 2;
            fork
                check(items[i], i);
            join_none
            #2;
        end
        wait (pending == 0);
    endtask
    task automatic refill();
        item items[2];
        pending = 2;
        for (int i = 0; i < 2; i++) begin
            items[i] = new;
            items[i].v = i + 5;
            fork
                check(items[i], i);
            join_none
            #2;
        end
        wait (pending == 0);
    endtask
    initial begin
        fill();
        a = sum;
        b = pending;
        refill();
        c = sum;
    end
endmodule
"#;
    // `fill`, `check` and the constructor of `item`.
    same_as_inlined(source, 3, 20, &["a", "b", "c"]);
    assert_eq!(
        simulate(source, false, true, "interpreter", 20, &["a", "b", "c"]),
        Ok(vec![value(206), value(0), value(317)])
    );
}

#[test]
fn forks_in_a_loop_give_each_child_its_own_variable() {
    // Each execution of the fork's declaration makes a new `k`, which the
    // child started then keeps.
    let source = r#"`timescale 1ns/1ns
module Top(output int a, output int b, output int c, output int d);
    int seen[$];
    function automatic void spawn_all(int n);
        for (int i = 0; i < n; i++) begin
            fork
                automatic int k = i;
                begin
                    #(n - k);
                    seen.push_back(k * 10);
                end
            join_none
        end
    endfunction
    int four = 4;
    initial begin
        spawn_all(four);
        #5;
        a = seen[0];
        b = seen[1];
        c = seen[2];
        d = seen[3];
    end
endmodule
"#;
    same_as_inlined(source, 1, 6, &["a", "b", "c", "d"]);
    assert_eq!(
        simulate(source, false, true, "interpreter", 6, &["a", "b", "c", "d"]),
        Ok(vec![value(30), value(20), value(10), value(0)])
    );
}

#[test]
fn forked_children_draw_on_their_own_process_state() {
    // A child's `process::self()` is its own process, and `srandom` seeds
    // its RNG, which its `$urandom` draws on: a unit's frames take the
    // child's state as they continue. Two children seeded alike draw alike.
    let source = r#"`timescale 1ns/1ns
module Top(output int a, output int b, output int c, output int d, output bit same, output bit distinct);
    int rands[$];
    process procs[$];
    function automatic void draws(int seed, bit reseed);
        fork
            begin
                process p = process::self();
                procs.push_back(p);
                if (reseed) p.srandom(seed);
                #1;
                rands.push_back($urandom);
            end
            begin
                #2;
                rands.push_back($urandom_range(1000));
            end
        join_none
        rands.push_back($urandom);
    endfunction
    int seed = 7;
    bit yes = 1, no = 0;
    initial begin
        draws(seed, no);
        draws(seed, yes);
        draws(seed, yes);
        #5;
        a = rands[0];
        b = rands[3];
        c = rands[6];
        d = rands[8];
        same = rands[4] == rands[5];
        distinct = procs[0] != procs[1] && procs[1] != procs[2];
    end
endmodule
"#;
    let names = ["a", "b", "c", "d", "same", "distinct"];
    same_as_inlined(source, 1, 6, &names);
    let run = simulate(source, false, true, "interpreter", 6, &names).expect("runs");
    assert_eq!(run[4..], [value(1), value(1)]);
}

#[test]
fn recursion_through_a_fork_goes_out_of_line() {
    // `walk` forks a child at each level and recurses: out of line by
    // default, where inlining cannot expand it.
    let source = r#"`timescale 1ns/1ns
module Top(output int a, output int b, output int c);
    int seen[$];
    function automatic void walk(int depth);
        if (depth == 0) return;
        fork
            begin
                #(depth);
                seen.push_back(100 + depth);
            end
        join_none
        walk(depth - 1);
    endfunction
    int three = 3;
    initial begin
        walk(three);
        #5;
        a = seen[0];
        b = seen[1];
        c = seen[2];
    end
endmodule
"#;
    assert_eq!(function_units(source, false), 1);
    for four_state in [false, true] {
        for forced in [false, true] {
            assert_eq!(
                simulate(
                    source,
                    four_state,
                    forced,
                    "interpreter",
                    6,
                    &["a", "b", "c"]
                ),
                Ok(vec![value(101), value(102), value(103)]),
                "four_state {four_state} forced {forced}"
            );
        }
    }
}

#[test]
fn forked_children_share_the_variables_they_write() {
    // `x` is the task's, which the parent and both children share, and `y`
    // the fork's, which both children share: each sees the others' writes
    // (IEEE 1800-2023 6.21, 9.3.2), through a box their frames hold.
    let source = r#"`timescale 1ns/1ns
module Top(output int a, output int b, output int c);
    int got[$];
    task automatic share(int start);
        int x = start;
        fork
            automatic int y = 0;
            begin
                #1 x = x + 1; y = 5;
            end
            begin
                #2 got.push_back(x); got.push_back(y);
                x = x + 10;
            end
        join_none
        #3 got.push_back(x);
    endtask
    int one = 1;
    initial begin
        share(one);
        a = got[0];
        b = got[1];
        c = got[2];
    end
endmodule
"#;
    same_as_inlined(source, 1, 4, &["a", "b", "c"]);
    assert_eq!(
        simulate(source, false, true, "interpreter", 4, &["a", "b", "c"]),
        Ok(vec![value(2), value(5), value(12)])
    );
}

#[test]
fn joins_and_fork_control_in_task_units() {
    // `join`, `join_any`, `wait fork` and `disable fork` in a unit act on
    // the children of the process it runs in.
    let source = r#"`timescale 1ns/1ns
module Top(output int a, output int b, output int c, output int d, output int e, output int f);
    int log_[$];
    task automatic joins(int base);
        fork
            #(base + 2) log_.push_back(1);
            #(base + 1) log_.push_back(2);
        join
        a = $time;
        fork
            #3 log_.push_back(3);
            #1 log_.push_back(4);
        join_any
        b = $time;
        fork
            #4 log_.push_back(5);
        join_none
        wait fork;
        c = $time;
        fork
            #10 log_.push_back(99);
        join_none
        #1 disable fork;
        d = $time;
    endtask
    int zero = 0;
    initial begin
        joins(zero);
        #20;
        e = log_.size();
        f = log_[log_.size() - 1];
    end
endmodule
"#;
    let names = ["a", "b", "c", "d", "e", "f"];
    same_as_inlined(source, 1, 30, &names);
    assert_eq!(
        simulate(source, false, true, "interpreter", 30, &names),
        Ok(vec![
            value(2),
            value(3),
            value(7),
            value(8),
            value(5),
            value(5)
        ])
    );
}

#[test]
fn boxes_of_forked_children_survive_collections() {
    // Only the saved frames of the children hold the box of `x` after the
    // parent returned; another process allocates enough for the heap to
    // collect while they wait.
    let source = r#"`timescale 1ns/1ns
module Top(output int a, output int n);
    class Item;
        int v;
        function new(int x);
            v = x;
        endfunction
    endclass
    function automatic void spawn(int start);
        int x = start;
        fork
            #2 x = x + 1;
            #5 a = x;
        join_none
    endfunction
    int seven = 7;
    int count = 2000;
    initial spawn(seven);
    initial begin
        Item t;
        #1;
        for (int i = 0; i < count; i++) begin
            t = new(i);
            n += t.v % 2;
        end
    end
endmodule
"#;
    same_as_inlined(source, 2, 10, &["a", "n"]);
    assert_eq!(
        simulate(source, false, true, "interpreter", 10, &["a"]),
        Ok(vec![value(8)])
    );
}

#[test]
fn forked_children_write_the_callers_variables_through_ref() {
    // `owner` passes its automatics by `ref` to `spawn2`, directly and
    // through `pass_on`, whose fork's two children write them in turn: the
    // owner boxes each, so both writes reach it (IEEE 1800-2023 13.5.2). A
    // `ref` formal may be named in a fork only when it joins all (9.3.2).
    let source = r#"`timescale 1ns/1ns
module Top(output int a, output int b);
    task automatic spawn2(ref int x);
        fork
            #1 x = x + 1;
            #2 x = x + 10;
        join
    endtask
    task automatic pass_on(ref int y);
        spawn2(y);
    endtask
    task automatic owner(input int start, output int r, output int s);
        int v = start;
        int w = start;
        spawn2(v);
        pass_on(w);
        r = v;
        s = w;
    endtask
    int zero = 0;
    initial owner(zero, a, b);
endmodule
"#;
    // `spawn2` has a function for each box it binds: a box's field is a
    // watched property, whose place selects the function.
    same_as_inlined(source, 4, 10, &["a", "b"]);
    assert_eq!(
        simulate(source, false, true, "interpreter", 10, &["a", "b"]),
        Ok(vec![value(11), value(11)])
    );
}

#[test]
fn state_files_keep_the_boxes_forked_children_share() {
    // Saved while three children of `spawn` wait, holding the box of `x`
    // in their frames: the file holds the box once, and a load on any
    // backend gives the frames one box again, so the second child's write
    // after the load reaches the third.
    let source = r#"`timescale 1ns/1ns
module Top(output int a);
    function automatic void spawn(int start);
        int x = start;
        fork
            #2 x = x + 1;
            #4 x = x + 10;
            #6 a = x;
        join_none
    endfunction
    int seven = 7;
    initial spawn(seven);
endmodule
"#;
    let build = || {
        Simulation::from_sv_sources(vec![(source, Path::new("calls.sv"))], "Top")
            .force_out_of_line_calls()
    };
    let mut origin = build().build_interpreter().expect("builds");
    origin.run_until(3).expect("the design runs");
    let mut bytes = Vec::new();
    origin
        .save_state()
        .expect("the boxes save")
        .write_to(&mut bytes)
        .expect("the file writes");
    let file = celox::StateFile::read_from(bytes.as_slice()).expect("the file reads");
    assert_eq!(
        file.schedule
            .as_ref()
            .expect("a schedule")
            .processes
            .boxes
            .len(),
        1,
        "one box, which three frames share"
    );
    origin.run_until(10).expect("the saved run continues");
    assert_eq!(origin.get(origin.signal("a")), 18u8.into());
    macro_rules! resumes {
        ($build:ident) => {{
            let mut target = build().$build().expect("builds");
            target.load_state(&file).expect("the boxes load");
            target.run_until(10).expect("the loaded design runs");
            assert_eq!(
                target.get(target.signal("a")),
                18u8.into(),
                stringify!($build)
            );
        }};
    }
    resumes!(build_interpreter);
    resumes!(build_cranelift);
    resumes!(build_wasm);
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    resumes!(build_native);
}

#[test]
fn state_files_keep_shared_strings_and_refuse_shared_containers() {
    // A string the children of `spawn` share is a value: the file holds
    // its contents with the box, and the frames share the box again after
    // a load on any backend. A shared queue is a container, which state
    // files do not record in a variable either.
    let strings = r#"`timescale 1ns/1ns
module Top(output int a);
    function automatic void spawn(int start);
        string s = "ab";
        fork
            #2 s = {s, "c"};
            #4 s = {s, "de"};
            #6 a = s.len() * 100 + start;
        join_none
    endfunction
    int seven = 7;
    initial spawn(seven);
endmodule
"#;
    let build = || {
        Simulation::from_sv_sources(vec![(strings, Path::new("calls.sv"))], "Top")
            .force_out_of_line_calls()
    };
    let mut origin = build().build_interpreter().expect("builds");
    origin.run_until(3).expect("the design runs");
    let mut bytes = Vec::new();
    origin
        .save_state()
        .expect("the string box saves")
        .write_to(&mut bytes)
        .expect("the file writes");
    let file = celox::StateFile::read_from(bytes.as_slice()).expect("the file reads");
    let boxes = &file.schedule.as_ref().expect("a schedule").processes.boxes;
    assert_eq!(boxes.len(), 1, "one box, which three frames share");
    assert_eq!(boxes[0].kind, celox_runtime::FrameValueKind::String);
    origin.run_until(10).expect("the saved run continues");
    assert_eq!(origin.get(origin.signal("a")), 507u16.into());
    macro_rules! resumes {
        ($build:ident) => {{
            let mut target = build().$build().expect("builds");
            target.load_state(&file).expect("the box loads");
            target.run_until(10).expect("the loaded design runs");
            assert_eq!(
                target.get(target.signal("a")),
                507u16.into(),
                stringify!($build)
            );
        }};
    }
    resumes!(build_interpreter);
    resumes!(build_cranelift);
    resumes!(build_wasm);
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    resumes!(build_native);

    let queues = r#"`timescale 1ns/1ns
module Top(output int a);
    function automatic void spawn(int start);
        int q[$];
        fork
            #2 q.push_back(start);
            #4 a = q.size();
        join_none
    endfunction
    int seven = 7;
    initial spawn(seven);
endmodule
"#;
    let mut sim = Simulation::from_sv_sources(vec![(queues, Path::new("calls.sv"))], "Top")
        .force_out_of_line_calls()
        .build_interpreter()
        .expect("builds");
    sim.run_until(3).expect("the design runs");
    assert!(
        matches!(sim.save_state(), Err(celox::StateError::Objects(_))),
        "a container in a frame"
    );
    sim.run_until(10).expect("the design runs on");
    assert_eq!(sim.get(sim.signal("a")), 1u8.into());
}

#[test]
fn ref_formals_bind_elements_of_the_callers_arrays() {
    // `owner` passes an element of its array to `spawn2`, whose two
    // children write it in turn, and to `bump`: the array is the caller's
    // whole variable, shared as any is (IEEE 1800-2023 13.5.2). A unit
    // names the element's array by the caller's storage for it, here the
    // copy an inlined activation of `owner` has; an array in the frame of
    // the calling unit keeps the call inlined.
    let source = r#"`timescale 1ns/1ns
module Top(output int a, output int b, output int c);
    task automatic spawn2(ref int x);
        fork
            #1 x = x + 1;
            #2 x = x + 10;
        join
    endtask
    task automatic bump(ref int x);
        x = x + 1;
    endtask
    task automatic owner(input int start, output int r, output int s);
        int v[2];
        v[0] = start;
        v[1] = start;
        spawn2(v[0]);
        bump(v[1]);
        bump(v[1]);
        r = v[0];
        s = v[1];
    endtask
    int zero = 0;
    initial begin
        owner(zero, a, b);
        #1 owner(zero + 5, c, b);
    end
endmodule
"#;
    let names = ["a", "b", "c"];
    // A function for each of `spawn2` and `bump` per array they bind: each
    // inlined call of `owner` has its own copy of `v`.
    same_as_inlined(source, 4, 20, &names);
    assert_eq!(
        simulate(source, false, true, "interpreter", 20, &names),
        Ok(vec![value(11), value(7), value(16)])
    );
}

#[test]
fn state_files_refuse_scheduler_objects_in_saved_frames() {
    // A semaphore is a scheduler object, whose class no box shares: a frame
    // holding one while the task waits is refused as a class handle is,
    // since state files do not record semaphores, and the run goes on with
    // the semaphore intact.
    let source = r#"`timescale 1ns/1ns
module Top(output int done);
    task automatic hold();
        semaphore s = new(1);
        s.get(1);
        #5 s.put(1);
        s.get(1);
        done = 1;
    endtask
    initial hold();
endmodule
"#;
    let mut sim = Simulation::from_sv_sources(vec![(source, Path::new("calls.sv"))], "Top")
        .force_out_of_line_calls()
        .build_interpreter()
        .expect("builds");
    sim.run_until(1).expect("the design runs");
    assert!(
        matches!(sim.save_state(), Err(celox::StateError::Objects(_))),
        "a semaphore in a frame"
    );
    sim.run_until(10).expect("the design runs on");
    assert_eq!(sim.get(sim.signal("done")), 1u8.into());
}

#[test]
fn units_whose_fork_branches_fail_stay_inlined() {
    // A branch of a fork in `spawn`, here one of a nested fork, that cannot
    // be lowered leaves the unit unlowered: the call is inlined and the
    // design gets the diagnostic an inlined body gets, with and without
    // out-of-line calls.
    let source = r#"`timescale 1ns/1ns
module Top(output int a);
    bit [1:0] bus;
    function automatic bit pick(int i);
        a++;
        return bus[i];
    endfunction
    function automatic void spawn(int i);
        fork
            begin
                fork
                    @(posedge pick(i));
                join_none
            end
        join_none
    endfunction
    int zero = 0;
    initial spawn(zero);
endmodule
"#;
    for forced in [false, true] {
        let run = simulate(source, false, forced, "interpreter", 10, &["a"]);
        assert!(
            run.as_ref()
                .is_err_and(|error| error.contains("impure call `pick` in an event expression")),
            "forced {forced}: {run:?}"
        );
    }
}

#[test]
fn forked_children_store_class_properties_through_formals() {
    // The child stores into a property of the object a formal of `one`
    // holds: the analyzer's temporaries for the update, which the body does
    // not declare, are shared like a formal, in a box made at the entry.
    let source = r#"`timescale 1ns/1ns
class st_t;
    int done;
endclass
module Top(output int a);
    task automatic one(st_t s, int n);
        fork
            begin
                #(n);
                s.done = s.done + 1;
            end
        join_none
    endtask
    int two = 2;
    st_t o = new;
    initial begin
        one(o, two);
        one(o, two + 1);
        #5 a = o.done;
    end
endmodule
"#;
    same_as_inlined(source, 2, 10, &["a"]);
    assert_eq!(
        simulate(source, false, true, "interpreter", 10, &["a"]),
        Ok(vec![value(2)])
    );
}

#[test]
fn forked_children_display_arrays_they_do_not_write() {
    // A `$display` argument is read, not written: `seen`, which only its
    // declaration writes, is copied into each child, not shared through a
    // box (which an unpacked array cannot have), so the recursive `walk`
    // still goes out of line.
    let source = r#"`timescale 1ns/1ns
module Top(output int a);
    function automatic void walk(int depth);
        int seen[2] = '{depth, depth * 10};
        if (depth == 0) return;
        fork
            begin
                #(depth);
                $display("level %0d %0d", seen[0], seen[1]);
                a = a + seen[1];
            end
        join_none
        walk(depth - 1);
    endfunction
    int two = 2;
    initial begin
        a = 0;
        walk(two);
    end
endmodule
"#;
    assert_eq!(function_units(source, false), 1);
    for four_state in [false, true] {
        for backend in backends() {
            let (values, events) = simulate_run(source, four_state, true, backend, 5, &["a"])
                .unwrap_or_else(|error| panic!("{backend}: {error}"));
            assert_eq!(
                values,
                vec![value(30)],
                "{backend}, four-state {four_state}"
            );
            let displays: Vec<String> = events
                .iter()
                .filter_map(|event| match event {
                    celox::RuntimeEvent::Display { message } => {
                        Some(String::from_utf8_lossy(message).into_owned())
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(
                displays,
                ["level 1 10", "level 2 20"],
                "{backend}, four-state {four_state}"
            );
        }
    }
}

#[test]
fn forked_children_write_parts_of_shared_variables() {
    // Each child writes part of the shared `x`: a bit, a part-select and
    // both indexed part-selects. Through the box each loads `x`, writes its
    // part and stores it back, as a write to part of a class property does.
    let source = r#"`timescale 1ns/1ns
module Top(output logic [15:0] a);
    task automatic parts(logic [15:0] start);
        logic [15:0] x = start;
        int i = 4;
        fork
            begin #1 x[0] = 1'b1; end
            begin #2 x[7:4] = 4'ha; end
            begin #3 x[i * 2 +: 4] = 4'h5; end
            begin #4 x[15 -: 2] = 2'b11; end
        join_none
        #5 a = x;
    endtask
    logic [15:0] zero = 0;
    initial parts(zero);
endmodule
"#;
    same_as_inlined(source, 1, 6, &["a"]);
    assert_eq!(
        simulate(source, false, true, "interpreter", 6, &["a"]),
        Ok(vec![value(0xc5a1)])
    );
}

#[test]
fn joined_children_read_ref_arrays_without_sharing_them() {
    // `show`'s child only reads `arr`, and `show` joins it: the caller waits
    // in the call, so the child reads the caller's array through the copied
    // frame and nothing is shared. The recursive `walk`, whose array could
    // not be shared, stays out of line.
    let source = r#"`timescale 1ns/1ns
module Top(output int a);
    task automatic show(ref int arr[3], input int depth);
        fork
            begin
                #1 a += arr[0] + arr[1] + arr[2] + depth;
            end
        join
    endtask
    task automatic walk(int depth);
        int arr[3] = '{depth, depth * 10, depth * 100};
        if (depth == 0) return;
        show(arr, depth);
        walk(depth - 1);
    endtask
    int two = 2;
    initial begin
        a = 0;
        walk(two);
    end
endmodule
"#;
    assert!(function_units(source, false) > 0, "out of line");
    for four_state in [false, true] {
        for forced in [false, true] {
            for backend in backends() {
                assert_eq!(
                    simulate(source, four_state, forced, backend, 5, &["a"]),
                    Ok(vec![value(336)]),
                    "{backend}, four-state {four_state}, forced {forced}"
                );
            }
        }
    }
}

/// A design whose task `step` waits at each level of a recursion for a
/// clock edge, an element of a queue and a state of an object, with
/// `RECURSE` choosing how it repeats: by recursion, out of line by default,
/// or by a loop, which inlines.
const WAITING_LEVELS: &str = r#"`timescale 1ns/1ns
class state_t;
    int state;
endclass
module Top(output int a, output int b, output int c, output int d, output int e, output int f);
    bit clk;
    int q[$];
    int log_[$];
    state_t st = new;
    initial forever #5 clk = ~clk;
    task automatic step(int n, state_t s);
        for (int m = n; m > 0; m = RECURSE) begin
            @(posedge clk);
            log_.push_back(int'($time));
            wait (q.size() != 0);
            log_.push_back(q.pop_front() * 100 + int'($time));
            wait (s.state >= 4 - m);
            log_.push_back(int'($time));
        end
    endtask
    int three = 3;
    initial begin
        step(three, st);
        a = log_[0];
        b = log_[1];
        c = log_[2];
        d = log_[4];
        e = log_[7];
        f = log_[8];
    end
    initial begin
        #12 q.push_back(1);
        #10 st.state = 1;
        #20 q.push_back(2);
        q.push_back(3);
        #3 st.state = 2;
        #2 st.state = 3;
    end
endmodule
"#;

#[test]
fn waits_in_recursive_units() {
    // Each level of `step` waits for a posedge of `clk`, for an element of
    // `q` and for a state of the object: out of line by default, as its
    // recursion, it waits at the module's function wait sites, the same as
    // the loop that inlines.
    let recursive = WAITING_LEVELS.replace(
        "for (int m = n; m > 0; m = RECURSE) begin",
        "if (n > 0) begin\n            automatic int m = n;",
    );
    let recursive = recursive.replace(
        "            log_.push_back(int'($time));\n        end\n    endtask",
        "            log_.push_back(int'($time));\n            step(n - 1, s);\n        end\n    endtask",
    );
    let looped = WAITING_LEVELS.replace("RECURSE", "m - 1");
    assert_eq!(function_units(&recursive, false), 1, "step");
    let names = ["a", "b", "c", "d", "e", "f"];
    for four_state in [false, true] {
        let expected = simulate(&looped, four_state, false, "interpreter", 200, &names)
            .expect("the loop runs");
        for forced in [false, true] {
            assert_eq!(
                simulate(&recursive, four_state, forced, "interpreter", 200, &names),
                Ok(expected.clone()),
                "four_state {four_state} forced {forced}"
            );
        }
    }
    assert_eq!(
        simulate(&looped, false, false, "interpreter", 200, &names),
        Ok(vec![
            value(5),
            value(112),
            value(22),
            value(242),
            value(355),
            value(55)
        ])
    );
}

#[test]
fn waits_in_units_are_those_of_the_inlined_body() {
    // A level wait on a module queue, one on a class property and an event
    // control, in units that waiting processes share.
    let source = r#"`timescale 1ns/1ns
class state_t;
    int state;
endclass
module Top(output int a, output int b, output int c, output int d);
    bit clk;
    int q[$];
    state_t st = new;
    initial forever #5 clk = ~clk;
    task automatic take(output int v);
        wait (q.size() != 0);
        v = q.pop_front();
    endtask
    task automatic until_state(state_t s, int want, output int at);
        wait (s.state == want);
        at = $time;
    endtask
    task automatic edges(int k, output int at);
        repeat (k) @(negedge clk);
        at = $time;
    endtask
    int two = 2, three = 3;
    initial begin
        fork
            take(a);
            until_state(st, two, b);
            edges(three, c);
            edges(two, d);
        join
    end
    initial begin
        #12 q.push_back(7);
        #10 st.state = 1;
        #10 st.state = 2;
    end
endmodule
"#;
    same_as_inlined(source, 4, 60, &["a", "b", "c", "d"]);
    assert_eq!(
        simulate(
            source,
            false,
            true,
            "interpreter",
            60,
            &["a", "b", "c", "d"]
        ),
        Ok(vec![value(7), value(32), value(30), value(20)])
    );
}

#[test]
fn units_wait_on_virtual_interface_clocks() {
    // Two processes wait at the same site of `count` on the clocks of two
    // interfaces, which the unit's frame selects: each keeps its own sample
    // of its clock and counts its own edges.
    let source = r#"`timescale 1ns/1ns
interface clk_if();
    bit clk;
endinterface
module Top(output int a, output int b, output int c);
    clk_if x();
    clk_if y();
    initial forever #5 x.clk = ~x.clk;
    initial forever #3 y.clk = ~y.clk;
    task automatic count(virtual clk_if v, int k, output int at);
        repeat (k) @(posedge v.clk);
        at = $time;
    endtask
    task automatic changes(virtual clk_if v, int k, output int at);
        repeat (k) @(v.clk);
        at = $time;
    endtask
    int three = 3, four = 4;
    initial fork
        count(x, three, a);
        count(y, three, b);
        changes(y, four, c);
    join
endmodule
"#;
    same_as_inlined(source, 2, 40, &["a", "b", "c"]);
    assert_eq!(
        simulate(source, false, true, "interpreter", 40, &["a", "b", "c"]),
        Ok(vec![value(25), value(15), value(12)])
    );
}

#[test]
fn units_count_each_edge_of_a_virtual_interface_member() {
    // `pulse` raises and lowers the clock of `x` in one run: the rise is an
    // event (IEEE 1800-2023 9.4.2), though the clock ends as it began. The
    // unit selects at the start of the wait the member the handle denotes,
    // and waits at the site that counts its rises where they happen.
    let source = r#"`timescale 1ns/1ns
interface clk_if();
    bit clk;
endinterface
module Top(output int a, output int b);
    clk_if x();
    clk_if y();
    task automatic rises(virtual clk_if v, int k, output int at);
        repeat (k) @(posedge v.clk);
        at = $time;
    endtask
    task automatic pulse();
        x.clk = 1;
        x.clk = 0;
    endtask
    int one = 1, two = 2;
    initial fork
        rises(x, one, a);
        rises(y, two, b);
    join
    initial begin
        #3 pulse();
        #2 y.clk = 1;
        #1 y.clk = 0;
        #1 y.clk = 1;
    end
endmodule
"#;
    same_as_inlined(source, 2, 20, &["a", "b"]);
    assert_eq!(
        simulate(source, false, true, "interpreter", 20, &["a", "b"]),
        Ok(vec![value(3), value(7)])
    );
}

#[test]
fn units_wait_on_the_status_of_a_process() {
    // A level wait on `p.status()` in a unit observes `p`, as an inlined
    // one does: `p` ending wakes it (IEEE 1800-2023 9.7).
    let source = r#"`timescale 1ns/1ns
module Top(output int a);
    task automatic until_done(process p, output int at);
        wait (p.status() == process::FINISHED);
        at = $time;
    endtask
    process child;
    initial begin
        fork
            begin
                child = process::self();
                #5;
            end
        join_none
        #1 until_done(child, a);
    end
endmodule
"#;
    same_as_inlined(source, 1, 20, &["a"]);
    assert_eq!(
        simulate(source, false, true, "interpreter", 20, &["a"]),
        Ok(vec![value(5)])
    );
}

#[test]
fn waits_in_units_are_flush_points_of_deferred_assertions() {
    // The deferred assertion queued before the call fails, but the wait in
    // `wait_rise` ends before the Observed region: its resume is a flush
    // point, which discards the queued `$fatal` (IEEE 1800-2023 16.4.2).
    let source = r#"`timescale 1ns/1ns
module Top(output int done);
    bit sig;
    bit c;
    task automatic wait_rise();
        @(posedge sig);
    endtask
    initial begin
        assert #0 (c) else $fatal(1, "not flushed");
        wait_rise();
        done = 1;
    end
    initial #0 sig = 1;
endmodule
"#;
    for four_state in [false, true] {
        for forced in [false, true] {
            assert_eq!(
                simulate(source, four_state, forced, "interpreter", 5, &["done"]),
                Ok(vec![value(1)]),
                "four_state {four_state} forced {forced}"
            );
        }
    }
    assert!(function_units(source, true) >= 1);
}

#[test]
fn iff_gates_of_unit_waits_hold_at_the_occurrence() {
    // An occurrence of `posedge bus[i] iff ...` counts only if the gate holds
    // when it happens (IEEE 1800-2023 9.4.2.3), which a comparison at the
    // waiter's resume cannot judge. `edge_en`, whose item reads the frame
    // and whose gate is module state that other processes change between
    // the edge and the resume, stays inlined, where the kernel judges the
    // gate when it observes the occurrence; `edge_k`, gated by its own
    // argument, which cannot change during the wait, goes out of line and
    // drops every edge while the argument is 0.
    let source = r#"`timescale 1ns/1ns
module Top(output int a, output int b, output int c);
    bit [1:0] bus;
    bit en;
    task automatic edge_en(int i, output int at);
        @(posedge bus[i] iff en);
        at = $time;
    endtask
    task automatic edge_k(int i, int k, output int at);
        @(posedge bus[i] iff (k > 0));
        at = $time;
    endtask
    initial begin
        #5 bus[0] = 1;
        #5 bus[0] = 0;
        #5 bus[0] = 1;
    end
    initial #5 en = 1;
    int zero = 0, one = 1;
    initial edge_en(zero, a);
    initial edge_k(zero, one, b);
    initial edge_k(zero, zero, c);
endmodule
"#;
    let names = ["a", "b", "c"];
    same_as_inlined(source, 1, 30, &names);
    assert_eq!(
        simulate(source, false, true, "interpreter", 30, &names),
        Ok(vec![value(5), value(5), value(0)])
    );
}

#[test]
fn impure_event_expressions_are_unsupported_in_units_too() {
    // The unit evaluates the item `pick(i)`, which reads its frame, at each
    // resume, as the kernel evaluates an event expression whenever it looks
    // for occurrences: a call with an effect is unsupported in both, with
    // the same diagnostic, with and without out-of-line calls.
    let source = r#"`timescale 1ns/1ns
module Top(output int a);
    bit [1:0] bus;
    int count;
    function automatic bit pick(int i);
        count++;
        return bus[i];
    endfunction
    task automatic edge_of(int i, output int at);
        @(posedge pick(i));
        at = $time;
    endtask
    int zero = 0;
    initial edge_of(zero, a);
    initial #5 bus[0] = 1;
endmodule
"#;
    for forced in [false, true] {
        let run = simulate(source, false, forced, "interpreter", 10, &["a"]);
        assert!(
            run.as_ref()
                .is_err_and(|error| error.contains("impure call `pick` in an event expression")),
            "forced {forced}: {run:?}"
        );
    }
}

#[test]
fn iff_gates_of_units_hold_only_when_known_true() {
    // A gate holds when its value is known nonzero (IEEE 1800-2023
    // 9.4.2.3): an x gate never does and leaves the definite one's site in
    // place, and a multi-bit gate holds on any set bit, not its least
    // significant one. The gates are frame values that select the unit's
    // wait site, or that its samples keep.
    let source = r#"`timescale 1ns/1ns
module Top(output int a, output int b);
    bit pa, pb;
    bit [1:0] bus;
    task automatic either(logic gx, logic g1, output int at);
        @(posedge pa iff gx or posedge pb iff g1);
        at = $time;
    endtask
    task automatic masked(int i, logic [1:0] mask, output int at);
        @(posedge bus[i] iff mask);
        at = $time;
    endtask
    int zero = 0;
    initial either(1'bx, 1'b1, a);
    initial masked(zero, 2'b10, b);
    initial begin
        #2 pa = 1;
        #2 pb = 1;
        #2 bus[0] = 1;
    end
endmodule
"#;
    same_as_inlined(source, 2, 10, &["a", "b"]);
    for four_state in [false, true] {
        assert_eq!(
            simulate(source, four_state, true, "interpreter", 10, &["a", "b"]),
            Ok(vec![value(4), value(6)]),
            "four_state {four_state}"
        );
    }
}

#[test]
fn units_watch_static_storage_as_kernels_do() {
    // A static task's formals and locals are module-wide storage, which a
    // concurrent call may change while the process waits: an event control
    // reading them gets a kernel's treatment, here the diagnostic for a
    // static formal in an event expression and the proxy of a static local,
    // with and without out-of-line calls.
    let formal = r#"`timescale 1ns/1ns
module Top(output int a);
    bit clk;
    task gated(input bit g, output int at);
        @(posedge clk iff g);
        at = $time;
    endtask
    bit no = 0, yes = 1;
    initial gated(no, a);
    initial #1 gated(yes, a);
    initial #3 clk = 1;
endmodule
"#;
    for forced in [false, true] {
        let run = simulate(formal, false, forced, "interpreter", 10, &["a"]);
        assert!(
            run.as_ref()
                .is_err_and(|error| error.contains("static task argument")),
            "forced {forced}: {run:?}"
        );
    }
    let local = r#"`timescale 1ns/1ns
module Top(output int a, output int b);
    task toggle(input bit on, output int at);
        static bit flag;
        if (on) begin
            #2 flag = 1;
            at = $time;
        end else begin
            @(posedge flag);
            at = $time;
        end
    endtask
    bit no = 0, yes = 1;
    initial toggle(no, a);
    initial #1 toggle(yes, b);
endmodule
"#;
    let names = ["a", "b"];
    for four_state in [false, true] {
        let inlined = simulate(local, four_state, false, "interpreter", 10, &names);
        assert_eq!(
            simulate(local, four_state, true, "interpreter", 10, &names),
            inlined,
            "four_state {four_state}"
        );
    }
}

#[test]
fn rejected_units_leave_no_event_proxies() {
    // The first wait of `t` makes the proxy of `(a && b)`; the second, on
    // an item of the frame gated by module state, keeps the unit inlined:
    // the lowering of the unit is undone, its proxy with it, and the inlined
    // body makes its own.
    let source = r#"`timescale 1ns/1ns
module Top(output int x, output int y);
    bit a, b, en;
    bit [1:0] bus;
    task automatic t(int i, output int first, output int second);
        @(posedge (a && b));
        first = $time;
        @(posedge bus[i] iff en);
        second = $time;
    endtask
    int zero = 0;
    initial t(zero, x, y);
    initial begin
        #1 a = 1;
        #1 b = 1;
        #1 en = 1;
        #1 bus[0] = 1;
    end
endmodule
"#;
    same_as_inlined(source, 0, 10, &["x", "y"]);
    assert_eq!(
        simulate(source, false, true, "interpreter", 10, &["x", "y"]),
        Ok(vec![value(2), value(4)])
    );
}

#[test]
fn gates_that_compare_equal_values_select_together() {
    // `i == 1` and `i == 32'd1` are the same condition, though spelled
    // apart: with `i` 1 both hold, so the unit waits at the site of both
    // items.
    let source = r#"`timescale 1ns/1ns
module Top(output int a, output int b);
    bit pa, pb;
    task automatic either(int i, output int at);
        @(posedge pa iff (i == 1) or posedge pb iff (i == 32'd1));
        at = $time;
    endtask
    int one = 1;
    initial either(one, a);
    initial #3 either(one, b);
    initial begin
        #2 pb = 1;
        #2 pa = 1;
    end
endmodule
"#;
    same_as_inlined(source, 1, 10, &["a", "b"]);
    assert_eq!(
        simulate(source, false, true, "interpreter", 10, &["a", "b"]),
        Ok(vec![value(2), value(4)])
    );
}

#[test]
fn units_wait_for_named_events() {
    // `@e` in a recursion, `@` on an event a class property holds, and
    // `wait (e.triggered)` park the process in units as in an inlined body
    // (IEEE 1800-2023 15.5.2, 15.5.3): UVM's objections and sequences wait
    // so inside its phase traversal.
    let source = r#"`timescale 1ns/1ns
class obj_t;
    event raised;
    int waiters;
endclass
module Top(output int got, output int waiting, output int at);
    event e;
    obj_t o = new;
    task automatic wait_named(int n);
        if (n == 0) return;
        @e;
        got = got + 1;
        wait_named(n - 1);
    endtask
    task automatic wait_prop(obj_t x);
        x.waiters++;
        @(x.raised);
        x.waiters--;
    endtask
    task automatic wait_trig();
        wait (e.triggered);
    endtask
    int two = 2;
    initial begin
        wait_named(two);
        wait_prop(o);
        wait_trig();
        waiting = o.waiters;
        at = $time;
    end
    initial begin
        #1 -> e;
        #1 -> e;
        #1 -> o.raised;
        #1 -> e;
    end
endmodule
"#;
    assert_eq!(function_units(source, false), 1, "wait_named");
    for four_state in [false, true] {
        for forced in [false, true] {
            assert_eq!(
                simulate(
                    source,
                    four_state,
                    forced,
                    "interpreter",
                    10,
                    &["got", "waiting", "at"]
                ),
                Ok(vec![value(2), value(0), value(4)]),
                "four_state {four_state} forced {forced}"
            );
        }
    }
}

#[test]
fn resuming_from_named_event_waits_flushes_deferred_assertions() {
    // A resume from `@e` or from `wait (e.triggered)` that suspended is a
    // flush point (IEEE 1800-2023 16.4.2): the failing deferred assertion
    // queued before it is discarded, in a kernel and in a unit alike. A
    // `wait (e.triggered)` that does not suspend is none, so the failure
    // queued before it is reported.
    let flushed = |wait: &str, call: bool| {
        let body = if call { "wait_it();" } else { wait };
        format!(
            "`timescale 1ns/1ns\nmodule Top(output int done);\n    event e;\n    bit c;\n    task automatic wait_it();\n        {wait}\n    endtask\n    initial begin\n        assert #0 (c) else $fatal(1, \"not flushed\");\n        {body}\n        done = 1;\n    end\n    initial #0 -> e;\nendmodule\n"
        )
    };
    for wait in ["@e;", "wait (e.triggered);"] {
        for call in [false, true] {
            let source = flushed(wait, call);
            for four_state in [false, true] {
                for forced in [false, true] {
                    assert_eq!(
                        simulate(&source, four_state, forced, "interpreter", 5, &["done"]),
                        Ok(vec![value(1)]),
                        "{wait} call {call} four_state {four_state} forced {forced}"
                    );
                }
            }
        }
    }
    let unsuspended = r#"`timescale 1ns/1ns
module Top(output int done);
    event e;
    bit c;
    task automatic wait_it();
        wait (e.triggered);
    endtask
    initial begin
        -> e;
        assert #0 (c) else $fatal(1, "reported");
        wait_it();
        done = 1;
    end
endmodule
"#;
    for forced in [false, true] {
        assert_eq!(
            simulate(unsuspended, false, forced, "interpreter", 5, &["done"]),
            Err("reported".to_string()),
            "forced {forced}"
        );
    }
}

/// A design whose task `bump` makes, at each level of a recursion or of a
/// loop as `REPEAT` chooses, nonblocking assignments to a module variable
/// and a static class property, waits for the first, and calls `wait_nba`,
/// the shape of UVM's `uvm_wait_for_nba_region`: a nonblocking assignment
/// to a static local, then an event control on it.
const NBA_LEVELS: &str = r#"`timescale 1ns/1ns
class cfg_t;
    static int level;
endclass
module Top(output int a, output int b, output int c, output int d);
    int m;
    int log_[$];
    task wait_nba;
        static int nba;
        static int next_nba;
        next_nba++;
        nba <= next_nba;
        @(nba);
    endtask
    task automatic bump(int n);
        REPEAT begin
            m <= m + 1;
            @(m);
            cfg_t::level <= cfg_t::level + 10;
            wait_nba();
            log_.push_back(m * 100 + cfg_t::level);
            NEXT
        end
    endtask
    int three = 3;
    initial begin
        bump(three);
        a = log_[0];
        b = log_[1];
        c = log_[2];
        d = $time;
    end
endmodule
"#;

#[test]
fn nonblocking_assignments_in_recursive_units() {
    // Out of line by default, as its recursion, `bump` stages each update
    // at a function NBA site, which the calling kernel lists after its own:
    // the results are those of the loop that inlines.
    let recursive = NBA_LEVELS
        .replace("REPEAT begin", "if (n > 0) begin")
        .replace("NEXT", "bump(n - 1);");
    let looped = NBA_LEVELS
        .replace("REPEAT begin", "for (int k = n; k > 0; k--) begin")
        .replace("NEXT", "");
    assert_eq!(function_units(&recursive, false), 1, "bump");
    let names = ["a", "b", "c", "d"];
    for four_state in [false, true] {
        let expected =
            simulate(&looped, four_state, false, "interpreter", 10, &names).expect("the loop runs");
        for forced in [false, true] {
            assert_eq!(
                simulate(&recursive, four_state, forced, "interpreter", 10, &names),
                Ok(expected.clone()),
                "four_state {four_state} forced {forced}"
            );
        }
    }
    assert_eq!(
        simulate(&looped, false, false, "interpreter", 10, &names),
        Ok(vec![value(110), value(220), value(330), value(0)])
    );
}

#[test]
fn nonblocking_assignments_in_units_are_those_of_the_inlined_body() {
    // The same updates from units that the inlined design makes in its
    // kernels, through every backend.
    let source = NBA_LEVELS
        .replace("REPEAT begin", "for (int k = n; k > 0; k--) begin")
        .replace("NEXT", "");
    same_as_inlined(&source, 2, 10, &["a", "b", "c", "d"]);
}

/// UVM's `uvm_sequencer_base::m_select_sequence` shape: a child of a fork
/// sets a variable it shares with the parent, which waits for it. `REPEAT`
/// chooses a recursion or a loop.
const SELECT_LEVELS: &str = r#"`timescale 1ns/1ns
module Top(output int a, output int b, output int c);
    int log_[$];
    task automatic select(int n);
        REPEAT begin
            automatic int completed = 0;
            fork
                begin
                    #(m);
                    completed = 1;
                end
            join_none
            wait (completed > 0);
            log_.push_back(m * 100 + int'($time));
            NEXT
        end
    endtask
    int three = 3;
    initial begin
        select(three);
        a = log_[0];
        b = log_[1];
        c = log_[2];
    end
endmodule
"#;

#[test]
fn waits_on_variables_forked_children_share() {
    // The shared variable is in a box, whose stores bump a change count of
    // its own, which the parent's wait watches as it watches a class
    // property's: the child's store wakes it. Out of line by default, as its
    // recursion, `select` gives the results of the loop that inlines.
    let recursive = SELECT_LEVELS
        .replace(
            "REPEAT begin",
            "if (n > 0) begin\n            automatic int m = n;",
        )
        .replace("NEXT", "select(n - 1);");
    let looped = SELECT_LEVELS
        .replace("REPEAT begin", "for (int m = n; m > 0; m--) begin")
        .replace("NEXT", "");
    assert_eq!(function_units(&recursive, false), 1, "select");
    let names = ["a", "b", "c"];
    for four_state in [false, true] {
        let expected =
            simulate(&looped, four_state, false, "interpreter", 20, &names).expect("the loop runs");
        for forced in [false, true] {
            assert_eq!(
                simulate(&recursive, four_state, forced, "interpreter", 20, &names),
                Ok(expected.clone()),
                "four_state {four_state} forced {forced}"
            );
        }
    }
    assert_eq!(
        simulate(&looped, false, false, "interpreter", 20, &names),
        Ok(vec![value(303), value(205), value(106)])
    );
    same_as_inlined(&looped, 1, 20, &names);
}

#[test]
fn rejected_units_leave_no_box_counts() {
    // `spawn` boxes `a`, then is rejected for sharing its output `out` with
    // its children, and is inlined: the box's count is gone with the
    // rejected lowering, so `randomize`, which counts the stores of the
    // properties, finds none of it.
    let source = r#"`timescale 1ns/1ns
class r_t;
    rand bit [7:0] v;
endclass
module Top(output int got, output int ok);
    r_t r = new;
    task automatic spawn(output int out);
        int a = 1;
        fork
            #1 begin a = a + 1; out = a; end
        join
    endtask
    initial begin
        spawn(got);
        ok = r.randomize();
    end
endmodule
"#;
    for forced in [false, true] {
        assert_eq!(
            simulate(source, false, forced, "interpreter", 5, &["got", "ok"]),
            Ok(vec![value(2), value(1)]),
            "forced {forced}"
        );
    }
}

/// The variables each process kernel of `source` stores, by name, with
/// calls forced out of line, and the names of all the variables; a variable
/// the module no longer has is `?<id>`.
fn kernel_stores(
    source: &str,
) -> (
    Vec<std::collections::BTreeSet<String>>,
    std::collections::BTreeSet<String>,
) {
    use celox_sir::SIRInstruction;
    let result = SimulatorBuilder::<Simulator>::from_sv_sources(
        vec![(source, Path::new("calls.sv"))],
        "Top",
    )
    .force_out_of_line_calls()
    .trace_sim_modules()
    .build_with_trace();
    let mut kernels = Vec::new();
    let mut variables = std::collections::BTreeSet::new();
    for module in result
        .trace
        .sim_modules
        .as_ref()
        .expect("traced modules")
        .values()
    {
        variables.extend(
            module
                .variables
                .values()
                .map(|variable| variable.path.join(".")),
        );
        for process in &module.processes {
            let mut stores = std::collections::BTreeSet::new();
            for block in process.kernel.blocks.values() {
                for instruction in &block.instructions {
                    if let SIRInstruction::Store(addr, ..) = instruction {
                        stores.insert(module.variables.get(&addr.var_id).map_or_else(
                            || format!("?{}", addr.var_id.0),
                            |variable| variable.path.join("."),
                        ));
                    }
                }
            }
            kernels.push(stores);
        }
    }
    (kernels, variables)
}

#[test]
fn randomize_counts_no_stores_of_shared_variables() {
    // `completed` lives in a box whose stores count apart from the class
    // properties': the kernel that randomizes, which counts a store of
    // every watched property, bumps none of the box's counts.
    let source = SELECT_LEVELS
        .replace(
            "REPEAT begin",
            "if (n > 0) begin\n            automatic int m = n;",
        )
        .replace("NEXT", "select(n - 1);")
        .replace(
            "module Top(output int a, output int b, output int c);",
            "class r_t;\n    rand bit [7:0] v;\nendclass\nmodule Top(output int a, output int b, output int c, output int ok);\n    r_t r = new;",
        )
        // Lowered after `select`, whose unit makes the box.
        .replace("endmodule", "    initial #1 ok = r.randomize();\nendmodule");
    let (kernels, variables) = kernel_stores(&source);
    assert!(
        variables.iter().any(|name| name.starts_with("box_changes")),
        "the box has a count: {variables:?}"
    );
    let randomizing = kernels
        .iter()
        .find(|stores| stores.contains("ok"))
        .expect("the kernel that randomizes");
    assert!(
        !randomizing
            .iter()
            .any(|name| name.starts_with("box_changes")),
        "{randomizing:?}"
    );
}

#[test]
fn event_controls_on_class_properties_count_each_store() {
    // `@(e.on)`, `@(posedge e.on)` and `@(negedge e.l)` (IEEE 1800-2023
    // 9.4.2): each store of the property counts the stored object's
    // occurrences, so a pulse within one step is an occurrence, another
    // object's store is none, and a handle that comes to denote an object
    // whose value differs is one. UVM's `uvm_event::wait_on` waits so on a
    // property of `this`.
    let source = r#"`timescale 1ns/1ns
class ev_t;
    bit on;
    logic l;
    int seen;
    task wait_on();
        @on;
        seen = 1;
    endtask
endclass
module Top(output int pulse, output int pos, output int neg, output int moved, output int other,
           output int method);
    ev_t a = new, b = new, c = new, h;
    task automatic wait_any(ev_t e, output int at);
        @(e.on);
        at = $time;
    endtask
    task automatic wait_pos(ev_t e, output int at);
        @(posedge e.on);
        at = $time;
    endtask
    task automatic wait_neg(ev_t e, output int at);
        @(negedge e.l);
        at = $time;
    endtask
    task automatic wait_h(output int at);
        @(h.on);
        at = $time;
    endtask
    initial begin
        h = c;
        fork
            wait_any(a, pulse);
            wait_pos(a, pos);
            wait_neg(a, neg);
            wait_h(moved);
            wait_pos(b, other);
            a.wait_on();
        join_none
        #2 a.on = 1;
        a.on = 0;
        #2 a.l = 1;
        #1 a.l = 0;
        #2 b.on = 1;
        #2 h = b;
        #1 method = a.seen;
    end
endmodule
"#;
    let names = ["pulse", "pos", "neg", "moved", "other", "method"];
    same_as_inlined(source, 6, 20, &names);
    for four_state in [false, true] {
        assert_eq!(
            simulate(source, four_state, true, "interpreter", 20, &names),
            Ok(vec![
                value(2),
                value(2),
                value(5),
                value(9),
                value(7),
                value(1)
            ]),
            "four_state {four_state}"
        );
    }
}

/// The time each of `names` holds at `until`, inlined and out of line in
/// both state modes, which must be the same everywhere.
fn waits_end_at(source: &str, units: usize, until: u64, names: &[&str], expected: &[u64]) {
    same_as_inlined(source, units, until, names);
    for four_state in [false, true] {
        assert_eq!(
            simulate(source, four_state, true, "interpreter", until, names),
            Ok(expected.iter().map(|&at| value(at)).collect()),
            "four_state {four_state}"
        );
    }
}

#[test]
fn randomize_stores_are_occurrences_of_watched_properties() {
    // `randomize` writes `v` through the heap's store, which counts it like
    // an assignment does.
    let source = r#"`timescale 1ns/1ns
class r_t;
    rand bit [7:0] v;
    constraint big { v > 10; }
endclass
module Top(output int at, output int ok);
    r_t r = new;
    task automatic wait_v(r_t o, output int at);
        @(o.v);
        at = $time;
    endtask
    initial begin
        fork
            wait_v(r, at);
        join_none
        #3 ok = r.randomize();
    end
endmodule
"#;
    waits_end_at(source, 3, 10, &["at", "ok"], &[3, 1]);
}

#[test]
fn container_write_backs_are_occurrences_of_watched_properties() {
    // `push_back` writes the queue property's new handle back through the
    // heap's store, which counts it.
    let source = r#"`timescale 1ns/1ns
class q_t;
    int q[$];
endclass
module Top(output int at);
    q_t o = new;
    task automatic wait_q(q_t e, output int at);
        @(e.q);
        at = $time;
    endtask
    initial begin
        fork
            wait_q(o, at);
        join_none
        #4 o.q.push_back(5);
    end
endmodule
"#;
    waits_end_at(source, 2, 10, &["at"], &[4]);
}

#[test]
fn waits_on_properties_follow_a_switched_handle() {
    // `h` comes to denote `b`, whose `on` is 0 where `a`'s was 1: no rise,
    // and the wait goes on with `b`, whose later rise is the occurrence.
    let source = r#"`timescale 1ns/1ns
class ev_t;
    bit on;
endclass
module Top(output int at);
    ev_t a = new, b = new, h;
    task automatic wait_h(output int at);
        @(posedge h.on);
        at = $time;
    endtask
    initial begin
        a.on = 1;
        h = a;
        fork
            wait_h(at);
        join_none
        #2 h = b;
        #2 b.on = 1;
    end
endmodule
"#;
    waits_end_at(source, 2, 10, &["at"], &[4]);
}

#[test]
fn event_lists_mix_properties_and_signals() {
    // Each item is waited on as what it is: a property by its object's
    // count, a signal by its value's edge.
    let source = r#"`timescale 1ns/1ns
class ev_t;
    bit on;
endclass
module Top(output int any_at, output int pos_at, output int neg_at);
    ev_t o = new, other = new;
    bit clk;
    task automatic wait_any(output int at);
        @(o.on or clk);
        at = $time;
    endtask
    task automatic wait_pos(output int at);
        @(posedge clk or other.on);
        at = $time;
    endtask
    task automatic wait_neg(output int at);
        @(negedge clk or other.on);
        at = $time;
    endtask
    initial begin
        fork
            wait_any(any_at);
            wait_pos(pos_at);
            wait_neg(neg_at);
        join_none
        #1 o.on = 1;
        #2 clk = 1;
        #2 clk = 0;
    end
endmodule
"#;
    waits_end_at(source, 4, 10, &["any_at", "pos_at", "neg_at"], &[1, 3, 5]);
}

#[test]
fn edges_of_real_properties_are_rejected() {
    // An edge is of the least significant bit of an integral value (IEEE
    // 1800-2023 9.4.2): as for any real expression.
    let source = r#"`timescale 1ns/1ns
class r_t;
    real r;
endclass
module Top(output int at);
    r_t o = new;
    initial begin
        @(posedge o.r);
        at = $time;
    end
endmodule
"#;
    for forced in [false, true] {
        let run = simulate(source, false, forced, "interpreter", 5, &["at"]);
        assert!(
            run.as_ref().is_err_and(|error| error.contains("posedge")),
            "forced {forced}: {run:?}"
        );
    }
}

#[test]
fn switched_handles_compare_real_properties_as_reals() {
    // `h` comes to denote an object whose `r` differs by an eighth: a
    // change, which an integer sample would round away (both round to 1).
    let source = r#"`timescale 1ns/1ns
class r_t;
    real r;
endclass
module Top(output int at);
    r_t a = new, b = new, h;
    task automatic wait_h(output int at);
        @(h.r);
        at = $time;
    endtask
    initial begin
        a.r = 1.25;
        b.r = 1.375;
        h = a;
        fork
            wait_h(at);
        join_none
        #2 h = b;
    end
endmodule
"#;
    waits_end_at(source, 2, 10, &["at"], &[2]);
}

#[test]
fn wide_property_stores_are_one_write_each() {
    // A 100-bit 4-state property is stored by one heap call with all its
    // words and both planes, which the heap counts once: the pulse at 2 is
    // a rise and a fall, and 1 -> x at 4 a fall only (IEEE 1800-2023 Table
    // 9-2).
    let source = r#"`timescale 1ns/1ns
class w_t;
    logic [99:0] w;
endclass
module Top(output int pos, output int neg, output int any_at, output int x_neg);
    w_t o = new, p = new;
    task automatic wait_pos(w_t e, output int at);
        @(posedge e.w);
        at = $time;
    endtask
    task automatic wait_neg(w_t e, output int at);
        @(negedge e.w);
        at = $time;
    endtask
    task automatic wait_any(w_t e, output int at);
        @(e.w);
        at = $time;
    endtask
    initial begin
        o.w = 0;
        p.w = 1;
        fork
            wait_pos(o, pos);
            wait_neg(o, neg);
            wait_any(o, any_at);
            wait_neg(p, x_neg);
        join_none
        #2 o.w = {1'b1, 99'h1};
        o.w = 0;
        #2 p.w = 'x;
    end
endmodule
"#;
    let names = ["pos", "neg", "any_at", "x_neg"];
    same_as_inlined(source, 4, 10, &names);
    assert_eq!(
        simulate(source, true, true, "interpreter", 10, &names),
        Ok(vec![value(2), value(2), value(2), value(4)])
    );
}

#[test]
fn iff_of_property_items_is_taken_at_the_start_of_the_wait() {
    // An occurrence on a property happens inside a store, where an `iff` is
    // not evaluated: a condition that only the waiting process writes, here
    // the task's `go`, has its value of the start at every occurrence.
    let source = r#"`timescale 1ns/1ns
class ev_t;
    bit on;
endclass
module Top(output int yes, output int no);
    ev_t o = new;
    task automatic wait_on(bit go, output int at);
        at = 0;
        @(posedge o.on iff go);
        at = $time;
    endtask
    initial wait_on(1, yes);
    initial wait_on(0, no);
    initial #2 o.on = 1;
endmodule
"#;
    same_as_inlined(source, 2, 10, &["yes", "no"]);
    assert_eq!(
        simulate(source, false, true, "interpreter", 10, &["yes", "no"]),
        Ok(vec![value(2), value(0)])
    );
    // A condition other processes write cannot be judged at the store.
    let shared = source
        .replace("output int no);", "output int no);\n    bit en = 1;")
        .replace("iff go", "iff en");
    for forced in [false, true] {
        let run = simulate(&shared, false, forced, "interpreter", 10, &["yes"]);
        assert!(
            run.as_ref()
                .is_err_and(|error| error.contains("an event control on a class property")),
            "forced {forced}: {run:?}"
        );
    }
}

#[test]
fn signals_listed_with_properties_count_each_edge() {
    // `clk` rises and falls in one run of another process: the rise is an
    // occurrence of `posedge clk` (IEEE 1800-2023 9.4.2), though `clk` ends
    // as it began; the writes count it where they happen.
    let source = r#"`timescale 1ns/1ns
class ev_t;
    bit on;
endclass
module Top(output int at);
    ev_t o = new;
    bit clk;
    task automatic wait_any(output int at);
        @(o.on or posedge clk);
        at = $time;
    endtask
    initial begin
        fork
            wait_any(at);
        join_none
        #3 begin
            clk = 1;
            clk = 0;
        end
    end
endmodule
"#;
    same_as_inlined(source, 2, 10, &["at"]);
    assert_eq!(
        simulate(source, false, true, "interpreter", 10, &["at"]),
        Ok(vec![value(3)])
    );
}

#[test]
fn waits_on_properties_of_null_handles_fault() {
    // The wait reads the property as the item does: through a null handle
    // it is a null dereference, not a wait that never ends.
    let source = r#"`timescale 1ns/1ns
class ev_t;
    bit on;
endclass
module Top(output int at);
    ev_t none;
    task automatic wait_on(ev_t e, output int at);
        @(e.on);
        at = $time;
    endtask
    initial wait_on(none, at);
endmodule
"#;
    for forced in [false, true] {
        let run = simulate(source, false, forced, "interpreter", 5, &["at"]);
        assert!(
            run.as_ref().is_err_and(|error| error.contains("null")),
            "forced {forced}: {run:?}"
        );
    }
}

#[test]
fn iff_of_property_items_gates_handle_switches() {
    // `h` comes to denote `y`, whose `on` is 1 where `x`'s was 0: a rise by
    // the switch, which the `iff` sampled at the start gates as it gates a
    // rise by a store.
    let source = r#"`timescale 1ns/1ns
class ev_t;
    bit on;
endclass
module Top(output int closed, output int open);
    ev_t x = new, y = new, h;
    initial begin
        y.on = 1;
        h = x;
    end
    initial begin
        bit go = 0;
        closed = 0;
        @(posedge h.on iff go);
        closed = $time;
    end
    initial begin
        bit go = 1;
        open = 0;
        @(posedge h.on iff go);
        open = $time;
    end
    initial #2 h = y;
endmodule
"#;
    for forced in [false, true] {
        assert_eq!(
            simulate(source, false, forced, "interpreter", 5, &["closed", "open"]),
            Ok(vec![value(0), value(2)]),
            "forced {forced}"
        );
    }
}

#[test]
fn handle_switches_are_seen_as_expression_events_are() {
    // An expression event sees a process's writes when the process reaches a
    // timing control or ends (docs/guide/systemverilog-reference.md): the
    // pulse `a = 1; a = 0;` is no edge of `a & b`, and the switches
    // `h = y; h = x;` are no edge of `h.on`, though `y.on` is 1.
    let expression = r#"`timescale 1ns/1ns
module Top(output int at);
    bit a, b = 1;
    initial begin
        at = 0;
        @(posedge (a & b));
        at = $time;
    end
    initial #2 begin
        a = 1;
        a = 0;
    end
endmodule
"#;
    let handle = r#"`timescale 1ns/1ns
class ev_t;
    bit on;
endclass
module Top(output int at);
    ev_t x = new, y = new, h;
    initial begin
        y.on = 1;
        h = x;
        at = 0;
        @(posedge h.on);
        at = $time;
    end
    initial #2 begin
        h = y;
        h = x;
    end
endmodule
"#;
    for source in [expression, handle] {
        for forced in [false, true] {
            assert_eq!(
                simulate(source, false, forced, "interpreter", 5, &["at"]),
                Ok(vec![value(0)]),
                "forced {forced}: {source}"
            );
        }
    }
}

#[test]
fn packed_dimensions_of_package_types_keep_their_bounds() {
    // `wide_t`'s bound names a package parameter, which the module's
    // constants do not hold by the name the range spells: the variables of
    // the type keep its packed dimension as the analysis resolved it, so a
    // write of a bit at a run-time position works, in the recursive unit
    // `fill` and in the `initial` kernel.
    let source = r#"`timescale 1ns/1ns
package p;
    parameter int PKG_W = 96;
    typedef logic [PKG_W-1:0] wide_t;
    class C;
        extern function int fill(int depth, int k);
    endclass
    // An out-of-block method declaration, as UVM's
    // `uvm_mem::backdoor_read_func`.
    function int C::fill(int depth, int k);
        wide_t val = 0;
        val[k] = 1'b1;
        val[k + 64] = 1'b1;
        if (depth == 0) return int'(val[k +: 8]) + int'(val[64 +: 32]);
        return fill(depth - 1, k + 1);
    endfunction
endpackage
import p::*;
module Top(output int unit_bits, output int kernel_bits);
    C c = new;
    int at = 3;
    initial begin
        wide_t val = 0;
        val[at] = 1'b1;
        val[at + 64] = 1'b1;
        kernel_bits = int'(val[7:0]) + int'(val[95:64]);
        unit_bits = c.fill(2, at);
    end
endmodule
"#;
    assert!(function_units(source, false) > 0, "out of line");
    for four_state in [false, true] {
        for forced in [false, true] {
            for backend in backends() {
                assert_eq!(
                    simulate(
                        source,
                        four_state,
                        forced,
                        backend,
                        2,
                        &["unit_bits", "kernel_bits"]
                    ),
                    Ok(vec![value(1 + (1 << 5)), value((1 << 3) + (1 << 3))]),
                    "{backend}, four-state {four_state}, forced {forced}"
                );
            }
        }
    }
}

#[test]
fn frames_hold_unpacked_arrays_of_handles() {
    // Each activation of the recursive `keep` holds objects and strings in
    // unpacked arrays of its frame across delays, while another process
    // allocates enough for the heap to collect: the collector scans each
    // element of a saved frame's arrays as a handle slot.
    let source = r#"`timescale 1ns/1ns
module Top(output int a, output int b, output int n);
    class Item;
        int v;
        function new(int x);
            v = x;
        endfunction
    endclass
    task automatic keep(int depth, output int sum, output int chars);
        Item items[3];
        string names[2];
        int below = 0;
        int below_chars = 0;
        // A loop of a run-time bound leaves no temporary holding all but
        // the last element.
        for (int i = 0; i < three; i++) begin
            items[i] = new(depth * 10 + i);
            names[i % 2] = $sformatf("d%0d_%0d", depth, i);
        end
        #2;
        if (depth > 0) keep(depth - 1, below, below_chars);
        #2;
        sum = below + items[0].v + items[1].v + items[2].v;
        chars = below_chars + names[0].len() + names[1].len();
    endtask
    int two = 2;
    int three = 3;
    int count = 3000;
    initial keep(two, a, b);
    initial begin
        Item t;
        #1;
        for (int i = 0; i < count; i++) begin
            t = new(i);
            n += t.v % 2;
            if (i % 500 == 0) #1;
        end
    end
endmodule
"#;
    assert_eq!(function_units(source, false), 1);
    for four_state in [false, true] {
        for backend in backends() {
            assert_eq!(
                simulate(source, four_state, true, backend, 20, &["a", "b", "n"]),
                Ok(vec![value(99), value(24), value(1500)]),
                "{backend}, four-state {four_state}"
            );
        }
    }
}

#[test]
fn unknown_conditions_run_both_arms_across_suspending_calls() {
    // An unknown condition runs both arms of a conditional operator (IEEE
    // 1800-2023 11.4.11). Whether the else arm runs is decided before the
    // then arm, whose call may suspend the unit and resume it there.
    let source = r#"`timescale 1ns/1ns
module Top(output int r, output int s, output int t);
    logic c;
    int fd;
    function automatic int show(int x);
        $fdisplay(fd, "show %0d", x);
        return x + 1;
    endfunction
    function automatic int pick(logic c);
        return c ? show(10) : show(20);
    endfunction
    initial begin
        fd = 1;
        c = 1'bx;
        r = pick(c);
        #1 c = 1;
        s = pick(c);
        #1 c = 0;
        t = pick(c);
    end
endmodule
"#;
    same_as_inlined(source, 1, 3, &["r", "s", "t"]);
}

/// `body`, the body of a function that the design calls three times with
/// `a` unknown, 1 and 0, runs as it does inlined. `show` makes a host
/// request, which suspends the process or unit that calls it and resumes it
/// there: no value the body holds across the call may live in a register.
fn same_across_suspensions(declarations: &str, body: &str) {
    let source = format!(
        r#"`timescale 1ns/1ns
module Top(output int r, output int s, output int t);
    int fd = 1;
    logic [7:0] a;
    int q[$];
    int d[];
    function automatic int show(int x);
        $fdisplay(fd, "show %0d", x);
        return x;
    endfunction
    function automatic void run();
        {declarations}
        {body}
    endfunction
    initial begin
        a = 8'bx;
        q = '{{1, 2, 3}};
        d = '{{4, 5, 6}};
        r = 0;
        s = 0;
        t = 0;
        run();
        #1 a = 1;
        run();
        #1 a = 0;
        run();
    end
endmodule
"#
    );
    assert!(function_units(&source, true) > 0, "function units");
    for four_state in [false, true] {
        let inlined = simulate_run(
            &source,
            four_state,
            false,
            "interpreter",
            3,
            &["r", "s", "t"],
        )
        .expect("the inlined design runs");
        for backend in backends() {
            assert_eq!(
                simulate_run(&source, four_state, true, backend, 3, &["r", "s", "t"]),
                Ok(inlined.clone()),
                "{backend}, four-state {four_state}"
            );
        }
    }
}

#[test]
fn short_circuits_across_suspending_calls() {
    same_across_suspensions(
        "",
        "r = r * 4 + int'((a != 0) && (show(1) != 0));
        s = s * 4 + int'((a == 0) || (show(2) != 0));
        t = t * 4 + int'((show(3) > 2) && (show(4) > 3));",
    );
}

#[test]
fn inside_across_suspending_calls() {
    same_across_suspensions(
        "",
        "r = r * 4 + int'(show(2) inside {1, show(2), [show(3):show(5)]});
        s = s * 4 + int'(a inside {show(0), show(1)});
        t = t * 4 + int'(show(int'(a)) inside {[show(0):show(1)]});",
    );
}

#[test]
fn case_selectors_across_suspending_calls() {
    same_across_suspensions(
        "",
        "case (show(a == 1 ? 1 : 2))
            show(1): r += 1;
            show(2): r += 2;
            default: r += 3;
        endcase
        case (a)
            show(0): s += 1;
            show(1): s += 2;
            default: s += 5;
        endcase
        casez (show(t))
            show(0): t += 1;
            default: t += 7;
        endcase",
    );
}

#[test]
fn loop_conditions_across_suspending_calls() {
    same_across_suspensions(
        "int j = 0;",
        "for (int i = 0; i < show(3); i++) r += show(i);
        while (show(j) < 2) j++;
        s += j;
        do t += 1; while (show(t) % 4 != 0);",
    );
}

#[test]
fn foreach_bounds_across_suspending_calls() {
    same_across_suspensions(
        "int m[3] = '{7, 8, 9};",
        "foreach (q[i]) r += show(q[i]);
        foreach (d[i]) s += show(d[i] + i);
        foreach (m[i]) t += show(m[i]);",
    );
}

#[test]
fn compound_assignments_across_suspending_calls() {
    same_across_suspensions(
        "int m[3] = '{1, 2, 3};",
        "r += show(4);
        s *= show(2) + 1;
        m[show(1)] += show(10);
        t -= show(m[1]);",
    );
}

#[test]
fn units_driving_clocks_of_other_instances() {
    // `Gen`'s task stores the variable that clocks `Top`'s flip-flop through
    // `Gen`'s output port, and a variable that is only wired to a port. A
    // unit a process calls stores an event as the process would: the store
    // is counted as the flip-flop's occurrence. So the task goes out of line
    // and the flip-flop sees every edge it makes.
    let source = r#"`timescale 1ns/1ns
module Gen(output logic clk, output logic [7:0] w);
    logic c = 0;
    logic [7:0] w_r = 0;
    assign clk = c;
    assign w = w_r;
    task automatic tick(input logic [7:0] v);
        #1 c = 1;
        w_r = v;
        #1 c = 0;
    endtask
    logic [7:0] n = 3;
    initial repeat (3) begin
        tick(n);
        n = n + 8'd1;
    end
endmodule
module Top(output logic [7:0] q, output logic [7:0] w);
    logic clk;
    Gen gen(.clk(clk), .w(w));
    always_ff @(posedge clk) q <= q + 8'd1;
    initial q = 0;
endmodule
"#;
    assert_eq!(function_units(source, true), 1, "function units");
    for four_state in [false, true] {
        let inlined = simulate_run(source, four_state, false, "interpreter", 10, &["q", "w"])
            .expect("the inlined design runs");
        for backend in backends() {
            assert_eq!(
                simulate_run(source, four_state, true, backend, 10, &["q", "w"]),
                Ok(inlined.clone()),
                "{backend}, four-state {four_state}"
            );
        }
    }
}

#[test]
fn units_driving_clocks_and_resets_of_their_module() {
    // A task only processes call toggles the clock and the asynchronous
    // reset of its own module's flip-flop: its stores are counted as the
    // flip-flop's events, as the process's own would be.
    let source = r#"`timescale 1ns/1ns
module Top(output logic [7:0] q);
    logic clk = 0;
    logic rst = 0;
    logic [7:0] q_r = 0;
    logic [7:0] n = 2;
    assign q = q_r;
    task automatic pulse(input logic [7:0] k);
        repeat (k) begin
            #1 clk = 1;
            #1 clk = 0;
        end
        #1 rst = 1;
        #1 rst = 0;
    endtask
    always_ff @(posedge clk or posedge rst)
        if (rst) q_r <= 8'd100;
        else q_r <= q_r + 8'd1;
    initial begin
        pulse(n);
        n = 3;
        pulse(n);
    end
endmodule
"#;
    same_as_inlined(source, 1, 9, &["q"]);
    let until_reset =
        simulate(source, false, true, "interpreter", 5, &["q"]).expect("the design runs");
    assert_eq!(
        until_reset,
        vec![value(100)],
        "the reset reached the flip-flop"
    );
}

#[test]
fn defaults_no_call_uses_do_not_stop_a_unit() {
    // A default is evaluated only by a call that uses it (IEEE 1800-2023
    // 13.5.3). This one reads another formal, as UVM's `uvm_report` does,
    // which the scope of the declaration does not resolve; no call omits
    // it, so the recursive function still runs out of line.
    let source = r#"module Top(output int f);
    function automatic int depth(int severity, int verbosity = (severity == 1) ? 0 : 200, int n = 0);
        if (n == 0) return verbosity + severity;
        return depth(severity, verbosity, n - 1) + 1;
    endfunction
    initial f = depth(1, 5, 3);
endmodule
"#;
    assert_eq!(function_units(source, false), 1);
    for four_state in [false, true] {
        for backend in backends() {
            assert_eq!(
                simulate(source, four_state, false, backend, 0, &["f"]),
                Ok(vec![value(9)]),
                "{backend}, four-state {four_state}"
            );
        }
    }
    // A call that uses it is an error.
    let omits = source.replace("depth(1, 5, 3)", "depth(1)");
    let built = simulate(&omits, false, false, "interpreter", 0, &["f"]);
    assert!(built.is_err(), "{built:?}");
}

#[test]
fn deferred_assertions_of_units_wait_in_the_queue_of_their_process() {
    // A recursive function goes out of line; the actions of its deferred
    // assertions wait in the queue of the process that calls it and mature
    // as those of the looped reference, inlined, do (IEEE 1800-2023
    // 16.4.1). The assertions keep their lines.
    let recursive = r#"`timescale 1ns/1ns
module Top(output int r);
    function automatic int count(int n);
        if (n == 0) return 0;
        assert #0 (n != 2) else $error("deep %0d", n);
        assert final (n != 3) else $warning("top %0d", n);
        return count(n - 1) + 1;
    endfunction
    int k = 3;
    initial begin
        r = count(k);
        #1 r = count(k - 1);
    end
endmodule
"#;
    let looped = recursive
        .replace("int count(int n);", "int count(int n0);")
        .replace(
            "        if (n == 0) return 0;",
            "        for (int n = n0; n > 0; n--) begin",
        )
        .replace("$warning(\"top %0d\", n);", "$warning(\"top %0d\", n); end")
        .replace("return count(n - 1) + 1;", "return n0;");
    assert_ne!(looped, recursive);
    assert_eq!(function_units(recursive, false), 1, "function units");
    for four_state in [false, true] {
        let expected = simulate_run(&looped, four_state, false, "interpreter", 2, &["r"])
            .expect("the looped design runs");
        assert!(!expected.1.is_empty(), "the looped design reports");
        for backend in backends() {
            assert_eq!(
                simulate_run(recursive, four_state, false, backend, 2, &["r"]),
                Ok(expected.clone()),
                "{backend}, four-state {four_state}"
            );
        }
    }
}

#[test]
fn flush_points_of_a_process_discard_the_deferred_actions_of_its_units() {
    // The unit queues a `$fatal` in the queue of the process; the resume
    // from the event control comes before the Observed region, a flush
    // point of the process, which discards it (IEEE 1800-2023 16.4.2).
    // Without the wait it matures.
    let source = r#"`timescale 1ns/1ns
module Top(output int done);
    bit sig;
    function automatic int count(int n);
        if (n == 0) return 0;
        assert #0 (n != 2) else $fatal(1, "not flushed");
        return count(n - 1) + 1;
    endfunction
    initial begin
        void'(count(3));
        @(posedge sig);
        done = 1;
    end
    initial #0 sig = 1;
endmodule
"#;
    let matured = source.replace("        @(posedge sig);\n", "");
    assert_ne!(matured, source);
    assert_eq!(function_units(source, false), 1, "function units");
    for four_state in [false, true] {
        for backend in backends() {
            assert_eq!(
                simulate(source, four_state, false, backend, 5, &["done"]),
                Ok(vec![value(1)]),
                "{backend}, four-state {four_state}"
            );
            assert_eq!(
                simulate(&matured, four_state, false, backend, 5, &["done"]),
                Err("not flushed".to_string()),
                "{backend}, four-state {four_state}"
            );
        }
    }
}

#[test]
fn units_call_the_dpi_c_functions_of_uvm_that_celox_provides() {
    // UVM's regular expressions and tool queries are heap calls: a
    // recursive function goes out of line with them and gives what the
    // looped reference, inlined, gives.
    let recursive = r#"`timescale 1ns/1ns
module Top(output int r, output int matched);
    import "DPI-C" function bit uvm_re_compexecfree(string re, string str, bit deglob, output int exec_ret);
    import "DPI-C" function string uvm_dpi_get_tool_name_c ();
    function automatic int count(int n, string s);
        int ret;
        if (n == 0) return 0;
        if (uvm_re_compexecfree("a*b", s, 1, ret) && ret == 0) matched++;
        return count(n - 1, {s, "b"}) + (uvm_dpi_get_tool_name_c() == "Celox");
    endfunction
    initial r = count(3, "a");
endmodule
"#;
    let looped = recursive
        .replace(
            "        if (n == 0) return 0;\n",
            "        int k = 0;\n        for (int i = 0; i < n; i++) begin\n",
        )
        .replace(
            "        return count(n - 1, {s, \"b\"}) + (uvm_dpi_get_tool_name_c() == \"Celox\");",
            "        k += uvm_dpi_get_tool_name_c() == \"Celox\"; s = {s, \"b\"}; end\n        return k;",
        );
    assert_ne!(looped, recursive);
    assert_eq!(function_units(recursive, false), 1, "function units");
    for four_state in [false, true] {
        let expected = simulate(
            &looped,
            four_state,
            false,
            "interpreter",
            0,
            &["r", "matched"],
        )
        .expect("the looped design runs");
        // "a" does not match the glob, "ab" and "abb" do.
        assert_eq!(expected, vec![value(3), value(2)], "the looped reference");
        for backend in backends() {
            assert_eq!(
                simulate(recursive, four_state, false, backend, 0, &["r", "matched"]),
                Ok(expected.clone()),
                "{backend}, four-state {four_state}"
            );
        }
    }
}
