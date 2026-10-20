//! Arbitrary-width bit-vectors and unsigned division/remainder.
use super::*;
use lydite_ir::{and, bv, bv_big, eq, node, not, var};

fn check(t: Term) -> Outcome {
    let proof = solve(&t, &Env::new(), Limits::default());
    let witness = solve_with_hint(&t, &Env::new(), Limits::default(), SearchHint::Sat);
    assert_eq!(proof.verdict, witness.verdict, "{:?}", proof.reason);
    assert_eq!(
        proof.original_formula_validated,
        witness.original_formula_validated
    );
    proof
}
fn w(name: &str, width: u32) -> Term {
    var(name.into(), Sort::Bv(width))
}
fn op(name: &str, a: Term, b: Term) -> Term {
    let sort = a.0.sort.clone();
    node(sort, name, vec![a, b])
}
fn or(a: Term, b: Term) -> Term {
    node(Sort::Bool, "or", vec![a, b])
}
fn big(text: &str) -> BigUint {
    text.parse().unwrap()
}
fn zext(t: Term, n: u32) -> Term {
    let Sort::Bv(width) = t.0.sort else {
        unreachable!()
    };
    node(Sort::Bv(width + n), format!("(_ zero_extend {n})"), vec![t])
}
fn eval1(t: &Term, inputs: &[(&str, Scalar)]) -> Scalar {
    let assignments = inputs
        .iter()
        .map(|(n, v)| (n.to_string(), v.clone()))
        .collect();
    evaluate_scalar_terms(
        &Env::from([("v".into(), t.clone())]),
        &assignments,
        Limits::default(),
    )
    .unwrap()["v"]
        .clone()
}

#[test]
fn evaluator_division_matches_smtlib_semantics() {
    for width in 1..=6u32 {
        let (x, y) = (w("x", width), w("y", width));
        let ones = (1u64 << width) - 1;
        for a in 0..1u64 << width {
            for b in 0..1u64 << width {
                let inputs = [("x", Scalar::bv(width, a)), ("y", Scalar::bv(width, b))];
                let (q, r) = (
                    a.checked_div(b).unwrap_or(ones),
                    a.checked_rem(b).unwrap_or(a),
                );
                let udiv = eval1(&op("bvudiv", x.clone(), y.clone()), &inputs);
                let urem = eval1(&op("bvurem", x.clone(), y.clone()), &inputs);
                assert_eq!(udiv, Scalar::bv(width, q), "{a} udiv {b} width {width}");
                assert_eq!(urem, Scalar::bv(width, r), "{a} urem {b} width {width}");
            }
        }
    }
}

/// Symbolic, so no constant folding can bypass the division circuit.
#[test]
fn division_encoding_satisfies_smtlib_specification() {
    for width in 1..=6u32 {
        let (x, y) = (w("x", width), w("y", width));
        let q = op("bvudiv", x.clone(), y.clone());
        let r = op("bvurem", x.clone(), y.clone());
        let ones = bv(width, u64::MAX);
        let zero = bv(width, 0);
        // y = 0 -> q = ones /\ r = x ; y != 0 -> x = q*y + r /\ r < y (in 2w bits).
        let by_zero = and(eq(q.clone(), ones), eq(r.clone(), x.clone()));
        let product = op(
            "bvadd",
            op("bvmul", zext(q.clone(), width), zext(y.clone(), width)),
            zext(r.clone(), width),
        );
        let nonzero = and(
            eq(product, zext(x.clone(), width)),
            node(Sort::Bool, "bvult", vec![r.clone(), y.clone()]),
        );
        let spec = and(
            or(not(eq(y.clone(), zero.clone())), by_zero),
            or(eq(y.clone(), zero), nonzero),
        );
        let result = check(not(spec));
        assert_eq!(result.verdict, Verdict::Unsat, "width {width}");
    }
}

/// Concrete operands, compared against Rust arithmetic in both directions:
/// UNSAT for the wrong-result negation and a validated SAT model for the result.
#[test]
fn division_encoding_agrees_with_rust_arithmetic() {
    for width in 1..=4u32 {
        let (x, y, z) = (w("x", width), w("y", width), w("z", width));
        let ones = (1u64 << width) - 1;
        for a in 0..1u64 << width {
            for b in 0..1u64 << width {
                let (q, r) = (
                    a.checked_div(b).unwrap_or(ones),
                    a.checked_rem(b).unwrap_or(a),
                );
                let operands = and(eq(x.clone(), bv(width, a)), eq(y.clone(), bv(width, b)));
                for (name, expected) in [("bvudiv", q), ("bvurem", r)] {
                    let e = op(name, x.clone(), y.clone());
                    let negated = and(operands.clone(), not(eq(e.clone(), bv(width, expected))));
                    assert_eq!(check(negated).verdict, Verdict::Unsat, "{name} {a} {b}");
                    let model = check(and(operands.clone(), eq(z.clone(), e)));
                    assert_eq!(model.verdict, Verdict::Sat);
                    assert!(model.original_formula_validated);
                    assert_eq!(model.assignments["z"], Scalar::bv(width, expected));
                }
            }
        }
    }
}

