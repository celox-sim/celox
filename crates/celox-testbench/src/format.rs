use num_bigint::{BigInt, BigUint, Sign};
use num_traits::ToPrimitive as _;

pub struct DisplayFormatArg<'a> {
    pub value: &'a BigUint,
    pub mask: Option<&'a BigUint>,
    pub width: usize,
    pub signed: bool,
    pub is_string: bool,
    /// Interpret the payload as IEEE bits rather than an integral value.
    pub float_precision: Option<celox_design::FloatPrecision>,
}

fn integer_float_bits(arg: &DisplayFormatArg<'_>) -> BigUint {
    BigUint::from(celox_design::eval_float_unary(
        celox_design::FloatUnaryOp::ToSigned {
            width: 64,
            round: true,
        },
        arg.float_precision.unwrap(),
        arg.value.to_u64().unwrap_or(0),
    ))
}
fn numeric_float(arg: &DisplayFormatArg<'_>) -> f64 {
    if let Some(precision) = arg.float_precision {
        let bits = arg.value.to_u64().unwrap_or(0);
        return match precision {
            celox_design::FloatPrecision::F32 => f32::from_bits(bits as u32) as f64,
            celox_design::FloatPrecision::F64 => f64::from_bits(bits),
        };
    }
    let mut value = masked_value(arg.value, arg.width);
    if let Some(mask) = arg.mask {
        value &= ((BigUint::from(1u8) << arg.width) - BigUint::from(1u8))
            ^ masked_value(mask, arg.width);
    }
    if arg.signed {
        let value = value_to_signed_bigint(&value, arg.width);
        value.to_f64().unwrap_or(if value.sign() == Sign::Minus {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        })
    } else {
        value.to_f64().unwrap_or(f64::INFINITY)
    }
}

