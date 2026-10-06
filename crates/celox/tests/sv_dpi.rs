//! SystemVerilog DPI-C imports: calls from `always_ff` into C functions.

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
    let mut sim = builder(source)
        .dpi_library("libc.so.6")
        .build_cranelift()
        .unwrap();
    let clk = sim.event("clk");
    let (a, y) = (sim.signal("a"), sim.signal("y"));
    sim.modify(|io| io.set(a, (-12i32) as u32)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(y), BigUint::from(12u32));
}

#[test]
fn unresolved_function_is_a_build_error() {
    let source = r#"
        module Top(input logic clk, input int a, output int y);
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
            r#"module Top(input logic clk, input int a, output int y);
                import "DPI-C" function void f(input int x, output int r);
                always_ff @(posedge clk) f(a, y);
            endmodule"#,
            "DPI-C output, inout or ref argument",
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
            r#"module Top(input logic clk, input int a, output int y);
                function int g(input int x); return x; endfunction
                export "DPI-C" function g;
                always_ff @(posedge clk) y <= g(a);
            endmodule"#,
            "DPI-C export",
        ),
        (
            r#"module Top(input logic clk, input logic [7:0] a, output int y);
                import "DPI-C" function int f(input bit [7:0] x);
                always_ff @(posedge clk) y <= f(a);
            endmodule"#,
            "DPI-C packed array argument",
        ),
        (
            r#"module Top(input logic clk, input int a, output int y);
                import "DPI-C" function int f(input integer x);
                always_ff @(posedge clk) y <= f(a);
            endmodule"#,
            "DPI-C argument of four-state integer type",
        ),
        (
            r#"module Top(input logic clk, input int a, output int y);
                import "DPI-C" function int f(input string x);
                always_ff @(posedge clk) y <= f(a);
            endmodule"#,
            "DPI-C argument of type `string`",
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
