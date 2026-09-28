//! This integration test is an external consumer: it has no Celox dependency.
use celox_test_suite_veryl::{
    Backend, BigUint, Category, CompilationRejected, Result, Scalar, SignalPath, Simulator, case,
    cases,
};
use std::collections::BTreeMap;

#[derive(Default)]
struct Bitwise {
    inputs: BTreeMap<String, BigUint>,
    outputs: BTreeMap<String, BigUint>,
    broken: bool,
}

impl Backend for Bitwise {
    fn write(&mut self, signal: &SignalPath, payload: BigUint, mask: BigUint) -> Result<()> {
        assert!(signal.instances.is_empty());
        assert_eq!(mask, BigUint::default());
        self.inputs.insert(signal.name.clone(), payload);
        Ok(())
    }
    fn read(&mut self, signal: &SignalPath) -> Result<(BigUint, BigUint)> {
        let value = self
            .outputs
            .get(&signal.name)
            .ok_or("output was not evaluated")?;
        Ok((value.clone(), BigUint::default()))
    }
    fn eval_comb(&mut self) -> Result<()> {
        // Both inputs must have been written before the batch is evaluated.
        let a = self.inputs.get("a").ok_or("a not driven")?;
        let b = self.inputs.get("b").ok_or("b not driven")?;
        self.outputs.insert("o_and".into(), a & b);
        self.outputs.insert(
            "o_or".into(),
            if self.broken {
                BigUint::default()
            } else {
                a | b
            },
        );
        self.outputs.insert("o_xor".into(), a ^ b);
        Ok(())
    }
    fn tick(&mut self, _: &str) -> Result<()> {
        Err("no clock in this model".into())
    }
}

#[test]
fn existing_case_runs_on_an_unrelated_backend() {
    let mut compiled = 0;
    case("operators::test_bitwise_operations")
        .unwrap()
        .run(&mut |design| {
            compiled += 1;
            assert_eq!(design.top, "Top");
            assert!(!design.four_state);
            assert_eq!(design.sources.len(), 1);
            assert!(design.sources[0].text.contains("o_and = a & b;"));
            Ok(Box::new(Bitwise::default()))
        });
    assert_eq!(compiled, 1);
}

#[test]
#[should_panic(expected = "assertion `left == right` failed")]
fn incorrect_backend_output_fails_the_original_assertion() {
    case("operators::test_bitwise_operations")
        .unwrap()
        .run(&mut |_| {
            Ok(Box::new(Bitwise {
                broken: true,
                ..Default::default()
            }))
        });
}

#[test]
#[should_panic(
    expected = "compile operators::test_bitwise_operations: compiler refused the design"
)]
fn compiler_failure_is_not_a_passing_or_skipped_case() {
    case("operators::test_bitwise_operations")
        .unwrap()
        .run(&mut |_| Err("compiler refused the design".into()));
}

#[test]
fn catalogue_has_unique_stable_names_and_categories() {
    let mut names = std::collections::BTreeSet::new();
    for case in cases() {
        assert!(names.insert(case.name), "duplicate {}", case.name);
        assert!(case.name.contains("::"));
    }
    assert_eq!(names.len(), 648);
    assert_eq!(
        case("operators::test_bitwise_operations").unwrap().category,
        Category::Operators
    );
    assert!(case("missing::case").is_none());
}

#[test]
fn scalar_conversion_is_little_endian_and_preserves_signed_bits() {
    assert_eq!((-2i16).to_bits(), BigUint::from(0xfffeu32));
    assert_eq!(i16::from_bits(&BigUint::from(0xfffeu32)), -2);
    assert_eq!(u8::from_bits(&BigUint::from(0x1234u32)), 0x34);
    assert_eq!(u128::MAX.to_bits(), (BigUint::from(1u8) << 128) - 1u8);
    assert!(bool::from_bits(&BigUint::from(3u8)));
    assert!(!bool::from_bits(&BigUint::from(2u8)));
}