#[test]
fn wide_increment_never_fixes_a_value() {
    let x = w("x", 128);
    let result = check(eq(op("bvadd", x.clone(), bv(128, 1)), x));
    assert_eq!(result.verdict, Verdict::Unsat);
}

#[test]
fn wide_multiplication_model_is_validated() {
    let x = w("x", 128);
    let target = big("123456789012345678901234567890123456789");
    let result = check(eq(op("bvmul", x, bv(128, 3)), bv_big(128, &target)));
    assert_eq!(result.verdict, Verdict::Sat, "{:?}", result.reason);
    assert!(result.original_formula_validated);
    let Scalar::Bv { width: 128, value } = &result.assignments["x"] else {
        panic!("128-bit model")
    };
    assert_eq!((value * 3u8) % (BigUint::from(1u8) << 128), target);
    assert_eq!(
        result.assignments["x"].json()["value"],
        json!(value.to_string())
    );
}

#[test]
fn wide_concat_extract_round_trip() {
    let (x, y) = (w("x", 100), w("y", 28));
    let c = node(Sort::Bv(128), "concat", vec![x.clone(), y.clone()]);
    let hi = node(Sort::Bv(100), "(_ extract 127 28)", vec![c.clone()]);
    let lo = node(Sort::Bv(28), "(_ extract 27 0)", vec![c]);
    let result = check(not(and(eq(hi, x), eq(lo, y))));
    assert_eq!(result.verdict, Verdict::Unsat);
}

/// Symbolic x = (x udiv y) * y + (x urem y) for y != 0. Plain CDCL has no
/// word-level multiplier reasoning, so the symbolic identity is only tractable
/// at small widths (8 bits: ~9M work; 12 bits: ~1.4G; 16+ bits exhausts it).
#[test]
fn division_identity_symbolic() {
    let width = 8;
    let (x, y) = (w("x", width), w("y", width));
    let q = op("bvudiv", x.clone(), y.clone());
    let r = op("bvurem", x.clone(), y.clone());
    let recomposed = op("bvadd", op("bvmul", q, y.clone()), r);
    let result = check(and(not(eq(y, bv(width, 0))), not(eq(recomposed, x))));
    assert_eq!(result.verdict, Verdict::Unsat, "{:?}", result.reason);
}

/// 96-bit operands above 2^64: the SAT model of quotient/remainder comes from
/// the division circuit and is validated by the evaluator; the negations are UNSAT.
#[test]
fn wide_division_concrete_operands() {
    let width = 96;
    let modulus = BigUint::from(1u8) << width;
    let ones = &modulus - 1u8;
    let a = big("61897001964269013744956211");
    let cases = [
        (a.clone(), big("1180591620717411316023")),
        (a.clone(), big("3")),
        (a.clone(), &a + 1u8),
        (a.clone(), BigUint::zero()),
    ];
    let (x, y) = (w("x", width), w("y", width));
    let (zq, zr) = (w("q", width), w("r", width));
    for (dividend, divisor) in cases {
        let (q, r) = if divisor.is_zero() {
            (ones.clone(), dividend.clone())
        } else {
            (&dividend / &divisor, &dividend % &divisor)
        };
        let operands = and(
            eq(x.clone(), bv_big(width, &dividend)),
            eq(y.clone(), bv_big(width, &divisor)),
        );
        let udiv = op("bvudiv", x.clone(), y.clone());
        let urem = op("bvurem", x.clone(), y.clone());
        let model = check(and(
            operands.clone(),
            and(eq(zq.clone(), udiv.clone()), eq(zr.clone(), urem.clone())),
        ));
        assert_eq!(model.verdict, Verdict::Sat, "{:?}", model.reason);
        assert!(model.original_formula_validated);
        assert_eq!(model.assignments["q"], Scalar::bv_big(width, q.clone()));
        assert_eq!(model.assignments["r"], Scalar::bv_big(width, r.clone()));
        let wrong = and(
            operands,
            or(
                not(eq(udiv, bv_big(width, &q))),
                not(eq(urem, bv_big(width, &r))),
            ),
        );
        assert_eq!(check(wrong).verdict, Verdict::Unsat);
    }
}

