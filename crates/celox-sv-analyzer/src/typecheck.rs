//! Type and width resolution helpers.

use std::collections::HashMap;

use num_bigint::BigUint;

use crate::ir::{BinaryOp, ConstExpr, PackedRange, UnaryOp};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegralLiteral {
    pub width: usize,
    pub signed: bool,
    pub value: BigUint,
    pub mask: BigUint,
}

pub fn resolve_packed_width(ranges: &[PackedRange]) -> Option<usize> {
    resolve_packed_width_with_env(ranges, &HashMap::new())
}

pub fn resolve_packed_width_with_env(
    ranges: &[PackedRange],
    constants: &HashMap<String, i128>,
) -> Option<usize> {
    if ranges.is_empty() {
        return Some(1);
    }

    ranges.iter().try_fold(1usize, |acc, range| {
        let left = eval_const_expr(range.left(), constants)?;
        let right = eval_const_expr(range.right(), constants)?;
        let width = left.abs_diff(right) as usize + 1;
        acc.checked_mul(width)
    })
}

pub fn eval_const_expr(expr: &ConstExpr, constants: &HashMap<String, i128>) -> Option<i128> {
    match expr {
        ConstExpr::Literal(value) => literal_as_i128(value),
        ConstExpr::Ident(name) => constants.get(name).copied(),
        ConstExpr::Select { expr, bit } => {
            let value = eval_const_expr(expr, constants)?;
            let bit = eval_const_expr(bit, constants)?;
            let bit = u32::try_from(bit).ok()?;
            value.checked_shr(bit).map(|value| value & 1)
        }
        ConstExpr::Function { name, args } => eval_const_function(name, args, constants),
        ConstExpr::Unary { op, expr } => {
            let value = eval_const_expr(expr, constants)?;
            match op {
                UnaryOp::Plus => Some(value),
                UnaryOp::Minus => value.checked_neg(),
                UnaryOp::BitNot => Some(!value),
                UnaryOp::LogicNot => Some((value == 0) as i128),
                UnaryOp::ToTwoState => Some(value),
                UnaryOp::RedAnd => Some((value == -1) as i128),
                UnaryOp::RedOr => Some((value != 0) as i128),
                UnaryOp::RedXor => Some((value.count_ones() & 1) as i128),
            }
        }
        ConstExpr::Binary { left, op, right } => {
            let left = eval_const_expr(left, constants)?;
            let right = eval_const_expr(right, constants)?;
            match op {
                BinaryOp::Add => left.checked_add(right),
                BinaryOp::Sub => left.checked_sub(right),
                BinaryOp::Mul => left.checked_mul(right),
                BinaryOp::Div => (right != 0).then(|| left / right),
                BinaryOp::Mod => (right != 0).then(|| left % right),
                BinaryOp::Shl => shift_amount(right).and_then(|right| left.checked_shl(right)),
                BinaryOp::Shr => shift_amount(right).and_then(|right| left.checked_shr(right)),
                BinaryOp::BitAnd => Some(left & right),
                BinaryOp::BitOr => Some(left | right),
                BinaryOp::BitXor => Some(left ^ right),
                BinaryOp::LogicAnd => Some(((left != 0) && (right != 0)) as i128),
                BinaryOp::LogicOr => Some(((left != 0) || (right != 0)) as i128),
                BinaryOp::Eq => Some((left == right) as i128),
                BinaryOp::Ne => Some((left != right) as i128),
                BinaryOp::EqWildcard => Some((left == right) as i128),
                BinaryOp::NeWildcard => Some((left != right) as i128),
                BinaryOp::Lt => Some((left < right) as i128),
                BinaryOp::Le => Some((left <= right) as i128),
                BinaryOp::Gt => Some((left > right) as i128),
                BinaryOp::Ge => Some((left >= right) as i128),
            }
        }
        ConstExpr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            if eval_const_expr(condition, constants)? != 0 {
                eval_const_expr(then_expr, constants)
            } else {
                eval_const_expr(else_expr, constants)
            }
        }
    }
}

fn eval_const_function(
    name: &str,
    args: &[ConstExpr],
    constants: &HashMap<String, i128>,
) -> Option<i128> {
    let [arg] = args else {
        return None;
    };
    let value = eval_const_expr(arg, constants)?;
    match name {
        "$clog2" => clog2(value),
        "$onehot" => nonnegative_u128(value).map(|value| (value.count_ones() == 1) as i128),
        "$onehot0" => nonnegative_u128(value).map(|value| (value.count_ones() <= 1) as i128),
        _ => None,
    }
}

