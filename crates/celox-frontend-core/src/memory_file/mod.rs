//! Memory files read by `$readmemh` and `$readmemb` (IEEE 1800-2023 21.4).

use celox_design::{InitialStateWriteRun, MemoryFileAddressing, MemoryFileLoad};
use std::path::{Path, PathBuf};

pub mod lowering;
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

/// Writes for an explicit SystemVerilog address range. Offsets are relative
/// to its lowest address; callers map them to the destination's layout.
pub struct RangedMemoryWrites {
    pub writes: ParsedMemoryWrites,
    pub warning: Option<String>,
}

/// Load in either direction, interpreting `@` directives as absolute indices
/// (IEEE 1800-2023 21.4). A data-count mismatch without directives is a warning.
pub fn parse_memory_range(
    content: &str,
    radix: u32,
    width: usize,
    start: i128,
    finish: i128,
) -> Result<RangedMemoryWrites, MemoryFileError> {
    let (writes, warning, error) = scan_memory_range(content, radix, width, start, finish);
    match error {
        Some(error) => Err(error),
        None => Ok(RangedMemoryWrites { writes, warning }),
    }
}

/// [`parse_memory_range`], which ends at the first error and returns the
/// writes before it with the error: a load that finds an error is
/// terminated (IEEE 1800-2023 21.4), keeping the words it already loaded.
fn scan_memory_range(
    content: &str,
    radix: u32,
    width: usize,
    start: i128,
    finish: i128,
) -> (ParsedMemoryWrites, Option<String>, Option<MemoryFileError>) {
    let low = start.min(finish);
    let high = start.max(finish);
    let step = if start <= finish { 1 } else { -1 };
    let mut address = Some(start);
    let mut runs = Vec::new();
    let mut words = 0usize;
    let mut directives = false;
    let mut error = None;
    for token in memory_tokens(content) {
        if let Some(text) = token.strip_prefix('@') {
            directives = true;
            let index = match i128::from_str_radix(text, 16) {
                Ok(index) => index,
                Err(parse) => {
                    error = Some(MemoryFileError::syntax(format!(
                        "invalid address directive {token}: {parse}"
                    )));
                    break;
                }
            };
            if index < low || index > high {
                error = Some(MemoryFileError::destination(format!(
                    "address {index} is outside requested range [{start}:{finish}]"
                )));
                break;
            }
            address = Some(index);
            continue;
        }
        let (value, mask) = match parse_memory_word(&token, radix, width) {
            Ok(word) => word,
            Err(word_error) => {
                error = Some(word_error);
                break;
            }
        };
        words += 1;
        if let Some(index) = address.filter(|index| *index >= low && *index <= high) {
            let Some(offset) = usize::try_from(index.abs_diff(low))
                .ok()
                .and_then(|index| index.checked_mul(width))
            else {
                error = Some(MemoryFileError::destination(
                    "memory address offset overflows",
                ));
                break;
            };
            runs.push(InitialStateWriteRun {
                bit_offset: offset,
                bit_width: width,
                value_bytes: biguint_to_fixed_le_bytes(&value, width),
                mask_bytes: biguint_to_fixed_le_bytes(&mask, width),
            });
            address = index.checked_add(step);
        }
    }
    let expected = start.abs_diff(finish).checked_add(1);
    let warning = (error.is_none() && !directives && expected != Some(words as u128)).then(|| format!(
        "file contains {words} words for requested range [{start}:{finish}] ({} words); excess words are ignored and missing words leave memory unchanged",
        expected.map_or_else(|| "too many".to_string(), |count| count.to_string()),
    ));
    (ParsedMemoryWrites { runs, words }, warning, error)
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
    let (writes, error) = scan_memory_write_runs(content, radix, width, start_addr, depth);
    match error {
        Some(error) => Err(error),
        None => Ok(writes),
    }
}

