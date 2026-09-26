//! Live adapter checks. Run explicitly with --ignored; these require external tools.
#![cfg(any(feature = "icarus", feature = "verilator"))]
use celox_test_suite_veryl::{Backend, BigUint, Design, Result, SignalPath, Simulator};
use std::path::Path;

const DESIGN: &str = r#"
module Child (
    clk: input clock_negedge,
    rst: input reset_async_high,
    d: input logic<130>,
    q: output logic<130>,
    previous: output logic<130>,
) {
    always_ff (clk, rst) {
        if_reset { q = 0; previous = 0; }
        else { q = d; previous = q; }
    }
}
module Top (
    clk: input clock_negedge,
    rst: input reset_async_high,
    d: input logic<130>,
    q: output logic<130>,
    previous: output logic<130>,
) {
    inst child: Child (clk, rst, d, q, previous);
}
"#;

fn signal(name: &str) -> SignalPath {
    SignalPath {
        instances: Vec::new(),
        name: name.into(),
    }
}

fn check_edges_and_wide_values(build: fn(&Design, &Path) -> Result<Box<dyn Backend>>, tool: &str) {
    let dir =
        std::env::temp_dir().join(format!("veryl-suite-adapter-{tool}-{}", std::process::id()));
    let mut backend = build(&Design::new(DESIGN, "Top"), &dir).unwrap();
    let zero = BigUint::default();
    let one = BigUint::from(1u8);
    let wide: BigUint = (&one << 129) | BigUint::from(0xabcdu16);
    backend
        .write(&signal("clk"), one.clone(), zero.clone())
        .unwrap();
    backend
        .write(&signal("rst"), one.clone(), zero.clone())
        .unwrap();
    backend
        .write(&signal("d"), wide.clone(), zero.clone())
        .unwrap();
    // Reads settle pending writes, including the asynchronous reset/NBA queue.
    assert_eq!(
        backend.read(&signal("q")).unwrap(),
        (zero.clone(), zero.clone())
    );
    backend.tick("clk").unwrap();
    assert_eq!(backend.read(&signal("rst")).unwrap().0, one);
    assert_eq!(backend.read(&signal("q")).unwrap().0, zero);
    backend
        .write(&signal("rst"), zero.clone(), zero.clone())
        .unwrap();
    backend.tick("clk").unwrap();
    assert_eq!(backend.read(&signal("q")).unwrap().0, wide);
    assert_eq!(backend.read(&signal("previous")).unwrap().0, zero);
    backend
        .write(&signal("d"), BigUint::from(7u8), zero.clone())
        .unwrap();
    backend.tick("clk").unwrap();
    assert_eq!(backend.read(&signal("previous")).unwrap().0, wide);
    assert_eq!(backend.read(&signal("q")).unwrap().0, BigUint::from(7u8));
    assert!(backend.tick("unknown_clock").is_err());
}

#[cfg(feature = "verilator")]
#[test]
#[ignore = "requires Verilator, C++, make and timeout on PATH"]
fn verilator_edges_nba_and_wide_values() {
    check_edges_and_wide_values(
        |d, p| {
            Ok(Box::new(
                celox_test_suite_veryl::verilator::Verilator::build(d, p)?,
            ))
        },
        "verilator",
    );
}

#[cfg(feature = "icarus")]
#[test]
#[ignore = "requires Icarus, iverilog-vpi, C++ and timeout on PATH"]
fn icarus_edges_nba_and_wide_values() {
    check_edges_and_wide_values(
        |d, p| {
            Ok(Box::new(celox_test_suite_veryl::icarus::Icarus::build(
                d, p,
            )?))
        },
        "icarus",
    );
}

#[cfg(feature = "icarus")]
#[test]
#[ignore = "requires Icarus, iverilog-vpi, C++ and timeout on PATH"]
fn icarus_preserves_xz_and_array_element_order() {
    let dir = std::env::temp_dir().join(format!("veryl-suite-adapter-xz-{}", std::process::id()));
    let design = Design::new(
        r#"
module Top (a: input logic<70>[2], y: output logic<70>[2]) {
    assign y[0] = a[0];
    assign y[1] = a[1];
}

"#,
        "Top",
    )
    .four_state(true);
    let mut backend = celox_test_suite_veryl::icarus::Icarus::build(&design, &dir).unwrap();
    let payload: BigUint = (BigUint::from(1u8) << 139) | BigUint::from(0x5au8);
    let mask: BigUint =
        (BigUint::from(1u8) << 139) | (BigUint::from(1u8) << 71) | BigUint::from(0xffu8);
    backend
        .write(&signal("a"), payload.clone(), mask.clone())
        .unwrap();
    assert_eq!(backend.read(&signal("y")).unwrap(), (payload, mask));
}

