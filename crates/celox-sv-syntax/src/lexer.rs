use crate::{Error, FileId, Origin, SourceMap, Span};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TokenKind {
    Whitespace,
    Newline,
    Comment,
    Identifier,
    /// A word inside macro-quoted text. It participates in formal substitution
    /// but is not a keyword/declaration in the original source.
    MacroQuotedIdentifier,
    EscapedIdentifier,
    SystemIdentifier,
    Number,
    String,
    /// Literal characters in macro-quoted text that are not ordinary language
    /// tokens (for example Unicode). Identifiers remain separate for substitution.
    StringFragment,
    Symbol,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub text: String,
    pub origin: Origin,
    /// Original contributors to a derived token (paste or stringification).
    /// Empty for a token whose spelling is exactly its original source span.
    pub contributors: Vec<Origin>,
    pub(crate) disabled_macros: Vec<String>,
}

impl Token {
    pub fn is_trivia(&self) -> bool {
        matches!(
            self.kind,
            TokenKind::Whitespace | TokenKind::Newline | TokenKind::Comment
        )
    }

    /// Escaped identifiers retain their spelling in `text`; callers that bind
    /// names use this accessor to remove the leading escape.
    pub fn identifier(&self) -> Option<&str> {
        match self.kind {
            TokenKind::Identifier | TokenKind::MacroQuotedIdentifier => Some(&self.text),
            TokenKind::EscapedIdentifier => self.text.strip_prefix('\\'),
            _ => None,
        }
    }
}

/// Lossless tokens. Even inactive preprocessor branches must obey lexical
/// conventions (IEEE 1800-2023 22.6). Preprocessor operators are indivisible
/// tokens so that quotes inside macro text are not mistaken for string literals.
pub fn lex(sources: &SourceMap, file: FileId) -> Result<Vec<Token>, Error> {
    lex_with_context(sources, file, false)
}

pub(crate) fn lex_quoted(sources: &SourceMap, file: FileId) -> Result<Vec<Token>, Error> {
    lex_with_context(sources, file, true)
}