/// [`parse_memory_write_runs`], which ends at the first error and returns
/// the writes before it with the error.
fn scan_memory_write_runs(
    content: &str,
    radix: u32,
    width: usize,
    start_addr: usize,
    depth: usize,
) -> (ParsedMemoryWrites, Option<MemoryFileError>) {
    let mut runs: Vec<InitialStateWriteRun> = Vec::new();
    let mut addr = 0usize;
    let mut words = 0usize;
    let mut error = None;
    for word_token in memory_tokens(content) {
        if let Some(address) = word_token.strip_prefix('@') {
            match usize::from_str_radix(address, 16) {
                Ok(address) => addr = address,
                Err(parse) => {
                    error = Some(MemoryFileError::syntax(format!(
                        "invalid address directive {word_token}: {parse}"
                    )));
                    break;
                }
            }
            continue;
        }
        let (value, mask) = match parse_memory_word(&word_token, radix, width) {
            Ok(word) => word,
            Err(word_error) => {
                error = Some(word_error);
                break;
            }
        };
        let Some(dst_addr) = start_addr.checked_add(addr) else {
            error = Some(MemoryFileError::destination(
                "address exceeds destination depth",
            ));
            break;
        };
        if dst_addr >= depth {
            error = Some(MemoryFileError::destination(format!(
                "address {dst_addr} exceeds destination depth {depth}"
            )));
            break;
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
        let Some(next) = addr.checked_add(1) else {
            error = Some(MemoryFileError::destination(
                "address exceeds destination depth",
            ));
            break;
        };
        addr = next;
    }
    (ParsedMemoryWrites { runs, words }, error)
}

/// The constant parameters of `load` as compiled code passes them to the
/// heap's memory-file reader (see [`lowering`]).
///
/// The call's location stays out of the code, whose report sites carry it.
pub fn encode_descriptor(load: &MemoryFileLoad) -> Vec<u8> {
    let load = MemoryFileLoad {
        location: None,
        ..load.clone()
    };
    serde_json::to_vec(&load).expect("a memory file load serializes")
}

/// The load [`encode_descriptor`] encoded, reading `file` instead of the
/// file it names, from `range.0` toward `range.1` when given.
pub fn decode_descriptor(
    descriptor: &[u8],
    file: &[u8],
    range: Option<(i64, i64)>,
) -> Option<MemoryFileLoad> {
    let mut load: MemoryFileLoad = serde_json::from_slice(descriptor).ok()?;
    load.file = String::from_utf8_lossy(file).into_owned();
    if let (
        Some((start, finish)),
        MemoryFileAddressing::Range {
            start: s,
            finish: f,
            ..
        },
    ) = (range, &mut load.addressing)
    {
        (*s, *f) = (start.into(), finish.into());
    }
    Some(load)
}

/// What a [`MemoryFileLoad`] does when it runs: the words it writes, at
/// their bits in the destination, and its messages.
#[derive(Debug, Default)]
pub struct MemoryLoad {
    pub runs: Vec<InitialStateWriteRun>,
    /// A file without address directives whose number of words differs from
    /// the range (IEEE 1800-2023 21.4).
    pub warning: Option<String>,
    /// The error that terminated the load; `runs` holds the words loaded
    /// before it (IEEE 1800-2023 21.4).
    pub error: Option<String>,
}

/// The paths `load` looks for its file at, in order: a relative name in
/// its base directory, then in the working directory.
pub fn memory_file_candidates(load: &MemoryFileLoad) -> Vec<PathBuf> {
    let file = Path::new(&load.file);
    let mut candidates = Vec::new();
    if !file.is_absolute()
        && let Some(base) = &load.base
    {
        candidates.push(Path::new(base).join(file));
    }
    candidates.push(file.to_path_buf());
    candidates
}

/// Run `load`, reading its file through `read`.
pub fn load_memory_file(
    load: &MemoryFileLoad,
    mut read: impl FnMut(&Path) -> std::io::Result<String>,
) -> MemoryLoad {
    let task = load.task();
    let width = load.element_width;
    // The addresses the range of the call names must be the array's.
    if let MemoryFileAddressing::Range {
        left,
        right,
        start,
        finish,
    } = load.addressing
    {
        let (low, high) = (left.min(right), left.max(right));
        if start < low || start > high || finish < low || finish > high {
            return MemoryLoad {
                error: Some(format!(
                    "{task}: address range [{start}:{finish}] is outside the destination range [{low}:{high}]"
                )),
                ..MemoryLoad::default()
            };
        }
    }
    // The first candidate that exists; a failure names the first one.
    let candidates = memory_file_candidates(load);
    let mut first_error = None;
    let mut found = None;
    for candidate in &candidates {
        match read(candidate) {
            Ok(content) => {
                found = Some((candidate, content));
                break;
            }
            Err(error) => {
                let missing = error.kind() == std::io::ErrorKind::NotFound;
                first_error.get_or_insert((candidate, error));
                if !missing {
                    break;
                }
            }
        }
    }
    let Some((path, content)) = found else {
        let (path, error) = first_error.expect("a memory file has a candidate path");
        return MemoryLoad {
            error: Some(format!(
                "{task}: failed to read {}: {error}",
                path.display()
            )),
            ..MemoryLoad::default()
        };
    };
    let (runs, warning, error) = match load.addressing {
        MemoryFileAddressing::Range {
            left,
            start,
            finish,
            ..
        } => {
            let (writes, warning, error) =
                scan_memory_range(&content, load.radix, width, start, finish);
            // Offsets count from the lowest address read; the destination
            // places a word by its distance from the left bound.
            let low = start.min(finish);
            let mut runs = Vec::with_capacity(writes.runs.len());
            for mut run in writes.runs {
                let index = low + (run.bit_offset / width.max(1)) as i128;
                run.bit_offset =
                    usize::try_from(index.abs_diff(left)).unwrap_or(usize::MAX) * width;
                runs.push(run);
            }
            (runs, warning, error)
        }
        MemoryFileAddressing::Offset { start, depth } => {
            let (writes, error) = scan_memory_write_runs(&content, load.radix, width, start, depth);
            (writes.runs, None, error)
        }
    };
    MemoryLoad {
        runs,
        warning: warning.map(|detail| format!("{task} reading {}: {detail}", path.display())),
        error: error.map(|error| format!("{task} reading {}: {}", path.display(), error.message)),
    }
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
            // Four-state bits are `(value, mask)`: X is `(1, 1)` and Z is
            // `(0, 1)`. `?` is another way of writing Z (IEEE 1800-2023
            // 5.7.1).
            'x' | 'X' => {
                let unknown = (BigUint::from(1u8) << bits_per_digit) - BigUint::from(1u8);
                value |= &unknown;
                mask |= unknown;
            }
            'z' | 'Z' | '?' => {
                mask |= (BigUint::from(1u8) << bits_per_digit) - BigUint::from(1u8);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranged_loads_follow_direction_and_absolute_directives() {
        for (start, finish, content, expected) in [
            (3, 1, "a b c", vec![(16, 10), (8, 11), (0, 12)]),
            (1, 3, "a @2 b c", vec![(0, 10), (8, 11), (16, 12)]),
            (3, 1, "@2 b c", vec![(8, 11), (0, 12)]),
            (-2, 0, "a b c", vec![(0, 10), (8, 11), (16, 12)]),
        ] {
            let parsed = parse_memory_range(content, 16, 8, start, finish).unwrap();
            assert!(parsed.warning.is_none());
            let actual = parsed
                .writes
                .runs
                .iter()
                .map(|run| (run.bit_offset, run.value_bytes[0]))
                .collect::<Vec<_>>();
            assert_eq!(actual, expected);
        }
    }

    /// A relative name is looked up in the base directory first, then in
    /// the working directory; a failure names the first place.
    #[test]
    fn loads_look_up_relative_names_in_the_base_directory_first() {
        let load = |base: Option<&str>| MemoryFileLoad {
            radix: 16,
            file: "test/boot.hex".to_string(),
            base: base.map(str::to_string),
            base_key: None,
            element_width: 8,
            addressing: MemoryFileAddressing::Offset { start: 0, depth: 2 },
            location: None,
        };
        let files = |present: &'static [(&'static str, &'static str)]| {
            move |path: &Path| {
                present
                    .iter()
                    .find(|(name, _)| Path::new(name) == path)
                    .map(|(_, content)| content.to_string())
                    .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))
            }
        };
        let first = |loaded: MemoryLoad| loaded.runs.first().map(|run| run.value_bytes[0]);
        let both = &[("tb/test/boot.hex", "11"), ("test/boot.hex", "22")];
        assert_eq!(
            first(load_memory_file(&load(Some("tb")), files(both))),
            Some(0x11)
        );
        let root = &[("test/boot.hex", "22")];
        assert_eq!(
            first(load_memory_file(&load(Some("tb")), files(root))),
            Some(0x22)
        );
        assert_eq!(
            first(load_memory_file(&load(None), files(both))),
            Some(0x22)
        );
        let missing = load_memory_file(&load(Some("tb")), files(&[]));
        let error = missing.error.unwrap();
        assert!(error.contains("failed to read tb/test/boot.hex"), "{error}");
    }

    /// An error terminates a load and keeps the words before it.
    #[test]
    fn errors_keep_the_words_loaded_before_them() {
        let load = MemoryFileLoad {
            radix: 16,
            file: "m.hex".to_string(),
            base: None,
            base_key: None,
            element_width: 8,
            addressing: MemoryFileAddressing::Range {
                left: 0,
                right: 3,
                start: 0,
                finish: 3,
            },
            location: None,
        };
        let loaded = load_memory_file(&load, |_| Ok("aa bb g cc".to_string()));
        let words: Vec<_> = loaded
            .runs
            .iter()
            .map(|run| (run.bit_offset, run.value_bytes[0]))
            .collect();
        assert_eq!(words, [(0, 0xaa), (8, 0xbb)]);
        assert!(loaded.error.unwrap().contains("invalid data token g"));
        assert!(loaded.warning.is_none());
    }

    #[test]
    fn mismatched_counts_warn_and_preserve_only_available_range_words() {
        for (content, loaded) in [("a", 1), ("a b c d", 2)] {
            let parsed = parse_memory_range(content, 16, 8, 3, 2).unwrap();
            assert!(
                parsed
                    .warning
                    .as_deref()
                    .unwrap()
                    .contains("requested range [3:2]")
            );
            assert_eq!(parsed.writes.runs.len(), loaded);
        }
        // Address directives are checked even if no following word exists.
        for content in ["@4", "@1 a", "a b @4"] {
            let error = parse_memory_range(content, 16, 8, 3, 2).err().unwrap();
            assert!(error.at_destination);
            assert!(error.message.contains("requested range [3:2]"));
            assert!(!error.message.contains("depth"));
        }
        assert!(parse_memory_range("g", 16, 8, 3, 2).is_err());
        assert!(parse_memory_range("2", 2, 8, 3, 2).is_err());
    }
}
