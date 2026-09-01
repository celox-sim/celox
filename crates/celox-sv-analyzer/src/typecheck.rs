//! Type and width resolution helpers.

use fxhash::FxHashMap as HashMap;

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
    resolve_packed_width_with_env(ranges, &HashMap::default())
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
        let width = usize::try_from(left.abs_diff(right)).ok()?.checked_add(1)?;
        acc.checked_mul(width)
    })
}

pub fn eval_const_expr(expr: &ConstExpr, constants: &HashMap<String, i128>) -> Option<i128> {
    match expr {
        ConstExpr::Literal(value) => literal_as_i128(value),
        ConstExpr::Ident(name) => constants.get(name).copied(),
        ConstExpr::Select { expr, bit } => {
            let bit = eval_const_expr(bit, constants)?;
            let bit = usize::try_from(bit).ok()?;
            if let ConstExpr::Literal(value) = &**expr {
                let value = parse_integral_literal(value)?;
                if bit >= value.width || value.mask.bit(bit as u64) {
                    return None;
                }
                return Some(value.value.bit(bit as u64) as i128);
            }
            let value = eval_const_expr(expr, constants)?;
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
                // The constant environment currently stores values without their
                // declared widths.  An all-ones reduction therefore cannot be
                // evaluated correctly for an identifier such as a 4-bit 4'hf.
                // Sized literal reductions are handled above; reject other
                // reduction-AND operands until constant widths are preserved.
                UnaryOp::RedAnd => None,
                UnaryOp::RedOr => Some((value != 0) as i128),
                UnaryOp::RedXor => Some((value.count_ones() & 1) as i128),
            }
        }
        ConstExpr::Binary { left, op, right } => {
            if let Some(result) = eval_literal_binary(left, *op, right) {
                return Some(result);
            }
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
                // The untyped constant environment cannot recover the declared
                // width needed to zero-fill a negative value. Typed parameter
                // evaluation substitutes a sized literal before reaching this
                // fallback; reject any remaining ambiguous logical shift.
                BinaryOp::Shr if left.is_negative() => None,
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
        } => match eval_const_truth(condition, constants)? {
            Some(true) => eval_const_expr(then_expr, constants),
            Some(false) => eval_const_expr(else_expr, constants),
            None => merge_unknown_const_mux_arms(then_expr, else_expr, constants),
        },
    }
}

/// Evaluate a constant expression while preserving declared parameter types.
///
/// The plain constant environment stores only mathematical values. Replacing
/// typed identifiers with sized literals before evaluation preserves the
/// SystemVerilog width and signedness rules for mixed-type operations.
pub fn eval_const_expr_with_types(
    expr: &ConstExpr,
    constants: &HashMap<String, i128>,
    types: &HashMap<String, (usize, bool)>,
) -> Option<i128> {
    let expr = substitute_typed_constants(expr.clone(), constants, types);
    eval_const_expr(&expr, constants)
}

/// Evaluate a literal constant expression without discarding four-state masks.
pub fn eval_const_integral_literal_with_types(
    expr: &ConstExpr,
    constants: &HashMap<String, i128>,
    types: &HashMap<String, (usize, bool)>,
) -> Option<IntegralLiteral> {
    let expr = substitute_typed_constants(expr.clone(), constants, types);
    integral_literal_from_const_expr(&expr)
}

/// Evaluate and resize a constant expression in a case selector's comparison
/// context without discarding X/Z masks.
pub fn context_size_const_integral_literal(
    expr: &ConstExpr,
    constants: &HashMap<String, i128>,
    types: &HashMap<String, (usize, bool)>,
    width: usize,
    signed: bool,
) -> Option<IntegralLiteral> {
    let literal = eval_const_integral_literal_with_types(expr, constants, types)?;
    let extension = signed_extension(&literal, literal.signed);
    Some(resize_integral_literal(literal, width, signed, extension))
}

pub fn format_integral_literal_binary(literal: &IntegralLiteral) -> String {
    let bits = (0..literal.width)
        .rev()
        .map(|bit| {
            let bit = bit as u64;
            if literal.mask.bit(bit) {
                if literal.value.bit(bit) { 'x' } else { 'z' }
            } else if literal.value.bit(bit) {
                '1'
            } else {
                '0'
            }
        })
        .collect::<String>();
    let signing = if literal.signed { "s" } else { "" };
    format!("{}'{signing}b{bits}", literal.width)
}

