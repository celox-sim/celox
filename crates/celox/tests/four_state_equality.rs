use celox::{BigUint, Simulator};

#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

// Payload/mask pairs use Celox's encoding: X=(1,1), Z=(0,1).
fn cases(
    lhs_width: usize,
    rhs_width: usize,
) -> Vec<(BigUint, BigUint, BigUint, BigUint, Option<bool>)> {
    let zero = BigUint::from(0u8);
    let one = BigUint::from(1u8);
    let mut cases = vec![
        (
            zero.clone(),
            zero.clone(),
            zero.clone(),
            zero.clone(),
            Some(true),
        ),
        (
            one.clone(),
            zero.clone(),
            zero.clone(),
            zero.clone(),
            Some(false),
        ),
    ];
    for payload in [zero.clone(), one.clone()] {
        cases.extend([
            (
                payload.clone(),
                one.clone(),
                zero.clone(),
                zero.clone(),
                None,
            ),
            (
                zero.clone(),
                zero.clone(),
                payload.clone(),
                one.clone(),
                None,
            ),
            (
                payload.clone(),
                one.clone(),
                payload.clone(),
                one.clone(),
                None,
            ),
        ]);
        if lhs_width.min(rhs_width) > 1 {
            let high = &one << (lhs_width.min(rhs_width) - 1);
            cases.extend([
                (
                    payload.clone(),
                    one.clone(),
                    high.clone(),
                    zero.clone(),
                    Some(false),
                ),
                (
                    high.clone(),
                    zero.clone(),
                    payload.clone(),
                    one.clone(),
                    Some(false),
                ),
                (
                    payload.clone() * &high,
                    high.clone(),
                    one.clone(),
                    zero.clone(),
                    Some(false),
                ),
                (
                    one.clone(),
                    zero.clone(),
                    payload.clone() * &high,
                    high.clone(),
                    Some(false),
                ),
                (
                    &high | &payload,
                    one.clone(),
                    zero.clone(),
                    zero.clone(),
                    Some(false),
                ),
            ]);
        }
        // A mismatch in the zero-extended portion still dominates the narrow
        // operand's unknown bit. This also crosses a machine-word boundary.
        if lhs_width != rhs_width {
            let high = &one << (lhs_width.max(rhs_width) - 1);
            if lhs_width > rhs_width {
                cases.push((high, zero.clone(), payload, one.clone(), Some(false)));
            } else {
                cases.push((payload, one.clone(), high, zero.clone(), Some(false)));
            }
        }
    }
    cases
}

macro_rules! equality_case {
    ($name:ident, $lhs_width:literal, $rhs_width:literal, $signed:literal, $ff:literal $(, $ignore:ident)*) => {
        all_backends! {
            fn $name(sim) {
                @ignore_on($($ignore),*);
                @setup {
                    let code = format!(r#"
                        module Top (
                            clk: input clock,
                            a: input {signed} logic<{lw}>,
                            b: input {signed} logic<{rw}>,
                            eq: output logic, ne: output logic,
                            q_eq: output logic, q_ne: output logic,
                        ) {{
                            assign eq = a == b;
                            assign ne = a != b;
                            {registered}

                        }}
                    "#, signed = $signed, lw = $lhs_width, rw = $rhs_width,
                        registered = if $ff {
                            "always_ff (clk) { q_eq = a == b; q_ne = a != b; }"
                        } else {
                            "assign q_eq = eq; assign q_ne = ne;"
                        });
                }
                @build Simulator::builder(&code, "Top").four_state(true);
                let a = sim.signal("a");
                let b = sim.signal("b");
                let eq = sim.signal("eq");
                let ne = sim.signal("ne");
                let q_eq = sim.signal("q_eq");
                let q_ne = sim.signal("q_ne");
                for (index, (av, am, bv, bm, expected)) in cases($lhs_width, $rhs_width).into_iter().enumerate() {
                    sim.modify(|io| {
                        io.set_four_state(a, av, am);
                        io.set_four_state(b, bv, bm);
                    }).unwrap();
                    if $ff {
                        let clk = sim.event("clk");
                        sim.tick(clk).unwrap();
                    }
                    for (signal, inverted) in [(eq, false), (ne, true), (q_eq, false), (q_ne, true)] {
                        let (value, mask) = sim.get_four_state(signal);
                        assert_eq!(mask, BigUint::from(u8::from(expected.is_none())), "case {index}");
                        if let Some(equal) = expected {
                            assert_eq!(value, BigUint::from(u8::from(equal ^ inverted)), "case {index}");
                        }
                    }
                }
            }
        }
    };
}

equality_case!(equality_1, 1, 1, "", false);
equality_case!(equality_8, 8, 8, "", false);
equality_case!(equality_64, 64, 64, "", false);
equality_case!(equality_65, 65, 65, "", false);
equality_case!(equality_130, 130, 130, "", false);
equality_case!(equality_narrow_wide, 8, 130, "", false);
equality_case!(equality_wide_narrow, 130, 8, "", false);
equality_case!(equality_signed_8, 8, 8, "signed", false);
equality_case!(equality_signed_130, 130, 130, "signed", false);

// SV four-state event signals are not yet supported by the SV frontend.
equality_case!(equality_ff_8, 8, 8, "", true, sv);
equality_case!(equality_ff_130, 130, 130, "", true, sv);
