use celox::Simulator;

#[path = "test_utils/nested_write.rs"]
mod nested_write;
use nested_write::NESTED_WRITE;

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

#[test]
fn complete_per_bit_interpreter_8_2state() {
    nested_write::complete!(build_interpreter, 8, false);
}

#[test]
fn incomplete_per_bit_interpreter_8_2state() {
    nested_write::incomplete!(build_interpreter, 8, false);
}

#[test]
fn complete_per_bit_interpreter_8_4state() {
    nested_write::complete!(build_interpreter, 8, true);
}

#[test]
fn incomplete_per_bit_interpreter_8_4state() {
    nested_write::incomplete!(build_interpreter, 8, true);
}

#[test]
fn complete_per_bit_native_8_2state() {
    nested_write::complete!(build_native, 8, false);
}

#[test]
fn incomplete_per_bit_native_8_2state() {
    nested_write::incomplete!(build_native, 8, false);
}

#[test]
fn complete_per_bit_native_8_4state() {
    nested_write::complete!(build_native, 8, true);
}

#[test]
fn incomplete_per_bit_native_8_4state() {
    nested_write::incomplete!(build_native, 8, true);
}

#[test]
fn complete_per_bit_cranelift_8_2state() {
    nested_write::complete!(build_cranelift, 8, false);
}

#[test]
fn incomplete_per_bit_cranelift_8_2state() {
    nested_write::incomplete!(build_cranelift, 8, false);
}

#[test]
fn complete_per_bit_cranelift_8_4state() {
    nested_write::complete!(build_cranelift, 8, true);
}

#[test]
fn incomplete_per_bit_cranelift_8_4state() {
    nested_write::incomplete!(build_cranelift, 8, true);
}

#[test]
fn small_incomplete_writes_strictly_reject() {
    nested_write::strictly_reject(8);
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