fn lex_with_context(
    sources: &SourceMap,
    file: FileId,
    mut macro_quote: bool,
) -> Result<Vec<Token>, Error> {
    let source = &sources.file(file).text;
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let start = at;
        let kind = match bytes[at] {
            b'\n' => {
                if start == 0 || bytes[start - 1] != b'\\' {
                    macro_quote = false;
                }
                at += 1;
                TokenKind::Newline
            }
            b'\r' if bytes.get(at + 1) == Some(&b'\n') => {
                if start == 0 || bytes[start - 1] != b'\\' {
                    macro_quote = false;
                }
                at += 2;
                TokenKind::Newline
            }
            b' ' | b'\t' | b'\r' | 12 => {
                at += 1;
                while at < bytes.len()
                    && matches!(bytes[at], b' ' | b'\t' | b'\r' | 12)
                    && !(bytes[at] == b'\r' && bytes.get(at + 1) == Some(&b'\n'))
                {
                    at += 1;
                }
                TokenKind::Whitespace
            }
            b'/' if !macro_quote && bytes.get(at + 1) == Some(&b'/') => {
                at += 2;
                while at < bytes.len() && !matches!(bytes[at], b'\r' | b'\n') {
                    at += 1;
                }
                TokenKind::Comment
            }
            b'/' if !macro_quote && bytes.get(at + 1) == Some(&b'*') => {
                at += 2;
                while at + 1 < bytes.len() && &bytes[at..at + 2] != b"*/" {
                    at += 1;
                }
                if at + 1 >= bytes.len() {
                    return Err(error(sources, file, start, "unterminated block comment"));
                }
                at += 2;
                TokenKind::Comment
            }
            b'`' => {
                at += if source[at..].starts_with("`\\`\"") || source[at..].starts_with("`\"\"\"") {
                    4
                } else if source[at..].starts_with("``") || source[at..].starts_with("`\"") {
                    2
                } else {
                    1
                };
                if matches!(&source[start..at], "`\"" | "`\"\"\"") {
                    macro_quote = !macro_quote;
                }
                TokenKind::Symbol
            }
            b'"' => {
                let triple = source[at..].starts_with("\"\"\"");
                at += if triple { 3 } else { 1 };
                loop {
                    if at >= bytes.len() {
                        return Err(error(sources, file, start, "unterminated string literal"));
                    }
                    if triple && source[at..].starts_with("\"\"\"") {
                        at += 3;
                        break;
                    }
                    if !triple && bytes[at] == b'"' {
                        at += 1;
                        break;
                    }
                    if !triple && matches!(bytes[at], b'\r' | b'\n') {
                        return Err(error(sources, file, start, "newline in string literal"));
                    }
                    if bytes[at] == b'\\' {
                        at += 1;
                        if bytes.get(at) == Some(&b'\r') && bytes.get(at + 1) == Some(&b'\n') {
                            at += 2;
                            continue;
                        }
                    }
                    if at < bytes.len() {
                        at += source[at..].chars().next().unwrap().len_utf8();
                    }
                }
                TokenKind::String
            }
            b'\\' if macro_quote || bytes.get(at + 1).is_none_or(|b| b.is_ascii_whitespace()) => {
                at += 1;
                TokenKind::Symbol
            }
            b'\\' => {
                at += 1;
                while at < bytes.len() && !bytes[at].is_ascii_whitespace() {
                    if !(33..=126).contains(&bytes[at]) {
                        return Err(error(sources, file, at, "non-ASCII escaped identifier"));
                    }
                    at += 1;
                }
                TokenKind::EscapedIdentifier
            }
            b'a'..=b'z' | b'A'..=b'Z' | b'_' | b'$' => {
                at += 1;
                while at < bytes.len() && identifier_byte(bytes[at]) {
                    at += 1;
                }
                if bytes[start] == b'$' {
                    TokenKind::SystemIdentifier
                } else if macro_quote {
                    TokenKind::MacroQuotedIdentifier
                } else {
                    TokenKind::Identifier
                }
            }
            b'0'..=b'9' => {
                while bytes
                    .get(at)
                    .is_some_and(|b| b.is_ascii_digit() || *b == b'_')
                {
                    at += 1;
                }
                if bytes.get(at) == Some(&b'\'')
                    && bytes.get(at + 1).is_some_and(|b| {
                        matches!(b.to_ascii_lowercase(), b's' | b'b' | b'o' | b'd' | b'h')
                    })
                {
                    at = based_number(bytes, at);
                } else {
                    if bytes.get(at) == Some(&b'.')
                        && bytes.get(at + 1).is_some_and(u8::is_ascii_digit)
                    {
                        at += 1;
                        while bytes
                            .get(at)
                            .is_some_and(|b| b.is_ascii_digit() || *b == b'_')
                        {
                            at += 1;
                        }
                    }
                    if bytes.get(at).is_some_and(|b| matches!(b, b'e' | b'E')) {
                        let mut exponent = at + 1;
                        if bytes
                            .get(exponent)
                            .is_some_and(|b| matches!(b, b'+' | b'-'))
                        {
                            exponent += 1;
                        }
                        if bytes.get(exponent).is_some_and(u8::is_ascii_digit) {
                            at = exponent + 1;
                            while bytes
                                .get(at)
                                .is_some_and(|b| b.is_ascii_digit() || *b == b'_')
                            {
                                at += 1;
                            }
                        }
                    }
                    for unit in ["ms", "us", "ns", "ps", "fs", "s"] {
                        if source[at..].starts_with(unit)
                            && !bytes
                                .get(at + unit.len())
                                .is_some_and(|b| identifier_byte(*b))
                        {
                            at += unit.len();
                            break;
                        }
                    }
                }
                TokenKind::Number
            }
            b'\''
                if bytes.get(at + 1).is_some_and(|b| {
                    matches!(
                        b,
                        b'0' | b'1'
                            | b'x'
                            | b'X'
                            | b'z'
                            | b'Z'
                            | b's'
                            | b'S'
                            | b'b'
                            | b'B'
                            | b'o'
                            | b'O'
                            | b'd'
                            | b'D'
                            | b'h'
                            | b'H'
                    )
                }) =>
            {
                at = if matches!(bytes[at + 1], b'0' | b'1' | b'x' | b'X' | b'z' | b'Z') {
                    at + 2
                } else {
                    based_number(bytes, at)
                };
                TokenKind::Number
            }
            b if b.is_ascii_graphic() => {
                const OPERATORS: &[&str] = &[
                    "<<<=", ">>>=", "<<<", ">>>", "===", "!==", "==?", "!=?", "<<=", ">>=", "->>",
                    "|->", "|=>", "<->", "&&&", "#-#", "#=#", "**", "++", "--", "&&", "||", "<=",
                    ">=", "==", "!=", "<<", ">>", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=",
                    "::", "+:", "-:", "~&", "~|", "~^", "^~", "->", "=>", "*>", "##", "@@", ":=",
                    ":/", ".*", "(*", "*)",
                ];
                let length = OPERATORS
                    .iter()
                    .find(|op| source[at..].starts_with(**op))
                    .map_or(1, |op| op.len());
                at += length;
                TokenKind::Symbol
            }
            _ if macro_quote => {
                at += source[at..].chars().next().unwrap().len_utf8();
                TokenKind::StringFragment
            }
            _ => {
                return Err(error(
                    sources,
                    file,
                    at,
                    "invalid character outside string or comment",
                ));
            }
        };
        tokens.push(Token {
            kind,
            text: source[start..at].to_owned(),
            origin: Origin {
                spelling: Span {
                    file,
                    start,
                    end: at,
                },
                expansion: None,
            },
            contributors: Vec::new(),
            disabled_macros: Vec::new(),
        });
    }
    Ok(tokens)
}

