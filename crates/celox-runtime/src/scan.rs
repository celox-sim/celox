//! Integral and real formatted input (IEEE 1800-2023 21.3.4.3).
//!
//! Supported conversions are `%d`, `%h`/`%x`, `%o`, `%b`, `%c`, and `%e`/`%f`/`%g`, with
//! assignment suppression and maximum numeric field widths. `%c` reads one
//! byte without skipping whitespace. Numeric assignments retain the low bits
//! of the destination; radix X/Z digits retain their four-state representation.
//! Real conversions accept real and shortreal destinations. String, raw binary,
//! and time-format conversions are diagnosed rather than partially interpreted.
use celox_design::FloatPrecision;
use num_bigint::BigUint;
use std::io;

/// A scanner uses at most one byte of pushback and leaves mismatching input unread.
pub trait Input {
    fn read_byte(&mut self) -> io::Result<Option<u8>>;
    fn unread_byte(&mut self, byte: u8) -> io::Result<()>;
}

/// Storage type of one assigned scan argument.
#[derive(Clone, Copy, Debug)]
pub struct Destination {
    pub width: usize,
    pub float_precision: Option<FloatPrecision>,
}

pub struct SliceInput<'a> {
    bytes: &'a [u8],
    position: usize,
}
impl<'a> SliceInput<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }
    pub fn position(&self) -> usize {
        self.position
    }
}
impl Input for SliceInput<'_> {
    fn read_byte(&mut self) -> io::Result<Option<u8>> {
        let byte = self.bytes.get(self.position).copied();
        self.position += usize::from(byte.is_some());
        Ok(byte)
    }
    fn unread_byte(&mut self, byte: u8) -> io::Result<()> {
        if self.position == 0 || self.bytes[self.position - 1] != byte {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        self.position -= 1;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Four-state bits use `(value, mask)`: X is `(1, 1)`, Z is `(0, 1)`.
pub struct Value {
    pub value: BigUint,
    pub mask: BigUint,
}
#[derive(Debug, PartialEq, Eq)]
pub struct Outcome {
    pub count: i32,
    pub values: Vec<Option<Value>>,
}
#[derive(Debug)]
enum Directive {
    Space,
    Literal(u8),
    Percent,
    Number {
        code: u8,
        width: Option<usize>,
        suppress: bool,
    },
}

fn format_directives(format: &[u8]) -> Result<Vec<Directive>, String> {
    let mut directives = Vec::new();
    let mut position = 0;
    while let Some(&byte) = format.get(position) {
        position += 1;
        if byte.is_ascii_whitespace() {
            directives.push(Directive::Space);
            continue;
        }
        if byte != b'%' {
            directives.push(Directive::Literal(byte));
            continue;
        }
        if format.get(position) == Some(&b'%') {
            position += 1;
            directives.push(Directive::Percent);
            continue;
        }
        let suppress = format.get(position) == Some(&b'*');
        position += usize::from(suppress);
        let start = position;
        let mut maximum = 0usize;
        while let Some(digit) = format.get(position).filter(|byte| byte.is_ascii_digit()) {
            maximum = maximum
                .checked_mul(10)
                .and_then(|v| v.checked_add((digit - b'0') as usize))
                .ok_or("scan field width is too large")?;
            position += 1;
        }
        let width = (position != start).then_some(maximum);
        if width == Some(0) {
            return Err("scan field width must be positive".into());
        }
        let code = format
            .get(position)
            .copied()
            .ok_or("incomplete scan conversion")?
            .to_ascii_lowercase();
        position += 1;
        match code {
            b'd'|b'h'|b'x'|b'o'|b'b'|b'c'|b'e'|b'f'|b'g'=>{},
            b's'=>return Err("scan string output conversion %s is unsupported; string variable mutation is unsupported".into()),
            b't'=>return Err("scan %t requires unsupported $timeformat scaling".into()),
            _=>return Err(format!("unsupported scan conversion %{}",code as char)),
        }
        directives.push(Directive::Number {
            code,
            width,
            suppress,
        });
    }
    Ok(directives)
}

/// Validate formats before consuming input. Excess output arguments are ignored.
pub fn validate_format(format: &[u8], destinations: usize) -> Result<(), String> {
    let needed = format_directives(format)?
        .iter()
        .filter(|directive| {
            matches!(
                directive,
                Directive::Number {
                    suppress: false,
                    ..
                }
            )
        })
        .count();
    if needed > destinations {
        Err("scan format has more assignments than destination arguments".into())
    } else {
        Ok(())
    }
}
fn whitespace(byte: u8, nul_is_space: bool) -> bool {
    byte.is_ascii_whitespace() || (nul_is_space && byte == 0)
}
fn skip_space(input: &mut impl Input, nul_is_space: bool) -> io::Result<()> {
    while let Some(byte) = input.read_byte()? {
        if !whitespace(byte, nul_is_space) {
            input.unread_byte(byte)?;
            break;
        }
    }
    Ok(())
}
enum Field {
    Matched(Value),
    Mismatch,
    Eof,
}
fn field(
    input: &mut impl Input,
    code: u8,
    width: Option<usize>,
    bits: usize,
    nul_is_space: bool,
) -> io::Result<Field> {
    if code != b'c' {
        skip_space(input, nul_is_space)?;
    }
    let Some(first) = input.read_byte().unwrap_or(None) else {
        return Ok(Field::Eof);
    };
    if code == b'c' {
        return Ok(Field::Matched(Value {
            value: BigUint::from(first) & ((BigUint::from(1u8) << bits) - BigUint::from(1u8)),
            mask: BigUint::default(),
        }));
    }
    let radix = match code {
        b'b' => 2,
        b'o' => 8,
        b'd' => 10,
        _ => 16,
    };
    let max = width.unwrap_or(usize::MAX);
    let mut used = 1;
    let mut byte = first;
    let mut negative = false;
    if radix == 10 && matches!(byte, b'+' | b'-') {
        negative = byte == b'-';
        if used == max {
            return Ok(Field::Mismatch);
        }
        let Some(next) = input.read_byte().unwrap_or(None) else {
            return Ok(Field::Mismatch);
        };
        byte = next;
        used += 1;
        if matches!(byte, b'x' | b'X' | b'z' | b'Z' | b'?') {
            input.unread_byte(byte)?;
            return Ok(Field::Mismatch);
        }
    }
    let mut digits = Vec::new();
    loop {
        let unknown = matches!(byte, b'x' | b'X' | b'z' | b'Z' | b'?');
        let valid =
            (byte as char).is_digit(radix) || unknown || (byte == b'_' && !digits.is_empty());
        if !valid {
            input.unread_byte(byte)?;
            break;
        }
        digits.push(byte);
        // Decimal X/Z is a single input value, not part of a digit sequence.
        if radix == 10 && unknown {
            break;
        }
        if used == max {
            break;
        }
        let Some(next) = input.read_byte().unwrap_or(None) else {
            break;
        };
        byte = next;
        used += 1;
    }
    if digits.is_empty() {
        return Ok(Field::Mismatch);
    }
    // A decimal X/Z cannot follow ordinary digits (the offending byte is unread).
    if radix == 10
        && digits.len() > 1
        && digits
            .last()
            .is_some_and(|b| matches!(b, b'x' | b'X' | b'z' | b'Z' | b'?'))
    {
        input.unread_byte(digits.pop().unwrap())?;
    }
    let width_mask = (BigUint::from(1u8) << bits) - BigUint::from(1u8);
    let mut value = BigUint::default();
    let mut mask = BigUint::default();
    let compact: Vec<_> = digits.into_iter().filter(|byte| *byte != b'_').collect();
    if radix == 10 {
        if matches!(compact[0], b'x' | b'X' | b'z' | b'Z' | b'?') {
            mask = width_mask.clone();
            if matches!(compact[0], b'x' | b'X') {
                value = width_mask.clone();
            }
        } else {
            value = BigUint::parse_bytes(&compact, 10).unwrap_or_default() & &width_mask;
            if negative && value != BigUint::default() {
                value = (&width_mask + BigUint::from(1u8)) - value;
            }
        }
    } else {
        let shift = match radix {
            2 => 1,
            8 => 3,
            _ => 4,
        };
        for &digit in &compact {
            value <<= shift;
            mask <<= shift;
            let group = BigUint::from((1u8 << shift) - 1);
            match digit {
                b'z' | b'Z' | b'?' => mask |= &group,
                b'x' | b'X' => {
                    value |= &group;
                    mask |= &group;
                }
                _ => value |= BigUint::from((digit as char).to_digit(radix).unwrap()),
            }
        }
        let occupied = compact.len().saturating_mul(shift);
        if occupied < bits && matches!(compact[0], b'x' | b'X' | b'z' | b'Z' | b'?') {
            let extension = &width_mask ^ ((BigUint::from(1u8) << occupied) - BigUint::from(1u8));
            mask |= &extension;
            if matches!(compact[0], b'x' | b'X') {
                value |= extension;
            }
        }
        value &= &width_mask;
        mask &= &width_mask;
    }
    Ok(Field::Matched(Value { value, mask }))
}

// A field consumes the longest floating-point prefix, retaining at most one
// mismatching byte. An incomplete exponent/sign is a matching failure, not an
// assignment of its shorter valid prefix (21.3.4.3, Table 21-7).
fn real_field(
    input: &mut impl Input,
    width: Option<usize>,
    precision: FloatPrecision,
    nul_is_space: bool,
) -> io::Result<Field> {
    skip_space(input, nul_is_space)?;
    let mut text = Vec::new();
    let mut digits = false;
    let mut point = false;
    let mut exponent = false;
    let mut exponent_digits = false;
    while text.len() < width.unwrap_or(usize::MAX) {
        let Some(byte) = input.read_byte().unwrap_or(None) else {
            break;
        };
        let valid = if byte.is_ascii_digit() {
            if exponent {
                exponent_digits = true;
            } else {
                digits = true;
            }
            true
        } else if matches!(byte, b'+' | b'-') {
            text.is_empty() || text.last().is_some_and(|b| matches!(b, b'e' | b'E'))
        } else if byte == b'.' && !point && !exponent {
            point = true;
            true
        } else if matches!(byte, b'e' | b'E') && digits && !exponent {
            exponent = true;
            true
        } else {
            false
        };
        if !valid {
            input.unread_byte(byte)?;
            break;
        }
        text.push(byte);
    }
    if text.is_empty() {
        // Distinguish a conflicting first character (unread above) from EOF.
        return match input.read_byte().unwrap_or(None) {
            Some(byte) => {
                input.unread_byte(byte)?;
                Ok(Field::Mismatch)
            }
            None => Ok(Field::Eof),
        };
    }
    if !digits || (exponent && !exponent_digits) {
        return Ok(Field::Mismatch);
    }
    let text = std::str::from_utf8(&text).expect("ASCII floating field");
    let payload = match precision {
        FloatPrecision::F32 => text.parse::<f32>().map(|value| u64::from(value.to_bits())),
        FloatPrecision::F64 => text.parse::<f64>().map(f64::to_bits),
    };
    match payload {
        Ok(bits) => Ok(Field::Matched(Value {
            value: bits.into(),
            mask: BigUint::default(),
        })),
        Err(_) => Ok(Field::Mismatch),
    }
}

/// Scan integral fields. Real fields require [`scan_typed`] with real destinations.
pub fn scan(
    input: &mut impl Input,
    format: &[u8],
    widths: &[usize],
    nul_is_space: bool,
) -> Result<Outcome, String> {
    let destinations = widths
        .iter()
        .map(|&width| Destination {
            width,
            float_precision: None,
        })
        .collect::<Vec<_>>();
    scan_typed(input, format, &destinations, nul_is_space)
}

/// Scan typed fields. Suppressed conversions consume input without assigning
/// or incrementing `count`; a conversion mismatch preserves the offending byte.
/// Real values are returned as IEEE payload bits at the destination precision.
pub fn scan_typed(
    input: &mut impl Input,
    format: &[u8],
    destinations: &[Destination],
    nul_is_space: bool,
) -> Result<Outcome, String> {
    validate_format(format, destinations.len())?;
    let directives = format_directives(format)?;
    let mut assigned = 0;
    for directive in &directives {
        if let Directive::Number {
            code,
            suppress: false,
            ..
        } = directive
        {
            let real = matches!(code, b'e' | b'f' | b'g');
            if real != destinations[assigned].float_precision.is_some() {
                return Err(if real {
                    "scan real conversion requires a real or shortreal destination"
                } else {
                    "scan integral conversion requires an integral destination"
                }
                .into());
            }
            assigned += 1;
        }
    }
    let mut values = vec![None; destinations.len()];
    let mut count = 0i32;
    let mut converted = false;
    let mut target = 0;
    for directive in directives {
        let status: io::Result<Option<bool>> = match directive {
            Directive::Space => skip_space(input, nul_is_space).map(|()| Some(true)),
            Directive::Percent => match input.read_byte() {
                Ok(Some(b'%')) => {
                    converted = true;
                    Ok(Some(true))
                }
                Ok(Some(actual)) => input.unread_byte(actual).map(|()| Some(false)),
                Ok(None) => Ok(None),
                Err(error) => Err(error),
            },
            Directive::Literal(expected) => match input.read_byte() {
                Ok(Some(actual)) if actual == expected => {
                    converted = true;
                    Ok(Some(true))
                }
                Ok(Some(actual)) => input.unread_byte(actual).map(|()| Some(false)),
                Ok(None) => Ok(None),
                Err(error) => Err(error),
            },
            Directive::Number {
                code,
                width,
                suppress,
            } => {
                let result = if matches!(code, b'e' | b'f' | b'g') {
                    let precision = if suppress {
                        FloatPrecision::F64
                    } else {
                        destinations[target]
                            .float_precision
                            .expect("validated real destination")
                    };
                    real_field(input, width, precision, nul_is_space)
                } else {
                    let bits = if suppress {
                        1
                    } else {
                        destinations[target].width
                    };
                    field(input, code, width, bits, nul_is_space)
                };
                match result {
                    Ok(Field::Matched(value)) => {
                        converted = true;
                        if !suppress {
                            values[target] = Some(value);
                            target += 1;
                            count += 1;
                        }
                        Ok(Some(true))
                    }
                    Ok(Field::Mismatch) => Ok(Some(false)),
                    Ok(Field::Eof) => Ok(None),
                    Err(error) => Err(error),
                }
            }
        };
        // I/O failures follow the same return-count rules as end of input.
        // The file manager retains the stream's error state where supported.
        match status.unwrap_or(None) {
            Some(true) => {}
            Some(false) => break,
            None => {
                if !converted {
                    count = -1;
                }
                break;
            }
        }
    }
    Ok(Outcome { count, values })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn widths_suppression_literals_and_pushback() {
        let mut input = SliceInput::new(b" 12,99 ffQ");
        let result = scan(&mut input, b"%2d,%*d %h", &[32, 8], true).unwrap();
        assert_eq!(result.count, 2);
        assert_eq!(result.values[0].as_ref().unwrap().value, 12u8.into());
        assert_eq!(result.values[1].as_ref().unwrap().value, 255u8.into());
        assert_eq!(input.read_byte().unwrap(), Some(b'Q'));
    }
    #[test]
    fn failure_eof_and_unknown_values() {
        for (text, format, count) in [
            ("", "%d", -1),
            ("q", "%d", 0),
            ("1", "%d%d", 1),
            ("1", "%*d%d", 0),
            ("-", "%d", 0),
            ("a", "a%d", 0),
            ("%", "%%%d", 0),
        ] {
            let result = scan(
                &mut SliceInput::new(text.as_bytes()),
                format.as_bytes(),
                &[32, 32],
                true,
            )
            .unwrap();
            assert_eq!(result.count, count, "{text}/{format}");
        }
        let result = scan(&mut SliceInput::new(b"x 1z"), b"%d %h", &[65, 8], true).unwrap();
        assert_eq!(
            result.values[0].as_ref().unwrap().mask,
            (BigUint::from(1u8) << 65) - BigUint::from(1u8)
        );
        assert_eq!(
            result.values[0].as_ref().unwrap().value,
            result.values[0].as_ref().unwrap().mask
        );
        assert_eq!(result.values[1].as_ref().unwrap().value, 16u8.into());
        assert_eq!(result.values[1].as_ref().unwrap().mask, 15u8.into());
    }
    #[test]
    fn format_validation_and_character_width() {
        for format in [b"%s".as_slice(), b"%t", b"%0d", b"%q"] {
            assert!(validate_format(format, 1).is_err());
        }
        let mut input = SliceInput::new(b"ab");
        let result = scan(&mut input, b"%2c%c", &[32, 4], false).unwrap();
        assert_eq!(result.count, 2);
        assert_eq!(result.values[0].as_ref().unwrap().value, 97u8.into());
        assert_eq!(result.values[1].as_ref().unwrap().value, 2u8.into());
    }
    #[test]
    fn input_error_finishes_an_existing_numeric_field() {
        struct ErrorAtEnd<'a>(SliceInput<'a>);
        impl Input for ErrorAtEnd<'_> {
            fn read_byte(&mut self) -> io::Result<Option<u8>> {
                match self.0.read_byte()? {
                    Some(byte) => Ok(Some(byte)),
                    None => Err(io::ErrorKind::UnexpectedEof.into()),
                }
            }
            fn unread_byte(&mut self, byte: u8) -> io::Result<()> {
                self.0.unread_byte(byte)
            }
        }
        let result = scan(
            &mut ErrorAtEnd(SliceInput::new(b"12")),
            b"%d%d",
            &[32, 32],
            false,
        )
        .unwrap();
        assert_eq!(result.count, 1);
        assert_eq!(result.values[0].as_ref().unwrap().value, 12u8.into());
        assert!(result.values[1].is_none());
    }
    fn real_destination(precision: FloatPrecision) -> Destination {
        Destination {
            width: precision.width(),
            float_precision: Some(precision),
        }
    }
    #[test]
    fn real_precision_overflow_and_negative_zero() {
        for (text, precision, expected) in [
            ("-0", FloatPrecision::F64, (-0.0f64).to_bits()),
            ("1.25e2", FloatPrecision::F64, 125.0f64.to_bits()),
            ("+.5", FloatPrecision::F64, 0.5f64.to_bits()),
            ("1.", FloatPrecision::F64, 1.0f64.to_bits()),
            ("1e9999", FloatPrecision::F64, f64::INFINITY.to_bits()),
            (
                "-1e9999",
                FloatPrecision::F32,
                u64::from(f32::NEG_INFINITY.to_bits()),
            ),
            (
                "1.0000000596046447753906250000001",
                FloatPrecision::F32,
                0x3f800001,
            ),
        ] {
            let result = scan_typed(
                &mut SliceInput::new(text.as_bytes()),
                b"%g",
                &[real_destination(precision)],
                false,
            )
            .unwrap();
            assert_eq!(result.count, 1, "{text}");
            assert_eq!(
                result.values[0].as_ref().unwrap().value,
                expected.into(),
                "{text}"
            );
        }
    }
    #[test]
    fn real_width_suppression_mismatch_and_eof() {
        let real = real_destination(FloatPrecision::F64);
        let integer = Destination {
            width: 32,
            float_precision: None,
        };
        let mut input = SliceInput::new(b" 1.25Q");
        let result = scan_typed(&mut input, b"%4f", &[real], false).unwrap();
        assert_eq!(
            result.values[0].as_ref().unwrap().value,
            1.25f64.to_bits().into()
        );
        assert_eq!(input.read_byte().unwrap(), Some(b'Q'));
        let mut input = SliceInput::new(b"1e+2Q");
        let result = scan_typed(&mut input, b"%3e", &[real], false).unwrap();
        assert_eq!(result.count, 0);
        assert!(result.values[0].is_none());
        assert_eq!(input.read_byte().unwrap(), Some(b'2'));
        for (text, expected) in [
            ("", -1),
            (" ", -1),
            ("-", 0),
            (".", 0),
            ("1e", 0),
            ("bad", 0),
        ] {
            let result =
                scan_typed(&mut SliceInput::new(text.as_bytes()), b"%f", &[real], false).unwrap();
            assert_eq!(result.count, expected, "{text}");
            assert!(result.values[0].is_none());
        }
        let result = scan_typed(
            &mut SliceInput::new(b"1.2e+3 7"),
            b"%*e%d",
            &[integer],
            false,
        )
        .unwrap();
        assert_eq!(result.count, 1);
        assert_eq!(result.values[0].as_ref().unwrap().value, 7u8.into());
        let result = scan_typed(
            &mut SliceInput::new(b"7 1.25 bad"),
            b"%d%f%g",
            &[integer, real, real],
            false,
        )
        .unwrap();
        assert_eq!(result.count, 2);
        assert!(result.values[2].is_none());
        assert!(
            scan_typed(&mut SliceInput::new(b"1"), b"%d", &[real], false)
                .unwrap_err()
                .contains("integral destination")
        );
        assert!(
            scan_typed(&mut SliceInput::new(b"1.25"), b"%f", &[integer], false)
                .unwrap_err()
                .contains("real or shortreal")
        );
    }
}
