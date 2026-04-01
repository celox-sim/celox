use celox::{BigUint, Simulator};

#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

// Tests for memory-backed wide shift operations (>= 256-bit / 4 chunks).
//
// The translator uses a memory-backed path (stack slots with O(1) dynamic
// indexing) for shift/sar when the operand width is >= MEM_SHIFT_THRESHOLD
// (4 chunks = 256 bits). These tests ensure correctness of that path.

// ============================================================
// 256-bit shifts (exactly at threshold: 4 chunks)
// ============================================================

// ============================================================
// 512-bit shifts (8 chunks, well above threshold)
// ============================================================

// ============================================================
// 512-bit shifts in always_ff (through clock edge)
// ============================================================

// ============================================================
// 1024-bit shifts (16 chunks)
// ============================================================

// ============================================================
// Edge cases
// ============================================================

// ============================================================
// Narrow source, wide destination (OOB regression)
// ============================================================

all_backends! {

    fn test_256bit_shift_left_by_zero(sim) {
        @ignore_on(wasm);
        @setup { let code = r#"
module Top (
a:   input  logic<256>,
amt: input  logic<9>,
o:   output logic<256>
) {
assign o = a << amt;
}
"#; }
        @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let amt = sim.signal("amt");
    let o = sim.signal("o");

    // Shift by 0 → identity
    let val = BigUint::from(0xDEAD_BEEF_CAFE_BABEu64);
    sim.modify(|io| {
        io.set_wide(a, val.clone());
        io.set(amt, 0u16);
    })
    .unwrap();
    assert_eq!(sim.get(o), val, "256-bit shl by 0 should be identity");

    }

    fn test_256bit_shift_left_within_chunk(sim) {
        @ignore_on(wasm);
        @setup { let code = r#"
module Top (
a:   input  logic<256>,
amt: input  logic<9>,
o:   output logic<256>
) {
assign o = a << amt;
}
"#; }
        @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let amt = sim.signal("amt");
    let o = sim.signal("o");

    // Shift by 4 (within a single chunk)
    let val = BigUint::from(0xFFu64);
    sim.modify(|io| {
        io.set_wide(a, val.clone());
        io.set(amt, 4u16);
    })
    .unwrap();
    assert_eq!(
        sim.get(o),
        BigUint::from(0xFF0u64),
        "256-bit shl by 4 failed"
    );

    }

    fn test_256bit_shift_left_exact_chunk_boundary(sim) {
        @ignore_on(wasm);
        @setup { let code = r#"
module Top (
a:   input  logic<256>,
amt: input  logic<9>,
o:   output logic<256>
) {
assign o = a << amt;
}
"#; }
        @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let amt = sim.signal("amt");
    let o = sim.signal("o");

    // Shift by exactly 64 → moves to next chunk
    let val = BigUint::from(1u64);
    sim.modify(|io| {
        io.set_wide(a, val.clone());
        io.set(amt, 64u16);
    })
    .unwrap();
    assert_eq!(
        sim.get(o),
        BigUint::from(1u64) << 64,
        "256-bit shl by 64 should move value to chunk 1"
    );

    // Shift by 128 → moves to chunk 2
    sim.modify(|io| io.set(amt, 128u16)).unwrap();
    assert_eq!(
        sim.get(o),
        BigUint::from(1u64) << 128,
        "256-bit shl by 128 should move value to chunk 2"
    );

    // Shift by 192 → moves to chunk 3 (highest)
    sim.modify(|io| io.set(amt, 192u16)).unwrap();
    assert_eq!(
        sim.get(o),
        BigUint::from(1u64) << 192,
        "256-bit shl by 192 should move value to chunk 3"
    );

    }

    fn test_256bit_shift_left_cross_chunk(sim) {
        @ignore_on(wasm);
        @setup { let code = r#"
module Top (
a:   input  logic<256>,
amt: input  logic<9>,
o:   output logic<256>
) {
assign o = a << amt;
}
"#; }
        @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let amt = sim.signal("amt");
    let o = sim.signal("o");

    // 0xFFFF_FFFF shifted by 48 → crosses chunk boundary (bits 48..79)
    let val = BigUint::from(0xFFFF_FFFFu64);
    sim.modify(|io| {
        io.set_wide(a, val.clone());
        io.set(amt, 48u16);
    })
    .unwrap();
    assert_eq!(
        sim.get(o),
        BigUint::from(0xFFFF_FFFFu64) << 48,
        "256-bit shl by 48 should cross chunk boundary"
    );

    }

    fn test_256bit_shift_left_overflow(sim) {
        @ignore_on(wasm);
        @setup { let code = r#"
module Top (
a:   input  logic<256>,
amt: input  logic<9>,
o:   output logic<256>
) {
assign o = a << amt;
}
"#; }
        @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let amt = sim.signal("amt");
    let o = sim.signal("o");

    // Shift by 256 → everything falls off
    let val = BigUint::from(0xFFFF_FFFF_FFFF_FFFFu64);
    sim.modify(|io| {
        io.set_wide(a, val.clone());
        io.set(amt, 256u16);
    })
    .unwrap();
    assert_eq!(
        sim.get(o),
        BigUint::from(0u64),
        "256-bit shl by 256 should produce zero"
    );

    }

    fn test_256bit_shift_right_logical(sim) {
        @ignore_on(wasm);
        @setup { let code = r#"
module Top (
a:   input  logic<256>,
amt: input  logic<9>,
o:   output logic<256>
) {
assign o = a >> amt;
}
"#; }
        @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let amt = sim.signal("amt");
    let o = sim.signal("o");

    // Value in chunk 3, shift right by 192 → should land in chunk 0
    let val: BigUint = BigUint::from(0xABCDu64) << 192;
    sim.modify(|io| {
        io.set_wide(a, val.clone());
        io.set(amt, 192u16);
    })
    .unwrap();
    assert_eq!(
        sim.get(o),
        BigUint::from(0xABCDu64),
        "256-bit shr by 192 failed"
    );

    // Cross-chunk shift right
    let val: BigUint = BigUint::from(0xFFu64) << 60;
    sim.modify(|io| {
        io.set_wide(a, val.clone());
        io.set(amt, 4u16);
    })
    .unwrap();
    let expected_cross: BigUint = BigUint::from(0xFFu64) << 56;
    assert_eq!(
        sim.get(o),
        expected_cross,
        "256-bit shr by 4 (cross-chunk) failed"
    );

    }

    fn test_256bit_arithmetic_shift_right(sim) {
        @ignore_on(wasm);
        @setup { let code = r#"
module Top (
a:   input  signed logic<256>,
amt: input  logic<9>,
o:   output signed logic<256>
) {
assign o = a >>> amt;
}
"#; }
        @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let amt = sim.signal("amt");
    let o = sim.signal("o");

    // Positive value: sar behaves like shr
    let val = BigUint::from(0x100u64);
    sim.modify(|io| {
        io.set_wide(a, val.clone());
        io.set(amt, 4u16);
    })
    .unwrap();
    assert_eq!(
        sim.get(o),
        BigUint::from(0x10u64),
        "256-bit sar of positive value should zero-fill"
    );

    // Negative value (MSB set): should sign-extend
    // Set all 256 bits (represents -1)
    let neg_one: BigUint = (BigUint::from(1u64) << 256) - 1u64;
    sim.modify(|io| {
        io.set_wide(a, neg_one.clone());
        io.set(amt, 100u16);
    })
    .unwrap();
    assert_eq!(
        sim.get(o),
        neg_one.clone(),
        "256-bit sar of -1 by any amount should remain -1"
    );

    // -2 >>> 1 = -1
    let neg_two = &neg_one - BigUint::from(1u64);
    sim.modify(|io| {
        io.set_wide(a, neg_two);
        io.set(amt, 1u16);
    })
    .unwrap();
    assert_eq!(sim.get(o), neg_one, "256-bit sar of -2 by 1 should be -1");

    }

    fn test_512bit_shift_left(sim) {
        @ignore_on(wasm);
        @setup { let code = r#"
module Top (
a:   input  logic<512>,
amt: input  logic<10>,
o:   output logic<512>
) {
assign o = a << amt;
}
"#; }
        @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let amt = sim.signal("amt");
    let o = sim.signal("o");

    // Shift 1 by various amounts
    let one = BigUint::from(1u64);

    for shift in [0u16, 1, 32, 63, 64, 65, 128, 255, 256, 384, 448, 511] {
        sim.modify(|io| {
            io.set_wide(a, one.clone());
            io.set(amt, shift);
        })
        .unwrap();
        let expected = &one << shift as usize;
        assert_eq!(sim.get(o), expected, "512-bit shl of 1 by {shift} failed");
    }

    }

    fn test_512bit_shift_right(sim) {
        @ignore_on(wasm);
        @setup { let code = r#"
module Top (
a:   input  logic<512>,
amt: input  logic<10>,
o:   output logic<512>
) {
assign o = a >> amt;
}
"#; }
        @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let amt = sim.signal("amt");
    let o = sim.signal("o");

    // Put a bit at position 511 (MSB), shift right
    let msb: BigUint = BigUint::from(1u64) << 511;
    for shift in [0u16, 1, 63, 64, 128, 256, 511] {
        sim.modify(|io| {
            io.set_wide(a, msb.clone());
            io.set(amt, shift);
        })
        .unwrap();
        let expected = &msb >> shift as usize;
        assert_eq!(sim.get(o), expected, "512-bit shr of MSB by {shift} failed");
    }

    }

    fn test_512bit_arithmetic_shift_right(sim) {
        @ignore_on(wasm);
        @setup { let code = r#"
module Top (
a:   input  signed logic<512>,
amt: input  logic<10>,
o:   output signed logic<512>
) {
assign o = a >>> amt;
}
"#; }
        @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let amt = sim.signal("amt");
    let o = sim.signal("o");

    let all_ones: BigUint = (BigUint::from(1u64) << 512) - 1u64; // -1 in 512-bit

    // -1 >>> N = -1 for all N
    for shift in [1u16, 64, 128, 256, 511] {
        sim.modify(|io| {
            io.set_wide(a, all_ones.clone());
            io.set(amt, shift);
        })
        .unwrap();
        assert_eq!(
            sim.get(o),
            all_ones.clone(),
            "512-bit sar of -1 by {shift} should remain -1"
        );
    }

    // MSB set, rest zero (minimum value): sar by 1 → sign extends
    // 0x8000...0000 >>> 1 = 0xC000...0000
    let min_val: BigUint = BigUint::from(1u64) << 511;
    sim.modify(|io| {
        io.set_wide(a, min_val);
        io.set(amt, 1u16);
    })
    .unwrap();
    // Expected: bit 511 and bit 510 set, rest 0
    let expected: BigUint = (BigUint::from(1u64) << 511) | (BigUint::from(1u64) << 510);
    assert_eq!(
        sim.get(o),
        expected,
        "512-bit sar of MIN_VALUE by 1 should sign-extend"
    );

    }

    fn test_512bit_shift_left_multiword_pattern(sim) {
        @ignore_on(wasm);
        @setup { let code = r#"
module Top (
a:   input  logic<512>,
amt: input  logic<10>,
o:   output logic<512>
) {
assign o = a << amt;
}
"#; }
        @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let amt = sim.signal("amt");
    let o = sim.signal("o");

    // Fill all chunks with a recognizable pattern
    let mut val = BigUint::from(0u64);
    for i in 0u64..8 {
        val |= BigUint::from(0x0101_0101_0101_0101u64 * (i + 1)) << (i as usize * 64);
    }
    let mask: BigUint = (BigUint::from(1u64) << 512) - 1u64;

    // Shift by 1 word (64 bits)
    sim.modify(|io| {
        io.set_wide(a, val.clone());
        io.set(amt, 64u16);
    })
    .unwrap();
    let expected = (&val << 64usize) & &mask;
    assert_eq!(
        sim.get(o),
        expected,
        "512-bit shl of multi-word pattern by 64 failed"
    );

    // Shift by 65 (word + 1 bit)
    sim.modify(|io| io.set(amt, 65u16)).unwrap();
    let expected = (&val << 65usize) & &mask;
    assert_eq!(
        sim.get(o),
        expected,
        "512-bit shl of multi-word pattern by 65 failed"
    );

    }

    fn test_512bit_shift_left_ff(sim) {
        @ignore_on(wasm);
        @setup { let code = r#"
module Top (
clk: input  clock,
a:   input  logic<512>,
amt: input  logic<10>,
o:   output logic<512>
) {
always_ff {
o = a << amt;
}
}
"#; }
        @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let a = sim.signal("a");
    let amt = sim.signal("amt");
    let o = sim.signal("o");

    let val = BigUint::from(0xCAFEu64);
    let mask: BigUint = (BigUint::from(1u64) << 512) - 1u64;

    sim.modify(|io| {
        io.set_wide(a, val.clone());
        io.set(amt, 200u16);
    })
    .unwrap();
    sim.tick(clk).unwrap();

    let expected = (&val << 200usize) & &mask;
    assert_eq!(
        sim.get(o),
        expected,
        "512-bit shl by 200 in always_ff failed"
    );

    }

    fn test_512bit_sar_ff(sim) {
        @ignore_on(wasm);
        @setup { let code = r#"
module Top (
clk: input  clock,
a:   input  signed logic<512>,
amt: input  logic<10>,
o:   output signed logic<512>
) {
always_ff {
o = a >>> amt;
}
}
"#; }
        @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let a = sim.signal("a");
    let amt = sim.signal("amt");
    let o = sim.signal("o");

    // -1 >>> 200 = -1
    let neg_one: BigUint = (BigUint::from(1u64) << 512) - 1u64;
    sim.modify(|io| {
        io.set_wide(a, neg_one.clone());
        io.set(amt, 200u16);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(
        sim.get(o),
        neg_one,
        "512-bit sar of -1 by 200 in always_ff should remain -1"
    );

    }

    fn test_1024bit_shift_left(sim) {
        @ignore_on(wasm);
        @setup { let code = r#"
module Top (
a:   input  logic<1024>,
amt: input  logic<11>,
o:   output logic<1024>
) {
assign o = a << amt;
}
"#; }
        @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let amt = sim.signal("amt");
    let o = sim.signal("o");

    let mask: BigUint = (BigUint::from(1u64) << 1024) - 1u64;

    // Shift 1 across various positions
    let one = BigUint::from(1u64);
    for shift in [0u16, 64, 128, 512, 1000, 1023] {
        sim.modify(|io| {
            io.set_wide(a, one.clone());
            io.set(amt, shift);
        })
        .unwrap();
        let expected = (&one << shift as usize) & &mask;
        assert_eq!(sim.get(o), expected, "1024-bit shl of 1 by {shift} failed");
    }

    }

    fn test_1024bit_shift_right(sim) {
        @ignore_on(wasm);
        @setup { let code = r#"
module Top (
a:   input  logic<1024>,
amt: input  logic<11>,
o:   output logic<1024>
) {
assign o = a >> amt;
}
"#; }
        @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let amt = sim.signal("amt");
    let o = sim.signal("o");

    // MSB at bit 1023
    let msb: BigUint = BigUint::from(1u64) << 1023;
    for shift in [0u16, 1, 64, 512, 1023] {
        sim.modify(|io| {
            io.set_wide(a, msb.clone());
            io.set(amt, shift);
        })
        .unwrap();
        let expected = &msb >> shift as usize;
        assert_eq!(
            sim.get(o),
            expected,
            "1024-bit shr of MSB by {shift} failed"
        );
    }

    }

    fn test_1024bit_sar_sign_extension(sim) {
        @ignore_on(wasm);
        @setup { let code = r#"
module Top (
a:   input  signed logic<1024>,
amt: input  logic<11>,
o:   output signed logic<1024>
) {
assign o = a >>> amt;
}
"#; }
        @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let amt = sim.signal("amt");
    let o = sim.signal("o");

    let all_ones: BigUint = (BigUint::from(1u64) << 1024) - 1u64;

    // -1 >>> 512 = -1
    sim.modify(|io| {
        io.set_wide(a, all_ones.clone());
        io.set(amt, 512u16);
    })
    .unwrap();
    assert_eq!(
        sim.get(o),
        all_ones,
        "1024-bit sar of -1 by 512 should remain -1"
    );

    }

    fn test_256bit_all_ones_shift_left_one(sim) {
        @ignore_on(wasm);
        @setup { let code = r#"
module Top (
a:   input  logic<256>,
o:   output logic<256>
) {
assign o = a << 1;
}
"#; }
        @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let o = sim.signal("o");

    let all_ones: BigUint = (BigUint::from(1u64) << 256) - 1u64;
    let mask = all_ones.clone();
    sim.modify(|io| io.set_wide(a, all_ones.clone())).unwrap();
    // (all_ones << 1) & mask = all_ones - 1 (MSB lost, LSB becomes 0)
    let expected = (&all_ones << 1usize) & &mask;
    assert_eq!(sim.get(o), expected, "256-bit shl of all-ones by 1 failed");

    }

    fn test_512bit_shift_right_complete_overflow(sim) {
        @ignore_on(wasm);
        @setup { let code = r#"
module Top (
a:   input  logic<512>,
amt: input  logic<10>,
o:   output logic<512>
) {
assign o = a >> amt;
}
"#; }
        @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let amt = sim.signal("amt");
    let o = sim.signal("o");

    // All ones >> 512 → 0
    let all_ones: BigUint = (BigUint::from(1u64) << 512) - 1u64;
    sim.modify(|io| {
        io.set_wide(a, all_ones);
        io.set(amt, 512u16);
    })
    .unwrap();
    assert_eq!(
        sim.get(o),
        BigUint::from(0u64),
        "512-bit shr by 512 should produce zero"
    );

    }

    fn test_256bit_shift_right_cross_chunk(sim) {
        @ignore_on(wasm);
        @setup { // Test that bits correctly flow between chunks during right shift
let code = r#"
module Top (
a:   input  logic<256>,
amt: input  logic<9>,
o:   output logic<256>
) {
assign o = a >> amt;
}
"#; }
        @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let amt = sim.signal("amt");
    let o = sim.signal("o");

    // Place 0xFF at bits 68..60 (spans chunk boundary between chunk 0 and 1)
    let val: BigUint = BigUint::from(0xFFu64) << 60;
    sim.modify(|io| {
        io.set_wide(a, val.clone());
        io.set(amt, 8u16);
    })
    .unwrap();
    let expected = &val >> 8usize;
    assert_eq!(
        sim.get(o),
        expected,
        "256-bit shr of cross-chunk value by 8 failed"
    );

    }

    // Regression test: when lhs is narrower than dst, the memory-backed source
    // slot must be zero-padded to num_chunks (= common_logical_width / 64).
    // Without padding, load_or_default reads uninitialised memory beyond the
    // source slot, producing garbage in the upper chunks.
    fn test_narrow_source_wide_dest_shift_left(sim) {
        @ignore_on(wasm);
        @setup { // dst is 512-bit (8 chunks), lhs is 256-bit (4 chunks).
// common_logical_width = max(512, 256, 64) = 512 → num_chunks = 8.
// Source slot must be 8 chunks with chunks 4..7 zeroed.
let code = r#"
module Top (
a:   input  logic<256>,
amt: input  logic<10>,
o:   output logic<512>
) {
assign o = (a as 512) << amt;
}
"#; }
        @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let amt = sim.signal("amt");
    let o = sim.signal("o");

    let mask_512: BigUint = (BigUint::from(1u64) << 512) - 1u64;

    // Shift by 0: upper chunks should be zero, not garbage
    let val = BigUint::from(0xDEAD_BEEF_CAFE_BABEu64);
    sim.modify(|io| {
        io.set_wide(a, val.clone());
        io.set(amt, 0u16);
    })
    .unwrap();
    assert_eq!(
        sim.get(o),
        val.clone(),
        "Narrow→wide shl by 0: upper bits should be zero"
    );

    // Shift by 256: value should move into the upper half cleanly
    sim.modify(|io| io.set(amt, 256u16)).unwrap();
    let expected = (&val << 256usize) & &mask_512;
    assert_eq!(sim.get(o), expected, "Narrow→wide shl by 256 failed");

    // Shift by 300: crosses into upper chunks
    sim.modify(|io| io.set(amt, 300u16)).unwrap();
    let expected = (&val << 300usize) & &mask_512;
    assert_eq!(sim.get(o), expected, "Narrow→wide shl by 300 failed");

    }

    fn test_narrow_source_wide_dest_shift_right(sim) {
        @ignore_on(wasm);
        @setup { // dst is 512-bit, lhs is 256-bit (cast up).
// Right-shifting should see zeros in the upper chunks, not garbage.
let code = r#"
module Top (
a:   input  logic<256>,
amt: input  logic<10>,
o:   output logic<512>
) {
assign o = (a as 512) >> amt;
}
"#; }
        @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let amt = sim.signal("amt");
    let o = sim.signal("o");

    // All 256 bits set, shift right by 1: upper 256 bits should remain zero
    let val: BigUint = (BigUint::from(1u64) << 256) - 1u64;
    sim.modify(|io| {
        io.set_wide(a, val.clone());
        io.set(amt, 1u16);
    })
    .unwrap();
    let expected = &val >> 1usize;
    assert_eq!(
        sim.get(o),
        expected,
        "Narrow→wide shr by 1: upper bits should stay zero"
    );

    }

    fn test_narrow_source_wide_dest_sar(sim) {
        @ignore_on(wasm);
        @setup { // 512-bit signed sar where source is a wide input.
// The source slot for the 512-bit operand must be fully initialised.
let code = r#"
module Top (
a:   input  signed logic<512>,
amt: input  logic<10>,
o:   output signed logic<512>
) {
assign o = a >>> amt;
}
"#; }
        @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let amt = sim.signal("amt");
    let o = sim.signal("o");

    // Positive value (fits in lower 256 bits, upper 256 bits are 0):
    // sar should zero-fill, not produce garbage from uninitialised memory.
    let val = BigUint::from(0x1234_5678_9ABC_DEF0u64);
    sim.modify(|io| {
        io.set_wide(a, val.clone());
        io.set(amt, 4u16);
    })
    .unwrap();
    assert_eq!(
        sim.get(o),
        &val >> 4usize,
        "512-bit sar of small positive value should zero-fill"
    );

    }
}