pub fn substitute_typed_constants(
    expr: ConstExpr,
    constants: &HashMap<String, i128>,
    types: &HashMap<String, (usize, bool)>,
) -> ConstExpr {
    match expr {
        ConstExpr::Ident(name) => match (constants.get(&name), types.get(&name)) {
            (Some(value), Some((width, signed))) => {
                ConstExpr::Literal(format_typed_constant_literal(*value, *width, *signed))
            }
            _ => ConstExpr::Ident(name),
        },
        ConstExpr::Literal(value) => ConstExpr::Literal(value),
        ConstExpr::Select { expr, bit } => ConstExpr::Select {
            expr: Box::new(substitute_typed_constants(*expr, constants, types)),
            bit: Box::new(substitute_typed_constants(*bit, constants, types)),
        },
        ConstExpr::Function { name, args } => ConstExpr::Function {
            name,
            args: args
                .into_iter()
                .map(|arg| substitute_typed_constants(arg, constants, types))
                .collect(),
        },
        ConstExpr::Unary { op, expr } => ConstExpr::Unary {
            op,
            expr: Box::new(substitute_typed_constants(*expr, constants, types)),
        },
        ConstExpr::Binary { left, op, right } => ConstExpr::Binary {
            left: Box::new(substitute_typed_constants(*left, constants, types)),
            op,
            right: Box::new(substitute_typed_constants(*right, constants, types)),
        },
        ConstExpr::Mux {
            condition,
            then_expr,
            else_expr,
        } => ConstExpr::Mux {
            condition: Box::new(substitute_typed_constants(*condition, constants, types)),
            then_expr: Box::new(substitute_typed_constants(*then_expr, constants, types)),
            else_expr: Box::new(substitute_typed_constants(*else_expr, constants, types)),
        },
    }
}

fn format_typed_constant_literal(value: i128, width: usize, signed: bool) -> String {
    let signing = if signed { "s" } else { "" };
    if width <= 128 {
        let mask = if width == 128 {
            u128::MAX
        } else {
            (1u128 << width) - 1
        };
        let bits = (value as u128) & mask;
        format!("{width}'{signing}d{bits}")
    } else {
        let extension = if value.is_negative() { '1' } else { '0' };
        let high_bits = extension.to_string().repeat(width - 128);
        let low_bits = value as u128;
        format!("{width}'{signing}b{high_bits}{low_bits:0128b}")
    }
}

