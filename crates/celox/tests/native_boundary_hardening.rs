//! Boundary-focused native backend hardening tests.
//!
//! These cases intentionally generate static bit ranges around byte and
//! 64-bit boundaries, then compare native against Cranelift. They also assert
//! that the optimized SIR still contains the risky store/commit shape, so the
//! tests do not silently drift away from the lowering paths they are meant to
//! cover.

#![cfg(target_arch = "x86_64")]

use celox::{BigUint, SimBackend, Simulator, SimulatorBuilder};

const WIDTHS: &[usize] = &[
    1, 2, 7, 8, 9, 15, 16, 17, 31, 32, 33, 63, 64, 65, 66, 127, 128, 129,
];

fn mask(width: usize) -> BigUint {
    if width == 0 {
        BigUint::from(0u32)
    } else {
        (BigUint::from(1u32) << width) - 1u32
    }
}

fn case_total_width(offset: usize, width: usize) -> usize {
    (offset + width + 16).div_ceil(64) * 64
}

fn patterned_value(seed: u64, width: usize) -> BigUint {
    let mut value = BigUint::from(0u32);
    let mut state = seed | 1;
    for bit in 0..width {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        if state & 1 != 0 {
            value |= BigUint::from(1u32) << bit;
        }
    }
    value
}

fn expected_replace(
    total_width: usize,
    offset: usize,
    width: usize,
    base: &BigUint,
    val: &BigUint,
) -> BigUint {
    let full = mask(total_width);
    let target_mask = mask(width) << offset;
    let keep_mask = &full ^ &target_mask;
    (base & keep_mask) | ((val & mask(width)) << offset)
}

fn chunk_widths(width: usize) -> Vec<usize> {
    let mut remaining = width;
    let mut chunks = Vec::new();
    while remaining > 0 {
        let chunk = remaining.min(64);
        chunks.push(chunk);
        remaining -= chunk;
    }
    chunks
}

fn value_ports(width: usize) -> String {
    let chunks = chunk_widths(width);
    if chunks.len() == 1 {
        format!("    val : input logic<{width}>,\n")
    } else {
        chunks
            .iter()
            .enumerate()
            .map(|(idx, chunk_width)| format!("    p{idx}  : input logic<{chunk_width}>,\n"))
            .collect()
    }
}

fn value_expr(width: usize) -> String {
    let chunks = chunk_widths(width);
    if chunks.len() == 1 {
        "val".to_string()
    } else {
        let parts = (0..chunks.len())
            .rev()
            .map(|idx| format!("p{idx}"))
            .collect::<Vec<_>>()
            .join(", ");
        format!("{{{parts}}}")
    }
}

fn comb_store_source(offset: usize, width: usize, total_width: usize) -> String {
    let hi = offset + width - 1;
    let value_ports = value_ports(width);
    let value_expr = value_expr(width);
    format!(
        r#"
module Top (
    base: input logic<{total_width}>,
{value_ports}
    o   : output logic<{total_width}>
) {{
    var wide: logic<{total_width}>;

    always_comb {{
        wide = base;
        wide[{hi}:{offset}] = {value_expr};
        o = wide;
    }}
}}
"#
    )
}

fn ff_commit_source(offset: usize, width: usize, total_width: usize) -> String {
    let hi = offset + width - 1;
    let value_ports = value_ports(width);
    let value_expr = value_expr(width);
    format!(
        r#"
module Top (
    clk : input clock,
    rst : input reset,
    base: input logic<{total_width}>,
{value_ports}
    o   : output logic<{total_width}>
) {{
    var wide: logic<{total_width}>;

    always_ff {{
        if_reset {{
            wide = 0;
        }} else {{
            wide = base;
            wide[{hi}:{offset}] = {value_expr};
        }}
    }}

    assign o = wide;
}}
"#
    )
}

fn four_state_comb_store_source(offset: usize, width: usize, total_width: usize) -> String {
    let hi = offset + width - 1;
    let value_ports = value_ports(width);
    let value_expr = value_expr(width);
    format!(
        r#"
module Top (
    base: input logic<{total_width}>,
{value_ports}
    o   : output logic<{total_width}>
) {{
    var wide: logic<{total_width}>;

    always_comb {{
        wide = base;
        wide[{hi}:{offset}] = {value_expr};
        o = wide;
    }}
}}
"#
    )
}

