//! S-expression reader for test scripts.
//!
//! Atoms are symbols, numbers and strings. A string is either `"..."` with
//! `\"`, `\\`, `\n` and `\t` escapes, or a raw string `#"..."#` (any number of
//! `#` marks, as in Rust) that holds Veryl source verbatim. `;` starts a comment
//! that runs to the end of the line.

use std::fmt;

/// A position in a script, for diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pos {
    pub line: usize,
    pub column: usize,
}

impl fmt::Display for Pos {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.line, self.column)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Sexpr {
    /// A symbol or number, kept as written.
    Atom(String, Pos),
    Str(String, Pos),
    List(Vec<Sexpr>, Pos),
}

impl Sexpr {
    pub fn pos(&self) -> Pos {
        match self {
            Sexpr::Atom(_, pos) | Sexpr::Str(_, pos) | Sexpr::List(_, pos) => *pos,
        }
    }

    pub fn atom(&self) -> Option<&str> {
        match self {
            Sexpr::Atom(atom, _) => Some(atom),
            _ => None,
        }
    }

    pub fn list(&self) -> Option<&[Sexpr]> {
        match self {
            Sexpr::List(items, _) => Some(items),
            _ => None,
        }
    }

    /// The head symbol of a list, as in `(head ...)`.
    pub fn head(&self) -> Option<&str> {
        self.list()?.first()?.atom()
    }
}

#[derive(Debug)]
pub struct SyntaxError {
    pub pos: Pos,
    pub message: String,
}

impl fmt::Display for SyntaxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.pos, self.message)
    }
}

impl std::error::Error for SyntaxError {}

struct Reader<'a> {
    text: &'a str,
    offset: usize,
    line: usize,
    column: usize,
}

impl Reader<'_> {
    fn pos(&self) -> Pos {
        Pos {
            line: self.line,
            column: self.column,
        }
    }

    fn peek(&self) -> Option<char> {
        self.text[self.offset..].chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.offset += c.len_utf8();
        if c == '\n' {
            self.line += 1;
            self.column = 1;
        } else {
            self.column += 1;
        }
        Some(c)
    }

    fn error<T>(&self, pos: Pos, message: impl Into<String>) -> Result<T, SyntaxError> {
        Err(SyntaxError {
            pos,
            message: message.into(),
        })
    }

    fn skip_space(&mut self) {
        while let Some(c) = self.peek() {
            if c == ';' {
                while self.peek().is_some_and(|c| c != '\n') {
                    self.bump();
                }
            } else if c.is_whitespace() {
                self.bump();
            } else {
                break;
            }
        }
    }

    fn read(&mut self) -> Result<Option<Sexpr>, SyntaxError> {
        self.skip_space();
        let pos = self.pos();
        let Some(c) = self.peek() else {
            return Ok(None);
        };
        match c {
            '(' => {
                self.bump();
                let mut items = Vec::new();
                loop {
                    self.skip_space();
                    match self.peek() {
                        None => return self.error(pos, "unclosed `(`"),
                        Some(')') => {
                            self.bump();
                            return Ok(Some(Sexpr::List(items, pos)));
                        }
                        Some(_) => items.push(self.read()?.expect("not at end")),
                    }
                }
            }
            ')' => self.error(pos, "unexpected `)`"),
            '"' => {
                self.bump();
                let mut text = String::new();
                loop {
                    match self.bump() {
                        None => return self.error(pos, "unterminated string"),
                        Some('"') => return Ok(Some(Sexpr::Str(text, pos))),
                        Some('\\') => match self.bump() {
                            Some('n') => text.push('\n'),
                            Some('t') => text.push('\t'),
                            Some(c @ ('"' | '\\')) => text.push(c),
                            _ => return self.error(self.pos(), "unknown escape"),
                        },
                        Some(c) => text.push(c),
                    }
                }
            }
            '#' if self.text[self.offset..]
                .trim_start_matches('#')
                .starts_with('"') =>
            {
                let hashes = self.text[self.offset..]
                    .chars()
                    .take_while(|c| *c == '#')
                    .count();
                for _ in 0..=hashes {
                    self.bump();
                }
                let terminator = format!("\"{}", "#".repeat(hashes));
                let Some(end) = self.text[self.offset..].find(&terminator) else {
                    return self.error(pos, "unterminated raw string");
                };
                let text = self.text[self.offset..self.offset + end].to_string();
                for _ in 0..text.chars().count() + terminator.len() {
                    self.bump();
                }
                Ok(Some(Sexpr::Str(text, pos)))
            }
            _ => {
                let start = self.offset;
                while self
                    .peek()
                    .is_some_and(|c| !c.is_whitespace() && !matches!(c, '(' | ')' | '"' | ';'))
                {
                    self.bump();
                }
                Ok(Some(Sexpr::Atom(
                    self.text[start..self.offset].to_string(),
                    pos,
                )))
            }
        }
    }
}

/// Read every top-level form of `text`.
pub fn parse(text: &str) -> Result<Vec<Sexpr>, SyntaxError> {
    let mut reader = Reader {
        text,
        offset: 0,
        line: 1,
        column: 1,
    };
    let mut forms = Vec::new();
    while let Some(form) = reader.read()? {
        forms.push(form);
    }
    Ok(forms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_lists_atoms_strings_and_raw_strings() {
        let forms =
            parse("; comment\n(case t (source \"a.veryl\" #\"module \"Top\" {}\"#) (set a 0x5))")
                .unwrap();
        assert_eq!(forms.len(), 1);
        let items = forms[0].list().unwrap();
        assert_eq!(items[0].atom(), Some("case"));
        let source = items[2].list().unwrap();
        assert_eq!(source[1], Sexpr::Str("a.veryl".into(), source[1].pos()));
        assert_eq!(
            source[2],
            Sexpr::Str("module \"Top\" {}".into(), source[2].pos())
        );
        assert_eq!(items[3].head(), Some("set"));
        assert_eq!(forms[0].pos(), Pos { line: 2, column: 1 });
    }

    #[test]
    fn raw_strings_use_the_matching_number_of_hashes() {
        let forms = parse("##\"a \"# b\"##").unwrap();
        assert_eq!(forms[0], Sexpr::Str("a \"# b".into(), forms[0].pos()));
    }

    #[test]
    fn reports_unclosed_lists() {
        let error = parse("(a (b)").unwrap_err();
        assert_eq!(error.pos, Pos { line: 1, column: 1 });
    }
}