fn eval_literal_binary(left: &ConstExpr, op: BinaryOp, right: &ConstExpr) -> Option<i128> {
    let left_fill = unbased_fill_from_const_expr(left);
    let right_fill = unbased_fill_from_const_expr(right);
    let (mut left, mut right) = match (left_fill, right_fill) {
        (Some(left_fill), Some(right_fill)) => (
            integral_fill_literal(left_fill, 1)?,
            integral_fill_literal(right_fill, 1)?,
        ),
        (Some(fill), None) => {
            let right = integral_literal_from_const_expr(right)?;
            (integral_fill_literal(fill, right.width)?, right)
        }
        (None, Some(fill)) => {
            let left = integral_literal_from_const_expr(left)?;
            let right = integral_fill_literal(fill, left.width)?;
            (left, right)
        }
        (None, None) => (
            integral_literal_from_const_expr(left)?,
            integral_literal_from_const_expr(right)?,
        ),
    };
    if matches!(op, BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar) {
        if left.mask != BigUint::default() || right.mask != BigUint::default() {
            return None;
        }
        let amount = usize::try_from(integral_literal_as_i128(&right, right.signed)?).ok()?;
        let width = left.width;
        let signed = left.signed;
        let width_mask = (BigUint::from(1u8) << width) - BigUint::from(1u8);
        let value = match op {
            BinaryOp::Shl if amount < width => (&left.value << amount) & &width_mask,
            BinaryOp::Shl | BinaryOp::Shr if amount >= width => BigUint::default(),
            BinaryOp::Shr => &left.value >> amount,
            BinaryOp::Sar if amount >= width => {
                if signed && width != 0 && left.value.bit((width - 1) as u64) {
                    width_mask
                } else {
                    BigUint::default()
                }
            }
            BinaryOp::Sar => {
                let shifted = &left.value >> amount;
                if signed && width != 0 && left.value.bit((width - 1) as u64) && amount != 0 {
                    let fill = (&width_mask << (width - amount)) & &width_mask;
                    shifted | fill
                } else {
                    shifted
                }
            }
            _ => unreachable!(),
        };
        left.value = value;
        return integral_literal_as_i128(&left, signed);
    }

    if !matches!(
        op,
        BinaryOp::Add
            | BinaryOp::Sub
            | BinaryOp::Mul
            | BinaryOp::Div
            | BinaryOp::Mod
            | BinaryOp::BitAnd
            | BinaryOp::BitOr
            | BinaryOp::BitXor
            | BinaryOp::LogicAnd
            | BinaryOp::LogicOr
            | BinaryOp::Eq
            | BinaryOp::Ne
            | BinaryOp::Lt
            | BinaryOp::Le
            | BinaryOp::Gt
            | BinaryOp::Ge
    ) {
        return None;
    }
    let width = left.width.max(right.width);
    let signed = left.signed && right.signed;
    left = resize_integral_literal(left.clone(), width, signed, signed_extension(&left, signed));
    right = resize_integral_literal(
        right.clone(),
        width,
        signed,
        signed_extension(&right, signed),
    );
    if matches!(op, BinaryOp::LogicAnd | BinaryOp::LogicOr)
        || left.mask != BigUint::default()
        || right.mask != BigUint::default()
    {
        if let Some(result) = eval_four_state_binary(&left, op, &right, signed) {
            return Some(result);
        }
        if left.mask != BigUint::default() || right.mask != BigUint::default() {
            return None;
        }
    }
    if matches!(
        op,
        BinaryOp::Eq | BinaryOp::Ne | BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge
    ) {
        let ordering = if signed {
            integral_literal_as_i128(&left, true)?.cmp(&integral_literal_as_i128(&right, true)?)
        } else {
            left.value.cmp(&right.value)
        };
        let result = match op {
            BinaryOp::Eq => ordering.is_eq(),
            BinaryOp::Ne => ordering.is_ne(),
            BinaryOp::Lt => ordering.is_lt(),
            BinaryOp::Le => ordering.is_le(),
            BinaryOp::Gt => ordering.is_gt(),
            BinaryOp::Ge => ordering.is_ge(),
            _ => unreachable!(),
        };
        return Some(result as i128);
    }
    let modulus = BigUint::from(1u8) << width;
    let width_mask = &modulus - BigUint::from(1u8);
    if matches!(op, BinaryOp::Div | BinaryOp::Mod) {
        if right.value == BigUint::default() {
            return None;
        }
        if signed {
            let left = integral_literal_as_i128(&left, true)?;
            let right = integral_literal_as_i128(&right, true)?;
            let value = match op {
                BinaryOp::Div if left == i128::MIN && right == -1 => i128::MIN,
                BinaryOp::Mod if left == i128::MIN && right == -1 => 0,
                BinaryOp::Div => left.checked_div(right)?,
                BinaryOp::Mod => left.checked_rem(right)?,
                _ => unreachable!(),
            };
            let value = BigUint::from(value as u128) & &width_mask;
            return integral_literal_as_i128(
                &IntegralLiteral {
                    width,
                    signed,
                    value,
                    mask: BigUint::default(),
                },
                true,
            );
        }
        let value = match op {
            BinaryOp::Div => left.value / right.value,
            BinaryOp::Mod => left.value % right.value,
            _ => unreachable!(),
        };
        return i128::try_from(value).ok();
    }
    let value = match op {
        BinaryOp::Add => (left.value + right.value) & &width_mask,
        BinaryOp::Sub => (left.value + &modulus - right.value) & &width_mask,
        BinaryOp::Mul => (left.value * right.value) & &width_mask,
        BinaryOp::BitAnd => left.value & right.value,
        BinaryOp::BitOr => left.value | right.value,
        BinaryOp::BitXor => left.value ^ right.value,
        _ => unreachable!(),
    };
    integral_literal_as_i128(
        &IntegralLiteral {
            width,
            signed,
            value,
            mask: BigUint::default(),
        },
        signed,
    )
}

