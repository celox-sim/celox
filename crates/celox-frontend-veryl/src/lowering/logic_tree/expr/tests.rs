use super::SLTNodeArena;
use super::coerce_node_width;
use celox_slt::SLTNode;
use num_bigint::BigUint;

#[test]
fn zero_width_coercion_is_a_structured_error() {
    let mut arena = SLTNodeArena::<u32>::new();
    let zero = arena
        .alloc(SLTNode::Constant(
            BigUint::from(0u8),
            BigUint::from(0u8),
            0,
            false,
        ))
        .unwrap();
    let nonzero = arena
        .alloc(SLTNode::Constant(
            BigUint::from(1u8),
            BigUint::from(0u8),
            8,
            false,
        ))
        .unwrap();

    assert_eq!(
        coerce_node_width(&mut arena, zero, Some(8), true)
            .unwrap_err()
            .invariant,
        "WIDTH.COERCE_SOURCE_NON_ZERO"
    );
    assert_eq!(
        coerce_node_width(&mut arena, nonzero, Some(0), false)
            .unwrap_err()
            .invariant,
        "WIDTH.COERCE_TARGET_NON_ZERO"
    );
}
