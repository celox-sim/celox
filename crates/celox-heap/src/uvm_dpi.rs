//! The functions of UVM's C library (UVM `src/dpi`) that Celox provides as
//! heap operations: regular expressions (`uvm_regex.cc`) and the tool's
//! command line (`uvm_svcmd_dpi.c`).
//!
//! A regular expression handle (a `chandle`) is a positive number, never
//! reused, `0` (`null`) on failure. Compiled expressions are rebuilt from
//! their patterns after a checkpoint is restored.

use super::*;
use std::collections::BTreeMap;
use std::collections::hash_map::Entry;

/// `REG_NOMATCH` of the C library UVM uses: `uvm_re_exec` returns `0` on a
/// match and this otherwise.
const REG_NOMATCH: u64 = 1;

/// `UVM_REGEX_MAX_LENGTH` of `uvm_regex.cc`.
const MAX_LENGTH: usize = 2048;

/// The regular-expression and command-line state of UVM's C library.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub(crate) struct UvmDpi {
    /// The pattern of each live regular expression handle.
    patterns: BTreeMap<u64, String>,
    /// The last handle given out.
    last_handle: u64,
    #[serde(skip)]
    compiled: CompiledCache,
    /// `uvm_re_buffer()`: the last message or deglobbed expression.
    buffer: Vec<u8>,
    /// The `exec_ret` of the last `uvm_re_compexec`.
    last_exec: u64,
    /// The tool's arguments, which `uvm_dpi_get_next_arg_c` returns in turn.
    arguments: Vec<String>,
    next_argument: usize,
}

/// Compiled expressions, rebuilt on demand; not part of the heap's state.
#[derive(Clone, Debug, Default)]
struct CompiledCache(HashMap<u64, regex::bytes::Regex>);

impl PartialEq for CompiledCache {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for CompiledCache {}

/// Compile `pattern` as a POSIX extended expression without `REG_NEWLINE`
/// in the C locale: `.` matches any byte, a newline included.
fn compile(pattern: &str) -> Result<regex::bytes::Regex, regex::Error> {
    regex::bytes::RegexBuilder::new(pattern)
        .dot_matches_new_line(true)
        .unicode(false)
        .build()
}

/// One element of an expression: a character, or a byte that is not
/// UTF-8.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Unit {
    Char(char),
    Byte(u8),
}

/// The pattern of the `regex` crate that matches what POSIX extended
/// expression `re` does (IEEE Std 1003.1 9.4), which UVM compiles with
/// `regcomp`. Bracket expressions differ: in POSIX a `]` first in the list
/// and a `[` that starts no class, collating symbol or equivalence class are
/// characters of the list, and `\` is one too; the `regex` crate reads them
/// as the end of the class, a nested class and an escape. Outside brackets
/// the syntax is shared. A byte that is not UTF-8 is written as an escape
/// matching that byte. A list that is not terminated or names an unknown
/// class is an error, with the message of glibc's `regerror`.
fn pattern_text(re: &[u8]) -> Result<String, &'static str> {
    let mut units = Vec::with_capacity(re.len());
    for chunk in re.utf8_chunks() {
        units.extend(chunk.valid().chars().map(Unit::Char));
        units.extend(chunk.invalid().iter().map(|&byte| Unit::Byte(byte)));
    }
    let mut text = String::with_capacity(re.len());
    let mut index = 0;
    while index < units.len() {
        match units[index] {
            Unit::Char('\\') if index + 1 < units.len() => {
                text.push('\\');
                push_unit(&mut text, units[index + 1]);
                index += 2;
            }
            Unit::Char('[') => {
                let (class, end) = bracket(&units, index)?;
                text.push_str(&class);
                index = end;
            }
            unit => {
                push_unit(&mut text, unit);
                index += 1;
            }
        }
    }
    Ok(text)
}

fn push_unit(text: &mut String, unit: Unit) {
    match unit {
        Unit::Char(character) => text.push(character),
        Unit::Byte(byte) => text.push_str(&format!("\\x{byte:02X}")),
    }
}

/// A character of a class of the `regex` crate that matches `unit` alone.
fn class_unit(unit: Unit) -> String {
    match unit {
        Unit::Char(character) if "\\[]^-&~".contains(character) => format!("\\{character}"),
        unit => {
            let mut text = String::new();
            push_unit(&mut text, unit);
            text
        }
    }
}

