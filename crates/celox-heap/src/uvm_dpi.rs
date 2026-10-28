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

/// The pattern text of expression bytes `re`, each byte that is not UTF-8
/// written as an escape matching that byte.
fn pattern_text(re: &[u8]) -> String {
    let mut text = String::with_capacity(re.len());
    for chunk in re.utf8_chunks() {
        text.push_str(chunk.valid());
        for byte in chunk.invalid() {
            text.push_str(&format!("\\x{byte:02X}"));
        }
    }
    text
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
        let pattern = pattern_text(&pattern);
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