fn eval_four_state_binary(
    left: &IntegralLiteral,
    op: BinaryOp,
    right: &IntegralLiteral,
    signed: bool,
) -> Option<i128> {
    if matches!(op, BinaryOp::LogicAnd | BinaryOp::LogicOr) {
        let left = integral_literal_truth(left);
        let right = integral_literal_truth(right);
        return match op {
            BinaryOp::LogicAnd if left == Some(false) || right == Some(false) => Some(0),
            BinaryOp::LogicAnd if left == Some(true) && right == Some(true) => Some(1),
            BinaryOp::LogicOr if left == Some(true) || right == Some(true) => Some(1),
            BinaryOp::LogicOr if left == Some(false) && right == Some(false) => Some(0),
            _ => None,
        };
    }
    if !matches!(op, BinaryOp::BitAnd | BinaryOp::BitOr | BinaryOp::BitXor) {
        return None;
    }

    let result = eval_four_state_binary_literal(left, op, right, signed)?;
    integral_literal_as_i128(&result, signed)
}

fn eval_four_state_binary_literal(
    left: &IntegralLiteral,
    op: BinaryOp,
    right: &IntegralLiteral,
    signed: bool,
) -> Option<IntegralLiteral> {
    if !matches!(op, BinaryOp::BitAnd | BinaryOp::BitOr | BinaryOp::BitXor) {
        return None;
    }

    let width = left.width;
    let width_mask = (BigUint::from(1u8) << width) - BigUint::from(1u8);
    let left_known = &width_mask ^ &left.mask;
    let right_known = &width_mask ^ &right.mask;
    let left_one = &left.value & &left_known;
    let right_one = &right.value & &right_known;
    let left_zero = &left_known ^ &left_one;
    let right_zero = &right_known ^ &right_one;
    let (known_zero, known_one) = match op {
        BinaryOp::BitAnd => (left_zero | right_zero, left_one & right_one),
        BinaryOp::BitOr => (left_zero & right_zero, left_one | right_one),
        BinaryOp::BitXor => {
            let known = &left_known & &right_known;
            let one = (&left.value ^ &right.value) & &known;
            (&known ^ &one, one)
        }
        _ => unreachable!(),
    };
    let known = &known_zero | &known_one;
    let mask = &width_mask ^ known;
    Some(IntegralLiteral {
        width,
        signed,
        value: known_one | &mask,
        mask,
    })
}

fn integral_literal_truth(literal: &IntegralLiteral) -> Option<bool> {
    let width_mask = (BigUint::from(1u8) << literal.width) - BigUint::from(1u8);
    let known = width_mask ^ &literal.mask;
    if (&literal.value & known) != BigUint::default() {
        Some(true)
    } else if literal.mask == BigUint::default() {
        Some(false)
    } else {
        None
    }
}

fn eval_const_truth(expr: &ConstExpr, constants: &HashMap<String, i128>) -> Option<Option<bool>> {
    if let Some(literal) = integral_literal_from_const_expr(expr) {
        return Some(integral_literal_truth(&literal));
    }
    eval_const_expr(expr, constants).map(|value| Some(value != 0))
}

fn merge_unknown_const_mux_arms(
    then_expr: &ConstExpr,
    else_expr: &ConstExpr,
    constants: &HashMap<String, i128>,
) -> Option<i128> {
    if let (Some(then_value), Some(else_value)) = (
        eval_const_expr(then_expr, constants),
        eval_const_expr(else_expr, constants),
    ) && then_value == else_value
    {
        return Some(then_value);
    }

    let mut then_literal = integral_literal_from_const_expr(then_expr)?;
    let mut else_literal = integral_literal_from_const_expr(else_expr)?;
    let width = then_literal.width.max(else_literal.width);
    let signed = then_literal.signed && else_literal.signed;
    let then_extension = signed_extension(&then_literal, signed);
    let else_extension = signed_extension(&else_literal, signed);
    then_literal = resize_integral_literal(then_literal, width, signed, then_extension);
    else_literal = resize_integral_literal(else_literal, width, signed, else_extension);

    let width_mask = (BigUint::from(1u8) << width) - BigUint::from(1u8);
    let same_value = &width_mask ^ (&then_literal.value ^ &else_literal.value);
    let same_mask = &width_mask ^ (&then_literal.mask ^ &else_literal.mask);
    let matching = same_value & same_mask;
    let mask = &then_literal.mask | &else_literal.mask | (&width_mask ^ &matching);
    let value = (&then_literal.value & &matching) | &mask;
    integral_literal_as_i128(
        &IntegralLiteral {
            width,
            signed,
            value,
            mask,
        },
        signed,
    )
}

