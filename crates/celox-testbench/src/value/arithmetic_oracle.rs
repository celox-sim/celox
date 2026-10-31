//! Test-only arithmetic oracle for native lowering differential tests.
use super::TestbenchValue;
use crate::TestbenchOperator as Op;
use num_bigint::{BigInt, BigUint, Sign};
use num_traits::ToPrimitive as _;

pub(crate) fn width_mask(width: usize) -> BigUint {
    if width == 0 {
        BigUint::ZERO
    } else {
        (BigUint::from(1u8) << width) - BigUint::from(1u8)
    }
}

#[inline]
pub(crate) fn width_mask_u64(width: usize) -> u64 {
    match width {
        0 => 0,
        1..=63 => (1u64 << width) - 1,
        _ => u64::MAX,
    }
}

#[inline]
pub(crate) fn signed_i128(value: u64, width: usize) -> i128 {
    let value = value & width_mask_u64(width);
    if width == 0 || width >= 64 {
        (value as i64) as i128
    } else if value & (1u64 << (width - 1)) == 0 {
        value as i128
    } else {
        value as i128 - (1i128 << width)
    }
}

pub(crate) fn normalized_bits(value: &TestbenchValue, width: usize) -> BigUint {
    value.to_biguint() & width_mask(width)
}

pub(crate) fn tb_value_from_bits(value: BigUint, width: usize) -> TestbenchValue {
    let value = value & width_mask(width);
    if width <= 64 {
        TestbenchValue::U64(value.to_u64().unwrap_or(0))
    } else {
        TestbenchValue::Wide(value)
    }
}

pub(crate) fn signed_bigint(value: &TestbenchValue, width: usize) -> BigInt {
    let raw = normalized_bits(value, width);
    if width == 0 || !raw.bit((width - 1) as u64) {
        BigInt::from(raw)
    } else {
        BigInt::from(raw) - (BigInt::from(1u8) << width)
    }
}

pub(crate) fn signed_bits(value: BigInt, width: usize) -> BigUint {
    if width == 0 {
        return BigUint::ZERO;
    }
    let modulus = BigUint::from(1u8) << width;
    match value.sign() {
        Sign::Minus => {
            let magnitude = (-value).to_biguint().unwrap_or_default() % &modulus;
            if magnitude == BigUint::ZERO {
                BigUint::ZERO
            } else {
                modulus - magnitude
            }
        }
        _ => value.to_biguint().unwrap_or_default() % modulus,
    }
}

