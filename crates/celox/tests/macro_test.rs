use celox::{SimulatorBuilder, veryl_test};
use std::fs;

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

mod arrays {
    use super::*;
    veryl_test!("tests/macro_array_project");

    #[test]
    fn modify_array_elements_preserves_other_elements() {
        let code = include_str!("macro_array_project/src/Arrays.veryl");
        let mut sim = SimulatorBuilder::new(code, "Arrays").build().unwrap();
        let ids = Arrays::new(&sim);
        let mut dut = ids.bind(&mut sim);
        dut.modify(|io| {
            io.set_a(0, 3);
            io.set_a(1, 5);
            io.set_a(2, 7);
            io.set_a(1, 9);
        });
        assert_eq!(dut.get_y(0), 3);
        assert_eq!(dut.get_y(1), 9);
        assert_eq!(dut.get_y(2), 7);
    }
    #[test]
    fn modify_array_elements_masks_values_and_supports_wide_elements() {
        let code = include_str!("macro_array_project/src/Arrays.veryl");
        let mut sim = SimulatorBuilder::new(code, "Arrays").build().unwrap();
        let ids = Arrays::new(&sim);
        let mut dut = ids.bind(&mut sim);
        let high = celox::BigUint::from(1u32) << 128usize;
        dut.modify(|io| {
            io.set_a(0, u8::MAX);
            io.set_a(1, 2);
            io.set_wide(0, &high | celox::BigUint::from(3u32));
            io.set_wide(1, (&high << 1usize) | celox::BigUint::from(7u32));
        });
        assert_eq!(dut.get_y(0), 31);
        assert_eq!(dut.get_y(1), 2);
        assert_eq!(dut.get_y(2), 0);
        assert_eq!(dut.get_wide_y(0), &high | celox::BigUint::from(3u32));
        assert_eq!(dut.get_wide_y(1), celox::BigUint::from(7u32));
        // The unbatched setter uses the same element update.
        dut.set_a(0, u8::MAX);
        dut.set_wide(1, high.clone());
        assert_eq!(dut.get_y(1), 2);
        assert_eq!(dut.get_wide_y(1), high);
    }

    #[test]
    fn modify_array_elements_preserves_neighbor_unknown_masks() {
        let code = include_str!("macro_array_project/src/Arrays.veryl");
        let mut sim = SimulatorBuilder::new(code, "Arrays")
            .four_state(true)
            .build()
            .unwrap();
        let ids = Arrays::new(&sim);
        // A mixture of X and Z bits in every element.
        let initial = celox::BigUint::from(0x5555u32);
        let unknown = celox::BigUint::from(0x7fffu32);
        sim.modify(|io| io.set_four_state(ids.a, initial.clone(), unknown))
            .unwrap();
        let mut dut = ids.bind(&mut sim);
        dut.modify(|io| io.set_a(1, 3));
        let cleared = celox::BigUint::from(31u32 << 5);
        let expected_payload = (&initial ^ (&initial & &cleared)) | celox::BigUint::from(3u32 << 5);
        let expected_mask = celox::BigUint::from(0x7fffu32 ^ (31u32 << 5));
        assert_eq!(
            dut.get_a_4state(),
            (expected_payload.clone(), expected_mask.clone())
        );
        assert_eq!(dut.get_y_4state(), (expected_payload, expected_mask));
        // Clearing another element must retain the remaining neighbor's X/Z.
        dut.set_a(0, 4);
        let (payload, mask) = dut.get_a_4state();
        assert_eq!(
            payload & celox::BigUint::from(31u32),
            celox::BigUint::from(4u32)
        );
        assert_eq!(mask, celox::BigUint::from(31u32 << 10));
    }

    #[test]
    fn modify_interface_array_elements() {
        let code = include_str!("macro_array_project/src/Arrays.veryl");
        let mut sim = SimulatorBuilder::new(code, "InterfaceArrays")
            .build()
            .unwrap();
        let ids = InterfaceArrays::new(&sim);
        let mut dut = ids.bind(&mut sim);
        dut.modify(|io| {
            io.bus().set_data(0, 3);
            io.bus().set_data(1, 5);
            io.bus().set_data(2, 7);
            io.bus().set_data(1, u8::MAX);
        });
        dut.bus().set_data(0, 9);
        assert_eq!(dut.get_y(0), 9);
        assert_eq!(dut.get_y(1), 31);
        assert_eq!(dut.get_y(2), 7);
        assert_eq!(dut.bus().get_data(2), 7);
    }

    #[test]
    #[should_panic(expected = "array index 3 out of bounds for 3 elements")]
    fn modify_array_elements_rejects_out_of_bounds() {
        let code = include_str!("macro_array_project/src/Arrays.veryl");
        let mut sim = SimulatorBuilder::new(code, "Arrays").build().unwrap();
        let ids = Arrays::new(&sim);
        ids.bind(&mut sim).modify(|io| io.set_a(3, 1));
    }
    #[test]
    fn interface_array_types_are_scoped_to_the_module() {
        let code = include_str!("macro_array_project/src/Arrays.veryl");
        let mut sim = SimulatorBuilder::new(code, "AnotherInterfaceArrays")
            .build()
            .unwrap();
        let ids = AnotherInterfaceArrays::new(&sim);
        let mut dut = ids.bind(&mut sim);
        dut.modify(|io| io.bus().set_data(2, 11));
        assert_eq!(dut.get_y(2), 11);
    }
}
