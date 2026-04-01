use celox::Simulator;

#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

const GRAY_ENCODER_SRC: &str =
    include_str!("../../../deps/veryl/crates/std/veryl/src/gray/gray_encoder.veryl");
const GRAY_DECODER_SRC: &str =
    include_str!("../../../deps/veryl/crates/std/veryl/src/gray/gray_decoder.veryl");

all_backends! {

    // Gray encode → decode roundtrip: o_bin == i_bin for all 8-bit values
    fn test_gray_roundtrip_8bit(sim) {
        @ignore_on(wasm);
        @setup { let top = r#"
module Top (
i_bin  : input  logic<8>,
o_gray : output logic<8>,
o_bin  : output logic<8>,
) {
inst u_enc: gray_encoder #(WIDTH: 8) (
i_bin,
o_gray,
);
inst u_dec: gray_decoder #(WIDTH: 8) (
i_gray: o_gray,
o_bin,
);
}
"#;
let code = format!("{GRAY_ENCODER_SRC}\n{GRAY_DECODER_SRC}\n{top}"); }
        @build Simulator::builder(&code, "Top");
    let i_bin = sim.signal("i_bin");
    let o_gray = sim.signal("o_gray");
    let o_bin = sim.signal("o_bin");

    for val in 0u8..=255 {
        sim.modify(|io| io.set(i_bin, val)).unwrap();

        let gray_out = sim.get_as::<u8>(o_gray);
        let bin_out = sim.get_as::<u8>(o_bin);

        assert_eq!(
            bin_out, val,
            "roundtrip failed: input={val}, gray={gray_out:#04x}, output={bin_out}"
        );
    }

    }

    // Verify Gray code property: adjacent binary values differ by exactly 1 bit in Gray
    fn test_gray_single_bit_change(sim) {
        @setup { let top = r#"
module Top (
i_bin  : input  logic<8>,
o_gray : output logic<8>,
) {
inst u_enc: gray_encoder #(WIDTH: 8) (
i_bin,
o_gray,
);
}
"#;
let code = format!("{GRAY_ENCODER_SRC}\n{top}"); }
        @build Simulator::builder(&code, "Top");
    let i_bin = sim.signal("i_bin");
    let o_gray = sim.signal("o_gray");

    let mut prev_gray: Option<u8> = None;
    for val in 0u16..256 {
        sim.modify(|io| io.set(i_bin, val as u8)).unwrap();
        let gray = sim.get_as::<u8>(o_gray);

        if let Some(prev) = prev_gray {
            let diff = prev ^ gray;
            assert!(
                diff.count_ones() == 1,
                "adjacent gray codes should differ by 1 bit: bin={val}, prev_gray={prev:#010b}, gray={gray:#010b}, diff={diff:#010b}"
            );
        }
        prev_gray = Some(gray);
    }

    }
}