struct Echo;
impl Backend for Echo {
    fn write(&mut self, signal: &SignalPath, payload: BigUint, mask: BigUint) -> Result<()> {
        assert_eq!(signal.name, "data");
        assert_eq!(signal.instances[0].name, "unit");
        assert_eq!(signal.instances[0].index, Some(3));
        assert_eq!(payload, BigUint::from(1u8) << 200);
        assert_eq!(mask, BigUint::from(3u8) << 199);
        Ok(())
    }
    fn read(&mut self, _: &SignalPath) -> Result<(BigUint, BigUint)> {
        Ok((BigUint::from(1u8) << 200, BigUint::from(3u8) << 199))
    }
    fn eval_comb(&mut self) -> Result<()> {
        Ok(())
    }
    fn tick(&mut self, event: &str) -> Result<()> {
        assert_eq!(event, "clock_b");
        Err("edge failed".into())
    }
}

#[test]
fn driver_preserves_hierarchy_wide_xz_bits_and_event_errors() {
    let mut sim = Simulator::new(Box::new(Echo));
    let signal = sim.child_signal(&[("unit", Some(3))], "data");
    let expected: (BigUint, BigUint) = (BigUint::from(1u8) << 200, BigUint::from(3u8) << 199);
    sim.modify(|io| io.set_four_state(signal, expected.0.clone(), expected.1.clone()))
        .unwrap();
    assert_eq!(sim.get_four_state(signal), expected);
    let clock = sim.event("clock_b");
    assert_eq!(sim.tick(clock).unwrap_err().to_string(), "edge failed");
}

#[test]
fn driver_distinguishes_plain_instances_from_array_element_zero() {
    struct Paths(usize);
    impl Backend for Paths {
        fn write(&mut self, signal: &SignalPath, _: BigUint, _: BigUint) -> Result<()> {
            assert_eq!(signal.instances[0].name, "unit");
            assert_eq!(signal.instances[0].index, [None, Some(0), Some(1)][self.0]);
            self.0 += 1;
            Ok(())
        }
        fn read(&mut self, _: &SignalPath) -> Result<(BigUint, BigUint)> {
            unreachable!()
        }
        fn eval_comb(&mut self) -> Result<()> {
            Ok(())
        }
        fn tick(&mut self, _: &str) -> Result<()> {
            unreachable!()
        }
    }
    let mut sim = Simulator::new(Box::new(Paths(0)));
    let signals =
        [None, Some(0), Some(1)].map(|index| sim.child_signal(&[("unit", index)], "data"));
    for signal in signals {
        sim.set(signal, 0u8);
    }
}

#[test]
fn rejection_cases_require_a_compiler_error() {
    let case = case("hierarchy::test_instance_output_concat_advances_each_destination").unwrap();
    assert_eq!(
        case.expectation,
        celox_test_suite_veryl::Expectation::CompilationError
    );
    case.run(&mut |_| Err(CompilationRejected("output destination is not constant".into()).into()));
    assert!(
        std::panic::catch_unwind(|| case.run(&mut |_| Ok(Box::new(Bitwise::default())))).is_err()
    );
}

#[test]
fn negative_cases_fail_on_adapter_errors_and_panics() {
    use std::io::{Error, ErrorKind};
    for case in cases()
        .filter(|case| case.expectation == celox_test_suite_veryl::Expectation::CompilationError)
    {
        for kind in [
            ErrorKind::NotFound,
            ErrorKind::TimedOut,
            ErrorKind::PermissionDenied,
        ] {
            assert!(
                std::panic::catch_unwind(|| {
                    case.run(&mut |_| Err(Error::new(kind, "compiler unavailable").into()));
                })
                .is_err(),
                "{} accepted {kind:?}",
                case.name
            );
        }
        assert!(
            std::panic::catch_unwind(|| {
                case.run(&mut |_| Err("compiler adapter failed".into()));
            })
            .is_err()
        );
        assert!(
            std::panic::catch_unwind(|| {
                case.run(&mut |_| panic!("compiler panicked"));
            })
            .is_err()
        );
    }
}