fn integral_literal_from_const_expr(expr: &ConstExpr) -> Option<IntegralLiteral> {
    match expr {
        ConstExpr::Literal(literal) => parse_integral_literal(literal),
        ConstExpr::Unary {
            op: UnaryOp::BitNot,
            expr,
        } => {
            let mut literal = integral_literal_from_const_expr(expr)?;
            let width_mask = (BigUint::from(1u8) << literal.width) - BigUint::from(1u8);
            let known = &width_mask ^ &literal.mask;
            literal.value = ((&width_mask ^ literal.value) & known) | &literal.mask;
            Some(literal)
        }
        ConstExpr::Unary {
            op: UnaryOp::ToTwoState,
            expr,
        } => {
            let mut literal = integral_literal_from_const_expr(expr)?;
            let width_mask = (BigUint::from(1u8) << literal.width) - BigUint::from(1u8);
            literal.value &= width_mask ^ &literal.mask;
            literal.mask = BigUint::default();
            Some(literal)
        }
        ConstExpr::Binary { left, op, right }
            if matches!(op, BinaryOp::BitAnd | BinaryOp::BitOr | BinaryOp::BitXor) =>
        {
            let mut left = integral_literal_from_const_expr(left)?;
            let mut right = integral_literal_from_const_expr(right)?;
            let width = left.width.max(right.width);
            let signed = left.signed && right.signed;
            let left_extension = signed_extension(&left, signed);
            let right_extension = signed_extension(&right, signed);
            left = resize_integral_literal(left, width, signed, left_extension);
            right = resize_integral_literal(right, width, signed, right_extension);
            eval_four_state_binary_literal(&left, *op, &right, signed)
        }
        _ => None,
    }
}

fn integral_literal_as_i128(literal: &IntegralLiteral, signed: bool) -> Option<i128> {
    if literal.width > 128 || literal.mask != BigUint::default() {
        return None;
    }
    let value = u128::try_from(literal.value.clone()).ok()?;
    if !signed || literal.width == 0 || value & (1u128 << (literal.width - 1)) == 0 {
        return i128::try_from(value).ok();
    }
    if literal.width == 128 {
        Some(value as i128)
    } else {
        i128::try_from(value)
            .ok()
            .and_then(|value| value.checked_sub(1i128 << literal.width))
    }
}