fn based_number(bytes: &[u8], apostrophe: usize) -> usize {
    let mut at = apostrophe + 1;
    if bytes.get(at).is_some_and(|b| matches!(b, b's' | b'S')) {
        at += 1;
    }
    if bytes
        .get(at)
        .is_some_and(|b| matches!(b.to_ascii_lowercase(), b'b' | b'o' | b'd' | b'h'))
    {
        at += 1;
    }
    let digits = at;
    while bytes.get(at).is_some_and(u8::is_ascii_whitespace) {
        at += 1;
    }
    if !bytes
        .get(at)
        .is_some_and(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'?'))
    {
        return digits;
    }
    while bytes
        .get(at)
        .is_some_and(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'?'))
    {
        at += 1;
    }
    at
}

fn identifier_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'$')
}

fn error(sources: &SourceMap, file: FileId, at: usize, message: &str) -> Error {
    sources.error(
        Origin {
            spelling: Span {
                file,
                start: at,
                end: at,
            },
            expansion: None,
        },
        message,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lossless_tokens_have_original_utf8_byte_locations() {
        let text = "/* λ */\r\nmodule \\module (input bit a); $display(\"λ\"); endmodule";
        let mut sources = SourceMap::default();
        let file = sources.add("original.sv", text);
        let tokens = lex(&sources, file).unwrap();
        assert_eq!(
            tokens.iter().map(|t| t.text.as_str()).collect::<String>(),
            text
        );
        for token in &tokens {
            assert_eq!(sources.text(token.origin.spelling), token.text);
        }
        let escaped = tokens
            .iter()
            .find(|t| t.kind == TokenKind::EscapedIdentifier)
            .unwrap();
        assert_eq!(escaped.identifier(), Some("module"));
        assert_eq!(
            sources
                .file(file)
                .line_column(escaped.origin.spelling.start),
            (2, 8)
        );
    }

    #[test]
    fn malformed_lexical_constructs_fail() {
        for text in ["/* unterminated", "\"unterminated", "\"line\nbreak\"", "λ"] {
            let mut sources = SourceMap::default();
            let file = sources.add("bad.sv", text);
            assert!(lex(&sources, file).is_err(), "{text}");
        }
    }

    #[test]
    fn strings_and_longest_operators_preserve_lexical_boundaries() {
        let text = "\"continued\\\r\nstring\" \"continued\\\nstring\" \"\"\"multi\nline\"\"\" <<<= >>>= |-> |=> ->> <->";
        let mut sources = SourceMap::default();
        let file = sources.add("lexical.sv", text);
        let tokens = lex(&sources, file).unwrap();
        let significant = tokens
            .iter()
            .filter(|token| !token.is_trivia())
            .collect::<Vec<_>>();
        assert!(
            significant[..3]
                .iter()
                .all(|token| token.kind == TokenKind::String)
        );
        assert_eq!(
            significant[3..]
                .iter()
                .map(|token| token.text.as_str())
                .collect::<Vec<_>>(),
            ["<<<=", ">>>=", "|->", "|=>", "->>", "<->"]
        );
    }
}
