use celox::Simulator;

const NESTED_WRITE: &str = r#"
pub module AdcGroupNestedWriteMre (
    i_primary       : input  logic [8],
    i_overflow      : input  logic [8],
    i_primary_active: input  logic [2],
    o_pop           : output logic [8],
) {
    always_comb {
        for bank in 0..2 {
            for lane in 0..4 {
                let index: u32 = bank * 4 + lane;
                o_pop[index] = 0;
                if i_primary[index] {
                    o_pop[index] = 1;
                }
                if i_overflow[index] && !i_primary_active[bank] {
                    o_pop[index] = 1;
                }
            }
        }
    }
}
"#;

#[test]
fn nested_loop_partial_writes_compile_and_simulate() {
    let mut sim = Simulator::builder(NESTED_WRITE, "AdcGroupNestedWriteMre")
        .build()
        .expect("nested partial writes should compile");
    let i_primary = sim.signal("i_primary");
    let i_overflow = sim.signal("i_overflow");
    let i_primary_active = sim.signal("i_primary_active");
    let o_pop = sim.signal("o_pop");

    sim.modify(|io| {
        io.set(i_primary, 0b0100_0001u8);
        io.set(i_overflow, 0b1010_1010u8);
        io.set(i_primary_active, 0b01u8);
    })
    .unwrap();

    // Bank 0 is active, so only its primary request pops. Bank 1 admits
    // overflow requests in addition to its primary request.
    assert_eq!(sim.get(o_pop), 0b1110_0001u8.into());
}

fn deep_nested_write(width: usize) -> String {
    let (banks, lanes) = if width == 257 { (1, 257) } else { (2, 256) };
    NESTED_WRITE
        .replace("logic [8]", &format!("logic [{width}]"))
        .replace("0..2", &format!("0..{banks}"))
        .replace("0..4", &format!("0..{lanes}"))
        .replace("bank * 4", &format!("bank * {lanes}"))
}