fn clog2(value: i128) -> Option<i128> {
    if value < 0 {
        return None;
    }
    let value = value as u128;
    if value <= 1 {
        return Some(0);
    }
    Some((u128::BITS - (value - 1).leading_zeros()) as i128)
}

fn nonnegative_u128(value: i128) -> Option<u128> {
    (value >= 0).then_some(value as u128)
}

fn shift_amount(value: i128) -> Option<u32> {
    u32::try_from(value).ok()
}

fn literal_as_i128(value: &str) -> Option<i128> {
    parse_integral_literal(value).and_then(|literal| {
        if literal.mask != BigUint::default() {
            return None;
        }
        i128::try_from(literal.value).ok()
    })
}

pub fn parse_integral_literal(value: &str) -> Option<IntegralLiteral> {
    let normalized = value.trim().replace('_', "");
    if let Some(literal) = parse_unbased_unsized_literal(&normalized) {
        return Some(literal);
    }
    if let Some((size, based)) = normalized.split_once('\'') {
        parse_based_literal(size, based)
    } else {
        parse_decimal_literal(&normalized)
    }
}

fn parse_unbased_unsized_literal(value: &str) -> Option<IntegralLiteral> {
    let mut chars = value.chars();
    (chars.next()? == '\'' && chars.clone().count() == 1).then_some(())?;
    let ch = chars.next()?.to_ascii_lowercase();
    let all_ones = (BigUint::from(1u8) << 32usize) - BigUint::from(1u8);
    let (value, mask) = match ch {
        '0' => (BigUint::default(), BigUint::default()),
        '1' => (all_ones, BigUint::default()),
        'x' => (all_ones.clone(), all_ones),
        'z' | '?' => (BigUint::default(), all_ones),
        _ => return None,
    };
    Some(IntegralLiteral {
        width: 32,
        signed: false,
        value,
        mask,
    })
}

fn parse_decimal_literal(value: &str) -> Option<IntegralLiteral> {
    let value = value.parse::<BigUint>().ok()?;
    let width = value.bits().max(32) as usize;
    Some(IntegralLiteral {
        width,
        signed: true,
        value,
        mask: BigUint::default(),
    })
}

fn parse_based_literal(size: &str, based: &str) -> Option<IntegralLiteral> {
    let width = if size.is_empty() {
        None
    } else {
        Some(size.parse::<usize>().ok()?.max(1))
    };
    let mut chars = based.chars();
    let first = chars.next()?.to_ascii_lowercase();
    let (signed, base) = if first == 's' {
        (true, chars.next()?.to_ascii_lowercase())
    } else {
        (false, first)
    };
    let digit_text = chars.as_str();
    let bits_per_digit = match base {
        'b' => 1,
        'o' => 3,
        'd' => 0,
        'h' => 4,
        _ => return None,
    };
    if digit_text.is_empty() {
        return None;
    }

    let leading_digit = digit_text.chars().next()?.to_ascii_lowercase();
    let parsed = if base == 'd' {
        parse_based_decimal_digits(digit_text)?
    } else {
        parse_power_of_two_based_digits(digit_text, bits_per_digit)?
    };
    let natural_width = if bits_per_digit == 0 {
        parsed.value.bits().max(32) as usize
    } else {
        digit_text.chars().count().checked_mul(bits_per_digit)?
    };
    let width = width.unwrap_or_else(|| natural_width.max(32));
    Some(resize_integral_literal(
        parsed,
        width,
        signed,
        extension_for_leading_digit(leading_digit),
    ))
}

fn parse_based_decimal_digits(digits: &str) -> Option<IntegralLiteral> {
    let mut value = BigUint::default();
    let mut mask = BigUint::default();
    for ch in digits.chars() {
        match ch {
            '0'..='9' => {
                value *= 10u8;
                value += ch.to_digit(10)?;
                mask *= 10u8;
            }
            'x' | 'X' => {
                value = BigUint::from(1u8);
                mask = BigUint::from(1u8);
            }
            'z' | 'Z' | '?' => {
                value = BigUint::default();
                mask = BigUint::from(1u8);
            }
            _ => return None,
        }
    }
    Some(IntegralLiteral {
        width: value.bits().max(1) as usize,
        signed: false,
        value,
        mask,
    })
}

