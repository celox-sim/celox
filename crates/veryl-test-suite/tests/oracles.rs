//! Live adapter checks. Run explicitly with --ignored; these require external tools.
#![cfg(any(feature = "icarus", feature = "verilator"))]
use std::path::Path;
use veryl_test_suite::{Backend, BigUint, Design, Result, SignalPath};

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
            Ok(Box::new(veryl_test_suite::verilator::Verilator::build(
                d, p,
            )?))
        },
        "verilator",
    );
}

#[cfg(feature = "icarus")]
#[test]
#[ignore = "requires Icarus, iverilog-vpi, C++ and timeout on PATH"]
fn icarus_edges_nba_and_wide_values() {
    check_edges_and_wide_values(
        |d, p| Ok(Box::new(veryl_test_suite::icarus::Icarus::build(d, p)?)),
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
    let mut backend = veryl_test_suite::icarus::Icarus::build(&design, &dir).unwrap();
    let payload: BigUint = (BigUint::from(1u8) << 139) | BigUint::from(0x5au8);
    let mask: BigUint =
        (BigUint::from(1u8) << 139) | (BigUint::from(1u8) << 71) | BigUint::from(0xffu8);
    backend
        .write(&signal("a"), payload.clone(), mask.clone())
        .unwrap();
    assert_eq!(backend.read(&signal("y")).unwrap(), (payload, mask));
}
