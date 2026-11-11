use super::FfParser;
use veryl_analyzer::{
    ir::{Comptime, Expression, Factor, Shape, Type, TypeKind, ValueVariant},
    value::Value,
};
use veryl_parser::token_range::TokenRange;

fn logic_constant(payload: u128, mask: u128, width: usize) -> Expression {
    let token = TokenRange::default();
    let mut ty = Type::new(TypeKind::Logic);
    ty.set_concrete_width(Shape::new(vec![Some(width)]));
    Expression::Term(Box::new(Factor::Value(Comptime {
        value: ValueVariant::Numeric(Value::from_u128(payload, mask, width, false)),
        r#type: ty,
        is_const: true,
        is_global: true,
        evaluated: true,
        token,
        ..Default::default()
    })))
}

#[test]
fn constant_truth_ignores_unknown_bits_but_keeps_known_ones() {
    // Veryl encodes X=(payload 0, mask 1) and Z=(payload 1, mask 1).
    for (payload, mask, expected) in [
        (0x80, 0x04, true),
        (0x00, 0x04, false),
        (0x04, 0x04, false),
        (0x00, 0x00, false),
        (0x80, 0x00, true),
    ] {
        let condition = logic_constant(payload, mask, 8);
        assert_eq!(
            FfParser::get_constant_procedural_truth(&condition),
            Some(expected),
            "payload={payload:#x}, mask={mask:#x}",
        );
    }
}