/// The class of the `regex` crate for the POSIX bracket expression that
/// starts at `units[start]`, and the index after it.
fn bracket(units: &[Unit], start: usize) -> Result<(String, usize), &'static str> {
    const UNMATCHED: &str = "Unmatched [, [^, [:, [., or [=";
    const INVALID_RANGE_END: &str = "Invalid range end";
    /// The character classes POSIX defines, which the crate knows too.
    const CLASSES: &[&str] = &[
        "alnum", "alpha", "blank", "cntrl", "digit", "graph", "lower", "print", "punct", "space",
        "upper", "xdigit",
    ];
    let char_at = |index: usize| match units.get(index) {
        Some(Unit::Char(character)) => Some(*character),
        _ => None,
    };
    let mut index = start + 1;
    let mut class = String::from("[");
    if char_at(index) == Some('^') {
        class.push('^');
        index += 1;
    }
    // One character of the list, which may be a collating symbol or an
    // equivalence class (`[.x.]`, `[=x=]`) of a single character.
    let element = |index: usize| -> Result<(Unit, usize), &'static str> {
        let unit = *units.get(index).ok_or(UNMATCHED)?;
        if unit == Unit::Char('[')
            && let Some(delimiter @ ('.' | '=')) = char_at(index + 1)
        {
            let symbol = *units.get(index + 2).ok_or(UNMATCHED)?;
            (char_at(index + 3) == Some(delimiter) && char_at(index + 4) == Some(']'))
                .then_some((symbol, index + 5))
                .ok_or("Invalid collation character")
        } else {
            Ok((unit, index + 1))
        }
    };
    let mut first = true;
    loop {
        let unit = *units.get(index).ok_or(UNMATCHED)?;
        if unit == Unit::Char(']') && !first {
            class.push(']');
            return Ok((class, index + 1));
        }
        first = false;
        if unit == Unit::Char('[') && char_at(index + 1) == Some(':') {
            let name_start = index + 2;
            let name_end = (name_start..units.len())
                .find(|&end| char_at(end) == Some(':') && char_at(end + 1) == Some(']'))
                .ok_or(UNMATCHED)?;
            let name = units[name_start..name_end]
                .iter()
                .map(|unit| match unit {
                    Unit::Char(character) => Some(*character),
                    Unit::Byte(_) => None,
                })
                .collect::<Option<String>>()
                .filter(|name| CLASSES.contains(&name.as_str()))
                .ok_or("Invalid character class name")?;
            class.push_str(&format!("[:{name}:]"));
            index = name_end + 2;
            // A class is no end of a range.
            if char_at(index) == Some('-') && char_at(index + 1).is_some_and(|next| next != ']') {
                return Err(INVALID_RANGE_END);
            }
            continue;
        }
        let (low, after) = element(index)?;
        // A range, unless the `-` is the last character of the list.
        if char_at(after) == Some('-') && char_at(after + 1) != Some(']') && after + 1 < units.len()
        {
            if char_at(after + 1) == Some('[') && char_at(after + 2) == Some(':') {
                return Err(INVALID_RANGE_END);
            }
            let (high, end) = element(after + 1)?;
            class.push_str(&format!("{}-{}", class_unit(low), class_unit(high)));
            index = end;
        } else {
            class.push_str(&class_unit(low));
            index = after;
        }
    }
}

/// The glob-to-ERE translation of `uvm_re_deglobbed`, or the message it
/// leaves in the buffer.
pub fn deglob(glob: &[u8], with_brackets: bool) -> Result<Vec<u8>, String> {
    const BRACKET: u8 = b'/';
    if glob.is_empty() || glob == [BRACKET] {
        return Ok(if with_brackets {
            b"/^$/".to_vec()
        } else {
            b"^$".to_vec()
        });
    }
    if glob.len() > 1 && glob[0] == BRACKET && glob[glob.len() - 1] == BRACKET {
        if glob.len() > MAX_LENGTH {
            return Err(format!(
                "uvm_glob_to_re() glob exceeds max length ({MAX_LENGTH})"
            ));
        }
        return Ok(if with_brackets {
            glob.to_vec()
        } else {
            glob[1..glob.len() - 1].to_vec()
        });
    }
    let mut out = Vec::with_capacity(glob.len() + 4);
    if with_brackets {
        out.push(BRACKET);
    }
    if glob[0] != b'^' {
        out.push(b'^');
    }
    for &byte in glob {
        match byte {
            b'*' | b'+' => out.extend([b'.', byte]),
            b'.' | b'[' | b']' | b'(' | b')' => out.extend([b'\\', byte]),
            b'?' => out.push(b'.'),
            _ => out.push(byte),
        }
    }
    if out.last() != Some(&b'$') {
        out.push(b'$');
    }
    if with_brackets {
        out.push(BRACKET);
    }
    if out.len() > MAX_LENGTH {
        return Err(format!(
            "uvm_glob_to_re() expansion exceeds max length({MAX_LENGTH})"
        ));
    }
    Ok(out)
}