fn assert_optimized_sir_has_static_store(code: &str, top: &str, offset: usize, width: usize) {
    let trace = SimulatorBuilder::new(code, top)
        .optimize(true)
        .trace_post_optimized_sir()
        .build_with_trace()
        .trace;
    let sir = trace
        .format_post_optimized_sir()
        .expect("missing post-optimized SIR");
    let found = sir.lines().any(|line| {
        line.contains("Store(")
            && line.contains(&format!("offset={offset}"))
            && line.contains(&format!("bits={width}"))
    });

    assert!(
        found,
        "post-optimized SIR did not contain Store(offset={offset}, bits={width})"
    );
}

fn set_comb_inputs<B: SimBackend>(
    sim: &mut Simulator<B>,
    width: usize,
    base: &BigUint,
    val: &BigUint,
) {
    let base_sig = sim.signal("base");
    sim.modify(|io| io.set_wide(base_sig, base.clone()))
        .unwrap();
    set_value_inputs(sim, width, val);
}

fn tick_ff_case<B: SimBackend>(
    sim: &mut Simulator<B>,
    width: usize,
    base: &BigUint,
    val: &BigUint,
) {
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let base_sig = sim.signal("base");
    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set_wide(base_sig, base.clone());
    })
    .unwrap();
    set_value_inputs(sim, width, val);
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(rst, 1u8)).unwrap();
    sim.tick(clk).unwrap();
}

fn set_value_inputs<B: SimBackend>(sim: &mut Simulator<B>, width: usize, val: &BigUint) {
    let mut bit_pos = 0usize;
    let chunks = chunk_widths(width);
    for (idx, chunk_width) in chunks.iter().copied().enumerate() {
        let sig_name = if chunks.len() == 1 {
            "val".to_string()
        } else {
            format!("p{idx}")
        };
        let sig = sim.signal(&sig_name);
        let chunk = (val >> bit_pos) & mask(chunk_width);
        sim.modify(|io| io.set_wide(sig, chunk)).unwrap();
        bit_pos += chunk_width;
    }
}

fn set_four_state_comb_inputs<B: SimBackend>(
    sim: &mut Simulator<B>,
    width: usize,
    base: &BigUint,
    val: &BigUint,
    val_mask: &BigUint,
) {
    let base_sig = sim.signal("base");
    sim.modify(|io| io.set_wide(base_sig, base.clone()))
        .unwrap();

    let mut bit_pos = 0usize;
    let chunks = chunk_widths(width);
    for (idx, chunk_width) in chunks.iter().copied().enumerate() {
        let sig_name = if chunks.len() == 1 {
            "val".to_string()
        } else {
            format!("p{idx}")
        };
        let sig = sim.signal(&sig_name);
        let chunk = (val >> bit_pos) & mask(chunk_width);
        let chunk_mask = (val_mask >> bit_pos) & mask(chunk_width);
        sim.modify(|io| io.set_four_state(sig, chunk, chunk_mask))
            .unwrap();
        bit_pos += chunk_width;
    }
}

fn run_comb_store_case(offset: usize, width: usize, base: BigUint, val: BigUint) {
    let total_width = case_total_width(offset, width);
    let code = comb_store_source(offset, width, total_width);
    let expected = expected_replace(total_width, offset, width, &base, &val);

    let mut native = SimulatorBuilder::new(&code, "Top")
        .optimize(true)
        .build_native()
        .unwrap();
    let mut cranelift = SimulatorBuilder::new(&code, "Top")
        .optimize(true)
        .build_cranelift()
        .unwrap();

    set_comb_inputs(&mut native, width, &base, &val);
    set_comb_inputs(&mut cranelift, width, &base, &val);

    let native_o: BigUint = native.get(native.signal("o"));
    let cranelift_o: BigUint = cranelift.get(cranelift.signal("o"));
    assert_eq!(
        native_o, cranelift_o,
        "native/cranelift store mismatch for offset={offset}, width={width}"
    );
    assert_eq!(
        native_o, expected,
        "store changed bits outside target range for offset={offset}, width={width}"
    );
}

