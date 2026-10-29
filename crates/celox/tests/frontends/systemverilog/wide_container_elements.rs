use super::*;

sv_backends! {
    fn wide_elements_keep_every_bit(sim) {
        @case "wide_container_elements::wide_elements_keep_every_bit";
    }
    fn wide_elements_default_to_x(sim) {
        @case "wide_container_elements::wide_elements_default_to_x";
    }
    fn wide_elements_of_parameterized_classes(sim) {
        @case "wide_container_elements::wide_elements_of_parameterized_classes";
    }
    fn wide_elements_extend_by_the_value_signedness(sim) {
        @case "wide_container_elements::wide_elements_extend_by_the_value_signedness";
    }
    fn structure_elements_have_no_packed_dimensions(sim) {
        // IEEE 1800-2023 20.7: an unpacked structure is not an array, so a
        // queue of them has its one unpacked dimension, though the structure
        // is held in a record like a wide integral element.
        @setup {
            let source = r#"module Top(output int value);
                typedef struct { bit [99:0] v; } s_t;
                s_t records[$];
                logic [129:0] wide[$];
                initial value = $dimensions(records) * 10 + $dimensions(wide);
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("dimensions.sv"))], "Top");
        sim.run_until(0).unwrap();
        let value = sim.signal("value");
        assert_eq!(sim.get(value), BigUint::from(12u8));
    }
}