impl Heap {
    /// Set the tool's command line: the program name, then its arguments.
    pub fn set_tool_arguments(&mut self, arguments: Vec<String>) {
        self.uvm.arguments = arguments;
        self.uvm.next_argument = 0;
    }

    fn uvm_string(&mut self, bytes: &[u8]) -> u64 {
        self.intern(bytes) as u64
    }

    /// `uvm_re_comp(re, deglob)`.
    fn re_comp(&mut self, re: Handle, deglob: bool) -> u64 {
        let re = self.string(re).to_vec();
        let pattern = if deglob {
            match self::deglob(&re, false) {
                Ok(pattern) => pattern,
                Err(message) => {
                    self.uvm.buffer = message.into_bytes();
                    return 0;
                }
            }
        } else if re.len() > MAX_LENGTH {
            self.uvm.buffer =
                format!("uvm_re_comp() re exceeds max length ({MAX_LENGTH})").into_bytes();
            return 0;
        } else if re.len() > 1 && re[0] == b'/' && re[re.len() - 1] == b'/' {
            re[1..re.len() - 1].to_vec()
        } else {
            re
        };
        let pattern = match pattern_text(&pattern) {
            Ok(pattern) => pattern,
            Err(message) => {
                self.uvm.buffer = message.as_bytes().to_vec();
                return 0;
            }
        };
        match compile(&pattern) {
            Ok(compiled) => {
                self.uvm.last_handle += 1;
                let handle = self.uvm.last_handle;
                self.uvm.patterns.insert(handle, pattern);
                self.uvm.compiled.0.insert(handle, compiled);
                handle
            }
            Err(error) => {
                self.uvm.buffer = error.to_string().into_bytes();
                0
            }
        }
    }

    /// The compiled expression of handle `rexp`, rebuilt if needed.
    fn compiled(&mut self, rexp: u64) -> Option<&regex::bytes::Regex> {
        let pattern = self.uvm.patterns.get(&rexp)?;
        match self.uvm.compiled.0.entry(rexp) {
            Entry::Occupied(entry) => Some(entry.into_mut()),
            Entry::Vacant(entry) => compile(pattern).ok().map(|regex| &*entry.insert(regex)),
        }
    }

    /// `uvm_re_exec(rexp, str)`: `0` on a match.
    fn re_exec(&mut self, rexp: u64, string: Handle) -> u64 {
        let string = self.string(string).to_vec();
        match self.compiled(rexp) {
            Some(regex) => u64::from(!regex.is_match(&string)) * REG_NOMATCH,
            None => {
                self.uvm.buffer = b"uvm_re_exec: NULL rexp".to_vec();
                REG_NOMATCH
            }
        }
    }

    /// `uvm_re_free(rexp)`.
    fn re_free(&mut self, rexp: u64) {
        self.uvm.patterns.remove(&rexp);
        self.uvm.compiled.0.remove(&rexp);
    }