pub(crate) fn eval_typed_binop_u64(
    l: u64,
    op: Op,
    r: u64,
    lhs_width: usize,
    rhs_width: usize,
    result_width: usize,
    lhs_signed: bool,
    rhs_signed: bool,
) -> u64 {
    let l = l & width_mask_u64(lhs_width);
    let r = r & width_mask_u64(rhs_width);
    let result_mask = width_mask_u64(result_width);
    let signed = lhs_signed && rhs_signed;
    let bool_value = |value: bool| u64::from(value);

    match op {
        Op::Eq | Op::EqWildcard => bool_value(l == r),
        Op::Ne | Op::NeWildcard => bool_value(l != r),
        Op::Less if signed => bool_value(signed_i128(l, lhs_width) < signed_i128(r, rhs_width)),
        Op::Less => bool_value(l < r),
        Op::LessEq if signed => bool_value(signed_i128(l, lhs_width) <= signed_i128(r, rhs_width)),
        Op::LessEq => bool_value(l <= r),
        Op::Greater if signed => bool_value(signed_i128(l, lhs_width) > signed_i128(r, rhs_width)),
        Op::Greater => bool_value(l > r),
        Op::GreaterEq if signed => {
            bool_value(signed_i128(l, lhs_width) >= signed_i128(r, rhs_width))
        }
        Op::GreaterEq => bool_value(l >= r),
        Op::LogicAnd => bool_value(l != 0 && r != 0),
        Op::LogicOr => bool_value(l != 0 || r != 0),
        Op::Add => l.wrapping_add(r) & result_mask,
        Op::Sub => l.wrapping_sub(r) & result_mask,
        Op::Mul => l.wrapping_mul(r) & result_mask,
        Op::Div if signed => {
            let divisor = signed_i128(r, rhs_width);
            if divisor == 0 {
                0
            } else {
                (signed_i128(l, lhs_width) / divisor) as u64 & result_mask
            }
        }
        Op::Div => l.checked_div(r).unwrap_or(0) & result_mask,
        Op::Rem if signed => {
            let divisor = signed_i128(r, rhs_width);
            if divisor == 0 {
                0
            } else {
                (signed_i128(l, lhs_width) % divisor) as u64 & result_mask
            }
        }
        Op::Rem => l.checked_rem(r).unwrap_or(0) & result_mask,
        Op::Pow => {
            let mut exponent = r;
            let mut base = l & result_mask;
            let mut value = 1u64 & result_mask;
            while exponent != 0 {
                if exponent & 1 != 0 {
                    value = ((value as u128 * base as u128) as u64) & result_mask;
                }
                exponent >>= 1;
                if exponent != 0 {
                    base = ((base as u128 * base as u128) as u64) & result_mask;
                }
            }
            value
        }
        Op::BitAnd => (l & r) & result_mask,
        Op::BitOr => (l | r) & result_mask,
        Op::BitXor => (l ^ r) & result_mask,
        Op::BitXnor => (!(l ^ r)) & result_mask,
        Op::BitNand => (!(l & r)) & result_mask,
        Op::BitNor => (!(l | r)) & result_mask,
        Op::LogicShiftL | Op::ArithShiftL => {
            if r >= result_width as u64 {
                0
            } else {
                l.wrapping_shl(r as u32) & result_mask
            }
        }
        Op::LogicShiftR => {
            if r >= result_width as u64 {
                0
            } else {
                (l >> r) & result_mask
            }
        }
        Op::ArithShiftR if lhs_signed => {
            let value = signed_i128(l, lhs_width);
            if r >= result_width as u64 {
                if value < 0 { result_mask } else { 0 }
            } else {
                ((value >> r) as u64) & result_mask
            }
        }
        Op::ArithShiftR => {
            if r >= result_width as u64 {
                0
            } else {
                (l >> r) & result_mask
            }
        }
        _ => unreachable!("operator is not a source-language binary op: {op:?}"),
    }
}

