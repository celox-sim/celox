use num_bigint::{BigInt, BigUint, Sign};
use num_traits::ToPrimitive as _;

pub struct DisplayFormatArg<'a> {
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

pub fn format_display_arg(arg: &DisplayFormatArg<'_>, spec: Option<char>) -> String {
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

/// Number of characters the largest value of `arg` takes in decimal.
fn decimal_digits(arg: &DisplayFormatArg<'_>) -> usize {
    if arg.width == 0 {
        return 1;
    }
    if arg.signed {
        // The most negative value, with its sign, is the widest.
        ((BigUint::from(1u8) << (arg.width - 1)).to_string().len()) + 1
    } else {
        ((BigUint::from(1u8) << arg.width) - BigUint::from(1u8))
            .to_string()
            .len()
    }
}

/// Formats `arg` with the sizing of IEEE 1800-2023 21.2.1.2. Without a
/// `field_width`, hexadecimal, octal, and binary values take as many digits
/// as the largest value of the argument's width, with leading zeros, and
/// decimal values as many characters, with leading spaces. With one, the
/// value is padded to that width (`0` displays it in the minimum width), and
/// a wider value is never truncated.
pub fn format_sized_display_arg(
    arg: &DisplayFormatArg<'_>,
    spec: char,
    field_width: Option<usize>,
) -> String {
    // Table 21-1 gives each uppercase specifier the meaning of its lowercase
    // one, so `%H` prints lowercase digits like `%h`.
    let spec = spec.to_ascii_lowercase();
    let text = format_display_arg(arg, Some(spec));
    if arg.is_string {
        return pad(text, ' ', field_width.unwrap_or(0));
    }
    let bits_per_digit = match spec {
        'b' => Some(1),
        'o' => Some(3),
        'x' | 'h' => Some(4),
        _ => None,
    };
    match bits_per_digit {
        Some(bits_per_digit) => {
            let text = if has_mask(arg) {
                unknown_digits(arg, bits_per_digit)
            } else {
                text
            };
            let minimal = text.trim_start_matches('0');
            let minimal = if minimal.is_empty() { "0" } else { minimal };
            let width = field_width.unwrap_or_else(|| arg.width.div_ceil(bits_per_digit).max(1));
            pad(minimal.to_string(), '0', width)
        }
        None if matches!(spec, 'd' | 'i') => {
            let text = match unknown_group(arg, 0, arg.width) {
                Some(digit) => digit.to_string(),
                None => text,
            };
            let width = field_width.unwrap_or_else(|| decimal_digits(arg));
            pad(text, ' ', width)
        }
        None => pad(text, ' ', field_width.unwrap_or(0)),
    }
}

/// The character IEEE 1800-2023 21.2.1.3 displays for the bits `start..end`
/// when any of them is unknown: lowercase when all of them are X (or all Z),
/// uppercase when only some are, and X over Z when both appear.
fn unknown_group(arg: &DisplayFormatArg<'_>, start: usize, end: usize) -> Option<char> {
    let mask = arg.mask?;
    let (mut x, mut z) = (0, 0);
    for bit_idx in start..end {
        if bit(mask, bit_idx) {
            if bit(arg.value, bit_idx) {
                x += 1;
            } else {
                z += 1;
            }
        }
    }
    let bits = end - start;
    match (x, z) {
        (0, 0) => None,
        (x, _) if x == bits => Some('x'),
        (0, z) if z == bits => Some('z'),
        (0, _) => Some('Z'),
        _ => Some('X'),
    }
}

/// Hexadecimal, octal, or binary digits of a value with unknown bits.
fn unknown_digits(arg: &DisplayFormatArg<'_>, bits_per_digit: usize) -> String {
    let digits = arg.width.div_ceil(bits_per_digit).max(1);
    (0..digits)
        .rev()
        .map(|digit_idx| {
            let start = digit_idx * bits_per_digit;
            let end = (start + bits_per_digit).min(arg.width);
            unknown_group(arg, start, end).unwrap_or_else(|| {
                let digit = (start..end)
                    .filter(|&bit_idx| bit(arg.value, bit_idx))
                    .fold(0, |digit, bit_idx| digit | 1 << (bit_idx - start));
                char::from_digit(digit, 1 << bits_per_digit).unwrap()
            })
        })
        .collect()
}

fn pad(text: String, fill: char, width: usize) -> String {
    let len = text.chars().count();
    if len >= width {
        return text;
    }
    let mut out: String = std::iter::repeat_n(fill, width - len).collect();
    out.push_str(&text);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sized(value: u32, width: usize, signed: bool, spec: char, field: Option<usize>) -> String {
        let value = BigUint::from(value);
        let arg = DisplayFormatArg {
            value: &value,
            mask: None,
            width,
            signed,
            is_string: false,
        };
        format_sized_display_arg(&arg, spec, field)
    }

    #[test]
    fn sizes_values_as_ieee_1800_2023_21_2_1_2_shows() {
        assert_eq!(sized(10, 32, false, 'd', None), "        10");
        assert_eq!(sized(10, 32, false, 'd', Some(0)), "10");
        assert_eq!(sized(10, 32, false, 'h', None), "0000000a");
        assert_eq!(sized(10, 32, false, 'h', Some(0)), "a");
        assert_eq!(sized(5, 32, false, 'd', Some(3)), "  5");
        assert_eq!(sized(1234, 32, false, 'd', Some(3)), "1234");
        assert_eq!(sized(5, 32, false, 'h', Some(3)), "005");
        assert_eq!(sized(0x1234, 32, false, 'h', Some(3)), "1234");
        assert_eq!(sized(0, 8, false, 'b', Some(0)), "0");
        assert_eq!(sized(0x80, 8, true, 'd', None), "-128");
        assert_eq!(sized(0xab, 8, false, 'H', None), "ab");
    }

    fn four_state(payload: u32, mask: u32, width: usize, spec: char) -> String {
        let (value, mask) = (BigUint::from(payload), BigUint::from(mask));
        let arg = DisplayFormatArg {
            value: &value,
            mask: Some(&mask),
            width,
            signed: false,
            is_string: false,
        };
        format_sized_display_arg(&arg, spec, None)
    }

    #[test]
    fn displays_unknown_bits_as_ieee_1800_2023_21_2_1_3_shows() {
        // X is payload 1 / mask 1, Z is payload 0 / mask 1.
        assert_eq!(four_state(1, 1, 1, 'd'), "x");
        // 14'bx01010
        assert_eq!(four_state(0x3fea, 0x3fe0, 14, 'h'), "xxXa");
        // 12'b001xxx101x01 as %h and %o
        assert_eq!(four_state(0x3ed, 0x1c4, 12, 'h'), "XXX");
        assert_eq!(four_state(0x3ed, 0x1c4, 12, 'o'), "1x5X");
        assert_eq!(four_state(0b1000, 0b1100, 4, 'b'), "xz00");
        assert_eq!(four_state(0, 0xff, 8, 'h'), "zz");
        assert_eq!(four_state(0, 0x0f, 8, 'h'), "0z");
        assert_eq!(four_state(0x01, 0x03, 8, 'h'), "0X");
        assert_eq!(four_state(0, 0xff, 8, 'd'), "  z");
        assert_eq!(four_state(0, 0x01, 8, 'd'), "  Z");
        assert_eq!(four_state(0x01, 0x03, 8, 'd'), "  X");
    }

    #[test]
    fn sizing_keeps_unknown_digits() {
        let value = BigUint::from(0u8);
        let mask = BigUint::from(0xf0u8);
        let arg = DisplayFormatArg {
            value: &value,
            mask: Some(&mask),
            width: 12,
            signed: false,
            is_string: false,
        };
        assert_eq!(format_sized_display_arg(&arg, 'h', None), "0z0");
        assert_eq!(format_sized_display_arg(&arg, 'h', Some(0)), "z0");
    }
}