    /// Perform UVM DPI operation `op` on `a`, `b` and `c`.
    pub(crate) fn uvm_call(&mut self, op: u64, a: u64, b: u64, c: u64) -> u64 {
        match op {
            ops::UVM_RE_COMP => self.re_comp(a as Handle, b & 1 != 0),
            ops::UVM_RE_EXEC => self.re_exec(a, b as Handle),
            ops::UVM_RE_FREE => {
                self.re_free(a);
                0
            }
            ops::UVM_RE_COMPEXEC => {
                let rexp = self.re_comp(a as Handle, c & 1 != 0);
                self.uvm.last_exec = if rexp == 0 {
                    REG_NOMATCH
                } else {
                    self.re_exec(rexp, b as Handle)
                };
                rexp
            }
            ops::UVM_RE_COMPEXECFREE => {
                let rexp = self.re_comp(a as Handle, c & 1 != 0);
                self.uvm.last_exec = if rexp == 0 {
                    REG_NOMATCH
                } else {
                    self.re_exec(rexp, b as Handle)
                };
                self.re_free(rexp);
                u64::from(rexp != 0)
            }
            ops::UVM_RE_LAST_EXEC => self.uvm.last_exec,
            ops::UVM_RE_BUFFER => {
                let buffer = self.uvm.buffer.clone();
                self.uvm_string(&buffer)
            }
            ops::UVM_RE_DEGLOBBED => {
                let glob = self.string(a as Handle).to_vec();
                match deglob(&glob, b & 1 != 0) {
                    Ok(regex) => {
                        self.uvm.buffer = regex.clone();
                        self.uvm_string(&regex)
                    }
                    // The C function returns NULL, which is "" (35.5.6).
                    Err(message) => {
                        self.uvm.buffer = message.into_bytes();
                        EMPTY_STRING as u64
                    }
                }
            }
            ops::UVM_NEXT_ARG => {
                if a as i32 == 1 {
                    self.uvm.next_argument = 0;
                }
                match self.uvm.arguments.get(self.uvm.next_argument).cloned() {
                    Some(argument) => {
                        self.uvm.next_argument += 1;
                        self.uvm_string(argument.as_bytes())
                    }
                    None => EMPTY_STRING as u64,
                }
            }
            ops::UVM_TOOL_NAME => self.uvm_string(b"Celox"),
            ops::UVM_TOOL_VERSION => self.uvm_string(env!("CARGO_PKG_VERSION").as_bytes()),
            _ => unreachable!("UVM DPI operation {op}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_translate_like_uvm_regex_cc() {
        for (glob, brackets, expected) in [
            ("", false, "^$"),
            ("/", true, "/^$/"),
            ("*", false, "^.*$"),
            ("top.u?.*", false, "^top\\.u.\\..*$"),
            ("^a+[b]", false, "^a.+\\[b\\]$"),
            ("ends$", true, "/^ends$/"),
            ("/^x.y$/", false, "^x.y$"),
            ("/^x.y$/", true, "/^x.y$/"),
        ] {
            assert_eq!(
                String::from_utf8(deglob(glob.as_bytes(), brackets).unwrap()).unwrap(),
                expected,
                "{glob}"
            );
        }
    }

    #[test]
    fn bracket_expressions_read_as_posix_reads_them() {
        let matches = |re: &[u8], text: &[u8]| {
            let pattern = pattern_text(re).expect("a valid expression");
            compile(&pattern)
                .unwrap_or_else(|error| panic!("{pattern}: {error}"))
                .is_match(text)
        };
        // UVM's component names (`uvm_component::m_set_cl_msg_args`,
        // `uvm_object::set_name`): a `]` first in the list and a `[` that
        // starts no class are characters of it.
        let name = br"^[][[:alnum:](){}_:-]([][[:alnum:](){} _:-]*[][[:alnum:](){}_:-])?$";
        for (text, expected) in [
            (&b"env"[..], true),
            (b"uvm_test_top", true),
            (b"agent[0]", true),
            (b"a b", true),
            (b" a", false),
            (b"a.b", false),
        ] {
            assert_eq!(
                matches(name, text),
                expected,
                "{}",
                String::from_utf8_lossy(text)
            );
        }
        // A `]` first after `^` is in the negated list.
        assert!(matches(b"^[^]a]$", b"b"));
        assert!(!matches(b"^[^]a]$", b"]"));
        assert!(!matches(b"^[^]a]$", b"a"));
        // `\` is a character of a list, not an escape.
        assert!(matches(br"^[\]$", br"\"));
        assert!(matches(br"^[\n]$", b"n"));
        assert!(!matches(br"^[\n]$", b"\n"));
        // Collating symbols and equivalence classes of one character.
        assert!(matches(b"^[[.-.]a]$", b"-"));
        assert!(matches(b"^[[=e=]]$", b"e"));
        // The crate's class operators are characters, as are a `-` last and
        // a range's ends.
        assert!(matches(b"^[&&]$", b"&"));
        assert!(matches(b"^[~~-]$", b"~"));
        assert!(matches(b"^[a-c-]$", b"-"));
        assert!(matches(b"^[a-c]$", b"b"));
        assert!(!matches(b"^[a-c]$", b"d"));
        assert!(matches(b"^[!-[]$", b"["));
        // A byte that is not UTF-8, in a list and outside one.
        assert!(matches(b"^[\xff]\xfe$", b"\xff\xfe"));
        // An unterminated list or an unknown class is rejected, as
        // `regcomp` rejects it, with its message.
        assert_eq!(pattern_text(b"[abc"), Err("Unmatched [, [^, [:, [., or [="));
        assert_eq!(pattern_text(b"[]"), Err("Unmatched [, [^, [:, [., or [="));
        assert_eq!(
            pattern_text(b"[[:word:]]"),
            Err("Invalid character class name")
        );
        // A class is no end of a range; a `-` after one that ends the list
        // is a character.
        assert_eq!(pattern_text(b"[[:alpha:]-z]"), Err("Invalid range end"));
        assert_eq!(pattern_text(b"[a-[:alpha:]]"), Err("Invalid range end"));
        assert!(matches(b"^[[:digit:]-]$", b"-"));
        let mut heap = Heap::new();
        let bad = heap.intern(b"[[:word:]]") as u64;
        assert_eq!(heap.call(ops::UVM_RE_COMP, bad, 0, 0), 0);
        let message = heap.call(ops::UVM_RE_BUFFER, 0, 0, 0);
        assert_eq!(
            heap.string(message as Handle),
            b"Invalid character class name"
        );
    }

    #[test]
    fn expressions_compile_match_and_free() {
        let mut heap = Heap::new();
        let pattern = heap.intern(b"uvm_test_top.*") as u64;
        let rexp = heap.call(ops::UVM_RE_COMP, pattern, 1, 0);
        assert_ne!(rexp, 0);
        let hit = heap.intern(b"uvm_test_top.env") as u64;
        let miss = heap.intern(b"other") as u64;
        assert_eq!(heap.call(ops::UVM_RE_EXEC, rexp, hit, 0), 0);
        assert_eq!(heap.call(ops::UVM_RE_EXEC, rexp, miss, 0), REG_NOMATCH);
        // A restored heap has no compiled expressions and rebuilds them.
        let mut restored = heap.clone();
        restored.uvm.compiled = CompiledCache::default();
        assert!(restored.same_contents(&heap));
        assert_eq!(restored.call(ops::UVM_RE_EXEC, rexp, hit, 0), 0);
        // `.` matches any byte, a newline included, as without `REG_NEWLINE`
        // in the C locale.
        let lines = heap.intern(b"uvm_test_top.a\nb\xff") as u64;
        assert_eq!(heap.call(ops::UVM_RE_EXEC, rexp, lines, 0), 0);
        let byte = heap.intern(b"^a\xffb$") as u64;
        let byte = heap.call(ops::UVM_RE_COMP, byte, 0, 0);
        let hit_byte = heap.intern(b"a\xffb") as u64;
        assert_eq!(heap.call(ops::UVM_RE_EXEC, byte, hit_byte, 0), 0);
        assert_eq!(heap.call(ops::UVM_RE_EXEC, byte, hit, 0), REG_NOMATCH);
        heap.call(ops::UVM_RE_FREE, byte, 0, 0);
        heap.call(ops::UVM_RE_FREE, rexp, 0, 0);
        assert_eq!(heap.call(ops::UVM_RE_EXEC, rexp, hit, 0), REG_NOMATCH);
        // Freed expressions leave nothing behind and handles are not reused.
        assert!(heap.uvm.patterns.is_empty() && heap.uvm.compiled.0.is_empty());
        assert_ne!(heap.call(ops::UVM_RE_COMP, pattern, 1, 0), rexp);
        let bad = heap.intern(b"(") as u64;
        assert_eq!(heap.call(ops::UVM_RE_COMP, bad, 0, 0), 0);
        let message = heap.call(ops::UVM_RE_BUFFER, 0, 0, 0);
        assert!(!heap.string(message as Handle).is_empty());
        assert_eq!(heap.call(ops::UVM_RE_COMPEXECFREE, pattern, hit, 1), 1);
        assert_eq!(heap.call(ops::UVM_RE_LAST_EXEC, 0, 0, 0), 0);
    }

    #[test]
    fn arguments_are_returned_in_turn() {
        let mut heap = Heap::new();
        heap.set_tool_arguments(vec!["celox".into(), "+UVM_TESTNAME=t".into()]);
        let next = |heap: &mut Heap, init| {
            let handle = heap.call(ops::UVM_NEXT_ARG, init, 0, 0) as Handle;
            String::from_utf8(heap.string(handle).to_vec()).unwrap()
        };
        assert_eq!(next(&mut heap, 1), "celox");
        assert_eq!(next(&mut heap, 0), "+UVM_TESTNAME=t");
        assert_eq!(next(&mut heap, 0), "");
        assert_eq!(next(&mut heap, 1), "celox");
    }
}