fn run_ff_commit_case(offset: usize, width: usize, base: BigUint, val: BigUint) {
    let total_width = case_total_width(offset, width);
    let code = ff_commit_source(offset, width, total_width);
    let expected = expected_replace(total_width, offset, width, &base, &val);

    let mut native = SimulatorBuilder::new(&code, "Top")
        .optimize(true)
        .build_native()
        .unwrap();

    tick_ff_case(&mut native, width, &base, &val);

    let native_o: BigUint = native.get(native.signal("o"));
    assert_eq!(
        native_o, expected,
        "commit changed bits outside target range for offset={offset}, width={width}"
    );
}

fn run_four_state_comb_store_case(offset: usize, width: usize, val: BigUint, val_mask: BigUint) {
    let total_width = case_total_width(offset, width);
    let code = four_state_comb_store_source(offset, width, total_width);

    let mut native = SimulatorBuilder::new(&code, "Top")
        .optimize(true)
        .four_state(true)
        .build_native()
        .unwrap();
    let mut cranelift = SimulatorBuilder::new(&code, "Top")
        .optimize(true)
        .four_state(true)
        .build_cranelift()
        .unwrap();

    let base = BigUint::from(0u32);
    set_four_state_comb_inputs(&mut native, width, &base, &val, &val_mask);
    set_four_state_comb_inputs(&mut cranelift, width, &base, &val, &val_mask);

    let native_o = native.get_four_state(native.signal("o"));
    let cranelift_o = cranelift.get_four_state(cranelift.signal("o"));
    assert_eq!(
        native_o, cranelift_o,
        "native/cranelift four-state store mismatch for offset={offset}, width={width}"
    );
}

#[test]
fn static_store_offset_width_boundary_table_preserves_neighbors() {
    for offset in 0..8 {
        for &width in WIDTHS {
            let total_width = case_total_width(offset, width);
            run_comb_store_case(offset, width, BigUint::from(0u32), mask(width));
            if width <= 64 {
                run_comb_store_case(offset, width, mask(total_width), BigUint::from(0u32));
                run_comb_store_case(
                    offset,
                    width,
                    patterned_value(
                        0xfeed_face_0000_0000 | ((offset as u64) << 32) | width as u64,
                        total_width,
                    ),
                    patterned_value((offset as u64) << 32 | width as u64, width),
                );
            }
        }
    }
}

#[test]
fn static_commit_offset_width_boundary_table_preserves_neighbors() {
    for offset in 0..8 {
        for &width in WIDTHS {
            let total_width = case_total_width(offset, width);
            run_ff_commit_case(offset, width, BigUint::from(0u32), mask(width));
            if offset + width <= 64 {
                run_ff_commit_case(offset, width, mask(total_width), BigUint::from(0u32));
            }
        }
    }
}

#[test]
fn post_optimized_sir_keeps_boundary_store_and_commit_corpus() {
    let wide_unaligned = ff_commit_source(1, 99, case_total_width(1, 99));
    assert_optimized_sir_has_static_store(&wide_unaligned, "Top", 1, 99);

    let commit = ff_commit_source(7, 66, case_total_width(7, 66));
    assert_optimized_sir_has_static_store(&commit, "Top", 7, 66);
}

#[test]
fn sir_shape_boundary_fuzz_matches_cranelift() {
    for seed in 0..64u64 {
        let offset = (seed as usize * 5 + 1) % 8;
        let width = WIDTHS[(seed as usize * 7 + 3) % WIDTHS.len()];
        let val = patterned_value(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15), width);

        let base = if offset + width <= 64 {
            patterned_value(
                seed.wrapping_mul(0x517c_c1b7_2722_0a95),
                case_total_width(offset, width),
            )
        } else {
            BigUint::from(0u32)
        };

        run_comb_store_case(offset, width, base.clone(), val.clone());
        run_ff_commit_case(offset, width, base, val);
    }
}

#[test]
fn four_state_static_store_boundary_fuzz_matches_cranelift_masks() {
    for seed in 0..32u64 {
        let offset = (seed as usize * 3 + 1) % 8;
        let width = WIDTHS[(seed as usize * 5 + 5) % WIDTHS.len()];
        let val = patterned_value(seed.wrapping_mul(0xd1b5_4a32_d192_ed03), width);
        let val_mask = patterned_value(seed.wrapping_mul(0x94d0_49bb_1331_11eb), width);

        run_four_state_comb_store_case(offset, width, val, val_mask);
    }
}
