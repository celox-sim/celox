use celox::BigUint;

#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {

    fn test_wide_context_addition_carry(sim) {
        @case "wide_context_width::test_wide_context_addition_carry";
    }

    fn test_wide_context_subtraction_underflow(sim) {
        @case "wide_context_width::test_wide_context_subtraction_underflow";
    }

    fn test_wide_context_shift_left(sim) {
        @ignore_on(sv);
        @case "wide_context_width::test_wide_context_shift_left";
    }

    fn test_wide_context_constant_folding(sim) {
        @case "wide_context_width::test_wide_context_constant_folding";
    }

    fn test_wide_runtime_shift_width_behavior(sim) {
        @ignore_on(sv);
        @case "wide_context_width::test_wide_runtime_shift_width_behavior";
    }

    fn test_wide_context_constant_folding_128bit(sim) {
        @case "wide_context_width::test_wide_context_constant_folding_128bit";
    }

    fn test_wide_context_multiplication_boundary(sim) {
        @ignore_on(sv);
        @case "wide_context_width::test_wide_context_multiplication_boundary";
    }

    fn test_wide_context_addition_mixed_boundary(sim) {
        @case "wide_context_width::test_wide_context_addition_mixed_boundary";
    }
}

#[test]
fn test_wide_context_nested_propagation() {
    // (120-bit + 120-bit) * 2'd2 in 122-bit context
    let code = r#"
        module Top (
            a: input  logic<120>,
            b: input  logic<120>,
            o: output logic<122>
        ) {
            assign o = (a + b) * 2'd2;
        }
    "#;
    let code_top = "Top";
    let mut sim = celox::SimulatorBuilder::new(code, code_top)
        .build()
        .expect("Build should succeed");

    let a = sim.signal("a");
    let b = sim.signal("b");
    let o = sim.signal("o");

    // a = 2^119, b = 2^119
    // a + b = 2^120
    // (a + b) * 2 = 2^121
    let val_a = BigUint::from(1u32) << 119;
    let val_b = BigUint::from(1u32) << 119;
    let expected = BigUint::from(1u32) << 121;

    sim.modify(|io| {
        io.set_wide(a, val_a);
        io.set_wide(b, val_b);
    })
    .unwrap();
    assert_eq!(sim.get(o), expected, "Nested 122-bit context width failed");
}
