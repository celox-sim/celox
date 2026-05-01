use num_bigint::{BigInt, BigUint, Sign};
use num_traits::ToPrimitive as _;

pub(crate) struct DisplayFormatArg<'a> {
    pub value: &'a BigUint,
    pub mask: Option<&'a BigUint>,
    pub width: usize,
    pub signed: bool,
    pub is_string: bool,
}

fn bit(value: &BigUint, bit: usize) -> bool {
    ((value >> bit) & BigUint::from(1u8)) != BigUint::from(0u8)
}

fn has_mask(arg: &DisplayFormatArg<'_>) -> bool {
    let Some(mask) = arg.mask else {
        return false;
    };
    for bit_idx in 0..arg.width {
        if bit(mask, bit_idx) {
            return true;
        }
    }
    false
}

fn masked_value(value: &BigUint, width: usize) -> BigUint {
    if width > 0 {
        value & ((BigUint::from(1u8) << width) - BigUint::from(1u8))
    } else {
        BigUint::from(0u8)
    }
}

fn value_to_signed_bigint(value: &BigUint, width: usize) -> BigInt {
    if width == 0 {
        return BigInt::from(0);
    }
    let unsigned = masked_value(value, width);
    let sign_bit = BigUint::from(1u8) << (width - 1);
    if (&unsigned & &sign_bit) != BigUint::from(0u8) {
        BigInt::from_biguint(Sign::Plus, unsigned) - (BigInt::from(1u8) << width)
    } else {
        BigInt::from_biguint(Sign::Plus, unsigned)
    }
}

fn value_to_utf8(value: &BigUint, width: usize) -> Option<String> {
    if !width.is_multiple_of(8) {
        return None;
    }
    let num_bytes = width / 8;
    let mut bytes = vec![0u8; num_bytes];
    let mut payload = masked_value(value, width);
    let byte_mask = BigUint::from(0xffu64);
    for idx in (0..num_bytes).rev() {
        bytes[idx] = (&payload & &byte_mask).to_u64().unwrap_or(0) as u8;
        payload >>= 8;
    }
    String::from_utf8(bytes).ok()
}

fn format_binary(arg: &DisplayFormatArg<'_>) -> String {
    let digits = arg.width.max(1);
    if !has_mask(arg) {
        let mut out = masked_value(arg.value, arg.width).to_str_radix(2);
        if out.len() < digits {
            out.insert_str(0, &"0".repeat(digits - out.len()));
        }
        return out;
    }
    let mask = arg.mask.expect("masked argument");
    let mut out = String::with_capacity(digits);
    for bit_idx in (0..digits).rev() {
        if bit(mask, bit_idx) {
            out.push('x');
        } else if bit(arg.value, bit_idx) {
            out.push('1');
        } else {
            out.push('0');
        }
    }
    out
}

fn format_masked_radix(arg: &DisplayFormatArg<'_>, bits_per_digit: usize) -> String {
    let mask = arg.mask.expect("masked argument");
    let digits = arg.width.div_ceil(bits_per_digit).max(1);
    let mut out = String::with_capacity(digits);
    for digit_idx in (0..digits).rev() {
        let start = digit_idx * bits_per_digit;
        let end = (start + bits_per_digit).min(arg.width);
        if (start..end).any(|bit_idx| bit(mask, bit_idx)) {
            out.push('x');
            continue;
        }
        let mut digit = 0u32;
        for bit_idx in start..end {
            if bit(arg.value, bit_idx) {
                digit |= 1 << (bit_idx - start);
            }
        }
        out.push(char::from_digit(digit, 1 << bits_per_digit).unwrap());
    }
    out
}

pub(crate) fn format_display_arg(arg: &DisplayFormatArg<'_>, spec: Option<char>) -> String {
    if arg.is_string {
        return value_to_utf8(arg.value, arg.width).unwrap_or_else(|| format!("{:?}", arg.value));
    }
    match spec.unwrap_or('d') {
        'b' | 'B' => format_binary(arg),
        'o' | 'O' => {
            if has_mask(arg) {
                format_masked_radix(arg, 3)
            } else {
                masked_value(arg.value, arg.width).to_str_radix(8)
            }
        }
        'x' | 'h' => {
            if has_mask(arg) {
                format_masked_radix(arg, 4)
            } else {
                masked_value(arg.value, arg.width).to_str_radix(16)
            }
        }
        'X' | 'H' => {
            let mut out = if has_mask(arg) {
                format_masked_radix(arg, 4)
            } else {
                masked_value(arg.value, arg.width).to_str_radix(16)
            };
            out.make_ascii_uppercase();
            out
        }
        'd' | 'D' | 'i' | 'I' => {
            if has_mask(arg) {
                "x".to_string()
            } else if arg.signed {
                value_to_signed_bigint(arg.value, arg.width).to_string()
            } else {
                masked_value(arg.value, arg.width).to_string()
            }
        }
        'c' | 'C' => {
            if has_mask(arg) {
                "x".to_string()
            } else {
                char::from((masked_value(arg.value, arg.width).to_u64().unwrap_or(0) & 0xff) as u8)
                    .to_string()
            }
        }
        's' | 'S' => {
            if has_mask(arg) {
                "x".to_string()
            } else {
                value_to_utf8(arg.value, arg.width).unwrap_or_else(|| format!("{:?}", arg.value))
            }
        }
        _ => {
            if has_mask(arg) {
                "x".to_string()
            } else if arg.signed {
                value_to_signed_bigint(arg.value, arg.width).to_string()
            } else {
                masked_value(arg.value, arg.width).to_string()
            }
        }
    }
}