fn normalize_exponent(text: String, upper: bool) -> String {
    let Some((body, exponent)) = text.split_once('e') else {
        return text;
    };
    let exponent = exponent.parse::<i32>().expect("Rust float exponent");
    format!("{body}{}{exponent:+03}", if upper { 'E' } else { 'e' })
}
fn trim_fraction(text: &str) -> String {
    if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        text.to_string()
    }
}
// IEEE 1800-2023 21.2.1.1 gives real e/f/g the C formatting semantics.
// Rust supplies decimal rounding; normalize its exponent and implement g's
// significant-digit selection and trailing-zero rules explicitly.
fn format_float(arg: &DisplayFormatArg<'_>, spec: char, field: FieldSpec) -> Vec<u8> {
    let value = numeric_float(arg);
    let upper = spec.is_ascii_uppercase();
    let precision = field.precision.unwrap_or(6).min(MAX_FIELD_WIDTH);
    let mut text = if value.is_nan() {
        format!(
            "{}{}",
            if value.is_sign_negative() { "-" } else { "" },
            if upper { "NAN" } else { "nan" }
        )
    } else if value.is_infinite() {
        format!(
            "{}{}",
            if value.is_sign_negative() { "-" } else { "" },
            if upper { "INF" } else { "inf" }
        )
    } else {
        match spec.to_ascii_lowercase() {
            'e' => {
                let mut raw = format!("{value:.precision$e}");
                if field.alternate && precision == 0 {
                    let i = raw.find('e').unwrap();
                    raw.insert(i, '.');
                }
                normalize_exponent(raw, upper)
            }
            'g' => {
                let digits = precision.max(1);
                let fractional = digits - 1;
                let scientific = format!("{value:.fractional$e}");
                let (mantissa, exponent) = scientific.split_once('e').unwrap();
                let exponent = exponent.parse::<i32>().unwrap();
                if exponent < -4 || exponent >= digits as i32 {
                    let mut mantissa = if field.alternate {
                        mantissa.to_string()
                    } else {
                        trim_fraction(mantissa)
                    };
                    if field.alternate && !mantissa.contains('.') {
                        mantissa.push('.');
                    }
                    normalize_exponent(format!("{mantissa}e{exponent}"), upper)
                } else {
                    let fractional = (digits as i32 - 1 - exponent).max(0) as usize;
                    let raw = format!("{value:.fractional$}");
                    let mut text = if field.alternate {
                        raw
                    } else {
                        trim_fraction(&raw)
                    };
                    if field.alternate && !text.contains('.') {
                        text.push('.');
                    }
                    text
                }
            }
            _ => {
                let mut text = format!("{value:.precision$}");
                if field.alternate && precision == 0 {
                    text.push('.');
                }
                text
            }
        }
    };
    if !text.starts_with('-') {
        if field.show_sign {
            text.insert(0, '+');
        } else if field.space_sign {
            text.insert(0, ' ');
        }
    }
    // C does not zero-pad infinities or NaNs.
    pad_veryl_field(
        text.into_bytes(),
        spec,
        FieldSpec {
            zero_pad: field.zero_pad && value.is_finite(),
            ..field
        },
    )
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

fn packed_bytes(value: &BigUint, width: usize) -> Vec<u8> {
    let mut bytes = masked_value(value, width).to_bytes_be();
    let count = width.div_ceil(8);
    if bytes.len() < count {
        let mut padded = vec![0; count - bytes.len()];
        padded.extend(bytes);
        bytes = padded;
    }
    bytes.truncate(count);
    bytes
}

/// Format an argument as bytes. Character conversions never decode UTF-8.
pub fn format_display_arg(arg: &DisplayFormatArg<'_>, spec: Option<char>) -> Vec<u8> {
    if arg.float_precision.is_some() && spec.is_none() {
        return format_float(arg, 'g', FieldSpec::default());
    }
    if matches!(spec, Some('f' | 'F' | 'e' | 'E' | 'g' | 'G')) {
        return format_float(arg, spec.unwrap(), FieldSpec::default());
    }
    if arg.float_precision.is_some() {
        let value = integer_float_bits(arg);
        let integer = DisplayFormatArg {
            value: &value,
            mask: None,
            width: 64,
            signed: true,
            is_string: false,
            float_precision: None,
        };
        return format_display_arg(&integer, spec);
    }
    if arg.is_string {
        return packed_bytes(arg.value, arg.width);
    }
    match spec.unwrap_or('d').to_ascii_lowercase() {
        'c' if !has_mask(arg) => {
            vec![
                (masked_value(arg.value, arg.width) & BigUint::from(0xffu8))
                    .to_u8()
                    .unwrap(),
            ]
        }
        's' if !has_mask(arg) => packed_bytes(arg.value, arg.width),
        _ => format_numeric_arg(arg, spec).into_bytes(),
    }
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

fn format_numeric_arg(arg: &DisplayFormatArg<'_>, spec: Option<char>) -> String {
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
        'c' | 'C' | 's' | 'S' => "x".to_string(),
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
) -> Vec<u8> {
    format_sv_display_arg(
        arg,
        spec,
        FieldSpec {
            width: field_width,
            ..FieldSpec::default()
        },
    )
}

/// IEEE display sizing with the `-` and leading `0` field flags supported
/// by Icarus and Verilator. As in Icarus, decimal zero padding follows the sign;
/// left justification takes precedence over zero padding.
pub fn format_sv_display_arg(arg: &DisplayFormatArg<'_>, spec: char, field: FieldSpec) -> Vec<u8> {
    if matches!(spec, 'f' | 'F' | 'e' | 'E' | 'g' | 'G') {
        return format_float(arg, spec, field);
    }
    if arg.float_precision.is_some() {
        let value = integer_float_bits(arg);
        let integer = DisplayFormatArg {
            value: &value,
            mask: None,
            width: 64,
            signed: true,
            is_string: false,
            float_precision: None,
        };
        return format_sv_display_arg(&integer, spec, field);
    }

    let field_width = field.width;
    // Table 21-1 gives each uppercase specifier the meaning of its lowercase
    // one, so `%H` prints lowercase digits like `%h`.
    let spec = spec.to_ascii_lowercase();
    // Packed string literals still undergo an explicit numeric conversion.
    let arg = &DisplayFormatArg {
        value: arg.value,
        mask: arg.mask,
        width: arg.width,
        signed: arg.signed,
        is_string: arg.is_string && spec == 's',
        float_precision: arg.float_precision,
    };
    if spec == 's' && !has_mask(arg) {
        // Integral %s is right-justified byte data; omit leading zero bytes
        // and include a partial most-significant byte (21.2.1.7).
        let bytes = packed_bytes(arg.value, arg.width);
        let start = bytes
            .iter()
            .position(|byte| *byte != 0)
            .unwrap_or(bytes.len());
        return pad_veryl_field(
            bytes[start..].to_vec(),
            spec,
            FieldSpec {
                width: Some(field_width.unwrap_or_else(|| arg.width.div_ceil(8))),
                ..field
            },
        );
    }
    if spec == 'c' {
        return pad_veryl_field(format_display_arg(arg, Some(spec)), spec, field);
    }
    let text = format_numeric_arg(arg, Some(spec));
    let bits_per_digit = match spec {
        'b' => Some(1),
        'o' => Some(3),
        'x' | 'h' => Some(4),
        _ => None,
    };
    match bits_per_digit {
        Some(bits_per_digit) => {
            let text = if has_mask(arg) {
                radix_digits(arg, bits_per_digit)
            } else {
                text
            };
            if field.left_justify {
                // Icarus retains the argument's radix digits before padding
                // on the right (Verilator instead uses the minimum digits).
                let width = arg.width.div_ceil(bits_per_digit).max(1);
                return pad_veryl_field(pad(text, '0', width).into_bytes(), spec, field);
            }
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
            return pad_veryl_field(
                text.into_bytes(),
                spec,
                FieldSpec {
                    width: Some(width),
                    ..field
                },
            );
        }
        None => return pad_veryl_field(text.into_bytes(), spec, field),
    }
    .into_bytes()
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

/// Hexadecimal, octal, or binary digits of a value, one for each group of
/// its bits, unknown ones included.
fn radix_digits(arg: &DisplayFormatArg<'_>, bits_per_digit: usize) -> String {
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

/// Ceiling on a field width: the digits come from source text, and a wider
/// field would only be an allocation the source sizes by typing. Veryl's
/// simulator caps its field widths the same way.
pub const MAX_FIELD_WIDTH: usize = 1 << 16;

/// The flags, field width, and precision between `%` and a conversion character.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FieldSpec {
    pub width: Option<usize>,
    /// `-`: pad on the right when flag parsing is enabled.
    pub left_justify: bool,
    /// A leading `0` digit: pad numbers with zeros.
    pub zero_pad: bool,
    /// Digits after the decimal point for f/e, significant digits for g.
    pub precision: Option<usize>,
    pub show_sign: bool,
    pub space_sign: bool,
    pub alternate: bool,
}

impl FieldSpec {
    /// Read the flags and width that follow a `%`. `-` is a flag only with
    /// `c_flags`; otherwise it is left as the conversion character.
    pub fn parse(
        chars: &mut std::iter::Peekable<impl Iterator<Item = char>>,
        c_flags: bool,
    ) -> Self {
        let mut left_justify = false;
        let mut zero_pad = false;
        let mut show_sign = false;
        let mut space_sign = false;
        let mut alternate = false;
        while let Some(&flag) = chars.peek() {
            match flag {
                '-' if c_flags => left_justify = true,
                '+' if c_flags => show_sign = true,
                ' ' if c_flags => space_sign = true,
                '#' if c_flags => alternate = true,
                '0' => zero_pad = true,
                _ => break,
            }
            chars.next();
            if !c_flags {
                break;
            }
        }
        let mut width = if zero_pad { Some(0) } else { None };
        while let Some(digit) = chars.peek().and_then(|c| c.to_digit(10)) {
            chars.next();
            width = Some(
                width
                    .unwrap_or(0usize)
                    .saturating_mul(10)
                    .saturating_add(digit as usize)
                    .min(MAX_FIELD_WIDTH),
            );
        }
        let mut precision = None;
        if chars.next_if_eq(&'.').is_some() {
            let mut value = 0usize;
            while let Some(digit) = chars.peek().and_then(|c| c.to_digit(10)) {
                chars.next();
                value = value
                    .saturating_mul(10)
                    .saturating_add(digit as usize)
                    .min(MAX_FIELD_WIDTH);
            }
            precision = Some(value);
        }
        Self {
            width,
            left_justify,
            zero_pad,
            precision,
            show_sign,
            space_sign,
            alternate,
        }
    }
}

/// Formats `arg` as Veryl's simulator does. Hexadecimal, octal, and binary
/// values take a digit for each group of the argument's bits, and decimal
/// values their minimum width. With `%0`, radices drop their leading zeros.
/// A field width then pads as [`pad_veryl_field`] does.
pub fn format_veryl_display_arg(
    arg: &DisplayFormatArg<'_>,
    spec: char,
    field: FieldSpec,
) -> Vec<u8> {
    if matches!(spec, 'f' | 'F' | 'e' | 'E' | 'g' | 'G') {
        return format_float(arg, spec, field);
    }
    if arg.float_precision.is_some() {
        let value = integer_float_bits(arg);
        let integer = DisplayFormatArg {
            value: &value,
            mask: None,
            width: 64,
            signed: true,
            is_string: false,
            float_precision: None,
        };
        return format_veryl_display_arg(&integer, spec, field);
    }

    let spec = spec.to_ascii_lowercase();
    let bits_per_digit = match spec {
        'b' => Some(1),
        'o' => Some(3),
        'x' | 'h' => Some(4),
        _ => None,
    };
    let body = match bits_per_digit {
        _ if arg.is_string => format_display_arg(arg, Some(spec)),
        Some(bits_per_digit) => {
            let digits = radix_digits(arg, bits_per_digit);
            if field.width == Some(0) {
                let minimal = digits.trim_start_matches('0');
                if minimal.is_empty() { "0" } else { minimal }.to_string()
            } else {
                digits
            }
            .into_bytes()
        }
        // Veryl prints a lowercase x for a decimal value with any unknown bit.
        None if matches!(spec, 'd' | 'i') && has_mask(arg) => b"x".to_vec(),
        None => format_display_arg(arg, Some(spec)),
    };
    pad_veryl_field(body, spec, field)
}

/// Pads a converted value to its field width as Veryl's simulator does: on
/// the left with spaces, on the right with `-`, or with zeros behind the sign
/// for a number with a leading `0`. A longer value is never truncated.
pub fn pad_veryl_field(mut body: Vec<u8>, spec: char, field: FieldSpec) -> Vec<u8> {
    let Some(width) = field.width else {
        return body;
    };
    if body.len() >= width {
        return body;
    }
    let count = width - body.len();
    let numeric = matches!(
        spec.to_ascii_lowercase(),
        'd' | 'i' | 'h' | 'x' | 'o' | 'b' | 'f' | 'e' | 'g'
    );
    if field.left_justify {
        body.extend(std::iter::repeat_n(b' ', count));
    } else {
        let zero = field.zero_pad && numeric;
        let at = usize::from(zero && matches!(body.first(), Some(b'-' | b'+' | b' ')));
        body.splice(
            at..at,
            std::iter::repeat_n(if zero { b'0' } else { b' ' }, count),
        );
    }
    body
}

fn pad(text: String, fill: char, width: usize) -> String {
    let len = text.chars().count();
    pad_counted(text, fill, width, len)
}

fn pad_counted(text: String, fill: char, width: usize, len: usize) -> String {
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

    #[test]
    fn character_conversions_preserve_each_byte_and_count_byte_columns() {
        let value = BigUint::from(0xff0080u32);
        let arg = DisplayFormatArg {
            value: &value,
            mask: None,
            width: 24,
            signed: false,
            is_string: false,
            float_precision: None,
        };
        assert_eq!(format_sized_display_arg(&arg, 's', None), [255, 0, 128]);
        assert_eq!(
            format_sized_display_arg(&arg, 's', Some(5)),
            [32, 32, 255, 0, 128]
        );
        assert_eq!(format_sized_display_arg(&arg, 'c', None), [128]);
        assert_eq!(
            format_veryl_display_arg(&arg, 's', FieldSpec::default()),
            [255, 0, 128]
        );
        let value = (BigUint::from(1u8) << 64) | BigUint::from(0xfeu8);
        let arg = DisplayFormatArg {
            value: &value,
            mask: None,
            width: 65,
            signed: false,
            is_string: false,
            float_precision: None,
        };
        assert_eq!(format_sized_display_arg(&arg, 'c', None), [254]);
        let value = BigUint::from(0u8);
        let arg = DisplayFormatArg {
            value: &value,
            mask: None,
            width: 0,
            signed: false,
            is_string: false,
            float_precision: None,
        };
        assert_eq!(format_sized_display_arg(&arg, 's', Some(0)), []);
    }

    fn sized(value: u32, width: usize, signed: bool, spec: char, field: Option<usize>) -> String {
        let value = BigUint::from(value);
        let arg = DisplayFormatArg {
            value: &value,
            mask: None,
            width,
            signed,
            is_string: false,
            float_precision: None,
        };
        String::from_utf8(format_sized_display_arg(&arg, spec, field)).unwrap()
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
        // 16'hc3a9 is two 8-bit codes, one UTF-8 character.
        let value = BigUint::from(0xc3a9u32);
        let arg = DisplayFormatArg {
            value: &value,
            mask: None,
            width: 16,
            signed: false,
            is_string: true,
            float_precision: None,
        };
        assert_eq!(
            format_sized_display_arg(&arg, 's', Some(3)),
            " \u{e9}".as_bytes()
        );
    }

    fn four_state(payload: u32, mask: u32, width: usize, spec: char) -> String {
        let (value, mask) = (BigUint::from(payload), BigUint::from(mask));
        let arg = DisplayFormatArg {
            value: &value,
            mask: Some(&mask),
            width,
            signed: false,
            is_string: false,
            float_precision: None,
        };
        String::from_utf8(format_sized_display_arg(&arg, spec, None)).unwrap()
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

    fn veryl(payload: u32, mask: u32, width: usize, signed: bool, spec: &str) -> String {
        let (value, mask) = (BigUint::from(payload), BigUint::from(mask));
        let arg = DisplayFormatArg {
            value: &value,
            mask: Some(&mask),
            width,
            signed,
            is_string: false,
            float_precision: None,
        };
        let mut chars = spec.chars().peekable();
        let field = FieldSpec::parse(&mut chars, true);
        String::from_utf8(format_veryl_display_arg(&arg, chars.next().unwrap(), field)).unwrap()
    }

    #[test]
    fn formats_as_veryls_simulator() {
        // The cases of Veryl's display_format_tests.
        assert_eq!(veryl(10, 0, 8, false, "0d"), "10");
        assert_eq!(veryl(11, 0, 8, false, "d"), "11");
        assert_eq!(veryl(12, 0, 32, false, "h"), "0000000c");
        assert_eq!(veryl(12, 0, 32, false, "0h"), "c");
        assert_eq!(veryl(7, 0, 8, false, "5d"), "    7");
        assert_eq!(veryl(7, 0, 8, false, "-5d"), "7    ");
        assert_eq!(veryl(9, 0, 32, false, "2h"), "00000009");
        assert_eq!(veryl(7, 0, 8, false, "05d"), "00007");
        assert_eq!(veryl(0xc, 0, 8, false, "04h"), "000c");
        assert_eq!(veryl(0xf9, 0, 8, true, "05d"), "-0007");
        assert_eq!(veryl(7, 0, 8, false, "-05d"), "7    ");
        assert_eq!(veryl(0x41, 0, 8, false, "05c"), "    A");
        // Uppercase specifiers print lowercase digits, and unknown digits
        // follow the IEEE rules; a decimal value with any is a lowercase x.
        assert_eq!(veryl(0xab, 0, 8, false, "H"), "ab");
        assert_eq!(veryl(0x01, 0x03, 8, false, "h"), "0X");
        assert_eq!(veryl(0b1000, 0b1100, 4, false, "b"), "xz00");
        assert_eq!(veryl(0, 0x01, 8, false, "d"), "x");
    }

    #[test]
    fn sv_decimal_flags_preserve_sign_and_do_not_truncate() {
        let value = BigUint::from(0xd6u8); // -42
        let arg = DisplayFormatArg {
            value: &value,
            mask: None,
            width: 8,
            signed: true,
            is_string: false,
            float_precision: None,
        };
        // Icarus places zeros behind the sign; Verilator instead prints 00-42.
        for (format, expected) in [("05d", "-0042"), ("-05d", "-42  "), ("02d", "-42")] {
            let mut chars = format.chars().peekable();
            let field = FieldSpec::parse(&mut chars, true);
            assert_eq!(
                format_sv_display_arg(&arg, chars.next().unwrap(), field),
                expected.as_bytes()
            );
        }
    }

    #[test]
    fn sv_left_justified_radices_keep_icarus_digit_sizing() {
        let value = BigUint::from(5u8);
        let arg = DisplayFormatArg {
            value: &value,
            mask: None,
            width: 8,
            signed: false,
            is_string: false,
            float_precision: None,
        };
        let field = FieldSpec {
            width: Some(5),
            left_justify: true,
            zero_pad: false,
            ..FieldSpec::default()
        };
        assert_eq!(format_sv_display_arg(&arg, 'h', field), b"05   ");
        assert_eq!(format_sv_display_arg(&arg, 'b', field), b"00000101");
    }

    #[test]
    fn caps_field_widths() {
        let mut chars = "99999999999999999999d".chars().peekable();
        assert_eq!(
            FieldSpec::parse(&mut chars, true).width,
            Some(MAX_FIELD_WIDTH)
        );
        assert_eq!(chars.next(), Some('d'));
        let mut chars = "-5d".chars().peekable();
        assert_eq!(FieldSpec::parse(&mut chars, false), FieldSpec::default());
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
            float_precision: None,
        };
        assert_eq!(format_sized_display_arg(&arg, 'h', None), b"0z0");
        assert_eq!(format_sized_display_arg(&arg, 'h', Some(0)), b"z0");
    }
}

#[cfg(test)]
mod floating_tests {
    use super::*;
    use celox_design::FloatPrecision;
    fn real(value: f64, format: &str) -> String {
        let bits = BigUint::from(value.to_bits());
        let arg = DisplayFormatArg {
            value: &bits,
            mask: None,
            width: 64,
            signed: false,
            is_string: false,
            float_precision: Some(FloatPrecision::F64),
        };
        let mut chars = format.chars().peekable();
        let field = FieldSpec::parse(&mut chars, true);
        let spec = chars.next().unwrap();
        assert!(chars.next().is_none());
        String::from_utf8(format_sv_display_arg(&arg, spec, field)).unwrap()
    }
    #[test]
    fn floating_formats_cover_precision_exponents_signs_and_padding() {
        for (value, spec, expected) in [
            (1.5, "f", "1.500000"),
            (1.5, "+010.2f", "+000001.50"),
            (-1.5, "010.2f", "-000001.50"),
            (1.5, "-10.2f", "1.50      "),
            (0.0, " .0f", " 0"),
            (2.0, "#.0f", "2."),
            (1.5, "e", "1.500000e+00"),
            (12345.0, ".2E", "1.23E+04"),
            (12345.0, ".3g", "1.23e+04"),
            (0.0001234, ".3g", "0.000123"),
            (0.00001234, ".3g", "1.23e-05"),
            (999.5, ".3g", "1e+03"),
            (1.0, "#.3g", "1.00"),
            (1.5, "#g", "1.50000"),
            (-0.0, ".2f", "-0.00"),
            (f64::INFINITY, "08f", "     inf"),
            (f64::NEG_INFINITY, "E", "-INF"),
            (f64::NAN, "+G", "+NAN"),
            (1.5, "0d", "2"),
            (-1.5, "0d", "-2"),
        ] {
            assert_eq!(real(value, spec), expected, "{value} %{spec}");
        }
    }
    #[test]
    fn integer_arguments_convert_numerically_and_shortreal_keeps_its_precision() {
        let value = BigUint::from(0xffff_ffd6u32);
        let arg = DisplayFormatArg {
            value: &value,
            mask: None,
            width: 32,
            signed: true,
            is_string: false,
            float_precision: None,
        };
        assert_eq!(
            format_sv_display_arg(&arg, 'f', FieldSpec::default()),
            b"-42.000000"
        );
        let masked = BigUint::from(255u8);
        let unknown = BigUint::from(240u8);
        let masked_arg = DisplayFormatArg {
            value: &masked,
            mask: Some(&unknown),
            width: 8,
            signed: false,
            is_string: false,
            float_precision: None,
        };
        assert_eq!(
            format_sv_display_arg(&masked_arg, 'f', FieldSpec::default()),
            b"15.000000"
        );
        let wide = BigUint::from(1u8) << 4095usize;
        let wide_arg = DisplayFormatArg {
            value: &wide,
            mask: None,
            width: 4096,
            signed: true,
            is_string: false,
            float_precision: None,
        };
        assert_eq!(
            format_sv_display_arg(&wide_arg, 'f', FieldSpec::default()),
            b"-inf"
        );
        let value = BigUint::from(1.1f32.to_bits());
        let arg = DisplayFormatArg {
            value: &value,
            mask: None,
            width: 32,
            signed: false,
            is_string: false,
            float_precision: Some(FloatPrecision::F32),
        };
        assert_eq!(
            format_veryl_display_arg(
                &arg,
                'f',
                FieldSpec {
                    precision: Some(9),
                    ..Default::default()
                }
            ),
            b"1.100000024"
        );
        assert_eq!(format_display_arg(&arg, None), b"1.1");
    }
}