macro_rules! deep_complete_regression {
    ($name:ident, $build:ident, $width:expr, $four_state:expr) => {
        #[test]
        fn $name() {
            let width = $width;
            let four_state = $four_state;
            let source = deep_nested_write(width);
            let mut sim = Simulator::builder(&source, "AdcGroupNestedWriteMre")
                .four_state(four_state)
                .$build()
                .expect("deep fully assigned writes must compile");
            let primary = sim.signal("i_primary");
            let overflow = sim.signal("i_overflow");
            let active = sim.signal("i_primary_active");
            let out = sim.signal("o_pop");
            let requests = (celox::BigUint::from(1u8) << (width - 1)) | celox::BigUint::from(1u8);
            let extra = celox::BigUint::from(1u8) << (width / 2);
            sim.modify(|io| {
                io.set_wide(primary, requests.clone());
                io.set_wide(overflow, extra.clone());
                io.set(active, 0u8);
            })
            .unwrap();
            assert_eq!(sim.get(out), requests | extra);
            sim.modify(|io| {
                io.set(primary, 0u8);
                io.set(overflow, 0u8);
            })
            .unwrap();
            assert_eq!(sim.get(out), celox::BigUint::from(0u8));
        }
    };
}
macro_rules! deep_incomplete_regression {
    ($name:ident, $build:ident, $four_state:expr) => {
        #[test]
        fn $name() {
            use celox::{DiagnosticLevel, DiagnosticPolicy};
            use miette::Diagnostic;
            let four_state = $four_state;
            let source = deep_nested_write(512).replace("0..256", "0..255");
            let mut policy = DiagnosticPolicy::default();
            policy.set("inferred_latch", DiagnosticLevel::Warn);
            let mut sim = Simulator::builder(&source, "AdcGroupNestedWriteMre")
                .diagnostic_policy(policy)
                .four_state(four_state)
                .$build()
                .unwrap();
            assert!(sim.warnings().iter().any(|warning| {
                warning
                    .code()
                    .is_some_and(|code| code.to_string() == "inferred_latch")
            }));
            let primary = sim.signal("i_primary");
            let out = sim.signal("o_pop");
            let overflow = sim.signal("i_overflow");
            let active = sim.signal("i_primary_active");
            // Unwritten bank tails retain their externally seeded values across activations.
            let retained =
                (celox::BigUint::from(1u8) << 511usize) | (celox::BigUint::from(1u8) << 255usize);
            sim.modify(|io| {
                io.set_wide(out, retained.clone());
                io.set(primary, 1u8);
                io.set(overflow, 0u8);
                io.set(active, 0u8);
            })
            .unwrap();
            assert_eq!(sim.get(out), &retained | celox::BigUint::from(1u8));
            sim.modify(|io| {
                io.set(primary, 0u8);
            })
            .unwrap();
            assert_eq!(sim.get(out), retained);
        }
    };
}
deep_complete_regression!(
    deep_per_bit_interpreter_257_2state,
    build_interpreter,
    257,
    false
);
deep_complete_regression!(
    deep_per_bit_interpreter_512_2state,
    build_interpreter,
    512,
    false
);
deep_incomplete_regression!(
    deep_incomplete_per_bit_interpreter_2state,
    build_interpreter,
    false
);
deep_complete_regression!(
    deep_per_bit_interpreter_257_4state,
    build_interpreter,
    257,
    true
);
deep_complete_regression!(
    deep_per_bit_interpreter_512_4state,
    build_interpreter,
    512,
    true
);
deep_incomplete_regression!(
    deep_incomplete_per_bit_interpreter_4state,
    build_interpreter,
    true
);
deep_complete_regression!(deep_per_bit_native_257_2state, build_native, 257, false);
deep_complete_regression!(deep_per_bit_native_512_2state, build_native, 512, false);
deep_incomplete_regression!(deep_incomplete_per_bit_native_2state, build_native, false);
deep_complete_regression!(deep_per_bit_native_257_4state, build_native, 257, true);
deep_complete_regression!(deep_per_bit_native_512_4state, build_native, 512, true);
deep_incomplete_regression!(deep_incomplete_per_bit_native_4state, build_native, true);
deep_complete_regression!(
    deep_per_bit_cranelift_257_2state,
    build_cranelift,
    257,
    false
);
deep_complete_regression!(
    deep_per_bit_cranelift_512_2state,
    build_cranelift,
    512,
    false
);
deep_incomplete_regression!(
    deep_incomplete_per_bit_cranelift_2state,
    build_cranelift,
    false
);
deep_complete_regression!(
    deep_per_bit_cranelift_257_4state,
    build_cranelift,
    257,
    true
);
deep_complete_regression!(
    deep_per_bit_cranelift_512_4state,
    build_cranelift,
    512,
    true
);
deep_incomplete_regression!(
    deep_incomplete_per_bit_cranelift_4state,
    build_cranelift,
    true
);

#[test]
fn deep_incomplete_per_bit_writes_strictly_reject() {
    let source = deep_nested_write(512).replace("0..256", "0..255");
    let error = Simulator::builder(&source, "AdcGroupNestedWriteMre")
        .build_interpreter()
        .expect_err("incomplete deep writes must infer a latch");
    assert!(error.to_string().contains("inferred_latch"), "{error}");
}

fn wide_nested_write(width: usize, incomplete: bool) -> String {
    let words = if width == 257 { 1 } else { 2 };
    let chunk = width / words;
    let banks = if incomplete { words - 1 } else { words };
    let tail = if incomplete {
        format!(
            "if i_primary[{}] != 0 {{ o_pop[{}] = i_primary[{}]; }}",
            words - 1,
            words - 1,
            words - 1
        )
    } else {
        String::new()
    };
    format!(
        r#"
module AdcGroupNestedWriteMre (
    i_primary: input logic<{chunk}>[{words}],
    i_overflow: input logic<{chunk}>[{words}],
    i_primary_active: input logic [2],
    o_pop: output logic<{chunk}>[{words}],
) {{
    always_comb {{
        for bank in 0..{banks} {{
            for lane in 0..1 {{
                let index: u32 = bank + lane;
                o_pop[index] = 0;
                if i_primary[index] != 0 {{
                    o_pop[index] = i_primary[index];
                }}
                if !i_primary_active[bank] {{
                    o_pop[index] = i_primary[index] | i_overflow[index];
                }}
            }}
        }}
        {tail}
    }}
}}
"#
    )
}

