//! The backend ALUs must distinguish the two-state zero fallback from the
//! IEEE 1800-2023 11.4.3 four-state X result, in both comb and FF execution.
use celox::{BigUint, SimBackend, Simulator};

fn source(width: usize, signed: bool, constant_zero: bool) -> String {
    let sign = if signed { "signed " } else { "" };
    let divisor = if constant_zero {
        format!("{width}'d0")
    } else {
        "b".into()
    };
    format!(
        r#"
module Top (
    clk: input clock,
    rst: input reset,
    a: input {sign}logic<{width}>,
    b: input {sign}logic<{width}>,
    q: output {sign}logic<{width}>,
    r: output {sign}logic<{width}>,
    q_ff: output {sign}logic<{width}>,
    r_ff: output {sign}logic<{width}>,
) {{
    assign q = a / {divisor};
    assign r = a % {divisor};
    always_ff (clk, rst) {{
        if_reset {{
            q_ff = 0;
            r_ff = 0;
        }} else {{
            q_ff = a / {divisor};
            r_ff = a % {divisor};
        }}
    }}
}}
"#
    )
}

fn check<B: SimBackend>(
    mut sim: Simulator<B>,
    width: usize,
    signed: bool,
    four_state: bool,
    constant_zero: bool,
) {
    let a = sim.signal("a");
    let b = sim.signal("b");
    let rst = sim.signal("rst");
    let clk = sim.event("clk");
    let all = (BigUint::from(1u8) << width) - 1u8;
    let zero = BigUint::from(0u8);
    let encode = |n: i128| BigUint::from(n as u128) & &all;
    sim.modify(|io| io.set(rst, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(rst, 1u8)).unwrap();

    // Repeated transitions catch a stale mask or a permanently unknown output.
    let mut samples: Vec<_> = [
        (100, 7),
        (-100, 0),
        (0, 0),
        (100, 0),
        (-100, 7),
        (100, -7),
        (100, 1),
    ]
    .into_iter()
    .map(|(n, d)| {
        let lhs = encode(n);
        let rhs = encode(d);
        let result = if d == 0 || constant_zero {
            let bits = if four_state {
                all.clone()
            } else {
                zero.clone()
            };
            ((bits.clone(), bits.clone()), (bits.clone(), bits))
        } else {
            let (q, r) = if signed {
                (encode(n / d), encode(n % d))
            } else {
                (&lhs / &rhs, &lhs % &rhs)
            };
            ((q, zero.clone()), (r, zero.clone()))
        };
        (lhs, rhs, zero.clone(), result)
    })
    .collect();
    if !constant_zero {
        // A nonzero high word must not be mistaken for a zero divisor.
        samples.push((
            1u8.into(),
            BigUint::from(1u8) << (width - 1),
            zero.clone(),
            ((zero.clone(), zero.clone()), (1u8.into(), zero.clone())),
        ));
        if four_state {
            // A high Z bit with zero payload remains unknown, not known zero.
            samples.push((
                1u8.into(),
                zero.clone(),
                BigUint::from(1u8) << (width - 1),
                ((all.clone(), all.clone()), (all.clone(), all.clone())),
            ));
            samples.push((
                100u8.into(),
                7u8.into(),
                zero.clone(),
                ((14u8.into(), zero.clone()), (2u8.into(), zero.clone())),
            ));
        }
    }
    for (lhs, rhs, mask, (q, r)) in samples {
        sim.modify(|io| {
            io.set_wide(a, lhs.clone());
            io.set_four_state(b, rhs.clone(), mask.clone());
        })
        .unwrap();
        sim.tick(clk).unwrap();
        for (name, expected) in [("q", &q), ("r", &r), ("q_ff", &q), ("r_ff", &r)] {
            assert_eq!(
                &sim.get_four_state(sim.signal(name)),
                expected,
                "{name}: width={width}, signed={signed}, four_state={four_state}, constant_zero={constant_zero}, lhs={lhs}, rhs={rhs}, mask={mask}"
            );
        }
    }
}

macro_rules! backend_test {
    ($name:ident, $build:ident) => {
        #[test]
        fn $name() {
            for width in [8, 64, 65, 128] {
                for signed in [false, true] {
                    for four_state in [false, true] {
                        for constant_zero in [false, true] {
                            let code = source(width, signed, constant_zero);
                            let sim = Simulator::builder(&code, "Top")
                                .four_state(four_state)
                                .$build()
                                .unwrap();
                            check(sim, width, signed, four_state, constant_zero);
                        }
                    }
                }
            }
        }
    };
}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
backend_test!(native, build_native);
backend_test!(cranelift, build_cranelift);
backend_test!(wasm, build_wasm);
backend_test!(interpreter, build_interpreter);
