use std::fs;
use celox::{SimulatorBuilder, veryl_test};

veryl_test!("tests/macro_project");

#[test]
fn test_macro_basic() {
    let code = fs::read_to_string("tests/macro_project/src/Module04.veryl").unwrap();
    let mut sim = SimulatorBuilder::new(&code, "Module04").build().unwrap();

    let dut = Module04::new(&sim);
    let mut dut = dut.bind(&mut sim);

    // Initial state
    assert_eq!(dut.get_b(), 0);

    // After reset (AsyncLow: rst=0 means active)
    dut.set_rst(0);
    dut.tick();
    assert_eq!(dut.get_b(), 0);

    // Input propagating (deactivate reset)
    dut.set_rst(1);
    dut.set_a(42);
    dut.tick();
    assert_eq!(dut.get_b(), 42);
}

#[test]
fn test_macro_io_modify() {
    let code = fs::read_to_string("tests/macro_project/src/Module04.veryl").unwrap();
    let mut sim = SimulatorBuilder::new(&code, "Module04").build().unwrap();
    let ids = Module04::new(&sim);

    let mut dut = ids.bind(&mut sim);

    dut.modify(|io| {
        io.set_a(100);
        io.set_c(1);
        io.set_rst(1); // AsyncLow: rst=1 means inactive
    });

    dut.tick();
    assert_eq!(dut.get_b(), 100);
}



