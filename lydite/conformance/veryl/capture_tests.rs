use super::*;
use crate::{Category, Factory, Simulator};
use capture_macros::capture_body;
fn row(run: fn(&mut Factory<'_>)) -> Value {
    let case = TestCase {
        name: "synthetic::capture_contract",
        category: Category::Regression,
        expectation: Expectation::Simulation,
        run,
    };
    run_case(&case)
}
fn sim(factory: &mut Factory<'_>) -> Simulator {
    Simulator::new(factory(&Design::new("module T {}", "T")).unwrap())
}
#[test]
fn expected_borrows_and_loops_are_preserved() {
    let r = row(|f| {
        let mut s = sim(f);
        let x = s.signal("x");
        capture_body!({
            let base = BigUint::from(11u8);
            for i in 0..3u8 {
                assert_eq!(s.get(x), &base + BigUint::from(i));
            }
            assert_eq!(s.get(x), base);
            assert_eq!(s.get(x), base);
        });
    });
    assert_eq!(r["status"], "extracted");
    let payloads: Vec<_> = r["actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["payload"].as_str().unwrap())
        .collect();
    assert_eq!(payloads, vec!["11", "12", "13", "11", "11"]);
}
#[test]
fn scalar_projection_and_signed_bits_are_explicit() {
    let r = row(|f| {
        let mut s = sim(f);
        let x = s.signal("x");
        capture_body!({
            assert_eq!(s.get_as::<i8>(x), -2);
            assert_eq!(s.get_as::<bool>(x), true);
        });
    });
    assert_eq!(r["actions"][0]["payload"], "254");
    assert_eq!(r["actions"][0]["assertion"]["scalar_width"], 8);
    assert_eq!(r["actions"][1]["assertion"]["scalar_width"], 1);
}
#[test]
fn borrowed_four_state_tuple_and_inline_signal_work() {
    let r = row(|f| {
        let mut s = sim(f);
        capture_body!({
            let expected = (BigUint::from(5u8), BigUint::from(3u8));
            assert_eq!(&s.get_four_state(s.signal("x")), &expected);
            assert_eq!(&s.get_four_state(s.signal("x")), &expected);
        });
    });
    assert_eq!(r["status"], "extracted");
    assert_eq!(r["actions"][0]["assertion"]["mask_constraint"], "3");
}
#[test]
fn reversed_operand_and_inequality_keep_comparison() {
    let r = row(|f| {
        let mut s = sim(f);
        let x = s.signal("x");
        capture_body!({
            assert_eq!(BigUint::from(4u8), s.get(x));
            assert_ne!(s.get(x), 7u8.into());
        });
    });
    assert_eq!(r["actions"][0]["payload"], "4");
    assert_eq!(r["actions"][1]["assertion"]["comparison"], "ne");
}
#[test]
fn original_operand_evaluation_order_is_preserved() {
    let r = row(|f| {
        let mut s = sim(f);
        let x = s.signal("x");
        capture_body!({
            assert_eq!(s.get(x), {
                s.set(x, 3u8);
                BigUint::from(3u8)
            });
            assert_eq!(
                {
                    s.set(x, 4u8);
                    BigUint::from(4u8)
                },
                s.get(x)
            );
        });
    });
    let actions = r["actions"].as_array().unwrap();
    assert_eq!(
        actions
            .iter()
            .map(|x| x["action"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["read", "write", "write", "read"]
    );
}
#[test]
fn diagnostic_reads_are_not_eagerly_executed() {
    let r = row(|f| {
        let mut s = sim(f);
        let x = s.signal("x");
        capture_body!({
            assert_eq!(s.get(x), 0u8.into(), "diagnostic {:?}", s.get(x));
        });
    });
    assert_eq!(r["status"], "extracted");
    assert_eq!(r["assertion_count"], 1);
}
fn is_unsupported(r: &Value) {
    assert_eq!(r["status"], "unsupported");
    assert!(r.get("actions").is_none());
    assert!(
        r["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["code"] == "uncaptured_actual_read")
    );
}
#[test]
fn actual_dependent_control_flow_is_rejected() {
    let r = row(|f| {
        let mut s = sim(f);
        let x = s.signal("x");
        capture_body!({
            if s.get_as::<u8>(x) == 0 {
                s.set(x, 1u8);
            }
            assert_eq!(s.get(x), 1u8.into());
        });
    });
    is_unsupported(&r);
}
#[test]
fn actual_derived_expected_values_are_rejected() {
    let r = row(|f| {
        let mut s = sim(f);
        let x = s.signal("x");
        capture_body!({
            let expected = s.get(x);
            assert_eq!(s.get(x), expected);
        });
    });
    is_unsupported(&r);
}
#[test]
fn actual_vs_actual_and_expected_hidden_reads_are_rejected() {
    let r = row(|f| {
        let mut s = sim(f);
        let x = s.signal("x");
        capture_body!({
            assert_eq!(s.get(x), s.get(x));
        });
    });
    is_unsupported(&r);
    let r = row(|f| {
        let mut s = sim(f);
        let x = s.signal("x");
        capture_body!({
            assert_eq!(s.get(x), { s.get(x) });
        });
    });
    is_unsupported(&r);
}
#[test]
fn custom_macro_hidden_reads_are_rejected() {
    let r = row(|f| {
        let mut s = sim(f);
        let x = s.signal("x");
        capture_body!({
            macro_rules! hidden {
                () => {
                    s.get(x)
                };
            }
            assert_eq!(hidden!(), BigUint::from(0u8));
        });
    });
    is_unsupported(&r);
}
#[test]
fn swallowed_read_failure_still_invalidates_case() {
    let r = row(|f| {
        let mut s = sim(f);
        let x = s.signal("x");
        capture_body!({
            let _ = catch_unwind(AssertUnwindSafe(|| s.get(x)));
            assert_eq!(s.get(x), 0u8.into());
        });
    });
    is_unsupported(&r);
}
#[test]
fn stimulus_from_actual_is_rejected() {
    let r = row(|f| {
        let mut s = sim(f);
        let x = s.signal("x");
        capture_body!({
            let v = s.get_as::<u8>(x);
            s.set(x, v);
            assert_eq!(s.get(x), 0u8.into());
        });
    });
    is_unsupported(&r);
}
#[test]
fn adjacent_immutable_alias_is_captured_at_sample_time() {
    let r = row(|f| {
        let mut s = sim(f);
        let x = s.signal("x");
        capture_body!({
            let result = s.get(x);
            assert_eq!(result, {
                s.set(x, 8u8);
                BigUint::from(5u8)
            });
        });
    });
    assert_eq!(r["status"], "extracted");
    assert_eq!(r["actions"][0]["action"], "read");
    assert_eq!(r["actions"][1]["action"], "write");
    assert_eq!(
        r["actions"][0]["assertion"]["actual_form"],
        "adjacent_immutable_alias"
    );
    assert!(
        r["actions"][0]["assertion"]["sample_location"]["line"]
            .as_u64()
            .unwrap()
            < r["actions"][0]["assertion"]["location"]["line"]
                .as_u64()
                .unwrap()
    );
}
#[test]
fn reversed_alias_still_samples_before_expected_operand() {
    let r = row(|f| {
        let mut s = sim(f);
        let x = s.signal("x");
        capture_body!({
            let result = s.get(x);
            assert_eq!(
                {
                    s.set(x, 8u8);
                    BigUint::from(5u8)
                },
                result
            );
        });
    });
    assert_eq!(r["status"], "extracted");
    assert_eq!(r["actions"][0]["action"], "read");
    assert_eq!(r["actions"][1]["action"], "write");
}
#[test]
fn aliases_are_not_moved_across_events() {
    let r = row(|f| {
        let mut s = sim(f);
        let x = s.signal("x");
        capture_body!({
            let result = s.get(x);
            s.eval_comb().unwrap();
            assert_eq!(result, 5u8.into());
        });
    });
    is_unsupported(&r);
}
#[test]
fn reused_and_mutable_aliases_remain_unsupported() {
    let r = row(|f| {
        let mut s = sim(f);
        let x = s.signal("x");
        capture_body!({
            let result = s.get(x);
            assert_eq!(result, 5u8.into());
            assert_eq!(result, 6u8.into());
        });
    });
    is_unsupported(&r);
    let r = row(|f| {
        let mut s = sim(f);
        let x = s.signal("x");
        capture_body!({
            let mut result = s.get(x);
            assert_eq!(result, 5u8.into());
            result += 1u8;
            assert_eq!(result, 6u8.into());
        });
    });
    is_unsupported(&r);
}
#[test]
fn scalar_alias_can_capture_nonzero_predicate() {
    let r = row(|f| {
        let mut s = sim(f);
        let x = s.signal("x");
        capture_body!({
            let result = s.get_as::<u8>(x);
            assert_ne!(result, 0);
        });
    });
    assert_eq!(r["status"], "extracted");
    assert_eq!(r["actions"][0]["assertion"]["comparison"], "ne");
}
#[test]
fn alias_used_only_in_diagnostics_is_not_discarded() {
    let r = row(|f| {
        let mut s = sim(f);
        let x = s.signal("x");
        capture_body!({
            let result = s.get(x);
            assert_eq!(s.get(x), 0u8.into(), "{:?}", result);
        });
    });
    is_unsupported(&r);
}
