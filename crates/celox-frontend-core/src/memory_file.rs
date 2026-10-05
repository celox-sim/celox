//! Memory files read by `$readmemh` and `$readmemb` (IEEE 1800-2023 21.4).

use celox_design::InitialStateWriteRun;
use num_bigint::BigUint;

/// An error in a memory file.
#[derive(Debug, Clone)]
pub struct MemoryFileError {
    pub message: String,
    /// Whether the error concerns the destination (an address out of its
    /// range) rather than the file's syntax.
    pub at_destination: bool,
}

impl MemoryFileError {
    fn syntax(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            at_destination: false,
        }
    }

    fn destination(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            at_destination: true,
        }
    }
}

/// The writes a memory file performs.
pub struct ParsedMemoryWrites {
    pub runs: Vec<InitialStateWriteRun>,
    pub words: usize,
}

/// Parse a memory file of `radix` (2 or 16) into writes of `width`-bit
/// words to a destination of `depth` words, starting at word `start_addr`.
pub fn parse_memory_write_runs(
    content: &str,
    radix: u32,
    width: usize,
    start_addr: usize,
    depth: usize,
) -> Result<ParsedMemoryWrites, MemoryFileError> {
    let mut runs: Vec<InitialStateWriteRun> = Vec::new();
    let mut addr = 0usize;
    let mut words = 0usize;
    for word_token in memory_tokens(content) {
        if let Some(address) = word_token.strip_prefix('@') {
            addr = usize::from_str_radix(address, 16).map_err(|err| {
                MemoryFileError::syntax(format!("invalid address directive {word_token}: {err}"))
            })?;
            continue;
        }
        let (value, mask) = parse_memory_word(&word_token, radix, width)?;
        let Some(dst_addr) = start_addr.checked_add(addr) else {
            return Err(MemoryFileError::destination(
                "address exceeds destination depth",
            ));
        };
        if dst_addr >= depth {
            return Err(MemoryFileError::destination(format!(
                "address {dst_addr} exceeds destination depth {depth}"
            )));
        }

        let bit_offset = dst_addr * width;
        let value_bytes = biguint_to_fixed_le_bytes(&value, width);
        let mask_bytes = biguint_to_fixed_le_bytes(&mask, width);

        if let Some(last) = runs.last_mut()
            && last.bit_offset + last.bit_width == bit_offset
            && last.bit_offset % 8 == 0
            && last.bit_width % 8 == 0
            && width.is_multiple_of(8)
        {
            last.bit_width += width;
            last.value_bytes.extend(value_bytes);
            last.mask_bytes.extend(mask_bytes);
        } else {
            runs.push(InitialStateWriteRun {
                bit_offset,
                bit_width: width,
                value_bytes,
                mask_bytes,
            });
        }

        words += 1;
        addr = addr
            .checked_add(1)
            .ok_or_else(|| MemoryFileError::destination("address exceeds destination depth"))?;
    }
    Ok(ParsedMemoryWrites { runs, words })
}

pub fn biguint_to_fixed_le_bytes(value: &BigUint, width: usize) -> Vec<u8> {
    let byte_len = width.div_ceil(8);
    let mut out = vec![0; byte_len];
    let src = value.to_bytes_le();
    let copy_len = src.len().min(byte_len);
    out[..copy_len].copy_from_slice(&src[..copy_len]);
    if !width.is_multiple_of(8) && !out.is_empty() {
        let keep = (1u8 << (width % 8)) - 1;
        *out.last_mut().unwrap() &= keep;
    }
    out
}

fn memory_tokens(content: &str) -> Vec<String> {
    let mut out = String::with_capacity(content.len());
    let bytes = content.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'/' && i + 1 < bytes.len() {
            match bytes[i + 1] {
                b'/' => {
                    i += 2;
                    while i < bytes.len() && bytes[i] != b'\n' {
                        i += 1;
                    }
                    out.push(' ');
                    continue;
                }
                b'*' => {
                    i += 2;
                    while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                        i += 1;
                    }
                    i = (i + 2).min(bytes.len());
                    out.push(' ');
                    continue;
                }
                _ => {}
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out.split_whitespace()
        .map(|token| token.replace('_', ""))
        .filter(|token| !token.is_empty())
        .collect()
}

fn parse_memory_word(
    token: &str,
    radix: u32,
    width: usize,
) -> Result<(BigUint, BigUint), MemoryFileError> {
    let bits_per_digit = match radix {
        2 => 1,
        16 => 4,
        _ => {
            return Err(MemoryFileError::syntax(format!(
                "unsupported memory file radix {radix}"
            )));
        }
    };
    let mut value = BigUint::default();
    let mut mask = BigUint::default();
    for ch in token.chars() {
        value <<= bits_per_digit;
        mask <<= bits_per_digit;
        match ch {
            '0'..='9' | 'a'..='f' | 'A'..='F' => {
                let Some(digit) = ch.to_digit(radix) else {
                    return Err(invalid_memory_word(token));
                };
                value |= BigUint::from(digit);
            }
            'x' | 'X' | '?' => {
                mask |= (BigUint::from(1u8) << bits_per_digit) - BigUint::from(1u8);
            }
            'z' | 'Z' => {
                let unknown = (BigUint::from(1u8) << bits_per_digit) - BigUint::from(1u8);
                value |= &unknown;
                mask |= unknown;
            }
            _ => return Err(invalid_memory_word(token)),
        }
    }

    if width == 0 {
        return Ok((BigUint::default(), BigUint::default()));
    }
    let keep = (BigUint::from(1u8) << width) - BigUint::from(1u8);
    Ok((value & &keep, mask & keep))
}

fn invalid_memory_word(token: &str) -> MemoryFileError {
    MemoryFileError::syntax(format!("invalid data token {token}"))
}
