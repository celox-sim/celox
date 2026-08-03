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
            if let ConstExpr::Literal(literal) = &**expr
                && let Some(result) = eval_literal_unary(*op, literal)
            {
                return Some(result);
            }
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
            if matches!(
                op,
                BinaryOp::EqCase | BinaryOp::NeCase | BinaryOp::EqWildcard | BinaryOp::NeWildcard
            ) && let Some(result) = eval_literal_four_state_equality(left, *op, right)
            {
                return Some(result as i128);
            }
            let left = eval_const_expr(left, constants)?;
            let right = eval_const_expr(right, constants)?;
            match op {
                BinaryOp::Add => left.checked_add(right),
                BinaryOp::Sub => left.checked_sub(right),
                BinaryOp::Mul => left.checked_mul(right),
                BinaryOp::Div => left.checked_div(right),
                BinaryOp::Mod => left.checked_rem(right),
                BinaryOp::Shl => shift_amount(right).and_then(|right| left.checked_shl(right)),
                BinaryOp::Shr => shift_amount(right).and_then(|right| left.checked_shr(right)),
                BinaryOp::Sar => shift_amount(right).and_then(|right| left.checked_shr(right)),
                BinaryOp::BitAnd => Some(left & right),
                BinaryOp::BitOr => Some(left | right),
                BinaryOp::BitXor => Some(left ^ right),
                BinaryOp::LogicAnd => Some(((left != 0) && (right != 0)) as i128),
                BinaryOp::LogicOr => Some(((left != 0) || (right != 0)) as i128),
                BinaryOp::Eq => Some((left == right) as i128),
                BinaryOp::Ne => Some((left != right) as i128),
                BinaryOp::EqCase => Some((left == right) as i128),
                BinaryOp::NeCase => Some((left != right) as i128),
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

fn eval_literal_unary(op: UnaryOp, literal: &str) -> Option<i128> {
    let literal_text = literal;
    let literal = parse_integral_literal(literal_text)?;
    if literal.mask != BigUint::default() {
        return None;
    }
    let value = literal_as_i128(literal_text)?;
    match op {
        UnaryOp::Plus => Some(value),
        UnaryOp::Minus => value.checked_neg(),
        UnaryOp::BitNot if literal.signed => Some(!value),
        UnaryOp::BitNot => {
            let width_mask = (BigUint::from(1u8) << literal.width) - BigUint::from(1u8);
            i128::try_from(width_mask ^ literal.value).ok()
        }
        UnaryOp::LogicNot => Some((literal.value == BigUint::default()) as i128),
        UnaryOp::ToTwoState => Some(value),
        UnaryOp::RedAnd => {
            let width_mask = (BigUint::from(1u8) << literal.width) - BigUint::from(1u8);
            Some((literal.value == width_mask) as i128)
        }
        UnaryOp::RedOr => Some((literal.value != BigUint::default()) as i128),
        UnaryOp::RedXor => Some(
            (literal
                .value
                .iter_u64_digits()
                .map(u64::count_ones)
                .sum::<u32>()
                & 1) as i128,
        ),
    }
}

fn eval_literal_four_state_equality(
    left: &ConstExpr,
    op: BinaryOp,
    right: &ConstExpr,
) -> Option<bool> {
    let ConstExpr::Literal(left) = left else {
        return None;
    };
    let ConstExpr::Literal(right) = right else {
        return None;
    };
    let left_fill = unbased_fill_literal(left);
    let right_fill = unbased_fill_literal(right);
    let (mut left, mut right) = match (left_fill, right_fill) {
        (Some(left_fill), Some(right_fill)) => (
            integral_fill_literal(left_fill, 1)?,
            integral_fill_literal(right_fill, 1)?,
        ),
        (Some(fill), None) => {
            let right = parse_integral_literal(right)?;
            (integral_fill_literal(fill, right.width)?, right)
        }
        (None, Some(fill)) => {
            let left = parse_integral_literal(left)?;
            let right = integral_fill_literal(fill, left.width)?;
            (left, right)
        }
        (None, None) => (
            parse_integral_literal(left)?,
            parse_integral_literal(right)?,
        ),
    };
    let width = left.width.max(right.width);
    let signed = left.signed && right.signed;
    let left_extension = signed_extension(&left, signed);
    let right_extension = signed_extension(&right, signed);
    left = resize_integral_literal(left, width, signed, left_extension);
    right = resize_integral_literal(right, width, signed, right_extension);
    let equal = match op {
        BinaryOp::EqCase | BinaryOp::NeCase => left.value == right.value && left.mask == right.mask,
        BinaryOp::EqWildcard | BinaryOp::NeWildcard => {
            let width_mask = (BigUint::from(1u8) << width) - BigUint::from(1u8);
            let compare_mask = &width_mask ^ &right.mask;
            let lhs_definite = &width_mask ^ &left.mask;
            let definite_compare = &compare_mask & lhs_definite;
            let mismatch = (&left.value ^ &right.value) & definite_compare;
            if mismatch != BigUint::default() {
                false
            } else if (&left.mask & compare_mask) != BigUint::default() {
                return None;
            } else {
                true
            }
        }
        _ => return None,
    };
    Some(if matches!(op, BinaryOp::NeCase | BinaryOp::NeWildcard) {
        !equal
    } else {
        equal
    })
}

fn unbased_fill_literal(value: &str) -> Option<char> {
    let normalized = value.trim().to_ascii_lowercase();
    let mut chars = normalized.chars();
    (chars.next()? == '\'' && chars.clone().count() == 1).then_some(chars.next()?)
}

fn integral_fill_literal(fill: char, width: usize) -> Option<IntegralLiteral> {
    let all_ones = (BigUint::from(1u8) << width) - BigUint::from(1u8);
    let (value, mask) = match fill {
        '0' => (BigUint::default(), BigUint::default()),
        '1' => (all_ones, BigUint::default()),
        'x' => (all_ones.clone(), all_ones),
        'z' | '?' => (BigUint::default(), all_ones),
        _ => return None,
    };
    Some(IntegralLiteral {
        width,
        signed: false,
        value,
        mask,
    })
}

fn signed_extension(literal: &IntegralLiteral, signed: bool) -> (bool, bool) {
    if !signed || literal.width == 0 {
        return (false, false);
    }
    let sign_bit = (literal.width - 1) as u64;
    (literal.value.bit(sign_bit), literal.mask.bit(sign_bit))
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
    let explicitly_signed = value
        .split_once('\'')
        .is_some_and(|(_, based)| matches!(based.trim_start().chars().next(), Some('s' | 'S')));
    parse_integral_literal(value).and_then(|literal| {
        if literal.mask != BigUint::default() {
            return None;
        }
        let value = u128::try_from(literal.value).ok()?;
        if !explicitly_signed || !literal.signed || literal.width == 0 {
            return i128::try_from(value).ok();
        }
        if literal.width > 128 {
            return None;
        }
        let sign_bit = 1u128 << (literal.width - 1);
        if value & sign_bit == 0 {
            i128::try_from(value).ok()
        } else if literal.width == 128 {
            Some(value as i128)
        } else {
            i128::try_from(value)
                .ok()
                .and_then(|value| value.checked_sub(1i128 << literal.width))
        }
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

    #[test]
    fn evaluates_unknown_literals_in_constant_case_equality() {
        let eq = ConstExpr::Binary {
            left: Box::new(ConstExpr::Literal("1'bx".to_string())),
            op: BinaryOp::EqCase,
            right: Box::new(ConstExpr::Literal("1'bx".to_string())),
        };
        let ne = ConstExpr::Binary {
            left: Box::new(ConstExpr::Literal("1'bx".to_string())),
            op: BinaryOp::NeCase,
            right: Box::new(ConstExpr::Literal("1'bz".to_string())),
        };

        assert_eq!(eval_const_expr(&eq, &HashMap::new()), Some(1));
        assert_eq!(eval_const_expr(&ne, &HashMap::new()), Some(1));
    }

    #[test]
    fn context_sizes_fills_in_constant_case_equality() {
        let eq = ConstExpr::Binary {
            left: Box::new(ConstExpr::Literal("8'hff".to_string())),
            op: BinaryOp::EqCase,
            right: Box::new(ConstExpr::Literal("'1".to_string())),
        };
        let ne = ConstExpr::Binary {
            left: Box::new(ConstExpr::Literal("4'bxxxx".to_string())),
            op: BinaryOp::NeCase,
            right: Box::new(ConstExpr::Literal("'x".to_string())),
        };

        assert_eq!(eval_const_expr(&eq, &HashMap::new()), Some(1));
        assert_eq!(eval_const_expr(&ne, &HashMap::new()), Some(0));
    }

    #[test]
    fn evaluates_masked_literals_in_constant_wildcard_equality() {
        let eq = ConstExpr::Binary {
            left: Box::new(ConstExpr::Literal("2'b10".to_string())),
            op: BinaryOp::EqWildcard,
            right: Box::new(ConstExpr::Literal("2'b1x".to_string())),
        };
        let ne = ConstExpr::Binary {
            left: Box::new(ConstExpr::Literal("2'b00".to_string())),
            op: BinaryOp::NeWildcard,
            right: Box::new(ConstExpr::Literal("2'b1z".to_string())),
        };
        let indeterminate = ConstExpr::Binary {
            left: Box::new(ConstExpr::Literal("2'bx0".to_string())),
            op: BinaryOp::EqWildcard,
            right: Box::new(ConstExpr::Literal("2'b10".to_string())),
        };

        assert_eq!(eval_const_expr(&eq, &HashMap::new()), Some(1));
        assert_eq!(eval_const_expr(&ne, &HashMap::new()), Some(1));
        assert_eq!(eval_const_expr(&indeterminate, &HashMap::new()), None);
    }

    #[test]
    fn rejects_overflowing_constant_division_and_remainder() {
        for op in [BinaryOp::Div, BinaryOp::Mod] {
            let expr = ConstExpr::Binary {
                left: Box::new(ConstExpr::Literal(i128::MIN.to_string())),
                op,
                right: Box::new(ConstExpr::Unary {
                    op: UnaryOp::Minus,
                    expr: Box::new(ConstExpr::Literal("1".to_string())),
                }),
            };

            assert_eq!(eval_const_expr(&expr, &HashMap::new()), None);
        }
    }

    #[test]
    fn sign_interprets_literals_before_unary_operations() {
        let minus = ConstExpr::Unary {
            op: UnaryOp::Minus,
            expr: Box::new(ConstExpr::Literal("8'shff".to_string())),
        };
        let bit_not = ConstExpr::Unary {
            op: UnaryOp::BitNot,
            expr: Box::new(ConstExpr::Literal("8'sh00".to_string())),
        };

        assert_eq!(eval_const_expr(&minus, &HashMap::new()), Some(1));
        assert_eq!(eval_const_expr(&bit_not, &HashMap::new()), Some(-1));
    }
}
