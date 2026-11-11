//! Numeric literal grammar (IEEE 1800-2023 A.8.7), without value evaluation.

fn unsigned(text: &str) -> bool {
    text.as_bytes().first().is_some_and(u8::is_ascii_digit)
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'_')
}

pub(super) fn integral(text: &str) -> bool {
    let Some((size, suffix)) = text.split_once('\'') else {
        return unsigned(text);
    };
    if !size.is_empty() && (!unsigned(size) || size.starts_with('0')) {
        return false;
    }
    let suffix = suffix.strip_prefix(['s', 'S']).unwrap_or(suffix);
    let Some(base) = suffix.as_bytes().first() else {
        return false;
    };
    let body = suffix[1..].trim_start_matches(|character: char| character.is_ascii_whitespace());
    if body.is_empty() || body.starts_with('_') {
        return false;
    }
    let unknown = |byte| matches!(byte, b'x' | b'X' | b'z' | b'Z' | b'?');
    match base.to_ascii_lowercase() {
        b'd' => {
            unsigned(body)
                || body.as_bytes().first().is_some_and(|byte| unknown(*byte))
                    && body[1..].bytes().all(|byte| byte == b'_')
        }
        b'b' => body
            .bytes()
            .all(|byte| matches!(byte, b'0' | b'1' | b'_') || unknown(byte)),
        b'o' => body
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'7' | b'_') || unknown(byte)),
        b'h' => body
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() || byte == b'_' || unknown(byte)),
        _ => false,
    }
}

/// The value of an integral number (IEEE 1800-2023 A.8.7) that is not
/// negative, has no unknown digits and fits 64 bits. A sized number is
/// truncated to its size first (5.7.1).
pub(super) fn integral_value(text: &str) -> Option<u64> {
    if !integral(text) {
        return None;
    }
    // The value of the digits modulo 2^128, and whether it is 2^128 or more.
    let digits = |body: &str, radix: u32| -> Option<(u128, bool)> {
        let mut value = 0u128;
        let mut wide = false;
        for digit in body.chars().filter(|character| *character != '_') {
            let digit = u128::from(digit.to_digit(radix)?);
            let (scaled, overflow) = value.overflowing_mul(u128::from(radix));
            let (sum, carry) = scaled.overflowing_add(digit);
            wide |= overflow || carry;
            value = sum;
        }
        Some((value, wide))
    };
    let Some((size, suffix)) = text.split_once('\'') else {
        let (value, wide) = digits(text, 10)?;
        return if wide {
            None
        } else {
            u64::try_from(value).ok()
        };
    };
    let signed = suffix.starts_with(['s', 'S']);
    let suffix = suffix.strip_prefix(['s', 'S']).unwrap_or(suffix);
    let radix = match suffix.as_bytes().first()?.to_ascii_lowercase() {
        b'd' => 10,
        b'b' => 2,
        b'o' => 8,
        _ => 16,
    };
    let (value, wide) = digits(suffix[1..].trim_start(), radix)?;
    let width = if size.is_empty() {
        // An unsized number is at least 32 bits wide; a wider signed one
        // keeps a sign bit above its value.
        let bits = if wide {
            return None;
        } else {
            128 - value.leading_zeros()
        };
        match bits {
            0..=32 => 32,
            _ if signed => bits + 1,
            _ => bits,
        }
    } else {
        let (size, wide) = digits(size, 10)?;
        if wide {
            return None;
        }
        u32::try_from(size).ok()?
    };
    // Bits above the 128 the digits were reduced to are unknown here.
    if wide && width > 128 {
        return None;
    }
    let value = if width < 128 {
        value & ((1 << width) - 1)
    } else {
        value
    };
    let negative = signed && width <= 128 && (value >> (width - 1)) & 1 == 1;
    if negative {
        return None;
    }
    u64::try_from(value).ok()
}

fn fixed(text: &str) -> bool {
    if let Some((whole, fraction)) = text.split_once('.') {
        unsigned(whole) && unsigned(fraction)
    } else {
        unsigned(text)
    }
}

pub(super) fn literal(text: &str) -> bool {
    if matches!(text, "'0" | "'1" | "'x" | "'X" | "'z" | "'Z") {
        return true;
    }
    if text.contains('\'') {
        return integral(text);
    }
    for unit in ["ms", "us", "ns", "ps", "fs", "s"] {
        if let Some(value) = text.strip_suffix(unit) {
            return fixed(value);
        }
    }
    if let Some((mantissa, exponent)) = text.split_once(['e', 'E']) {
        fixed(mantissa) && unsigned(exponent.strip_prefix(['+', '-']).unwrap_or(exponent))
    } else {
        fixed(text)
    }
}
