use celox::{BigUint, Simulator};

#[test]
fn native_shifts_respect_source_width_and_large_counts() {
    let code = r#"
module Top (
    sh: input logic<8>,
    u8v: input logic<8>,
    s8v: input signed logic<8>,
    shl8: output logic<8>,
    shr8: output logic<8>,
    sar8: output signed logic<8>,
    u16v: input logic<16>,
    s16v: input signed logic<16>,
    shl16: output logic<16>,
    shr16: output logic<16>,
    sar16: output signed logic<16>,
    u32v: input logic<32>,
    s32v: input signed logic<32>,
    shl32: output logic<32>,
    shr32: output logic<32>,
    sar32: output signed logic<32>,
    u64v: input logic<64>,
    s64v: input signed logic<64>,
    shl64: output logic<64>,
    shr64: output logic<64>,
    sar64: output signed logic<64>
) {
    assign shl8 = u8v << sh;
    assign shr8 = u8v >> sh;
    assign sar8 = s8v >>> sh;
    assign shl16 = u16v << sh;
    assign shr16 = u16v >> sh;
    assign sar16 = s16v >>> sh;
    assign shl32 = u32v << sh;
    assign shr32 = u32v >> sh;
    assign sar32 = s32v >>> sh;
    assign shl64 = u64v << sh;
    assign shr64 = u64v >> sh;
    assign sar64 = s64v >>> sh;
}
"#;
    let mut sim = Simulator::builder(code, "Top").build_native().unwrap();
    let sh = sim.signal("sh");
    let cases = [
        (
            8u32,
            sim.signal("u8v"),
            sim.signal("s8v"),
            sim.signal("shl8"),
            sim.signal("shr8"),
            sim.signal("sar8"),
        ),
        (
            16,
            sim.signal("u16v"),
            sim.signal("s16v"),
            sim.signal("shl16"),
            sim.signal("shr16"),
            sim.signal("sar16"),
        ),
        (
            32,
            sim.signal("u32v"),
            sim.signal("s32v"),
            sim.signal("shl32"),
            sim.signal("shr32"),
            sim.signal("sar32"),
        ),
        (
            64,
            sim.signal("u64v"),
            sim.signal("s64v"),
            sim.signal("shl64"),
            sim.signal("shr64"),
            sim.signal("sar64"),
        ),
    ];

    sim.modify(|io| {
        for (width, unsigned, signed, ..) in cases {
            let value = test_value(width);
            match width {
                8 => {
                    io.set(unsigned, value as u8);
                    io.set(signed, value as u8);
                }
                16 => {
                    io.set(unsigned, value as u16);
                    io.set(signed, value as u16);
                }
                32 => {
                    io.set(unsigned, value as u32);
                    io.set(signed, value as u32);
                }
                64 => {
                    io.set(unsigned, value);
                    io.set(signed, value);
                }
                _ => unreachable!(),
            }
        }
    })
    .unwrap();

    for count in [
        7u64, 8, 9, 15, 16, 17, 31, 32, 33, 63, 64, 65, 127, 128, 129,
    ] {
        sim.modify(|io| io.set(sh, count as u8)).unwrap();
        for (width, _, _, shl, shr, sar) in cases {
            let value = test_value(width);
            let mask = width_mask(width);
            let expected_shl = if count >= u64::from(width) {
                0
            } else {
                (value << count) & mask
            };
            let expected_shr = if count >= u64::from(width) {
                0
            } else {
                value >> count
            };
            let expected_sar = if count >= u64::from(width) {
                mask
            } else {
                (((value | !mask) as i64) >> count) as u64 & mask
            };
            assert_eq!(
                sim.get(shl),
                BigUint::from(expected_shl),
                "shl width={width} count={count}"
            );
            assert_eq!(
                sim.get(shr),
                BigUint::from(expected_shr),
                "shr width={width} count={count}"
            );
            assert_eq!(
                sim.get(sar),
                BigUint::from(expected_sar),
                "sar width={width} count={count}"
            );
        }
    }
}

fn width_mask(width: u32) -> u64 {
    if width == 64 {
        u64::MAX
    } else {
        (1u64 << width) - 1
    }
}

fn test_value(width: u32) -> u64 {
    (1u64 << (width - 1)) | 3
}
