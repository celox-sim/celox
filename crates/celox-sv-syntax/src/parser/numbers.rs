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