#[test]
fn constants_above_two_to_the_64() {
    let x = w("x", 80);
    let c = big("1180591620717411303429"); // 2^70 + 5
    let high = &BigUint::from(1u8) << 79u32;
    let sum = op("bvadd", x.clone(), bv_big(80, &high));
    assert_eq!(
        check(and(
            eq(x.clone(), bv_big(80, &c)),
            not(eq(sum.clone(), bv_big(80, &(&c + &high))))
        ))
        .verdict,
        Verdict::Unsat
    );
    let result = check(eq(sum, bv_big(80, &(&c + &high))));
    assert_eq!(result.verdict, Verdict::Sat);
    assert!(result.original_formula_validated);
    assert_eq!(result.assignments["x"], Scalar::bv_big(80, c.clone()));
    assert_eq!(
        result.assignments["x"].json(),
        json!({"sort":"Bv","width":80,"value":c.to_string()})
    );
    assert_eq!(result.assignments["x"].smt(), format!("(_ bv{c} 80)"));
    // Wrap-around, subtraction and shifts beyond 64 bits.
    let y = w("y", 130);
    let all = (BigUint::from(1u8) << 130u32) - 1u8;
    let shifted = op("bvshl", y.clone(), bv(130, 100));
    let back = op("bvlshr", shifted.clone(), bv(130, 100));
    let below = op("bvsub", bv(130, 0), bv(130, 1));
    assert_eq!(eval1(&below, &[]), Scalar::bv_big(130, all.clone()));
    assert_eq!(
        eval1(&back, &[("y", Scalar::bv_big(130, all.clone()))]),
        Scalar::bv(130, (1u64 << 30) - 1)
    );
    assert_eq!(
        check(not(eq(below, bv_big(130, &all)))).verdict,
        Verdict::Unsat
    );
    // y << 100 >> 100 keeps exactly the low 30 bits.
    let low = node(Sort::Bv(30), "(_ extract 29 0)", vec![y.clone()]);
    assert_eq!(check(not(eq(back, zext(low, 100)))).verdict, Verdict::Unsat);
    // Signed comparison and sign extension use bit width-1, not bit 63.
    let negative = bv_big(70, &(BigUint::from(1u8) << 69u32));
    assert_eq!(
        eval1(
            &node(Sort::Bool, "bvslt", vec![negative.clone(), bv(70, 0)]),
            &[]
        ),
        Scalar::Bool(true)
    );
    let extended = node(Sort::Bv(100), "(_ sign_extend 30)", vec![negative]);
    let expected = (BigUint::from(1u8) << 100u32) - (BigUint::from(1u8) << 69u32);
    assert_eq!(eval1(&extended, &[]), Scalar::bv_big(100, expected));
}

#[test]
fn width_limit_and_malformed_wide_constants() {
    let max = w("x", MAX_WIDTH);
    assert_eq!(check(not(eq(max.clone(), max))).verdict, Verdict::Unsat);
    let over = w("x", MAX_WIDTH + 1);
    assert_eq!(check(not(eq(over.clone(), over))).verdict, Verdict::Unknown);
    for text in ["(_ bv+5 8)", "(_ bv1_0 8)", "(_ bv 8)", "(_ bv-1 8)"] {
        let x = w("x", 8);
        let malformed = node(Sort::Bv(8), text, vec![]);
        assert_eq!(check(eq(x, malformed)).verdict, Verdict::Unknown, "{text}");
    }
    // Out-of-range decimal constants are reduced modulo 2^w, as before.
    let wrapped = node(Sort::Bv(8), "(_ bv300 8)", vec![]);
    assert_eq!(eval1(&wrapped, &[]), Scalar::bv(8, 44));
}

#[test]
fn scalar_json_values_round_trip() {
    for (width, value) in [
        (8, BigUint::from(255u8)),
        (64, BigUint::from(u64::MAX)),
        (65, BigUint::from(u64::MAX) + 1u8),
        (
            200,
            big("1606938044258990275541962092341162602522202993782792835301375"),
        ),
    ] {
        let scalar = Scalar::bv_big(width, value);
        let json = scalar.json_value();
        assert_eq!(json.is_string(), width > 64);
        assert_eq!(Scalar::bv_from_json(width, &json).unwrap(), scalar);
    }
    assert!(Scalar::bv_from_json(8, &json!(256)).is_err());
    assert!(Scalar::bv_from_json(70, &json!("1180591620717411303424")).is_err());
    assert!(Scalar::bv_from_json(70, &json!("+1")).is_err());
    assert!(Scalar::bv_from_json(70, &json!(-1)).is_err());
}