fn check_generated_instance_paths(
    build: fn(&Design, &Path) -> Result<Box<dyn Backend>>,
    tool: &str,
) {
    let directory =
        std::env::temp_dir().join(format!("veryl-suite-paths-{tool}-{}", std::process::id()));
    let design = Design::new(
        r#"
module Child (d: input logic<8>, q: output logic<8>) {
    assign q = d;
}
module Top (
    d: input logic<8>[2], q: output logic<8>[2],
    plain_d: input logic<8>, plain_q: output logic<8>,
) {
    inst plain: Child (d: plain_d, q: plain_q);
    for i in 0..2: unit {
        inst leaf: Child (d: d[i], q: q[i]);
    }
}
"#,
        "Top",
    );
    let mut sim = Simulator::new(build(&design, &directory).unwrap());
    let d = sim.signal("d");
    let plain_d = sim.signal("plain_d");
    sim.modify(|io| {
        io.set(d, 0x3322u16);
        io.set(plain_d, 0x11u8);
    })
    .unwrap();
    let plain = sim.child_signal(&[("plain", None)], "q");
    let first = sim.child_signal(&[("unit", Some(0)), ("leaf", None)], "q");
    let second = sim.child_signal(&[("unit", Some(1)), ("leaf", None)], "q");
    assert_eq!(sim.get_as::<u8>(plain), 0x11);
    assert_eq!(sim.get_as::<u8>(first), 0x22);
    assert_eq!(sim.get_as::<u8>(second), 0x33);
}

#[cfg(feature = "verilator")]
#[test]
#[ignore = "requires Verilator, C++, make and timeout on PATH"]
fn verilator_generated_instance_paths() {
    check_generated_instance_paths(
        |d, p| {
            Ok(Box::new(
                celox_test_suite_veryl::verilator::Verilator::build(d, p)?,
            ))
        },
        "verilator",
    );
}

#[cfg(feature = "icarus")]
#[test]
#[ignore = "requires Icarus, iverilog-vpi, C++ and timeout on PATH"]
fn icarus_generated_instance_paths() {
    check_generated_instance_paths(
        |d, p| {
            Ok(Box::new(celox_test_suite_veryl::icarus::Icarus::build(
                d, p,
            )?))
        },
        "icarus",
    );
}

#[cfg(feature = "icarus")]
#[test]
#[ignore = "requires Icarus and timeout on PATH"]
fn icarus_rejects_invalid_output_connections() {
    let directory =
        std::env::temp_dir().join(format!("veryl-suite-rejections-{}", std::process::id()));
    let mut checked = 0;
    for case in celox_test_suite_veryl::cases()
        .filter(|case| case.expectation == celox_test_suite_veryl::Expectation::CompilationError)
    {
        let output = directory.join(case.name.replace("::", "/"));
        case.run(&mut |design| {
            Ok(Box::new(celox_test_suite_veryl::icarus::Icarus::build(
                design, &output,
            )?))
        });
        checked += 1;
    }
    assert_eq!(checked, 8);
}

#[cfg(feature = "verilator")]
#[test]
#[ignore = "requires Verilator and timeout on PATH"]
fn verilator_rejects_invalid_assignment() {
    let directory = std::env::temp_dir().join(format!(
        "veryl-suite-verilator-rejection-{}",
        std::process::id()
    ));
    // The other seven negative cases remain excluded for Verilator's acceptance
    // of dynamic output destinations. This one reports an assignment type error.
    let case = celox_test_suite_veryl::cases()
        .find(|case| {
            case.name == "hierarchy::test_dynamic_prefix_colon_output_port_allows_zero_lsb"
        })
        .unwrap();
    case.run(&mut |design| {
        Ok(Box::new(
            celox_test_suite_veryl::verilator::Verilator::build(design, &directory)?,
        ))
    });
}
