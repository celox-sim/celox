//! Regressions for the test harness itself, independent of Celox backends.
#[path = "test_utils/veryl_sim.rs"]
mod veryl_sim;

use std::path::Path;
use veryl_sim::build_veryl_adapter;

#[test]
fn scalar_writes_preserve_all_128_bits() {
    let code = r#"
        module Top (a: input logic<128>, y: output logic<128>) {
            assign y = a;
        }
    "#;
    let mut sim = build_veryl_adapter(&[(code, Path::new("test.veryl"))], "Top", false);
    let a = sim.signal("a");
    let y = sim.signal("y");
    for value in [
        1u128 << 127,
        0x123456789abcdef0_fedcba9876543210,
        u128::MAX,
        0,
    ] {
        sim.modify(|io| io.set(a, value)).unwrap();
        assert_eq!(sim.get(y), value.into());
        sim.set(a, !value);
        assert_eq!(sim.get(y), (!value).into());
    }
    sim.modify(|io| io.set(a, i128::MIN)).unwrap();
    assert_eq!(sim.get(y), (i128::MIN as u128).into());
    sim.set(a, -2i128);
    assert_eq!(sim.get(y), (-2i128 as u128).into());
}

#[test]
#[should_panic(expected = "MismatchType")]
fn invalid_array_assignment_is_rejected_before_simulation() {
    let code = r#"
        module Top (o: output logic<8> [3]) {
            assign o = '{0};
        }
    "#;
    build_veryl_adapter(&[(code, Path::new("test.veryl"))], "Top", false);
}

#[test]
#[should_panic(expected = "UndefinedIdentifier")]
fn undefined_identifier_is_rejected_before_simulation() {
    let code = r#"
        module Top (o: output logic) {
            assign o = missing;
        }
    "#;
    build_veryl_adapter(&[(code, Path::new("test.veryl"))], "Top", false);
}

#[test]
#[should_panic(expected = "CombinationalLoop")]
fn combinational_loop_is_rejected_by_post_pass_validation() {
    let code = r#"
        module Top (a: input logic, o: output logic) {
            assign o = a ^ o;
        }
    "#;
    build_veryl_adapter(&[(code, Path::new("test.veryl"))], "Top", false);
}
