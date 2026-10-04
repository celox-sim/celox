//! Exercise resize boundaries and X/Z transport through comb and FF lowering.

use celox::{BigUint, SimBackend, Simulator};

#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

const RESIZE_WIDTHS: [usize; 5] = [32, 64, 96, 128, 192];
const CONTEXT_WIDTHS: [usize; 3] = [64, 96, 192];

fn extend(bits: u8, cast_width: usize, width: usize, signed: bool) -> BigUint {
    let mut result = BigUint::from(0u8);
    for bit in 0..width {
        if (bit < cast_width || signed) && bits & (1 << bit.min(cast_width - 1).min(7)) != 0 {
            result.set_bit(bit as u64, true);
        }
    }
    result
}

fn design(inputs: &str, expressions: &[(String, &str, usize)]) -> String {
    let ports = expressions
        .iter()
        .flat_map(|(name, _, width)| {
            [
                format!("o_{name}: output logic<{width}>"),
                format!("q_{name}: output logic<{width}>"),
            ]
        })
        .collect::<Vec<_>>()
        .join(",\n");
    let comb = expressions
        .iter()
        .map(|(name, expr, _)| format!("assign o_{name} = {expr};"))
        .collect::<Vec<_>>()
        .join("\n");
    let ff = expressions
        .iter()
        .map(|(name, expr, _)| format!("q_{name} = {expr};"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "module Top (clk: input clock, {inputs}, {ports}) {{ {comb} always_ff (clk) {{ {ff} }} }}"
    )
}

const RESIZE_CASES: [(&str, &str, usize, bool); 14] = [
    ("widened", "if c ? (a as 16) : 16'd0", 16, false),
    ("narrowed", "if c ? (a as 4) : 4'd0", 4, false),
    ("same_width", "if c ? (a as 8) : 8'd0", 8, false),
    ("signed_widened", "a as 16", 16, true),
    ("signed_narrowed", "a as 4", 4, true),
    ("wide_narrowed", "wide as 16", 16, true),
    ("medium_widened", "if c ? (a as 96) : 96'd0", 96, false),
    ("wide_widened", "if c ? (a as 192) : 192'd0", 192, false),
    ("signed_wide", "a as 192", 192, true),
    ("masked", "(a as 16) | 32'd0", 16, false),
    ("concatenated", "{a as 16}", 16, false),
    ("nested", "(a as 4) as 16", 4, true),
    ("sign_cast", "$signed(a)", 8, true),
    ("unsign_cast", "$unsigned(a)", 8, false),
];

fn resize_design() -> String {
    let expressions = RESIZE_WIDTHS
        .into_iter()
        .flat_map(|width| {
            RESIZE_CASES
                .into_iter()
                .map(move |(name, expr, _, _)| (format!("w{width}_{name}"), expr, width))
        })
        .collect::<Vec<_>>();
    design(
        "a: input signed logic<8>, wide: input signed logic<192>, c: input logic",
        &expressions,
    )
}

fn resize_cases<B: SimBackend>(sim: &mut Simulator<B>, four_state: bool) {
    let mut failures = Vec::new();
    let mut count = 0;
    for width in RESIZE_WIDTHS {
        let a = sim.signal("a");
        let wide = sim.signal("wide");
        let c = sim.signal("c");
        let clk = sim.event("clk");
        for (payload, mask) in [
            (0u8, 0u8),
            (1, 0),
            (0x7f, 0),
            (0x80, 0),
            (0xff, 0),
            (1, 2),
            (3, 2),
            (0x81, 2),
            (0x83, 2),
            (1, 0x80),
            (0x81, 0x80),
            (1, 8),
            (9, 8),
        ] {
            if !four_state && mask != 0 {
                continue;
            }
            for selected in [false, true] {
                sim.modify(|io| {
                    io.set_four_state(a, BigUint::from(payload), BigUint::from(mask));
                    io.set_four_state(
                        wide,
                        extend(payload, 192, 192, true),
                        extend(mask, 192, 192, true),
                    );
                    io.set(c, u8::from(selected));
                })
                .unwrap();
                sim.tick(clk).unwrap();
                for (name, expr, cast_width, signed) in &RESIZE_CASES {
                    let selected_value = selected || !expr.starts_with("if c");
                    let (p, m) = if selected_value {
                        (payload, mask)
                    } else {
                        (0, 0)
                    };
                    // Celox encodes X as (1,1), Z as (0,1); bitwise OR normalizes Z to X.
                    let p = if *name == "masked" { p | m } else { p };
                    let expected = (
                        extend(p, *cast_width, width, *signed),
                        extend(m, *cast_width, width, *signed),
                    );
                    for prefix in ["o", "q"] {
                        count += 1;
                        let signal = sim.signal(&format!("{prefix}_w{width}_{name}"));
                        let actual = sim.get_four_state(signal);
                        if actual != expected {
                            failures.push(format!("{prefix}_w{width}_{name}: width={width}, payload={payload:#x}, mask={mask:#x}, c={selected}, four_state={four_state}: actual={actual:x?}, expected={expected:x?}"));
                        }
                    }
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} failures in {count} checks:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

fn context_expressions(width: usize) -> [(&'static str, &'static str, usize); 24] {
    // Runtime operands keep constant folding out of the width and signedness checks.
    [
        ("unsigned_sum", "((query + s) as 8) + 32'd0", width),
        ("signed_sum", "((query + s) as 8) + 0", width),
        ("unsigned_less", "((query + s) as 8) <: 32'd256", 1),
        ("signed_less", "((query + s) as 8) <: -1", 1),
        ("branch", "(if c ? (s as 8) : (8'sd1 as 8)) + 32'd0", width),
        ("widened", "(s as 16) + 32'd0", width),
        ("signed_widened", "(s as 16) + 0", width),
        ("reinterpreted", "((s as u8) as i16) + 32'd0", width),
        ("bitnot", "(query + ~s) as 32", width),
        ("ternary", "if c ? s : query", width),
        ("unsigned_ternary", "(if c ? s : query) + 32'd0", width),
        ("cmp_lt", "(query <: s) <: (s <: query)", 1),
        ("cmp_le", "(query <= s) <: (s <: query)", 1),
        ("cmp_gt", "(query >: s) <: (s <: query)", 1),
        ("cmp_ge", "(query >= s) <: (s <: query)", 1),
        ("cmp_eq", "(query == s) <: (s <: query)", 1),
        ("cmp_ne", "(query != s) <: (s <: query)", 1),
        ("cmp_weq", "(query ==? s) <: (s <: query)", 1),
        ("cmp_wne", "(query !=? s) <: (s <: query)", 1),
        ("cmp_cast", "(query >: s) as 1", width),
        ("signed_div", "s / query", width),
        ("unsigned_div", "s / u", width),
        ("signed_rem", "s % query", width),
        ("unsigned_rem", "s % u", width),
    ]
}

fn context_design() -> String {
    let expressions = CONTEXT_WIDTHS
        .into_iter()
        .flat_map(|width| {
            context_expressions(width)
                .into_iter()
                .map(move |(name, expr, out_width)| (format!("w{width}_{name}"), expr, out_width))
        })
        .collect::<Vec<_>>();
    design(
        "s: input signed logic<8>, query: input signed logic<32>, u: input logic<32>, c: input logic",
        &expressions,
    )
}

fn context_cases<B: SimBackend>(sim: &mut Simulator<B>) {
    let mut failures = Vec::new();
    let mut count = 0;
    for width in CONTEXT_WIDTHS {
        let expressions = context_expressions(width);
        let s_signal = sim.signal("s");
        let query_signal = sim.signal("query");
        let u_signal = sim.signal("u");
        let c_signal = sim.signal("c");
        let clk = sim.event("clk");
        for query in [5i64, 129] {
            for s in [-128i64, -1, 0, 5, 6, 126, 127] {
                for c in [0u8, 1] {
                    sim.modify(|io| {
                        io.set(s_signal, s as u8);
                        io.set(query_signal, query as u32);
                        io.set(u_signal, query as u32);
                        io.set(c_signal, c);
                    })
                    .unwrap();
                    sim.tick(clk).unwrap();
                    let truncated = (query + s) as i8;
                    let cmp = |left: bool| i64::from(u8::from(left) < u8::from(s < query));
                    let expected_values = [
                        truncated as u8 as i64,
                        truncated as i64,
                        1,
                        i64::from(truncated < -1),
                        if c == 1 { s as u8 as i64 } else { 1 },
                        s as i16 as u16 as i64,
                        s,
                        s as u8 as i64,
                        query + !s,
                        if c == 1 { s } else { query },
                        if c == 1 { s as u8 as i64 } else { query },
                        cmp(query < s),
                        cmp(query <= s),
                        cmp(query > s),
                        cmp(query >= s),
                        cmp(query == s),
                        cmp(query != s),
                        cmp(query == s),
                        cmp(query != s),
                        i64::from(query > s),
                        s / query,
                        s as u8 as i64 / query,
                        s % query,
                        s as u8 as i64 % query,
                    ];
                    for ((name, _, out_width), expected) in expressions.iter().zip(expected_values)
                    {
                        let modulus = BigUint::from(1u8) << *out_width;
                        let expected = if expected < 0 {
                            &modulus - BigUint::from(expected.unsigned_abs())
                        } else {
                            BigUint::from(expected as u64)
                        };
                        for prefix in ["o", "q"] {
                            count += 1;
                            let signal = sim.signal(&format!("{prefix}_w{width}_{name}"));
                            let actual = sim.get_four_state(signal);
                            if actual != (expected.clone(), BigUint::from(0u8)) {
                                failures.push(format!("{prefix}_w{width}_{name}: width={width}, query={query}, s={s}, c={c}: actual={actual:x?}, expected={expected:x}"));
                            }
                        }
                    }
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} failures in {count} checks:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

const IDENTITY_CASES: [(&str, &str, usize); 17] = [
    ("or_zero", "a | 8'd0", 8),
    ("zero_or", "8'd0 | a", 8),
    ("xor_zero", "a ^ 8'd0", 8),
    ("zero_xor", "8'd0 ^ a", 8),
    ("and_self", "a & a", 8),
    ("or_self", "a | a", 8),
    ("and_ones", "a & 8'hff", 8),
    ("ones_and", "8'hff & a", 8),
    ("add_zero", "a + 8'd0", 8),
    ("zero_add", "8'd0 + a", 8),
    ("mul_one", "a * 8'd1", 8),
    ("one_mul", "8'd1 * a", 8),
    ("reduce_and", "&a[0]", 1),
    ("reduce_or", "|a[0]", 1),
    ("reduce_xor", "^a[0]", 1),
    ("concat", "{8'h5a, a}", 16),
    ("repeat", "{a repeat 2}", 16),
];

fn identity_design() -> String {
    let expressions = IDENTITY_CASES
        .into_iter()
        .map(|(name, expr, width)| (name.to_string(), expr, width))
        .collect::<Vec<_>>();
    design("a: input logic<8>", &expressions)
}

fn identity_cases<B: SimBackend>(sim: &mut Simulator<B>) {
    let a = sim.signal("a");
    let clk = sim.event("clk");
    for (payload, mask) in [
        (0u16, 0u16),
        (0xff, 0),
        (0x80, 0),
        (0, 1),
        (1, 1),
        (1, 2),
        (3, 2),
        (0, 0x80),
        (0x80, 0x80),
        (0, 0xff),
        (0xff, 0xff),
    ] {
        sim.modify(|io| io.set_four_state(a, BigUint::from(payload), BigUint::from(mask)))
            .unwrap();
        sim.tick(clk).unwrap();
        for (name, _, _) in &IDENTITY_CASES {
            let (expected_payload, expected_mask) = match *name {
                "add_zero" | "zero_add" | "mul_one" | "one_mul" if mask != 0 => (0xff, 0xff),
                "reduce_and" | "reduce_or" | "reduce_xor" => ((payload | mask) & 1, mask & 1),
                "concat" => (0x5a00 | payload, mask),
                "repeat" => ((payload << 8) | payload, (mask << 8) | mask),
                _ => (payload | mask, mask),
            };
            for prefix in ["o", "q"] {
                let signal = sim.signal(&format!("{prefix}_{name}"));
                assert_eq!(
                    sim.get_four_state(signal),
                    (
                        BigUint::from(expected_payload),
                        BigUint::from(expected_mask)
                    ),
                    "{prefix}_{name}, payload={payload:#x}, mask={mask:#x}"
                );
            }
        }
    }
}

// The SV frontend rejects size casts and four-state always_ff event signals.
all_backends! {
    fn resize_two_state_unoptimized(sim) {
        @omit_veryl;
        @setup { let code = resize_design(); }
        @build Simulator::builder(&code, "Top")
            .four_state(false)
            .optimize(false);

        resize_cases(&mut sim, false);
    }

    fn resize_two_state_optimized(sim) {
        @omit_veryl;
        @setup { let code = resize_design(); }
        @build Simulator::builder(&code, "Top")
            .four_state(false)
            .optimize(true);

        resize_cases(&mut sim, false);
    }

    fn resize_four_state_unoptimized(sim) {
        @omit_veryl;
        @ignore_on(sv);
        @setup { let code = resize_design(); }
        @build Simulator::builder(&code, "Top")
            .four_state(true)
            .optimize(false);

        resize_cases(&mut sim, true);
    }

    fn resize_four_state_optimized(sim) {
        @omit_veryl;
        @ignore_on(sv);
        @setup { let code = resize_design(); }
        @build Simulator::builder(&code, "Top")
            .four_state(true)
            .optimize(true);

        resize_cases(&mut sim, true);
    }

    fn context_two_state_unoptimized(sim) {
        @omit_veryl;
        @setup { let code = context_design(); }
        @build Simulator::builder(&code, "Top")
            .four_state(false)
            .optimize(false);

        context_cases(&mut sim);
    }

    fn context_two_state_optimized(sim) {
        @omit_veryl;
        @setup { let code = context_design(); }
        @build Simulator::builder(&code, "Top")
            .four_state(false)
            .optimize(true);

        context_cases(&mut sim);
    }

    fn context_four_state_unoptimized(sim) {
        @omit_veryl;
        @ignore_on(sv);
        @setup { let code = context_design(); }
        @build Simulator::builder(&code, "Top")
            .four_state(true)
            .optimize(false);

        context_cases(&mut sim);
    }

    fn context_four_state_optimized(sim) {
        @omit_veryl;
        @ignore_on(sv);
        @setup { let code = context_design(); }
        @build Simulator::builder(&code, "Top")
            .four_state(true)
            .optimize(true);

        context_cases(&mut sim);
    }

    fn identity_four_state_unoptimized(sim) {
        @omit_veryl;
        @ignore_on(sv);
        @setup { let code = identity_design(); }
        @build Simulator::builder(&code, "Top")
            .four_state(true)
            .optimize(false);

        identity_cases(&mut sim);
    }

    fn identity_four_state_optimized(sim) {
        @omit_veryl;
        @ignore_on(sv);
        @setup { let code = identity_design(); }
        @build Simulator::builder(&code, "Top")
            .four_state(true)
            .optimize(true);

        identity_cases(&mut sim);
    }
}
