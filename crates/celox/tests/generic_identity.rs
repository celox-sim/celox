use celox::SimulatorBuilder;

#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {
    fn nested_value_generics_keep_distinct_bodies(sim) {
        @build SimulatorBuilder::new(r#"
module Leaf::<FLAG: u32> (a: input logic, y: output logic) {
    always_comb { y = 1'b0; if FLAG != 0 { y = a; } }
}
module Wrapped(a: input logic, y: output logic) {
    inst leaf: Leaf::<1>(a, y);
}
module Repro(a: input logic, one: output logic, zero: output logic) {
    inst wrapped: Wrapped(a, y: one);
    inst disabled: Leaf::<0>(a, y: zero);
}
"#, "Repro");
        let input = sim.signal("a");
        let one = sim.signal("one");
        let zero = sim.signal("zero");
        for value in [0u8, 1, 0, 1] {
            sim.modify(|io| io.set(input, value)).unwrap();
            assert_eq!(sim.get(one), value.into());
            assert_eq!(sim.get(zero), 0u8.into());
        }
    }

    fn default_and_explicit_value_generics_preserve_instance_inputs(sim) {
        @build SimulatorBuilder::new(r#"
module Select::<ENABLED: u32 = 1> (a: input logic, y: output logic) {
    always_comb { y = 1'b0; if ENABLED != 0 { y = a; } }
}
module Repro(a: input logic, b: input logic, first: output logic,
    second: output logic, disabled: output logic) {
    inst off: Select::<0>(a, y: disabled);
    inst default_on: Select(a, y: first);
    inst explicit_on: Select::<1>(a: b, y: second);
}
"#, "Repro");
        let input_a = sim.signal("a");
        let input_b = sim.signal("b");
        let first = sim.signal("first");
        let second = sim.signal("second");
        let disabled = sim.signal("disabled");
        for value in 0u8..4 {
            sim.modify(|io| {
                io.set(input_a, value & 1);
                io.set(input_b, (value >> 1) & 1);
            }).unwrap();
            assert_eq!(sim.get(first), (value & 1).into());
            assert_eq!(sim.get(second), ((value >> 1) & 1).into());
            assert_eq!(sim.get(disabled), 0u8.into());
        }
    }
}
