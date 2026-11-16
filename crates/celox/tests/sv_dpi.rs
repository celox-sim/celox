//! SystemVerilog DPI-C imports: calls from `always_ff` into C functions.
#![allow(
    clippy::disallowed_types,
    reason = "test-only: DPI imports are C functions that record into statics"
)]

use std::path::Path;
use std::sync::Mutex;

use celox::{Simulator, SimulatorBuilder, SimulatorErrorKind};
use num_bigint::BigUint;

fn builder(source: &'static str) -> SimulatorBuilder<'static> {
    Simulator::from_sv_sources(vec![(source, Path::new("dpi.sv"))], "Top")
}

// Each test runs on every backend that executes extern calls.
macro_rules! dpi_backends {
    () => {};
    (
        fn $name:ident($sim:ident) {
            @build $builder:expr;
            $($body:tt)*
        }
        $($rest:tt)*
    ) => {
        mod $name {
            use super::*;

            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            #[test]
            fn native() {
                let mut $sim = { $builder }.build_native().unwrap();
                $($body)*
            }

            #[test]
            fn cranelift() {
                let mut $sim = { $builder }.build_cranelift().unwrap();
                $($body)*
            }

            #[test]
            fn interpreter() {
                let mut $sim = { $builder }.build_interpreter().unwrap();
                $($body)*
            }

            #[test]
            fn wasm() {
                let mut $sim = { $builder }.build_wasm().unwrap();
                $($body)*
            }

            #[test]
            fn cranelift_parallel() {
                let mut $sim = { $builder }
                    .threads(4)
                    .parallel_partition(celox::ParallelPartition::Always)
                    .build_cranelift()
                    .unwrap();
                $($body)*
            }
        }
        dpi_backends! { $($rest)* }
    };
}

extern "C" fn dpi_add(x: i32, y: i32) -> i32 {
    x.wrapping_add(y)
}

extern "C" fn dpi_digits(first: i32, second: i32) -> i32 {
    first * 10 + second
}

extern "C" fn dpi_negate_byte(x: i8) -> i8 {
    x.wrapping_neg()
}

extern "C" fn dpi_identity_u64(x: u64) -> u64 {
    x
}

extern "C" fn dpi_halve_short(x: u16) -> u16 {
    x / 2
}

/// `svLogic` encoding: 0, 1, 2 (`z`) and 3 (`x`).
extern "C" fn dpi_logic_not(x: u8) -> u8 {
    match x {
        0 => 1,
        1 => 0,
        _ => 3,
    }
}

extern "C" fn dpi_bit_and(x: u8, y: u8) -> u8 {
    x & y
}

extern "C" fn dpi_sum3(x: i32, y: i32, z: i32) -> i32 {
    x + y + z
}

#[allow(clippy::too_many_arguments)]
extern "C" fn dpi_sum10(
    a: i32,
    b: i32,
    c: i32,
    d: i32,
    e: i32,
    f: i32,
    g: i32,
    h: i32,
    i: i32,
    j: i32,
) -> i32 {
    a + 2 * b + 3 * c + 4 * d + 5 * e + 6 * f + 7 * g + 8 * h + 9 * i + 10 * j
}

static RECORDED: Mutex<Vec<(&'static str, i32)>> = Mutex::new(Vec::new());

fn record(tag: &'static str, value: i32) {
    RECORDED.lock().unwrap().push((tag, value));
}

fn recorded(tag: &str) -> Vec<i32> {
    RECORDED
        .lock()
        .unwrap()
        .iter()
        .filter(|(recorded, _)| *recorded == tag)
        .map(|(_, value)| *value)
        .collect()
}

macro_rules! recorder {
    ($name:ident, $tag:literal) => {
        extern "C" fn $name(value: i32) {
            record($tag, value);
        }
    };
}

recorder!(record_native, "native");
recorder!(record_cranelift, "cranelift");
recorder!(record_interpreter, "interpreter");
recorder!(record_wasm, "wasm");
recorder!(record_cranelift_parallel, "cranelift_parallel");

dpi_backends! {
    fn named_imports_bind_formals_and_evaluate_actuals_in_source_order(sim) {
        @build unsafe {
            builder(r#"
                module Top(input logic clk, input int a, b,
                  output int named_result, mixed_result, effects_result, tmp);
                    import "DPI-C" function int digits(input int first, input int second);
                    function automatic int write_tmp(input int v, output int dst);
                        dst = v;
                        return v;
                    endfunction
                    always_ff @(posedge clk) begin
                        named_result <= digits(.second(a), .first(b));
                        mixed_result <= digits(a, .second(b));
                        tmp = 0;
                        effects_result <= digits(.second(write_tmp(a, tmp)), .first(tmp));
                        digits(.second(write_tmp(b, tmp)), .first(tmp));
                    end
                endmodule
            "#).dpi_function("digits", dpi_digits as *const ())
        };
        let clk = sim.event("clk");
        let (a, b) = (sim.signal("a"), sim.signal("b"));
        for (x, y) in [(3u32, 4u32), (7, 2)] {
            sim.modify(|io| { io.set(a, x); io.set(b, y); }).unwrap();
            sim.tick(clk).unwrap();
            assert_eq!(sim.get(sim.signal("named_result")), BigUint::from(10 * y + x));
            assert_eq!(sim.get(sim.signal("mixed_result")), BigUint::from(10 * x + y));
            assert_eq!(sim.get(sim.signal("effects_result")), BigUint::from(11 * x));
            assert_eq!(sim.get(sim.signal("tmp")), BigUint::from(y));
        }
    }

    // A generate block can import another declaration than its module
    // (#1206): named arguments bind to the formals of the block's import.
    fn generate_block_imports_bind_their_own_dpi_formals(sim) {
        @build unsafe {
            builder(r#"
                package p;
                    import "DPI-C" digits = function int f(input int first, input int second);
                endpackage
                package q;
                    import "DPI-C" digits = function int f(input int second, input int first);
                endpackage
                module Top(input logic clk, input int a, b, output int block_result, module_result);
                    import q::*;
                    if (1) begin : g
                        import p::*;
                        always_ff @(posedge clk) block_result <= f(.first(a), .second(b));
                    end
                    always_ff @(posedge clk) module_result <= f(.first(a), .second(b));
                endmodule
            "#).dpi_function("digits", dpi_digits as *const ())
        };
        let clk = sim.event("clk");
        let (a, b) = (sim.signal("a"), sim.signal("b"));
        sim.modify(|io| { io.set(a, 3u32); io.set(b, 4u32); }).unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(sim.signal("block_result")), BigUint::from(34u32));
        assert_eq!(sim.get(sim.signal("module_result")), BigUint::from(43u32));
    }

    fn int_function_in_nonblocking_assignment(sim) {
        @build unsafe {
            builder(r#"
                module Top(input logic clk, input int a, input int b, output int y);
                    import "DPI-C" function int dpi_add(input int x, input int y);
                    always_ff @(posedge clk) y <= dpi_add(a, b) + 1;
                endmodule
            "#)
            .dpi_function("dpi_add", dpi_add as *const ())
        };
        let clk = sim.event("clk");
        let (a, b, y) = (sim.signal("a"), sim.signal("b"), sim.signal("y"));
        sim.modify(|io| {
            io.set(a, 40u32);
            io.set(b, 1u32);
        })
        .unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(y), BigUint::from(42u32));
        sim.modify(|io| {
            io.set(a, 0x7fff_ffffu32);
            io.set(b, 1u32);
        })
        .unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(y), BigUint::from(0x8000_0001u32));
    }

    fn signed_byte_arguments_and_results_extend_by_sign(sim) {
        @build unsafe {
            builder(r#"
                module Top(input logic clk, input logic [7:0] a, output logic [15:0] y);
                    import "DPI-C" function byte negate(input byte x);
                    always_ff @(posedge clk) y <= negate(a);
                endmodule
            "#)
            .dpi_function("negate", dpi_negate_byte as *const ())
        };
        let clk = sim.event("clk");
        let (a, y) = (sim.signal("a"), sim.signal("y"));
        sim.modify(|io| io.set(a, 0xf0u8)).unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(y), BigUint::from(16u32));
        sim.modify(|io| io.set(a, 5u8)).unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(y), BigUint::from(0xfffbu32));
    }

    fn unsigned_integers_keep_their_width(sim) {
        @build unsafe {
            builder(r#"
                module Top(
                    input logic clk,
                    input logic [63:0] a,
                    input logic [15:0] b,
                    output logic [63:0] y,
                    output logic [31:0] z
                );
                    import "DPI-C" function longint unsigned identity(input longint unsigned x);
                    import "DPI-C" function shortint unsigned halve(input shortint unsigned x);
                    always_ff @(posedge clk) begin
                        y <= identity(a);
                        z <= halve(b);
                    end
                endmodule
            "#)
            .dpi_function("identity", dpi_identity_u64 as *const ())
            .dpi_function("halve", dpi_halve_short as *const ())
        };
        let clk = sim.event("clk");
        let (a, b, y, z) = (sim.signal("a"), sim.signal("b"), sim.signal("y"), sim.signal("z"));
        sim.modify(|io| {
            io.set(a, 0xdead_beef_1234_5678u64);
            io.set(b, 0xfffeu16);
        })
        .unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(y), BigUint::from(0xdead_beef_1234_5678u64));
        assert_eq!(sim.get(z), BigUint::from(0x7fffu32));
    }

    fn bit_and_logic_scalars(sim) {
        @build unsafe {
            builder(r#"
                module Top(input logic clk, input logic a, input logic b, output logic y, output logic z);
                    import "DPI-C" function logic logic_not(input logic x);
                    import "DPI-C" function bit bit_and(input bit x, input bit y);
                    always_ff @(posedge clk) begin
                        y <= logic_not(a);
                        z <= bit_and(a, b);
                    end
                endmodule
            "#)
            .dpi_function("logic_not", dpi_logic_not as *const ())
            .dpi_function("bit_and", dpi_bit_and as *const ())
        };
        let clk = sim.event("clk");
        let (a, b, y, z) = (sim.signal("a"), sim.signal("b"), sim.signal("y"), sim.signal("z"));
        sim.modify(|io| {
            io.set(a, 1u8);
            io.set(b, 1u8);
        })
        .unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(y), BigUint::from(0u8));
        assert_eq!(sim.get(z), BigUint::from(1u8));
        sim.modify(|io| io.set(a, 0u8)).unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(y), BigUint::from(1u8));
        assert_eq!(sim.get(z), BigUint::from(0u8));
    }

    fn c_name_links_by_its_own_symbol(sim) {
        @build unsafe {
            builder(r#"
                module Top(input logic clk, input int a, output int y);
                    import "DPI-C" c_sum3 = function int sum3(input int x, input int y, input int z);
                    always_ff @(posedge clk) y <= sum3(a, a, 1);
                endmodule
            "#)
            .dpi_function("c_sum3", dpi_sum3 as *const ())
        };
        let clk = sim.event("clk");
        let (a, y) = (sim.signal("a"), sim.signal("y"));
        sim.modify(|io| io.set(a, 20u32)).unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(y), BigUint::from(41u32));
    }

    fn arguments_beyond_the_argument_registers(sim) {
        @build unsafe {
            builder(r#"
                module Top(input logic clk, input int a, output int y);
                    import "DPI-C" function int sum10(
                        input int a, input int b, input int c, input int d, input int e,
                        input int f, input int g, input int h, input int i, input int j
                    );
                    always_ff @(posedge clk)
                        y <= sum10(a, a + 1, a + 2, a + 3, a + 4, a + 5, a + 6, a + 7, a + 8, a + 9);
                endmodule
            "#)
            .dpi_function("sum10", dpi_sum10 as *const ())
        };
        let clk = sim.event("clk");
        let (a, y) = (sim.signal("a"), sim.signal("y"));
        sim.modify(|io| io.set(a, 1u32)).unwrap();
        sim.tick(clk).unwrap();
        // sum of k * (k) for k = 1..=10
        assert_eq!(sim.get(y), BigUint::from(385u32));
    }

    fn call_inside_a_user_function(sim) {
        @build unsafe {
            builder(r#"
                module Top(input logic clk, input int a, output int y);
                    import "DPI-C" pure function int dpi_add(input int x, input int y);
                    function automatic int twice_plus(input int v, input int k);
                        return dpi_add(v, v) + k;
                    endfunction
                    always_ff @(posedge clk) y <= twice_plus(a, 3);
                endmodule
            "#)
            .dpi_function("dpi_add", dpi_add as *const ())
        };
        let clk = sim.event("clk");
        let (a, y) = (sim.signal("a"), sim.signal("y"));
        sim.modify(|io| io.set(a, 7u32)).unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(y), BigUint::from(17u32));
    }
}

/// A void import called as a statement runs once per clock edge, in program
/// order, even though the process writes no signal.
macro_rules! void_call_test {
    ($name:ident, $tag:literal, $record:ident, $build:ident $(, $method:ident($($arg:expr),*))*) => {
        #[test]
        fn $name() {
            let source = r#"
                module Top(input logic clk, input int a);
                    import "DPI-C" function void record(input int value);
                    always_ff @(posedge clk) begin
                        record(a);
                        record(a + 100);
                    end
                endmodule
            "#;
            let mut sim = unsafe { builder(source).dpi_function("record", $record as *const ()) }
                $(.$method($($arg),*))*
                .$build()
                .unwrap();
            let clk = sim.event("clk");
            let a = sim.signal("a");
            sim.modify(|io| io.set(a, 1u32)).unwrap();
            sim.tick(clk).unwrap();
            sim.modify(|io| io.set(a, 2u32)).unwrap();
            sim.tick(clk).unwrap();
            assert_eq!(recorded($tag), vec![1, 101, 2, 102]);
        }
    };
}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
void_call_test!(void_calls_native, "native", record_native, build_native);
void_call_test!(
    void_calls_cranelift,
    "cranelift",
    record_cranelift,
    build_cranelift
);
void_call_test!(
    void_calls_interpreter,
    "interpreter",
    record_interpreter,
    build_interpreter
);
void_call_test!(void_calls_wasm, "wasm", record_wasm, build_wasm);
void_call_test!(
    void_calls_cranelift_parallel,
    "cranelift_parallel",
    record_cranelift_parallel,
    build_cranelift,
    threads(4),
    parallel_partition(celox::ParallelPartition::Always)
);

// An actual evaluated before one that formats a string keeps its value
// across the formatting host request.
#[test]
fn actuals_survive_a_later_formatting_request() {
    let source = r#"
        module Top(output int result);
            import "DPI-C" function int digits(input int first, input int second);
            function automatic int formatted_length(input int v);
                string s = $sformatf("%0d", v);
                return s.len();
            endfunction
            int a = 7;
            initial result = digits(a, formatted_length(123));
        endmodule
    "#;
    let builder = || unsafe {
        celox::Simulation::from_sv_sources(vec![(source, Path::new("dpi.sv"))], "Top")
            .dpi_function("digits", dpi_digits as *const ())
    };
    macro_rules! check {
        ($build:ident) => {{
            let mut sim = builder().$build().unwrap();
            sim.run_until(0).unwrap();
            let result = sim.signal("result");
            assert_eq!(sim.get(result), BigUint::from(73u32), stringify!($build));
        }};
    }
    check!(build_cranelift);
    check!(build_interpreter);
    check!(build_wasm);
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    check!(build_native);
}

#[test]
fn logic_arguments_keep_unknown_bits_in_four_state_simulation() {
    let source = r#"
        module Top(input logic clk, input logic a, output logic y);
            import "DPI-C" function logic logic_not(input logic x);
            always_ff @(posedge clk) y <= logic_not(a);
        endmodule
    "#;
    let mut sim = unsafe { builder(source).dpi_function("logic_not", dpi_logic_not as *const ()) }
        .four_state(true)
        .build_cranelift()
        .unwrap();
    let clk = sim.event("clk");
    let (a, y) = (sim.signal("a"), sim.signal("y"));
    sim.modify(|io| io.set_four_state(a, BigUint::from(1u8), BigUint::from(1u8)))
        .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(
        sim.get_four_state(y),
        (BigUint::from(1u8), BigUint::from(1u8))
    );
    sim.modify(|io| io.set_four_state(a, BigUint::from(0u8), BigUint::from(0u8)))
        .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(
        sim.get_four_state(y),
        (BigUint::from(1u8), BigUint::from(0u8))
    );
}

#[cfg(target_os = "linux")]
#[test]
fn links_functions_from_a_shared_library() {
    let source = r#"
        module Top(input logic clk, input int a, output int y);
            import "DPI-C" function int abs(input int x);
            always_ff @(posedge clk) y <= abs(a);
        endmodule
    "#;
    // SAFETY: libc's `abs` is `int abs(int)`.
    let mut sim = unsafe { builder(source).dpi_library("libc.so.6") }
        .build_cranelift()
        .unwrap();
    let clk = sim.event("clk");
    let (a, y) = (sim.signal("a"), sim.signal("y"));
    sim.modify(|io| io.set(a, (-12i32) as u32)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(y), BigUint::from(12u32));
}

extern "C" fn dpi_sub(x: i32, y: i32) -> i32 {
    x.wrapping_sub(y)
}

#[test]
fn registering_a_name_again_replaces_the_function() {
    let source = r#"
        module Top(input logic clk, input int a, input int b, output int y);
            import "DPI-C" function int combine(input int x, input int y);
            always_ff @(posedge clk) y <= combine(a, b);
        endmodule
    "#;
    let mut sim = unsafe {
        builder(source)
            .dpi_function("combine", dpi_add as *const ())
            .dpi_function("combine", dpi_sub as *const ())
    }
    .build_cranelift()
    .unwrap();
    let clk = sim.event("clk");
    let (a, b, y) = (sim.signal("a"), sim.signal("b"), sim.signal("y"));
    sim.modify(|io| {
        io.set(a, 10u32);
        io.set(b, 3u32);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(y), BigUint::from(7u32));
}

#[test]
fn unresolved_function_is_a_build_error() {
    let source = r#"
        module Top(input logic clk, input int a, output int y);
            initial $exit;
            import "DPI-C" function int missing_dpi_function(input int x);
            always_ff @(posedge clk) y <= missing_dpi_function(a);
        endmodule
    "#;
    let error = match builder(source).build_cranelift() {
        Ok(_) => panic!("design with an unresolved DPI-C import built"),
        Err(error) => error,
    };
    assert!(
        matches!(error.kind(), SimulatorErrorKind::Dpi(_)),
        "unexpected error: {error}"
    );
    assert!(
        error.to_string().contains("missing_dpi_function"),
        "{error}"
    );
    let warnings: Vec<_> = error
        .warnings()
        .iter()
        .filter_map(|warning| match warning {
            celox::CompilationWarning::SystemVerilog(warning) => Some(warning),
            _ => None,
        })
        .collect();
    assert_eq!(warnings.len(), 1);
    assert_eq!(
        warnings[0].kind,
        celox::SvFrontendWarningKind::ExitOutsideProgram
    );
    let location = warnings[0].location.as_ref().unwrap();
    assert_eq!((location.path.as_str(), location.line), ("dpi.sv", 3));
}

extern "C" fn dpi_twice(x: i32) -> i32 {
    x.wrapping_mul(2)
}

dpi_backends! {
    fn imports_declared_in_a_package(sim) {
        @build unsafe {
            builder(r#"
                package p;
                    import "DPI-C" function int twice(input int x);
                endpackage
                module Top(input logic clk, input int a, output int y, output int z);
                    import p::*;
                    always_ff @(posedge clk) begin
                        y <= twice(a);
                        z <= p::twice(a + 1);
                    end
                endmodule
            "#)
            .dpi_function("twice", dpi_twice as *const ())
        };
        let clk = sim.event("clk");
        let (a, y, z) = (sim.signal("a"), sim.signal("y"), sim.signal("z"));
        sim.modify(|io| io.set(a, 20u32)).unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(y), BigUint::from(40u32));
        assert_eq!(sim.get(z), BigUint::from(42u32));
    }
}

#[test]
fn casts_of_results_use_the_declared_signedness() {
    let source = r#"
        module Top(input logic clk, input logic [7:0] a, output logic [31:0] y);
            import "DPI-C" function byte negate(input byte x);
            always_ff @(posedge clk) y <= int'(negate(a));
        endmodule
    "#;
    let mut sim = unsafe { builder(source).dpi_function("negate", dpi_negate_byte as *const ()) }
        .build_cranelift()
        .unwrap();
    let clk = sim.event("clk");
    let (a, y) = (sim.signal("a"), sim.signal("y"));
    sim.modify(|io| io.set(a, 5u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(y), BigUint::from(0xffff_fffbu32));
}

#[test]
fn imports_of_one_c_name_must_share_a_prototype() {
    let source = r#"
        module Child(input logic clk, input int a, output int y);
            import "DPI-C" function int dpi_add(input int x, input int y);
            always_ff @(posedge clk) y <= dpi_add(a, a);
        endmodule
        module Top(input logic clk, input int a, output int y, output int z);
            import "DPI-C" function longint dpi_add(input int x, input int y);
            Child u(.clk(clk), .a(a), .y(y));
            always_ff @(posedge clk) z <= int'(dpi_add(a, 1));
        endmodule
    "#;
    let error = match unsafe { builder(source).dpi_function("dpi_add", dpi_add as *const ()) }
        .build_cranelift()
    {
        Ok(_) => panic!("conflicting DPI-C prototypes built"),
        Err(error) => error.to_string(),
    };
    assert!(
        error.contains("function int dpi_add(input int, input int)")
            && error.contains("function longint dpi_add(input int, input int)"),
        "{error}"
    );
}

/// A generate block that imports a DPI-C function hides a constant function of
/// the same name from the module, so a constant expression of the block
/// cannot call either (#1206).
#[test]
fn generate_block_dpi_imports_hide_outer_constant_functions() {
    for outer in [
        "function automatic int f(int a); return 99; endfunction",
        "import q::*;",
    ] {
        let source = format!(
            r#"package p; import "DPI-C" function int f(input int a); endpackage
            package q; function automatic int f(int a); return 99; endfunction endpackage
            module Top(output int y);
                {outer}
                int a;
                if (1) begin : g
                    import p::*;
                    localparam int Y = f(1);
                    assign a = Y;
                end
                assign y = a + f(0);
            endmodule"#
        );
        let error = build_error(&source);
        assert!(error.contains("user constant function call"), "{error}");
    }
}

fn build_error(source: &str) -> String {
    match Simulator::from_sv_sources(vec![(source, Path::new("dpi.sv"))], "Top").build_cranelift() {
        Ok(_) => panic!("design unexpectedly compiled:\n{source}"),
        // The rendered diagnostic wraps long messages behind `│` gutters.
        Err(error) => error
            .to_string()
            .split_whitespace()
            .filter(|word| *word != "│")
            .collect::<Vec<_>>()
            .join(" "),
    }
}

#[test]
fn rejects_unsupported_imports_and_calls() {
    let cases = [
        (
            r#"module Top(input int a, output int y);
                import "DPI-C" function int f(input int x);
                assign y = f(a);
            endmodule"#,
            "called in combinational logic",
        ),
        (
            r#"module Top(input int a, output int y);
                import "DPI-C" function int f(input int x);
                always_comb y = f(a);
            endmodule"#,
            "called in combinational logic",
        ),
        (
            r#"module Top(input int a, output int y);
                import "DPI-C" function void f(input int x);
                always_comb f(a);
                assign y = a;
            endmodule"#,
            "DPI-C function `f` called in combinational logic",
        ),
        (
            r#"module Top(input int a, output int y);
                import "DPI-C" function void f(input int x);
                function automatic int g(input int v); f(v); return v; endfunction
                assign y = g(a);
            endmodule"#,
            "DPI-C function `f` called in combinational logic",
        ),
        (
            r#"module Top(input logic clk, input int a, output int y);
                import "DPI-C" function void f(input int x, ref int r);
                always_ff @(posedge clk) f(a, y);
            endmodule"#,
            "DPI-C ref argument",
        ),
        (
            r#"module Top(input logic clk, input int a, output int y);
                import "DPI-C" pure function int f(input int x, output int r);
                always_ff @(posedge clk) y <= f(a, y);
            endmodule"#,
            "DPI-C pure import with an output or inout argument",
        ),
        (
            r#"module Top(input logic clk, input int a, output int y);
                import "DPI-C" context function int f(input int x);
                always_ff @(posedge clk) y <= f(a);
            endmodule"#,
            "DPI-C context import",
        ),
        (
            r#"module Top(input logic clk, input int a);
                import "DPI-C" task f(input int x);
                always_ff @(posedge clk) f(a);
            endmodule"#,
            "DPI-C task import",
        ),
        (
            r#"module Top(input logic clk, input logic [7:0] a, output logic [7:0] y);
                import "DPI-C" function bit [7:0] f(input bit [7:0] x);
                always_ff @(posedge clk) y <= f(a);
            endmodule"#,
            "DPI-C packed array result",
        ),
        (
            r#"module Top(input logic clk, input int a, output int y);
                import "DPI-C" function int f(input real x);
                always_ff @(posedge clk) y <= f(a);
            endmodule"#,
            "DPI-C argument of type `real`",
        ),
    ];
    for (source, expected) in cases {
        let message = build_error(source);
        assert!(
            message.contains(expected),
            "expected `{expected}` in:\n{message}"
        );
    }
}

static QUERY_DPI_CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

extern "C" fn dpi_query_value() -> i32 {
    QUERY_DPI_CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    42
}

dpi_backends! {
    fn unpacked_dimensions_queries_local_import_without_calling_it(sim) {
        @build unsafe {
            builder(r#"
                module Top(output int constant_rank, runtime_rank);
                    import "DPI-C" function int value();
                    localparam N = $unpacked_dimensions(value());
                    assign constant_rank = N;
                    assign runtime_rank = $unpacked_dimensions(value());
                endmodule
            "#)
            .dpi_function("value", dpi_query_value as *const ())
        };
        sim.eval_comb().unwrap();
        for name in ["constant_rank", "runtime_rank"] {
            assert_eq!(sim.get(sim.signal(name)), BigUint::from(0u32));
        }
        assert_eq!(QUERY_DPI_CALLS.load(std::sync::atomic::Ordering::SeqCst), 0);
    }
}

static CHANDLE_TARGET: [i32; 2] = [17, 29];

/// A `chandle` is a C pointer (IEEE 1800-2023 6.14): one function returns
/// one, another dereferences it.
extern "C" fn dpi_element_handle(index: i32) -> *const i32 {
    if index < 0 {
        std::ptr::null()
    } else {
        &CHANDLE_TARGET[index as usize]
    }
}

extern "C" fn dpi_read_handle(handle: *const i32) -> i32 {
    if handle.is_null() {
        -1
    } else {
        // SAFETY: the handle comes from `dpi_element_handle`.
        unsafe { *handle }
    }
}

dpi_backends! {
    fn chandles_pass_c_pointers_through(sim) {
        @build unsafe {
            builder(r#"
                module Top(input logic clk, input int index, output int value, output bit is_null);
                    import "DPI-C" function chandle element(input int index);
                    import "DPI-C" function int read(input chandle handle);
                    chandle handle;
                    always_ff @(posedge clk) begin
                        handle <= element(index);
                        value <= read(handle);
                        is_null <= handle == null;
                    end
                endmodule
            "#)
            .dpi_function("element", dpi_element_handle as *const ())
            .dpi_function("read", dpi_read_handle as *const ())
        };
        let clk = sim.event("clk");
        let (index, value, is_null) = (sim.signal("index"), sim.signal("value"), sim.signal("is_null"));
        sim.modify(|io| io.set(index, 1u32)).unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(value), BigUint::from(0xffff_ffffu32));
        assert_eq!(sim.get(is_null), BigUint::from(1u32));
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(value), BigUint::from(29u32));
        assert_eq!(sim.get(is_null), BigUint::from(0u32));
        sim.modify(|io| io.set(index, 0xffff_ffffu32)).unwrap();
        sim.tick(clk).unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(value), BigUint::from(0xffff_ffffu32));
        assert_eq!(sim.get(is_null), BigUint::from(1u32));
    }
}

/// `flash_read` of a memory model: the data comes back through a pointer
/// (IEEE 1800-2023 35.5.1.2, H.8.8).
extern "C" fn dpi_read(addr: i32, data: *mut i64) {
    // SAFETY: an `output longint` is a pointer to a `long long`.
    unsafe { *data = 0x1122_3344_5566_7700 + i64::from(addr) }
}

extern "C" fn dpi_bump(count: *mut i32) -> i32 {
    // SAFETY: an `inout int` is a pointer to an `int` holding the actual.
    unsafe {
        *count += 3;
        *count * 2
    }
}

extern "C" fn dpi_scalars(b: *mut u8, l: *mut u8, by: *mut i8, sh: *mut i16, handle: *mut usize) {
    // SAFETY: each output is a pointer to the C type of its formal.
    unsafe {
        *b = 1;
        // `sv_x`.
        *l = 3;
        *by = -3;
        *sh = -300;
        *handle = 0x1234_5678_9abc;
    }
}

dpi_backends! {
    fn output_and_inout_arguments_are_copied_out(sim) {
        @build unsafe {
            builder(r#"
                import "DPI-C" function void read(input int addr, output longint data);
                module Top(
                    input logic clk, input int a,
                    output logic [63:0] data, output int count, doubled,
                    output bit b, output logic l, output byte by, output shortint sh,
                    output bit handle_set, output logic [15:0] low, output int element
                );
                    import "DPI-C" function int bump(inout int count);
                    import "DPI-C" function void scalars(
                        output bit b, output logic l, output byte by, output shortint sh,
                        output chandle handle);
                    chandle handle;
                    int elements [3];
                    logic [47:0] high;
                    always @(posedge clk) begin
                        read(a, data);
                        doubled = bump(count);
                        scalars(b, l, by, sh, handle);
                        handle_set = handle != null;
                        // A concatenation and an element are assigned as by
                        // an assignment from the formal.
                        read(a + 1, {high, low});
                        read(a, elements[a[0]]);
                        element = elements[1];
                    end
                endmodule
            "#)
            .dpi_function("read", dpi_read as *const ())
            .dpi_function("bump", dpi_bump as *const ())
            .dpi_function("scalars", dpi_scalars as *const ())
        };
        let clk = sim.event("clk");
        let a = sim.signal("a");
        macro_rules! get {
            ($name:literal) => {
                sim.get(sim.signal($name))
            };
        }
        sim.modify(|io| io.set(a, 5u32)).unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(get!("data"), BigUint::from(0x1122_3344_5566_7705u64));
        assert_eq!(get!("count"), BigUint::from(3u32));
        assert_eq!(get!("doubled"), BigUint::from(6u32));
        assert_eq!(get!("b"), BigUint::from(1u32));
        assert_eq!(get!("by"), BigUint::from(0xfdu32));
        assert_eq!(get!("sh"), BigUint::from(0xfed4u32));
        assert_eq!(get!("handle_set"), BigUint::from(1u32));
        assert_eq!(get!("low"), BigUint::from(0x7706u32));
        assert_eq!(get!("element"), BigUint::from(0x5566_7705u32));
        sim.tick(clk).unwrap();
        assert_eq!(get!("count"), BigUint::from(6u32));
        assert_eq!(get!("doubled"), BigUint::from(12u32));
    }
}

/// A packed array of `bit` is an array of `svBitVecVal` (`uint32_t`) words,
/// the least significant first (IEEE 1800-2023 H.7.7).
extern "C" fn dpi_reverse_words(write: u8, input: *const u32, output: *mut u32) {
    // SAFETY: both formals are `bit [159:0]`, five words each.
    unsafe {
        for index in 0..5 {
            let word = *input.add(4 - index);
            *output.add(index) = if write != 0 { !word } else { word };
        }
    }
}

/// A packed array of `logic` is an array of `svLogicVecVal`: `aval` then
/// `bval` for each 32 bits.
extern "C" fn dpi_swap_planes(input: *const [u32; 2], output: *mut [u32; 2]) {
    // SAFETY: both formals are `logic [70:0]`, three words each.
    unsafe {
        for index in 0..3 {
            let [aval, bval] = *input.add(index);
            *output.add(index) = [bval, aval];
        }
    }
}

/// `integer` is one `svLogicVecVal`.
extern "C" fn dpi_swap_integer(input: *const [u32; 2], output: *mut [u32; 2]) {
    // SAFETY: both formals are `integer`, one word each.
    unsafe {
        let [aval, bval] = *input;
        *output = [bval, aval];
    }
}

const PACKED_ARRAYS: &str = r#"
    module Top #(parameter int W = 160) (
        input logic clk, input bit write, input bit [W-1:0] wdata, input logic [70:0] planes,
        output bit [W-1:0] rdata, output logic [70:0] swapped, output integer narrow
    );
        import "DPI-C" function void reverse_words(
            input bit write, input bit [W-1:0] wdata, output bit [W-1:0] rdata);
        import "DPI-C" function void swap_planes(input logic [70:0] in, output logic [70:0] out);
        import "DPI-C" function void swap_integer(input integer in, output integer out);
        always @(posedge clk) begin
            reverse_words(write, wdata, rdata);
            swap_planes(planes, swapped);
            swap_integer(planes[31:0], narrow);
        end
    endmodule
"#;

fn packed_arrays() -> SimulatorBuilder<'static> {
    // SAFETY: the functions have the prototypes of their imports.
    unsafe {
        builder(PACKED_ARRAYS)
            .dpi_function("reverse_words", dpi_reverse_words as *const ())
            .dpi_function("swap_planes", dpi_swap_planes as *const ())
            .dpi_function("swap_integer", dpi_swap_integer as *const ())
    }
}

/// `words`, least significant first, as one value.
fn from_words(words: &[u32]) -> BigUint {
    words.iter().rev().fold(BigUint::from(0u8), |value, word| {
        (value << 32) | BigUint::from(*word)
    })
}

dpi_backends! {
    fn packed_arrays_are_passed_as_canonical_words(sim) {
        @build packed_arrays();
        let clk = sim.event("clk");
        let (write, wdata, planes) = (sim.signal("write"), sim.signal("wdata"), sim.signal("planes"));
        let words = [0x1000_0001u32, 0x2000_0002, 0x3000_0003, 0x4000_0004, 0x5000_0005];
        sim.modify(|io| {
            io.set_wide(wdata, from_words(&words));
            io.set_wide(planes, BigUint::from(0x12_3456_789a_bcdeu64));
        })
        .unwrap();
        sim.tick(clk).unwrap();
        let mut reversed = words;
        reversed.reverse();
        assert_eq!(sim.get(sim.signal("rdata")), from_words(&reversed));
        // Two-state simulation: the unknown bits the function gives read as
        // zero, and it was given none.
        assert_eq!(sim.get(sim.signal("swapped")), BigUint::from(0u8));
        assert_eq!(sim.get(sim.signal("narrow")), BigUint::from(0u8));
        sim.modify(|io| io.set(write, 1u8)).unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(
            sim.get(sim.signal("rdata")),
            from_words(&reversed.map(|word| !word))
        );
    }
}

#[test]
fn logic_packed_arrays_keep_both_planes_in_four_state_simulation() {
    macro_rules! check {
        ($build:ident) => {{
            let mut sim = packed_arrays().four_state(true).$build().unwrap();
            let clk = sim.event("clk");
            let planes = sim.signal("planes");
            // 71 bits: the top word holds 7 of them.
            let value: BigUint =
                (BigUint::from(0x55u8) << 64u32) | BigUint::from(0x0123_4567_89ab_cdefu64);
            let mask: BigUint =
                (BigUint::from(0x41u8) << 64u32) | BigUint::from(0xf000_0000_0000_00f0u64);
            sim.modify(|io| io.set_four_state(planes, value.clone(), mask.clone()))
                .unwrap();
            sim.tick(clk).unwrap();
            assert_eq!(
                sim.get_four_state(sim.signal("swapped")),
                (mask.clone(), value.clone()),
                stringify!($build)
            );
            let low = BigUint::from(0xffff_ffffu32);
            assert_eq!(
                sim.get_four_state(sim.signal("narrow")),
                (&mask & &low, &value & &low),
                stringify!($build)
            );
        }};
    }
    check!(build_cranelift);
    check!(build_interpreter);
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    check!(build_native);
}

extern "C" fn dpi_put(target: *mut i32, value: i32) {
    // SAFETY: an `output int` is a pointer to an `int`.
    unsafe { *target = value }
}

// An output or inout actual is the place it names when the call starts
// (IEEE 1800-2023 13.5): its selectors are evaluated once, before the
// actuals after it, and an inout is read from and written to that place.
#[test]
fn output_actuals_are_captured_before_later_actuals() {
    let source = r#"
        module Top(output int first, second, third, calls);
            import "DPI-C" function int bump(inout int count);
            import "DPI-C" function void put(output int target, input int value);
            int elements [3] = '{10, 20, 30};
            int index = 0;
            int selected = 0;
            int ignored;
            function automatic int next();
                selected++;
                return selected;
            endfunction
            function automatic int advance();
                index++;
                return 100 + index;
            endfunction
            initial begin
                ignored = bump(elements[next()]);
                put(elements[index], advance());
                first = elements[0];
                second = elements[1];
                third = elements[2];
                calls = selected;
            end
        endmodule
    "#;
    let builder = || unsafe {
        celox::Simulation::from_sv_sources(vec![(source, Path::new("dpi.sv"))], "Top")
            .dpi_function("bump", dpi_bump as *const ())
            .dpi_function("put", dpi_put as *const ())
    };
    macro_rules! check {
        ($build:ident) => {{
            let mut sim = builder().$build().unwrap();
            sim.run_until(0).unwrap();
            let values: Vec<_> = ["first", "second", "third", "calls"]
                .map(|name| sim.get(sim.signal(name)))
                .to_vec();
            let expected: Vec<BigUint> = [101u32, 23, 30, 1].map(BigUint::from).to_vec();
            assert_eq!(values, expected, stringify!($build));
        }};
    }
    check!(build_cranelift);
    check!(build_interpreter);
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    check!(build_native);
}

/// The strings `dpi_upper` and `dpi_append` gave back. A C function keeps
/// its result valid after it returns, in a static buffer for instance; these
/// stay for the rest of the test run.
static RETURNED: Mutex<Vec<std::ffi::CString>> = Mutex::new(Vec::new());

fn returned(bytes: Vec<u8>) -> *const std::ffi::c_char {
    let string = std::ffi::CString::new(bytes).unwrap();
    // The characters are on the heap and do not move with the string.
    let pointer = string.as_ptr();
    RETURNED.lock().unwrap().push(string);
    pointer
}

extern "C" fn dpi_upper(text: *const std::ffi::c_char) -> *const std::ffi::c_char {
    // SAFETY: an `input string` is a NUL-terminated C string.
    returned(
        unsafe { std::ffi::CStr::from_ptr(text) }
            .to_bytes()
            .to_ascii_uppercase(),
    )
}

extern "C" fn dpi_length(text: *const std::ffi::c_char) -> i32 {
    // SAFETY: an `input string` is a NUL-terminated C string.
    unsafe { std::ffi::CStr::from_ptr(text) }.to_bytes().len() as i32
}

extern "C" fn dpi_describe(value: i32, text: *mut *const std::ffi::c_char) {
    let described = if value < 0 { c"negative" } else { c"" };
    // SAFETY: an `output string` is a pointer to a `const char *`.
    unsafe {
        *text = if value == 0 {
            std::ptr::null()
        } else {
            described.as_ptr()
        }
    }
}

extern "C" fn dpi_append(text: *mut *const std::ffi::c_char) {
    // SAFETY: an `inout string` points at the C string of the actual.
    let bytes = unsafe { std::ffi::CStr::from_ptr(*text) }.to_bytes();
    let appended = returned([bytes, b"!"].concat());
    // SAFETY: as above.
    unsafe { *text = appended }
}

// Strings are `const char *` (IEEE 1800-2023 35.5.6, H.7.4): the function
// reads a copy of the actual, and what it gives back is copied at once.
#[test]
fn strings_are_passed_as_c_strings() {
    let source = r#"
        import "DPI-C" function string upper(input string s);
        module Top(output int lengths, output bit [4:0] checks);
            import "DPI-C" function int length(input string s);
            import "DPI-C" function void describe(input int value, output string s);
            import "DPI-C" function void append(inout string s);
            string greeting = "Hello World";
            string described, both;
            initial begin
                greeting = upper(greeting);
                checks[0] = greeting == "HELLO WORLD";
                // Two strings live in one call, and a literal actual.
                lengths = length({greeting, upper("abc")}) * 100 + length("") * 10 + length("xy");
                both = {upper("a"), upper("b")};
                checks[1] = both == "AB";
                describe(-1, described);
                checks[2] = described == "negative";
                // A null pointer is the empty string.
                describe(0, described);
                checks[3] = described.len() == 0;
                append(greeting);
                checks[4] = greeting == "HELLO WORLD!";
            end
        endmodule
    "#;
    let builder = |four_state| unsafe {
        celox::Simulation::from_sv_sources(vec![(source, Path::new("dpi.sv"))], "Top")
            .dpi_function("upper", dpi_upper as *const ())
            .dpi_function("length", dpi_length as *const ())
            .dpi_function("describe", dpi_describe as *const ())
            .dpi_function("append", dpi_append as *const ())
            .four_state(four_state)
    };
    macro_rules! check {
        ($build:ident) => {{
            for four_state in [false, true] {
                let mut sim = builder(four_state).$build().unwrap();
                sim.run_until(0).unwrap();
                let (lengths, checks) = (sim.signal("lengths"), sim.signal("checks"));
                assert_eq!(sim.get(lengths), BigUint::from(1402u32), stringify!($build));
                assert_eq!(sim.get(checks), BigUint::from(0x1fu32), stringify!($build));
            }
        }};
    }
    check!(build_cranelift);
    check!(build_interpreter);
    check!(build_wasm);
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    check!(build_native);
}

// An import declared at compilation-unit scope is visible in every module of
// the unit, and a module may import the same function again (IEEE 1800-2023
// 35.2.1, 35.5.4).
dpi_backends! {
    fn compilation_unit_imports_are_visible_in_modules(sim) {
        @build unsafe {
            builder(r#"
                import "DPI-C" function int add(input int x, input int y);
                module Sub(input logic clk, input int a, output int y);
                    always @(posedge clk) y <= add(a, 1);
                endmodule
                module Top(input logic clk, input int a, output int y, z);
                    import "DPI-C" function int add(input int x, input int y);
                    Sub sub(.clk, .a, .y);
                    always_ff @(posedge clk) z <= add(a, a);
                endmodule
            "#)
            .dpi_function("add", dpi_add as *const ())
        };
        let clk = sim.event("clk");
        let a = sim.signal("a");
        sim.modify(|io| io.set(a, 20u32)).unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(sim.signal("y")), BigUint::from(21u32));
        assert_eq!(sim.get(sim.signal("z")), BigUint::from(40u32));
    }
}