pub(crate) fn eval_typed_binop(
    l: TestbenchValue,
    op: Op,
    r: TestbenchValue,
    lhs_width: usize,
    rhs_width: usize,
    result_width: usize,
    lhs_signed: bool,
    rhs_signed: bool,
) -> TestbenchValue {
    if let (TestbenchValue::U64(l), TestbenchValue::U64(r)) = (&l, &r)
        && lhs_width <= 64
        && rhs_width <= 64
        && result_width <= 64
    {
        return TestbenchValue::U64(eval_typed_binop_u64(
            *l,
            op,
            *r,
            lhs_width,
            rhs_width,
            result_width,
            lhs_signed,
            rhs_signed,
        ));
    }
    let lb = normalized_bits(&l, lhs_width);
    let rb = normalized_bits(&r, rhs_width);
    let signed = lhs_signed && rhs_signed;

    let comparison = |value: bool| TestbenchValue::U64(u64::from(value));
    match op {
        Op::Eq | Op::EqWildcard => comparison(lb == rb),
        Op::Ne | Op::NeWildcard => comparison(lb != rb),
        Op::Less if signed => {
            comparison(signed_bigint(&l, lhs_width) < signed_bigint(&r, rhs_width))
        }
        Op::Less => comparison(lb < rb),
        Op::LessEq if signed => {
            comparison(signed_bigint(&l, lhs_width) <= signed_bigint(&r, rhs_width))
        }
        Op::LessEq => comparison(lb <= rb),
        Op::Greater if signed => {
            comparison(signed_bigint(&l, lhs_width) > signed_bigint(&r, rhs_width))
        }
        Op::Greater => comparison(lb > rb),
        Op::GreaterEq if signed => {
            comparison(signed_bigint(&l, lhs_width) >= signed_bigint(&r, rhs_width))
        }
        Op::GreaterEq => comparison(lb >= rb),
        Op::LogicAnd => comparison(lb != BigUint::ZERO && rb != BigUint::ZERO),
        Op::LogicOr => comparison(lb != BigUint::ZERO || rb != BigUint::ZERO),
        Op::Add => tb_value_from_bits(lb + rb, result_width),
        Op::Sub => tb_value_from_bits(
            signed_bits(BigInt::from(lb) - BigInt::from(rb), result_width),
            result_width,
        ),
        Op::Mul => tb_value_from_bits(lb * rb, result_width),
        Op::Div if signed => {
            let divisor = signed_bigint(&r, rhs_width);
            if divisor == BigInt::from(0u8) {
                TestbenchValue::U64(0)
            } else {
                let quotient = signed_bigint(&l, lhs_width) / divisor;
                tb_value_from_bits(signed_bits(quotient, result_width), result_width)
            }
        }
        Op::Div => {
            if rb == BigUint::ZERO {
                TestbenchValue::U64(0)
            } else {
                tb_value_from_bits(lb / rb, result_width)
            }
        }
        Op::Rem if signed => {
            let divisor = signed_bigint(&r, rhs_width);
            if divisor == BigInt::from(0u8) {
                TestbenchValue::U64(0)
            } else {
                let remainder = signed_bigint(&l, lhs_width) % divisor;
                tb_value_from_bits(signed_bits(remainder, result_width), result_width)
            }
        }
        Op::Rem => {
            if rb == BigUint::ZERO {
                TestbenchValue::U64(0)
            } else {
                tb_value_from_bits(lb % rb, result_width)
            }
        }
        Op::Pow => {
            if result_width == 0 {
                TestbenchValue::U64(0)
            } else {
                let modulus = BigUint::from(1u8) << result_width;
                tb_value_from_bits(lb.modpow(&rb, &modulus), result_width)
            }
        }
        Op::BitAnd => tb_value_from_bits(lb & rb, result_width),
        Op::BitOr => tb_value_from_bits(lb | rb, result_width),
        Op::BitXor => tb_value_from_bits(lb ^ rb, result_width),
        Op::BitXnor => tb_value_from_bits((lb ^ rb) ^ width_mask(result_width), result_width),
        Op::LogicShiftL | Op::ArithShiftL => {
            let shift = rb.to_usize().unwrap_or(usize::MAX);
            if shift >= result_width {
                TestbenchValue::U64(0)
            } else {
                tb_value_from_bits(lb << shift, result_width)
            }
        }
        Op::LogicShiftR => {
            let shift = rb.to_usize().unwrap_or(usize::MAX);
            if shift >= result_width {
                TestbenchValue::U64(0)
            } else {
                tb_value_from_bits(lb >> shift, result_width)
            }
        }
        Op::ArithShiftR if lhs_signed => {
            let shift = rb.to_usize().unwrap_or(usize::MAX);
            let value = signed_bigint(&l, lhs_width);
            let shifted = if shift >= result_width {
                if value.sign() == Sign::Minus {
                    BigInt::from(-1)
                } else {
                    BigInt::from(0)
                }
            } else {
                value >> shift
            };
            tb_value_from_bits(signed_bits(shifted, result_width), result_width)
        }
        Op::ArithShiftR => {
            let shift = rb.to_usize().unwrap_or(usize::MAX);
            if shift >= result_width {
                TestbenchValue::U64(0)
            } else {
                tb_value_from_bits(lb >> shift, result_width)
            }
        }
        Op::BitNand => tb_value_from_bits((lb & rb) ^ width_mask(result_width), result_width),
        Op::BitNor => tb_value_from_bits((lb | rb) ^ width_mask(result_width), result_width),
        _ => unreachable!("operator is not a source-language binary op: {op:?}"),
    }
}