fn parse_power_of_two_based_digits(digits: &str, bits_per_digit: usize) -> Option<IntegralLiteral> {
    let mut value = BigUint::default();
    let mut mask = BigUint::default();
    for ch in digits.chars() {
        value <<= bits_per_digit;
        mask <<= bits_per_digit;
        match ch {
            '0'..='9' | 'a'..='f' | 'A'..='F' => {
                let digit = ch.to_digit(16)?;
                if digit >= (1 << bits_per_digit) {
                    return None;
                }
                value |= BigUint::from(digit);
            }
            'x' | 'X' => {
                let unknown = (BigUint::from(1u8) << bits_per_digit) - BigUint::from(1u8);
                value |= &unknown;
                mask |= unknown;
            }
            'z' | 'Z' | '?' => {
                mask |= (BigUint::from(1u8) << bits_per_digit) - BigUint::from(1u8);
            }
            _ => return None,
        }
    }
    Some(IntegralLiteral {
        width: digits.chars().count() * bits_per_digit,
        signed: false,
        value,
        mask,
    })
}

fn resize_integral_literal(
    literal: IntegralLiteral,
    width: usize,
    signed: bool,
    extension: (bool, bool),
) -> IntegralLiteral {
    let keep = if width == 0 {
        BigUint::default()
    } else {
        (BigUint::from(1u8) << width) - BigUint::from(1u8)
    };
    let (extend_value, extend_mask) = extension;
    let extension_bits = width.saturating_sub(literal.width);
    let extension_mask = if extension_bits == 0 {
        BigUint::default()
    } else {
        ((BigUint::from(1u8) << extension_bits) - BigUint::from(1u8)) << literal.width
    };
    let extended_value = if extend_value {
        literal.value | &extension_mask
    } else {
        literal.value
    };
    let extended_mask = if extend_mask {
        literal.mask | extension_mask
    } else {
        literal.mask
    };
    IntegralLiteral {
        width,
        signed,
        value: extended_value & &keep,
        mask: extended_mask & keep,
    }
}

fn extension_for_leading_digit(ch: char) -> (bool, bool) {
    match ch {
        'x' => (true, true),
        'z' | '?' => (false, true),
        _ => (false, false),
    }
}

#[cfg(test)]
mod literal_tests {
    use super::*;

    #[test]
    fn parses_sized_based_literals() {
        let lit = parse_integral_literal("8'hf_f").unwrap();
        assert_eq!(lit.width, 8);
        assert!(!lit.signed);
        assert_eq!(lit.value, BigUint::from(0xffu32));
        assert_eq!(lit.mask, BigUint::default());
    }

    #[test]
    fn parses_unsized_based_literals_as_at_least_32_bits() {
        let lit = parse_integral_literal("'b1010").unwrap();
        assert_eq!(lit.width, 32);
        assert_eq!(lit.value, BigUint::from(10u32));
    }

    #[test]
    fn preserves_unknown_and_high_impedance_masks() {
        let lit = parse_integral_literal("4'b10xz").unwrap();
        assert_eq!(lit.width, 4);
        assert_eq!(lit.value, BigUint::from(0b1010u32));
        assert_eq!(lit.mask, BigUint::from(0b0011u32));

        let lit = parse_integral_literal("8'hz?").unwrap();
        assert_eq!(lit.width, 8);
        assert_eq!(lit.value, BigUint::default());
        assert_eq!(lit.mask, BigUint::from(0xffu32));

        let lit = parse_integral_literal("8'hx").unwrap();
        assert_eq!(lit.width, 8);
        assert_eq!(lit.value, BigUint::from(0xffu32));
        assert_eq!(lit.mask, BigUint::from(0xffu32));
    }

    #[test]
    fn parses_signed_based_literals() {
        let lit = parse_integral_literal("12'sd42").unwrap();
        assert_eq!(lit.width, 12);
        assert!(lit.signed);
        assert_eq!(lit.value, BigUint::from(42u32));
    }

    #[test]
    fn truncates_to_explicit_size() {
        let lit = parse_integral_literal("4'hff").unwrap();
        assert_eq!(lit.width, 4);
        assert_eq!(lit.value, BigUint::from(0xfu32));
        assert_eq!(lit.mask, BigUint::default());
    }

    #[test]
    fn rejects_invalid_digits_for_base() {
        assert!(parse_integral_literal("4'b102").is_none());
        assert!(parse_integral_literal("4'o8").is_none());
    }
}
