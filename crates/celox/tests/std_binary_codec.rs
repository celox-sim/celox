use celox::Simulator;

const BINARY_ENCODER_SRC: &str =
    include_str!("../../../deps/veryl/crates/std/veryl/src/binary_enc_dec/binary_encoder.veryl");
const BINARY_DECODER_SRC: &str =
    include_str!("../../../deps/veryl/crates/std/veryl/src/binary_enc_dec/binary_decoder.veryl");

/// Binary encoder: onehot input -> binary output (UNARY_WIDTH=8, BIN_WIDTH=3)
#[test]
fn test_binary_encoder() {
    let top = r#"
module Top (
    i_unary: input  logic<8>,
    o_bin  : output logic<3>,
) {
    inst u: binary_encoder #(UNARY_WIDTH: 8) (
        i_en   : 1'b1,
        i_unary,
        o_bin,
    );
}
"#;
    let code = format!("{BINARY_ENCODER_SRC}\n{top}");
    let mut sim = Simulator::builder(&code, "Top").build().unwrap();
    let i_unary = sim.signal("i_unary");
    let o_bin = sim.signal("o_bin");

    for bit_pos in 0u8..8 {
        let onehot_val: u8 = 1 << bit_pos;
        sim.modify(|io| io.set(i_unary, onehot_val)).unwrap();

        let bin_out = sim.get_as::<u8>(o_bin);
        assert_eq!(
            bin_out, bit_pos,
            "encoder({onehot_val:#010b}): expected={bit_pos}, got={bin_out}"
        );
    }
}

/// Binary decoder: binary input -> onehot output (BIN_WIDTH=3, UNARY_WIDTH=8)
#[test]
fn test_binary_decoder() {
    let top = r#"
module Top (
    i_bin  : input  logic<3>,
    o_unary: output logic<8>,
) {
    inst u: binary_decoder #(BIN_WIDTH: 3) (
        i_en: 1'b1,
        i_bin,
        o_unary,
    );
}
"#;
    let code = format!("{BINARY_DECODER_SRC}\n{top}");
    let mut sim = Simulator::builder(&code, "Top").build().unwrap();
    let i_bin = sim.signal("i_bin");
    let o_unary = sim.signal("o_unary");

    for val in 0u8..8 {
        sim.modify(|io| io.set(i_bin, val)).unwrap();

        let unary_out = sim.get_as::<u8>(o_unary);
        let expected: u8 = 1 << val;
        assert_eq!(
            unary_out, expected,
            "decoder({val}): expected={expected:#010b}, got={unary_out:#010b}"
        );
    }
}

/// Roundtrip: encoder -> decoder (onehot -> binary -> onehot)
#[test]
fn test_binary_codec_roundtrip() {
    let top = r#"
module Top (
    i_unary: input  logic<8>,
    o_unary: output logic<8>,
    o_bin  : output logic<3>,
) {
    inst u_enc: binary_encoder #(UNARY_WIDTH: 8) (
        i_en   : 1'b1,
        i_unary,
        o_bin,
    );
    inst u_dec: binary_decoder #(BIN_WIDTH: 3) (
        i_en   : 1'b1,
        i_bin  : o_bin,
        o_unary,
    );
}
"#;
    let code = format!("{BINARY_ENCODER_SRC}\n{BINARY_DECODER_SRC}\n{top}");
    let mut sim = Simulator::builder(&code, "Top").build().unwrap();
    let i_unary = sim.signal("i_unary");
    let o_unary = sim.signal("o_unary");

    for bit_pos in 0u8..8 {
        let onehot_val: u8 = 1 << bit_pos;
        sim.modify(|io| io.set(i_unary, onehot_val)).unwrap();

        let roundtrip = sim.get_as::<u8>(o_unary);
        assert_eq!(
            roundtrip, onehot_val,
            "roundtrip failed for bit {bit_pos}: input={onehot_val:#010b}, output={roundtrip:#010b}"
        );
    }
}

/// Encoder with enable=0: output should be 0 (masked)
#[test]
fn test_binary_encoder_disabled() {
    let top = r#"
module Top (
    i_en   : input  logic,
    i_unary: input  logic<8>,
    o_bin  : output logic<3>,
) {
    inst u: binary_encoder #(UNARY_WIDTH: 8) (
        i_en,
        i_unary,
        o_bin,
    );
}
"#;
    let code = format!("{BINARY_ENCODER_SRC}\n{top}");
    let mut sim = Simulator::builder(&code, "Top").build().unwrap();
    let i_en = sim.signal("i_en");
    let i_unary = sim.signal("i_unary");
    let o_bin = sim.signal("o_bin");

    // Disabled: should encode bit 0 (because masked input becomes {0...0, 1'b1})
    sim.modify(|io| {
        io.set(i_en, 0u8);
        io.set(i_unary, 0b10000000u8);
    })
    .unwrap();
    let bin_disabled = sim.get_as::<u8>(o_bin);
    assert_eq!(bin_disabled, 0, "disabled encoder should output 0");

    // Enabled: should encode bit 7
    sim.modify(|io| io.set(i_en, 1u8)).unwrap();
    let bin_enabled = sim.get_as::<u8>(o_bin);
    assert_eq!(bin_enabled, 7, "enabled encoder should output 7 for bit 7");
}