#[test]
fn fully_assigned_large_nested_writes_do_not_infer_latches() {
    for width in [257, 512] {
        let source = wide_nested_write(width, false);
        let mut sim = Simulator::builder(&source, "AdcGroupNestedWriteMre")
            .build_interpreter()
            .unwrap_or_else(|error| panic!("fully assigned {width}-bit output: {error}"));
        let primary = sim.signal("i_primary");
        let overflow = sim.signal("i_overflow");
        let active = sim.signal("i_primary_active");
        let out = sim.signal("o_pop");
        let requests = (celox::BigUint::from(1u8) << (width - 1)) | celox::BigUint::from(1u8);
        let extra = celox::BigUint::from(1u8) << (width / 2);
        sim.modify(|io| {
            io.set_wide(primary, requests.clone());
            io.set_wide(overflow, extra.clone());
            io.set(active, 0u8);
        })
        .unwrap();
        assert_eq!(sim.get(out), requests | extra, "{width}-bit writes");
        // A later activation must clear requests that were previously set.
        sim.modify(|io| {
            io.set(primary, 0u8);
            io.set(overflow, 0u8);
        })
        .unwrap();
        assert_eq!(sim.get(out), celox::BigUint::from(0u8));
    }
}

#[test]
fn incomplete_large_nested_writes_still_infer_latches() {
    let source = wide_nested_write(512, true);
    assert_infers_latch(&source);
}

#[test]
fn nested_loop_incomplete_writes_still_infer_latches() {
    // Each bank leaves its last bit untouched. The fixed-loop proof must
    // preserve those old values rather than treating any bounded loop as full.
    let source = NESTED_WRITE.replace("0..4", "0..3");
    assert_infers_latch(&source);
}

#[test]
fn nested_loop_break_preserves_unwritten_bits() {
    let source = NESTED_WRITE.replace("o_pop[index] = 0;", "o_pop[index] = 0; break;");
    assert_infers_latch(&source);
}

#[test]
fn nested_zero_trip_loop_preserves_old_values() {
    // Veryl removes an entirely empty loop. A preceding conditional write
    // leaves a real retained state path that the empty loop cannot complete.
    let source = NESTED_WRITE.replace("0..4", "0..0").replace(
        "for bank in",
        "if i_primary_active[0] { o_pop[0] = 0; } for bank in",
    );
    assert_infers_latch(&source);
}

#[test]
fn nested_runtime_bound_is_not_proven_complete() {
    // A runtime trip count can be zero even when a single iteration writes
    // the complete output. It must not inherit the fixed-loop proof.
    let source = r#"
module AdcGroupNestedWriteMre (
    count: input logic<32>,
    data: input logic<8>,
    out: output logic<8>,
) {
    always_comb {
        for lane in 0..count {
            out = data;
        }
    }
}
"#;
    assert_infers_latch_with_runtime_limit(source, true);
}

fn assert_infers_latch(source: &str) {
    assert_infers_latch_with_runtime_limit(source, false);
}

fn assert_infers_latch_with_runtime_limit(source: &str, runtime_limit: bool) {
    use celox::{DiagnosticLevel, DiagnosticPolicy};
    use miette::Diagnostic;

    let error = Simulator::builder(source, "AdcGroupNestedWriteMre")
        .build()
        .expect_err("incomplete nested writes must fail under the strict default");
    assert!(error.to_string().contains("inferred_latch"), "{error}");

    let mut policy = DiagnosticPolicy::default();
    policy.set("inferred_latch", DiagnosticLevel::Warn);
    let result = Simulator::builder(source, "AdcGroupNestedWriteMre")
        .diagnostic_policy(policy)
        .build();
    // Dynamic bounds also hit an independent upstream combinational-loop
    // limitation. Its error must retain the warning selected by this policy.
    let warnings = match &result {
        Ok(sim) => sim.warnings(),
        Err(error) if runtime_limit => error.warnings(),
        Err(error) => panic!("warn policy should permit retained bits: {error}"),
    };
    assert_eq!(
        warnings
            .iter()
            .filter(|warning| warning
                .code()
                .is_some_and(|code| code.to_string() == "inferred_latch"))
            .count(),
        1,
        "{warnings:?}"
    );
}
