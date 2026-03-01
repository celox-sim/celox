use std::{fs, u16};
use celox::{SimulatorBuilder, veryl_test};

veryl_test!("tests/macro_project");

#[test]
fn test_linear_sorter_basic() {
    let depth = 8;
    let max_val = u16::MAX;

    let code = fs::read_to_string("tests/macro_project/src/linear_sorter.veryl").unwrap();
    let result = SimulatorBuilder::new(&code, "LinearSorter").build_with_trace();
    let mut sim = result.res.unwrap();
    let dut_ids = LinearSorter::new(&sim);
    let mut dut = dut_ids.bind(&mut sim);

    // --- 1. Reset Phase (AsyncLow: rst=0 means active) ---
    dut.set_rst(0);
    dut.set_en(0);
    dut.tick();

    for i in 0..depth {
        assert_eq!(dut.get_d_out(i), max_val);
    }

    // --- 2. Sorting Phase ---
    dut.set_rst(1);
    dut.set_en(1);

    // Inputs: [50, 20, 80, 10]
    let inputs = vec![50, 20, 80, 10];

    // Expectations for d_out[0..DEPTH] after each tick
    let expectations: Vec<Vec<u16>> = vec![
        // Cycle 1: 50 is inserted into cell 0
        vec![50, 65535, 65535, 65535, 65535, 65535, 65535, 65535],
        // Cycle 2: 20 < 50, cell 0 gets 20, 50 pushed to cell 1
        vec![20, 50, 65535, 65535, 65535, 65535, 65535, 65535],
        // Cycle 3: 80 > 20 and 50, passes through to cell 2
        vec![20, 50, 80, 65535, 65535, 65535, 65535, 65535],
        // Cycle 4: 10 is smallest, shifts everything
        vec![10, 20, 50, 80, 65535, 65535, 65535, 65535],
    ];

    for (time, &val) in inputs.iter().enumerate() {
        dut.set_d_in(val);
        dut.tick(); // FF values update here

        for i in 0..depth {
            let current_out = dut.get_d_out(i);
            assert_eq!(
                current_out,
                expectations[time][i],
                "Mismatch at cycle {} at cell {}",
                time + 1,
                i
            );
        }
    }
}

#[test]
fn test_linear_sorter_hierarchy() {
    let depth = 8;
    let max_val = u16::MAX;

    let sorter_code =
        fs::read_to_string("tests/macro_project/src/linear_sorter.veryl").unwrap();
    let wrapper_code =
        fs::read_to_string("tests/macro_project/src/linear_sorter_wrapper.veryl").unwrap();
    let code = format!("{sorter_code}\n{wrapper_code}");

    let result = SimulatorBuilder::new(&code, "LinearSorterWrapper").build_with_trace();
    let mut sim = result.res.unwrap();
    let dut_ids = LinearSorterWrapper::new(&sim);
    let mut dut = dut_ids.bind(&mut sim);

    // Reset
    dut.set_rst(0);
    dut.set_en(0);
    dut.tick();

    for i in 0..depth {
        assert_eq!(dut.get_d_out(i), max_val, "reset: d_out[{i}] should be max");
    }

    // Sort [50, 20, 80, 10]
    dut.set_rst(1);
    dut.set_en(1);

    let inputs = [50u16, 20, 80, 10];
    let expectations: Vec<Vec<u16>> = vec![
        vec![50, 65535, 65535, 65535, 65535, 65535, 65535, 65535],
        vec![20, 50, 65535, 65535, 65535, 65535, 65535, 65535],
        vec![20, 50, 80, 65535, 65535, 65535, 65535, 65535],
        vec![10, 20, 50, 80, 65535, 65535, 65535, 65535],
    ];

    for (time, &val) in inputs.iter().enumerate() {
        dut.set_d_in(val);
        dut.tick();

        for i in 0..depth {
            assert_eq!(
                dut.get_d_out(i),
                expectations[time][i],
                "Mismatch at cycle {} cell {} (through hierarchy)",
                time + 1,
                i
            );
        }
    }
}