fn eval_literal_unary(op: UnaryOp, literal: &str) -> Option<i128> {
    let literal_text = literal;
    let literal = parse_integral_literal(literal_text)?;
    if op == UnaryOp::ToTwoState {
        let mut literal = literal;
        let width_mask = (BigUint::from(1u8) << literal.width) - BigUint::from(1u8);
        literal.value &= width_mask ^ &literal.mask;
        literal.mask = BigUint::default();
        return integral_literal_as_i128(&literal, literal.signed);
    }
    if literal.mask != BigUint::default() {
        return None;
    }
    let value = literal_as_i128(literal_text)?;
    match op {
        UnaryOp::Plus => Some(value),
        UnaryOp::Minus => {
            let modulus = BigUint::from(1u8) << literal.width;
            let negated = if literal.value == BigUint::default() {
                BigUint::default()
            } else {
                &modulus - literal.value
            };
            integral_literal_as_i128(
                &IntegralLiteral {
                    width: literal.width,
                    signed: literal.signed,
                    value: negated,
                    mask: BigUint::default(),
                },
                literal.signed,
            )
        }
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

fn unbased_fill_from_const_expr(expr: &ConstExpr) -> Option<char> {
    let ConstExpr::Literal(value) = expr else {
        return None;
    };
    unbased_fill_literal(value)
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
    match name {
        "$clog2" => clog2(eval_const_expr(arg, constants)?),
        "$onehot" | "$onehot0" => {
            let value = const_expr_bit_pattern(arg, constants)?;
            let ones = value.iter_u64_digits().map(u64::count_ones).sum::<u32>();
            Some(match name {
                "$onehot" => (ones == 1) as i128,
                "$onehot0" => (ones <= 1) as i128,
                _ => unreachable!(),
            })
        }
        _ => None,
    }
}

fn const_expr_bit_pattern(expr: &ConstExpr, constants: &HashMap<String, i128>) -> Option<BigUint> {
    if let Some(literal) = integral_literal_from_const_expr(expr) {
        return (literal.mask == BigUint::default()).then_some(literal.value);
    }
    let value = eval_const_expr(expr, constants)?;
    (value >= 0).then(|| BigUint::from(value as u128))
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
            // Constant values are stored together with their width and signedness
            // in parallel environments. Preserve all 128 bits of an unsigned
            // literal in the i128 slot as a bit pattern; typed substitution turns
            // it back into a sized unsigned literal before evaluation.
            if !literal.signed && literal.width == 128 {
                return Some(value as i128);
            }
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
    fn preserves_128_bit_unsigned_literals_as_bit_patterns() {
        let expr = ConstExpr::Literal("128'h80000000000000000000000000000000".to_string());

        assert_eq!(eval_const_expr(&expr, &HashMap::default()), Some(i128::MIN));
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

        assert_eq!(eval_const_expr(&eq, &HashMap::default()), Some(1));
        assert_eq!(eval_const_expr(&ne, &HashMap::default()), Some(1));
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

        assert_eq!(eval_const_expr(&eq, &HashMap::default()), Some(1));
        assert_eq!(eval_const_expr(&ne, &HashMap::default()), Some(0));
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

        assert_eq!(eval_const_expr(&eq, &HashMap::default()), Some(1));
        assert_eq!(eval_const_expr(&ne, &HashMap::default()), Some(1));
        assert_eq!(eval_const_expr(&indeterminate, &HashMap::default()), None);
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

            assert_eq!(eval_const_expr(&expr, &HashMap::default()), None);
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

        assert_eq!(eval_const_expr(&minus, &HashMap::default()), Some(1));
        assert_eq!(eval_const_expr(&bit_not, &HashMap::default()), Some(-1));
    }

    #[test]
    fn wraps_unsigned_literal_negation_to_its_width() {
        let expr = ConstExpr::Unary {
            op: UnaryOp::Minus,
            expr: Box::new(ConstExpr::Literal("8'd1".to_string())),
        };

        assert_eq!(eval_const_expr(&expr, &HashMap::default()), Some(0xff));
    }

    #[test]
    fn wraps_sized_literal_arithmetic_to_expression_width() {
        let expr = ConstExpr::Binary {
            left: Box::new(ConstExpr::Literal("8'hff".to_string())),
            op: BinaryOp::Add,
            right: Box::new(ConstExpr::Literal("8'h01".to_string())),
        };

        assert_eq!(eval_const_expr(&expr, &HashMap::default()), Some(0));
    }

    #[test]
    fn logically_shifts_signed_literals_right() {
        let expr = ConstExpr::Binary {
            left: Box::new(ConstExpr::Literal("8'shfe".to_string())),
            op: BinaryOp::Shr,
            right: Box::new(ConstExpr::Literal("1".to_string())),
        };

        assert_eq!(eval_const_expr(&expr, &HashMap::default()), Some(0x7f));
    }

    #[test]
    fn compares_mixed_signed_literals_as_unsigned() {
        let expr = ConstExpr::Binary {
            left: Box::new(ConstExpr::Literal("8'shff".to_string())),
            op: BinaryOp::Lt,
            right: Box::new(ConstExpr::Literal("8'h01".to_string())),
        };

        assert_eq!(eval_const_expr(&expr, &HashMap::default()), Some(0));
    }

    #[test]
    fn divides_and_remainders_mixed_signed_literals_as_unsigned() {
        for (op, expected) in [(BinaryOp::Div, 127), (BinaryOp::Mod, 0)] {
            let expr = ConstExpr::Binary {
                left: Box::new(ConstExpr::Literal("8'shfe".to_string())),
                op,
                right: Box::new(ConstExpr::Literal("8'h02".to_string())),
            };

            assert_eq!(eval_const_expr(&expr, &HashMap::default()), Some(expected));
        }
    }
}
