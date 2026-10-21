//! Integral formatted input (IEEE 1800-2023 21.3.4.3).
//!
//! Supported conversions are `%d`, `%h`/`%x`, `%o`, `%b`, and `%c`, with
//! assignment suppression and maximum numeric field widths. `%c` reads one
//! byte without skipping whitespace. Numeric assignments retain the low bits
//! of the destination; radix X/Z digits retain their four-state representation.
//! String, real, raw binary, and time-format conversions are diagnosed rather
//! than partially interpreted.
use num_bigint::BigUint;
use std::io;

/// A scanner uses at most one byte of pushback and leaves mismatching input unread.
pub trait Input {
    fn read_byte(&mut self) -> io::Result<Option<u8>>;
    fn unread_byte(&mut self, byte: u8) -> io::Result<()>;
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
            b'd'|b'h'|b'x'|b'o'|b'b'|b'c'=>{},
            b's'=>return Err("scan string output conversion %s is unsupported; only integral numeric/character output is supported".into()),
            b'e'|b'f'|b'g'=>return Err("scan real output conversions are unsupported".into()),
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

/// Scan integral fields. Suppressed conversions consume input without assigning
/// or incrementing `count`; a conversion mismatch preserves the offending byte.
pub fn scan(
    input: &mut impl Input,
    format: &[u8],
    widths: &[usize],
    nul_is_space: bool,
) -> Result<Outcome, String> {
    validate_format(format, widths.len())?;
    let directives = format_directives(format)?;
    let mut values = vec![None; widths.len()];
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
                let bits = if suppress { 1 } else { widths[target] };
                match field(input, code, width, bits, nul_is_space) {
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
        for format in [b"%s".as_slice(), b"%f", b"%t", b"%0d", b"%q"] {
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
}
