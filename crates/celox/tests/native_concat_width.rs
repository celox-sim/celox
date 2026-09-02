//! Regression coverage for wide SDK concatenations in the native backend.

#![cfg(target_arch = "x86_64")]

use celox::frontend_sdk::{BinaryOp, Edge, FrontendArtifact, ModuleBuilder, ValueType};
use celox::{DiagnosticsOptions, Simulator};

fn artifact(data_width: usize) -> FrontendArtifact {
    let bit = ValueType::bits(1).unwrap();
    let mut builder = ModuleBuilder::new("NativeConcatWidth").unwrap();
    let clock = builder.input("clock", bit).unwrap();
    let data = builder
        .input("data", ValueType::bits(data_width).unwrap())
        .unwrap();
    let select = builder
        .input("select", ValueType::bits(2).unwrap())
        .unwrap();
    let result = builder.output("result", bit).unwrap();
    let bank_width = data_width + 3;
    let bank = builder
        .internal("bank", ValueType::bits(bank_width).unwrap())
        .unwrap();

    let mut next_bits = Vec::with_capacity(bank_width);
    for lsb in 0..data_width {
        let slice = builder.slice(data, lsb, 1).unwrap();
        next_bits.push(builder.read_slice(slice).unwrap());
    }
    for lsb in 0..2 {
        let slice = builder.slice(select, lsb, 1).unwrap();
        next_bits.push(builder.read_slice(slice).unwrap());
    }
    let data_q0 = builder
        .read_slice(builder.slice(bank, 0, 1).unwrap())
        .unwrap();
    let select_q0 = builder
        .read_slice(builder.slice(bank, data_width, 1).unwrap())
        .unwrap();
    next_bits.push(
        builder
            .binary(BinaryOp::Xor, data_q0, select_q0, bit)
            .unwrap(),
    );
    next_bits.reverse();

    let next = builder.concat(next_bits).unwrap();
    builder
        .register(
            builder.whole(bank).unwrap(),
            next,
            clock,
            Edge::Posedge,
            None,
            None,
        )
        .unwrap();
    let result_q = builder
        .read_slice(builder.slice(bank, data_width + 2, 1).unwrap())
        .unwrap();
    builder
        .assign(builder.whole(result).unwrap(), result_q)
        .unwrap();

    let artifact = builder.finish();
    artifact.validate().unwrap();
    artifact
}

#[test]
fn native_wide_concat_folding_preserves_all_operands() {
    let mut diagnostics = DiagnosticsOptions::default();
    diagnostics.sir.verify_passes = true;
    Simulator::from_frontend(artifact(129))
        .diagnostics(diagnostics)
        .build_wasm()
        .unwrap();
    Simulator::from_frontend(artifact(128))
        .build_native()
        .unwrap();
    Simulator::from_frontend(artifact(129))
        .build_native()
        .unwrap();
}
