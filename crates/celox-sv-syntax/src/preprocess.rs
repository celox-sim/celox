use fxhash::FxHashMap as HashMap;
use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
};

use crate::lexer::lex_quoted;
use crate::{Error, Expansion, Origin, SourceMap, Span, Token, TokenKind, lex};

const MAX_DEPTH: usize = 64;
// IEEE 1800-2023 22.4 requires at least 15 nested include levels.
const MAX_INCLUDE_DEPTH: usize = 32;
const MAX_CONTEXT_DEPTH: usize = MAX_INCLUDE_DEPTH + 1;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum IncludePolicy {
    /// Resolve relative to the including file, then the configured search paths.
    #[default]
    Read,
    /// Explicit compatibility with callers that provide compilation units
    /// independently and previously ignored `include directives.
    Ignore,
}

#[derive(Clone, Debug, Default)]
pub struct Options {
    pub includes: IncludePolicy,
    pub include_paths: Vec<PathBuf>,
    pub strip_comments: bool,
}

#[derive(Clone, Debug)]
struct Formal {
    name: String,
    default: Option<Vec<Token>>,
}

#[derive(Clone, Debug)]
struct Macro {
    formals: Option<Vec<Formal>>,
    body: Vec<Token>,
    definition: Option<Span>,
    builtin: Option<&'static str>,
}

#[derive(Clone, Debug, Default)]
pub struct Preprocessed {
    pub tokens: Vec<Token>,
    /// Active source observations, including define/undef directives that do
    /// not belong in the expanded parser stream. Conservative source-policy
    /// checks can inspect uninvoked definitions without changing parser tokens.
    pub observations: Vec<Token>,
}

impl Preprocessed {
    fn push(&mut self, token: Token) {
        self.observations.push(token.clone());
        self.tokens.push(token);
    }

    fn extend(&mut self, tokens: impl IntoIterator<Item = Token>) {
        for token in tokens {
            self.push(token);
        }
    }

    fn merge(&mut self, other: Self) {
        self.tokens.extend(other.tokens);
        self.observations.extend(other.observations);
    }

    /// Active text including source macro definitions and undefinitions. This
    /// is a conservative inspection view, not input for a second parser pass.
    pub fn inspection_text(&self) -> String {
        self.observations
            .iter()
            .map(|token| token.text.as_str())
            .collect()
    }

    /// A compatibility view for text-based consumers. Syntax consumers should
    /// use tokens and their original origins instead of parsing this text again.
    pub fn text(&self) -> String {
        self.tokens
            .iter()
            .map(|token| token.text.as_str())
            .collect()
    }
}

/// One compilation unit: macro definitions and source identities survive across
/// calls to `preprocess`, as required by IEEE 1800-2023 5.6.4 and 22.5.
#[derive(Clone, Debug)]
pub struct Preprocessor {
    pub sources: SourceMap,
    pub options: Options,
    defines: HashMap<String, Macro>,
    context_depth: usize,
}

impl Default for Preprocessor {
    fn default() -> Self {
        // IEEE 1800-2023 40.3.1. Predefined constants have no authored
        // definition; their tokens are anchored at the real invocation.
        let constants = [
            ("SV_COV_START", "0"),
            ("SV_COV_STOP", "1"),
            ("SV_COV_RESET", "2"),
            ("SV_COV_CHECK", "3"),
            ("SV_COV_MODULE", "10"),
            ("SV_COV_HIER", "11"),
            ("SV_COV_ASSERTION", "20"),
            ("SV_COV_FSM_STATE", "21"),
            ("SV_COV_STATEMENT", "22"),
            ("SV_COV_TOGGLE", "23"),
            ("SV_COV_OVERFLOW", "-2"),
            ("SV_COV_ERROR", "-1"),
            ("SV_COV_NOCOV", "0"),
            ("SV_COV_OK", "1"),
            ("SV_COV_PARTIAL", "2"),
        ];
        Self {
            sources: SourceMap::default(),
            options: Options::default(),
            context_depth: 0,
            defines: constants
                .into_iter()
                .map(|(name, value)| {
                    (
                        name.to_owned(),
                        Macro {
                            formals: None,
                            body: Vec::new(),
                            definition: None,
                            builtin: Some(value),
                        },
                    )
                })
                .collect(),
        }
    }
}

#[derive(Debug)]
struct Conditional {
    parent: bool,
    active: bool,
    taken: bool,
    saw_else: bool,
    origin: Origin,
}

impl Preprocessor {
    pub fn new(options: Options) -> Self {
        Self {
            options,
            ..Self::default()
        }
    }

    /// Define an empty command-line/tool macro without manufacturing a source
    /// location. Definitions authored in source always retain their real span.
    pub fn define(&mut self, name: impl Into<String>) {
        self.defines.insert(
            name.into(),
            Macro {
                formals: None,
                body: Vec::new(),
                definition: None,
                builtin: None,
            },
        );
    }

    pub fn is_defined(&self, name: &str) -> bool {
        self.defines.contains_key(name)
    }

    pub fn preprocess(
        &mut self,
        code: &str,
        path: impl AsRef<Path>,
    ) -> Result<Preprocessed, Error> {
        self.preprocess_file(code, path.as_ref(), 0)
    }

    fn preprocess_file(
        &mut self,
        code: &str,
        path: &Path,
        depth: usize,
    ) -> Result<Preprocessed, Error> {
        let file = self.sources.add(path, code);
        let tokens = lex(&self.sources, file)?;
        self.process(&tokens, depth)
    }

    fn process(&mut self, input: &[Token], include_depth: usize) -> Result<Preprocessed, Error> {
        if self.context_depth >= MAX_CONTEXT_DEPTH
            && let Some(first) = input.first()
        {
            return Err(self.error(
                first.origin,
                "recursive preprocessing context limit exceeded",
            ));
        }
        self.context_depth += 1;
        let result = self.process_inner(input, include_depth);
        self.context_depth -= 1;
        result
    }

    fn process_inner(
        &mut self,
        input: &[Token],
        include_depth: usize,
    ) -> Result<Preprocessed, Error> {
        // Replacements are pushed onto the front of a queue. Rescanning can
        // consume following original tokens without shifting the remaining
        // source for every macro invocation.
        let mut tokens: VecDeque<Token> = input.iter().cloned().collect();
        let mut output = Preprocessed::default();
        let mut conditions: Vec<Conditional> = Vec::new();
        while let Some(token) = tokens.pop_front() {
            let active = conditions.last().is_none_or(|condition| condition.active);
            if token.text != "`" {
                if active {
                    if matches!(token.text.as_str(), "``" | "`\"" | "`\"\"\"" | "`\\`\"") {
                        return Err(
                            self.error(token.origin, "macro operator outside a macro replacement")
                        );
                    }
                    if self.options.strip_comments && token.kind == TokenKind::Comment {
                        output.push(space(&token));
                    } else {
                        output.push(token);
                    }
                }
                continue;
            }
            let origin = token.origin;
            let Some(name_token) = tokens.pop_front() else {
                return Err(self.error(origin, "expected a directive or macro name after `"));
            };
            let Some(name) = name_token.identifier() else {
                return Err(self.error(origin, "expected a directive or macro name after `"));
            };
            let name = name.to_owned();
            match name.as_str() {
                "ifdef" | "ifndef" => {
                    let test = self.condition(&mut tokens, origin)?;
                    let test = if name == "ifndef" { !test } else { test };
                    conditions.push(Conditional {
                        parent: active,
                        active: active && test,
                        taken: test,
                        saw_else: false,
                        origin,
                    });
                }
                "elsif" => {
                    let test = self.condition(&mut tokens, origin)?;
                    let Some(condition) = conditions.last_mut() else {
                        return Err(self.error(origin, "`elsif without `ifdef"));
                    };
                    if condition.saw_else {
                        return Err(self.error(origin, "`elsif after `else"));
                    }
                    condition.active = condition.parent && !condition.taken && test;
                    condition.taken |= test;
                }
                "else" => {
                    let Some(condition) = conditions.last_mut() else {
                        return Err(self.error(origin, "`else without `ifdef"));
                    };
                    if condition.saw_else {
                        return Err(self.error(origin, "duplicate `else"));
                    }
                    condition.active = condition.parent && !condition.taken;
                    condition.taken = true;
                    condition.saw_else = true;
                }
                "endif" => {
                    if conditions.pop().is_none() {
                        return Err(self.error(origin, "`endif without `ifdef"));
                    }
                }
                "define" => {
                    let (line, original) = definition_line(&mut tokens);
                    if active {
                        self.define_source(&line, origin)?;
                        output.observations.extend([token, name_token]);
                        output.observations.extend(original);
                    }
                }
                "undef" => {
                    let mut original = vec![token, name_token];
                    while tokens.front().is_some_and(Token::is_trivia) {
                        original.push(tokens.pop_front().unwrap());
                    }
                    let Some(identifier) = tokens.pop_front() else {
                        return Err(self.error(origin, "expected macro name after `undef"));
                    };
                    let Some(name) = identifier.identifier() else {
                        return Err(self.error(origin, "expected macro name after `undef"));
                    };
                    if active {
                        self.defines.remove(name);
                        original.push(identifier);
                        output.observations.extend(original);
                    }
                }
                "undefineall" => {
                    if active {
                        self.defines.clear();
                        output.observations.extend([token, name_token]);
                    }
                }
                "include" => {
                    // A directive produced by a macro ends with that expansion,
                    // not with a later invocation on the same source line.
                    let line = if let Some(expansion) = origin.expansion {
                        let mut line = Vec::new();
                        while tokens
                            .front()
                            .is_some_and(|token| token.origin.expansion == Some(expansion))
                        {
                            line.push(tokens.pop_front().unwrap());
                        }
                        line
                    } else {
                        take_line(&mut tokens)
                    };
                    if active {
                        self.validate_include(&line, origin)?;
                    }
                    if active && self.options.includes == IncludePolicy::Read {
                        if include_depth >= MAX_INCLUDE_DEPTH {
                            return Err(self.error(origin, "include nesting limit exceeded"));
                        }
                        let expanded = self.process(&line, include_depth)?;
                        let argument = significant(&expanded.tokens);
                        let filename = match argument.as_slice() {
                            [token] if token.kind == TokenKind::String => {
                                Some(self.filename(token)?)
                            }
                            items
                                if items.first().is_some_and(|t| t.text == "<")
                                    && items.last().is_some_and(|t| t.text == ">") =>
                            {
                                Some(
                                    items[1..items.len() - 1]
                                        .iter()
                                        .map(|t| t.text.as_str())
                                        .collect(),
                                )
                            }
                            _ => None,
                        }
                        .ok_or_else(|| {
                            self.error(
                                origin,
                                "expected a quoted or angle-bracket include filename",
                            )
                        })?;
                        let including = &self.sources.file(origin.spelling.file).path;
                        let mut candidates =
                            vec![including.parent().unwrap_or(Path::new("")).join(&filename)];
                        candidates.extend(
                            self.options
                                .include_paths
                                .iter()
                                .map(|path| path.join(&filename)),
                        );
                        let Some(path) = candidates
                            .into_iter()
                            .find(|candidate| candidate.exists() && !candidate.is_dir())
                        else {
                            return Err(
                                self.error(origin, format!("include file `{filename}` not found"))
                            );
                        };
                        let code = std::fs::read_to_string(&path).map_err(|error| {
                            self.error(
                                origin,
                                format!("cannot read include `{}`: {error}", path.display()),
                            )
                        })?;
                        output.merge(self.preprocess_file(&code, &path, include_depth + 1)?);
                    } else if active {
                        output.push(space(&token));
                    }
                }
                "line" if active => {
                    let line_tokens = take_line(&mut tokens);
                    let span = self.sources.invocation(origin);
                    let source = &self.sources.file(span.file).text;
                    let line_start = source[..span.start].rfind('\n').map_or(0, |at| at + 1);
                    if !source[line_start..span.start]
                        .bytes()
                        .all(|b| b.is_ascii_whitespace())
                        || line_tokens.iter().any(|t| t.kind == TokenKind::Comment)
                    {
                        return Err(
                            self.error(origin, "only whitespace may share a `line directive line")
                        );
                    }
                    let args = significant(&line_tokens);
                    let [line, filename, level] = args.as_slice() else {
                        return Err(self.error(origin, "expected `line number \"filename\" level"));
                    };
                    let line = line
                        .text
                        .parse::<usize>()
                        .ok()
                        .filter(|value| *value > 0)
                        .ok_or_else(|| self.error(origin, "invalid `line number"))?;
                    if filename.kind != TokenKind::String
                        || !matches!(level.text.as_str(), "0" | "1" | "2")
                    {
                        return Err(self.error(origin, "invalid `line filename or level"));
                    }
                    let filename = self.filename(filename)?;
                    self.sources.set_line(
                        self.sources.invocation(origin),
                        line,
                        filename.into(),
                    )?;
                    output.extend([token, name_token]);
                    output.extend(line_tokens);
                }
                name if passthrough(name) => {
                    if active {
                        output.extend([token, name_token]);
                    }
                }
                "__FILE__" | "__LINE__" if active => {
                    let span = self.sources.invocation(origin);
                    let (path, line, _) = self.sources.logical_location(span);
                    let (kind, text) = if name == "__FILE__" {
                        (
                            TokenKind::String,
                            format!(
                                "\"{}\"",
                                path.to_string_lossy()
                                    .replace('\\', "\\\\")
                                    .replace('"', "\\\"")
                            ),
                        )
                    } else {
                        (TokenKind::Number, line.to_string())
                    };
                    output.push(Token {
                        kind,
                        text,
                        origin,
                        contributors: vec![origin],
                        disabled_macros: token.disabled_macros,
                    });
                }
                _ if !active => {}
                _ => {
                    let Some(definition) = self.defines.get(&name).cloned() else {
                        return Err(self.error(origin, format!("undefined macro `{name}`")));
                    };
                    if self.sources.expansion_depth(origin) >= MAX_DEPTH
                        || token.disabled_macros.contains(&name)
                    {
                        return Err(
                            self.error(origin, format!("recursive expansion of macro `{name}`"))
                        );
                    }
                    let expansion = self.sources.add_expansion(Expansion {
                        name: name.clone(),
                        invocation: origin,
                        definition: definition.definition,
                    });
                    if let Some(value) = definition.builtin {
                        let mut temporary = SourceMap::default();
                        let file = temporary.add("<predefined macro>", value);
                        for mut generated in
                            lex(&temporary, file).expect("coverage constants are valid tokens")
                        {
                            generated.origin = Origin {
                                spelling: origin.spelling,
                                expansion: Some(expansion),
                            };
                            generated.contributors = vec![origin];
                            output.push(generated);
                        }
                        continue;
                    }
                    let mut actuals = HashMap::default();
                    if let Some(formals) = &definition.formals {
                        let Some(opening) = take_nontrivia(&mut tokens) else {
                            return Err(self.error(
                                origin,
                                format!("macro `{name}` requires an argument list"),
                            ));
                        };
                        if opening.text != "(" {
                            return Err(self.error(
                                origin,
                                format!("macro `{name}` requires an argument list"),
                            ));
                        }
                        let mut group = vec![opening];
                        let mut nesting = 1;
                        while let Some(next) = tokens.pop_front() {
                            if next.text == "(" {
                                nesting += 1;
                            }
                            if next.text == ")" {
                                nesting -= 1;
                            }
                            group.push(next);
                            if nesting == 0 {
                                break;
                            }
                        }
                        let (arguments, _) = self.arguments(&group, 0, origin)?;
                        let arguments = if formals.is_empty()
                            && arguments.len() == 1
                            && significant(&arguments[0]).is_empty()
                        {
                            Vec::new()
                        } else {
                            arguments
                        };
                        if arguments.len() > formals.len() {
                            return Err(self
                                .error(origin, format!("too many arguments for macro `{name}`")));
                        }
                        for (index, formal) in formals.iter().enumerate() {
                            let argument = match arguments.get(index) {
                                Some(argument) if !significant(argument).is_empty() => {
                                    trimmed(argument).to_vec()
                                }
                                Some(_) => formal.default.clone().unwrap_or_default(),
                                None => formal.default.clone().ok_or_else(|| {
                                    self.error(
                                        origin,
                                        format!(
                                            "missing argument `{}` for macro `{name}`",
                                            formal.name
                                        ),
                                    )
                                })?,
                            };
                            actuals.insert(formal.name.clone(), argument);
                        }
                    }
                    let mut replaced = Vec::new();
                    for body_token in &definition.body {
                        if let Some(argument) = body_token
                            .identifier()
                            .and_then(|identifier| actuals.get(identifier))
                        {
                            replaced.extend(argument.iter().cloned().map(|mut actual| {
                                actual.origin.expansion = Some(expansion);
                                actual
                                    .disabled_macros
                                    .extend(token.disabled_macros.iter().cloned());
                                actual
                            }));
                        } else {
                            let mut body_token = body_token.clone();
                            body_token.origin.expansion = Some(expansion);
                            if body_token.kind == TokenKind::String
                                && !body_token.text.starts_with("\"\"\"")
                            {
                                let text =
                                    body_token.text.replace("\\\r\n", "").replace("\\\n", "");
                                if text != body_token.text {
                                    body_token.contributors.push(body_token.origin);
                                    body_token.text = text;
                                }
                            }
                            body_token.disabled_macros = token.disabled_macros.clone();
                            body_token.disabled_macros.push(name.clone());
                            replaced.push(body_token);
                        }
                    }
                    let replaced = self.paste(replaced)?;
                    let quoted = self.stringify(replaced, include_depth)?;
                    for token in quoted.into_iter().rev() {
                        tokens.push_front(token);
                    }
                }
            }
        }
        if let Some(condition) = conditions.first() {
            return Err(self.error(
                condition.origin,
                "unterminated conditional compilation directive",
            ));
        }
        Ok(output)
    }

    fn validate_include(&self, line: &[Token], origin: Origin) -> Result<(), Error> {
        let significant = significant(line);
        let valid = match significant.as_slice() {
            [token] if token.kind == TokenKind::String => !token.text.starts_with("\"\"\""),
            items if items.first().is_some_and(|t| t.text == "<") => {
                items.len() > 2
                    && items.last().is_some_and(|t| t.text == ">")
                    && !items[1..items.len() - 1]
                        .iter()
                        .any(|t| matches!(t.text.as_str(), "<" | ">"))
            }
            [tick, name, rest @ ..] if tick.text == "`" && name.identifier().is_some() => {
                if rest.is_empty() {
                    true
                } else if rest[0].text == "(" {
                    let opening = line.iter().position(|t| std::ptr::eq(t, rest[0])).unwrap();
                    let (_, end) = self.arguments(line, opening, origin)?;
                    trimmed(&line[end..]).is_empty()
                } else {
                    false
                }
            }
            _ => false,
        };
        if valid {
            Ok(())
        } else {
            Err(self.error(
                origin,
                "expected only an include filename or filename macro on the directive line",
            ))
        }
    }

    fn filename(&self, token: &Token) -> Result<String, Error> {
        let text = token
            .text
            .strip_prefix('"')
            .and_then(|t| t.strip_suffix('"'))
            .filter(|_| !token.text.starts_with("\"\"\""))
            .ok_or_else(|| self.error(token.origin, "expected a quoted filename"))?;
        let mut chars = text.chars().peekable();
        let mut result = String::new();
        while let Some(ch) = chars.next() {
            if ch != '\\' {
                result.push(ch);
                continue;
            }
            let Some(escaped) = chars.next() else {
                return Err(self.error(token.origin, "unterminated filename escape"));
            };
            let ch = match escaped {
                '\n' => continue,
                '\r' if chars.peek() == Some(&'\n') => {
                    chars.next();
                    continue;
                }
                'n' => '\n',
                't' => '\t',
                'v' => '\u{b}',
                'f' => '\u{c}',
                'a' => '\u{7}',
                '0'..='7' | 'x' => {
                    let radix = if escaped == 'x' { 16 } else { 8 };
                    let mut digits = if escaped == 'x' {
                        String::new()
                    } else {
                        escaped.to_string()
                    };
                    let limit = if radix == 16 { 2 } else { 3 };
                    while digits.len() < limit && chars.peek().is_some_and(|ch| ch.is_digit(radix))
                    {
                        digits.push(chars.next().unwrap());
                    }
                    let value = u8::from_str_radix(&digits, radix)
                        .map_err(|_| self.error(token.origin, "invalid filename escape"))?;
                    if !value.is_ascii() {
                        return Err(self.error(
                            token.origin,
                            "non-ASCII byte escape in filename is not supported",
                        ));
                    }
                    char::from(value)
                }
                ch => ch,
            };
            if ch == '\0' {
                return Err(self.error(token.origin, "NUL in filename"));
            }
            result.push(ch);
        }
        Ok(result)
    }

    fn define_source(&mut self, line: &[Token], origin: Origin) -> Result<(), Error> {
        let start = skip_trivia(line, 0);
        let Some(token) = line.get(start) else {
            return Err(self.error(origin, "expected macro name after `define"));
        };
        let Some(name) = token.identifier() else {
            return Err(self.error(token.origin, "invalid macro name"));
        };
        if directive(name) || matches!(name, "__FILE__" | "__LINE__") {
            return Err(self.error(
                token.origin,
                "cannot redefine a compiler directive or predefined macro",
            ));
        }
        let mut at = start + 1;
        if token.kind == TokenKind::EscapedIdentifier
            && line
                .get(at)
                .is_some_and(|t| t.kind == TokenKind::Whitespace && t.text.len() == 1)
        {
            at += 1;
        }
        let formals = if line.get(at).is_some_and(|t| t.text == "(") {
            let (arguments, next) = self.arguments(line, at, origin)?;
            at = next;
            let mut formals: Vec<Formal> = Vec::new();
            if !(arguments.len() == 1 && significant(&arguments[0]).is_empty()) {
                for argument in arguments {
                    let items = significant(&argument);
                    let Some(token) = items.first() else {
                        return Err(self.error(origin, "empty macro formal argument"));
                    };
                    if token.kind != TokenKind::Identifier {
                        return Err(
                            self.error(token.origin, "macro formal must be a simple identifier")
                        );
                    }
                    let name = token.text.clone();
                    if formals.iter().any(|formal| formal.name == name) {
                        return Err(self.error(token.origin, "duplicate macro formal argument"));
                    }
                    let default = if items.len() == 1 {
                        None
                    } else {
                        if items[1].text != "=" {
                            return Err(
                                self.error(items[1].origin, "expected = in macro formal argument")
                            );
                        }
                        let equals = argument.iter().position(|t| t.text == "=").unwrap();
                        Some(trimmed(&argument[equals + 1..]).to_vec())
                    };
                    formals.push(Formal { name, default });
                }
            }
            Some(formals)
        } else {
            None
        };
        let body = trimmed(&line[at..]).to_vec();
        self.defines.insert(
            name.to_owned(),
            Macro {
                formals,
                body,
                definition: Some(token.origin.spelling),
                builtin: None,
            },
        );
        Ok(())
    }

    fn arguments(
        &self,
        tokens: &[Token],
        opening: usize,
        origin: Origin,
    ) -> Result<(Vec<Vec<Token>>, usize), Error> {
        let mut arguments = Vec::new();
        let mut argument = Vec::new();
        let mut delimiters = vec![")"];
        let mut at = opening + 1;
        while let Some(token) = tokens.get(at) {
            match token.text.as_str() {
                "(" => delimiters.push(")"),
                "[" => delimiters.push("]"),
                "{" => delimiters.push("}"),
                ")" | "]" | "}" => {
                    if delimiters.pop() != Some(token.text.as_str()) {
                        return Err(
                            self.error(token.origin, "unbalanced delimiter in macro arguments")
                        );
                    }
                    if delimiters.is_empty() {
                        arguments.push(argument);
                        return Ok((arguments, at + 1));
                    }
                }
                "," if delimiters.len() == 1 => {
                    arguments.push(std::mem::take(&mut argument));
                    at += 1;
                    continue;
                }
                _ => {}
            }
            argument.push(token.clone());
            at += 1;
        }
        Err(self.error(origin, "unterminated macro argument list"))
    }

    fn condition(&self, tokens: &mut VecDeque<Token>, origin: Origin) -> Result<bool, Error> {
        let Some(token) = take_nontrivia(tokens) else {
            return Err(self.error(origin, "expected conditional macro name"));
        };
        if token.text == "(" {
            return Err(self.error(
                token.origin,
                "SystemVerilog-2023 conditional macro expressions are not supported yet",
            ));
        }
        let Some(name) = token.identifier() else {
            return Err(self.error(token.origin, "expected conditional macro name"));
        };
        Ok(self.defines.contains_key(name))
    }

    fn paste(&mut self, tokens: Vec<Token>) -> Result<Vec<Token>, Error> {
        let mut result: Vec<Token> = Vec::new();
        let mut at = 0;
        let mut quoted = false;
        while at < tokens.len() {
            if tokens[at].text != "``" {
                if matches!(tokens[at].text.as_str(), "`\"" | "`\"\"\"") {
                    quoted = !quoted;
                }
                result.push(tokens[at].clone());
                at += 1;
                continue;
            }
            // `` only removes the delimiter, not surrounding authored spaces.
            if result.last().is_some_and(|token| !token.is_trivia())
                && tokens
                    .get(at + 1)
                    .is_some_and(|token| !token.is_trivia() && token.text != "``")
            {
                let left = result.pop().unwrap();
                let right = &tokens[at + 1];
                let text = format!("{}{}", left.text, right.text);
                let mut temporary = SourceMap::default();
                let file = temporary.add("<token paste>", text.as_str());
                let pasted = if quoted {
                    lex_quoted(&temporary, file)
                } else {
                    lex(&temporary, file)
                }
                .map_err(|error| {
                    self.error(
                        tokens[at].origin,
                        format!("invalid token paste: {}", error.message),
                    )
                })?;
                for mut pasted in pasted {
                    if matches!(pasted.text.as_str(), "`\"" | "`\"\"\"") {
                        quoted = !quoted;
                    }
                    let anchor = if pasted.origin.spelling.start < left.text.len() {
                        &left
                    } else {
                        right
                    };
                    pasted.origin = anchor.origin;
                    pasted.contributors = vec![left.origin, right.origin];
                    pasted
                        .contributors
                        .extend(left.contributors.iter().chain(&right.contributors).copied());
                    pasted.disabled_macros = left.disabled_macros.clone();
                    pasted
                        .disabled_macros
                        .extend(right.disabled_macros.iter().cloned());
                    result.push(pasted);
                }
                at += 2;
            } else {
                at += 1;
            }
        }
        Ok(result)
    }

    fn stringify(&mut self, tokens: Vec<Token>, include_depth: usize) -> Result<Vec<Token>, Error> {
        let mut result = Vec::new();
        let mut at = 0;
        while at < tokens.len() {
            if tokens[at].text == "`\"\"\"" {
                return Err(self.error(
                    tokens[at].origin,
                    "triple-quoted macro stringification is not supported yet",
                ));
            }
            if tokens[at].text == "`\\`\"" {
                return Err(self.error(
                    tokens[at].origin,
                    "macro escaped quote outside stringification",
                ));
            }
            if tokens[at].text != "`\"" {
                result.push(tokens[at].clone());
                at += 1;
                continue;
            }
            let opening = &tokens[at];
            let start = at + 1;
            at = start;
            while at < tokens.len() && tokens[at].text != "`\"" {
                at += 1;
            }
            if at == tokens.len() {
                return Err(self.error(opening.origin, "unterminated macro stringification"));
            }
            let content = tokens[start..at]
                .iter()
                .cloned()
                .map(|mut token| {
                    if token.text == "`\\`\"" {
                        token.text = "\\\"".to_owned();
                    }
                    token
                })
                .collect::<Vec<_>>();
            let expanded = self.process(&content, include_depth)?;
            let text: String = expanded
                .tokens
                .iter()
                .filter(|token| token.kind != TokenKind::Newline || token.contributors.is_empty())
                .map(|t| t.text.as_str())
                .collect();
            if text.contains('\n') || text.contains('\r') {
                return Err(self.error(opening.origin, "newline in macro stringification"));
            }
            let quoted = format!("\"{text}\"");
            let mut temporary = SourceMap::default();
            let file = temporary.add("<macro stringification>", quoted.as_str());
            let checked = lex(&temporary, file).map_err(|error| {
                self.error(
                    opening.origin,
                    format!("invalid macro stringification: {}", error.message),
                )
            })?;
            if checked.len() != 1 || checked[0].kind != TokenKind::String {
                return Err(self.error(
                    opening.origin,
                    "macro stringification does not form a string literal",
                ));
            }
            result.push(Token {
                kind: TokenKind::String,
                text: quoted,
                origin: opening.origin,
                contributors: expanded
                    .tokens
                    .iter()
                    .flat_map(|t| std::iter::once(t.origin).chain(t.contributors.iter().copied()))
                    .collect(),
                disabled_macros: opening.disabled_macros.clone(),
            });
            at += 1;
        }
        Ok(result)
    }

    fn error(&self, origin: Origin, message: impl Into<String>) -> Error {
        self.sources.error(origin, message)
    }
}

fn skip_trivia(tokens: &[Token], mut at: usize) -> usize {
    while tokens.get(at).is_some_and(Token::is_trivia) {
        at += 1;
    }
    at
}
fn significant(tokens: &[Token]) -> Vec<&Token> {
    tokens.iter().filter(|t| !t.is_trivia()).collect()
}
fn trimmed(tokens: &[Token]) -> &[Token] {
    let start = skip_trivia(tokens, 0);
    let end = tokens
        .iter()
        .rposition(|t| !t.is_trivia())
        .map_or(start, |i| i + 1);
    &tokens[start..end]
}
fn space(token: &Token) -> Token {
    Token {
        text: " ".to_owned(),
        kind: TokenKind::Whitespace,
        origin: token.origin,
        contributors: std::iter::once(token.origin)
            .chain(token.contributors.iter().copied())
            .collect(),
        disabled_macros: token.disabled_macros.clone(),
    }
}
fn take_nontrivia(tokens: &mut VecDeque<Token>) -> Option<Token> {
    while tokens.front().is_some_and(Token::is_trivia) {
        tokens.pop_front();
    }
    tokens.pop_front()
}

fn take_line(tokens: &mut VecDeque<Token>) -> Vec<Token> {
    let mut line = Vec::new();
    while tokens.front().is_some_and(|t| t.kind != TokenKind::Newline) {
        line.push(tokens.pop_front().unwrap());
    }
    line
}

fn definition_line(tokens: &mut VecDeque<Token>) -> (Vec<Token>, Vec<Token>) {
    let mut line = Vec::new();
    let mut original = Vec::new();
    while let Some(token) = tokens.front() {
        if token.kind == TokenKind::Newline && !line.last().is_some_and(|t: &Token| t.text == "\\")
        {
            break;
        }
        let token = tokens.pop_front().unwrap();
        original.push(token.clone());
        if token.kind == TokenKind::Newline {
            let slash = line.pop().unwrap();
            let mut newline = token;
            newline.contributors.push(slash.origin);
            line.push(newline);
        } else if token.kind == TokenKind::Comment {
            let continuation = token.text.starts_with("//") && token.text.ends_with('\\');
            line.push(space(&token));
            if continuation {
                let mut slash = token;
                slash.text = "\\".to_owned();
                slash.kind = TokenKind::Symbol;
                line.push(slash);
            }
        } else {
            line.push(token);
        }
    }
    (line, original)
}

fn passthrough(name: &str) -> bool {
    matches!(
        name,
        "timescale"
            | "default_nettype"
            | "resetall"
            | "celldefine"
            | "endcelldefine"
            | "unconnected_drive"
            | "nounconnected_drive"
            | "begin_keywords"
            | "end_keywords"
            | "line"
            | "pragma"
            | "default_decay_time"
            | "default_trireg_strength"
            | "delay_mode_distributed"
            | "delay_mode_path"
            | "delay_mode_unit"
            | "delay_mode_zero"
            | "uselib"
            | "accelerate"
            | "noaccelerate"
            | "expand_vectornets"
            | "noexpand_vectornets"
            | "autoexpand_vectornets"
            | "remove_gatenames"
            | "noremove_gatenames"
            | "remove_netnames"
            | "noremove_netnames"
            | "protect"
            | "endprotect"
            | "protected"
            | "endprotected"
    )
}
fn directive(name: &str) -> bool {
    passthrough(name)
        || matches!(
            name,
            "define"
                | "undef"
                | "undefineall"
                | "include"
                | "ifdef"
                | "ifndef"
                | "elsif"
                | "else"
                | "endif"
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preprocess(code: &str) -> String {
        Preprocessor::default()
            .preprocess(code, "original.sv")
            .unwrap()
            .text()
    }

    fn words(code: &str) -> String {
        Preprocessor::default()
            .preprocess(code, "original.sv")
            .unwrap()
            .tokens
            .iter()
            .filter(|token| !token.is_trivia())
            .map(|t| t.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn macro_generated_includes_end_at_each_invocation() {
        let mut pp = Preprocessor::new(Options {
            includes: IncludePolicy::Ignore,
            ..Default::default()
        });
        let output = pp.preprocess(
            "`define INCLUDE(F) `include F\n`INCLUDE(\"one.svh\") `INCLUDE(\"two.svh\")\nmodule M; endmodule",
            "macro-include.sv",
        ).unwrap();
        assert!(output.text().contains("module M; endmodule"));
    }

    #[cfg(unix)]
    #[test]
    fn includes_accept_readable_special_files() {
        let mut pp = Preprocessor::default();
        let output = pp
            .preprocess("`include \"/dev/null\"\nmodule M; endmodule", "include.sv")
            .unwrap();
        assert!(output.text().contains("module M; endmodule"));
    }

    #[test]
    fn definitions_persist_and_inactive_directives_do_not_execute() {
        let mut pp = Preprocessor::default();
        pp.define("TOOL");
        pp.preprocess("`define SHARED 17\n", "first.sv").unwrap();
        let output = pp.preprocess("`ifdef TOOL\n`ifndef SHARED\nbad\n`elsif SHARED\n`SHARED\n`else\nbad\n`endif\n`else\n`undef SHARED\n`define HIDDEN 1\n`include \"missing\"\n`endif\n", "second.sv").unwrap();
        assert_eq!(output.text().trim(), "17");
        assert!(pp.is_defined("SHARED"));
        assert!(!pp.is_defined("HIDDEN"));
        pp.preprocess("`undef SHARED\n`undefineall\n", "third.sv")
            .unwrap();
        assert!(!pp.is_defined("TOOL"));
        assert!(!pp.is_defined("SHARED"));
    }

    #[test]
    fn arguments_defaults_and_nested_delimiters_follow_22_5_1() {
        let definition = "`define M(a=5,b,c=\"C\") a+b+c\n";
        assert_eq!(words(&format!("{definition}`M(1,,3)")), "1 + + 3");
        assert_eq!(words(&format!("{definition}`M(,2,)")), "5 + 2 + \"C\"");
        assert_eq!(words(&format!("{definition}`M(,2)")), "5 + 2 + \"C\"");
        assert_eq!(words("`define M(a,b=) a+b\n`M(1)"), "1 +");
        assert_eq!(
            words("`define M(a,b) a+b\n`M({1,2},f(3,[4,5]))"),
            "{ 1 , 2 } + f ( 3 , [ 4 , 5 ] )"
        );
        assert_eq!(
            words("`define M(a) a\n`M(\\name,with)paren )"),
            "\\name,with)paren"
        );
        for usage in ["`M(1)", "`M()", "`M(1,2,3)", "`M", "`M(1,2]"] {
            assert!(
                Preprocessor::default()
                    .preprocess(&format!("`define M(a,b) a+b\n{usage}"), "bad.sv")
                    .is_err(),
                "{usage}"
            );
        }
        // Whitespace before a definition's '(' makes an object-like macro;
        // whitespace before an invocation's '(' is allowed.
        assert_eq!(words("`define M (a)\n`M"), "( a )");
        assert_eq!(words("`define M(a) a\n`M (2)"), "2");
        assert_eq!(words("`define \\M@CRO (a=5) a\n`\\M@CRO ( )"), "5");
    }

    #[test]
    fn nested_expansion_alias_rescan_and_recursion() {
        assert_eq!(
            words("`define TOP(a,b) a+b\n`TOP(`TOP(b,1),`TOP(42,a))"),
            "b + 1 + 42 + a"
        );
        assert_eq!(
            words("`define ADD(x) x+1\n`define ALIAS `ADD\n`ALIAS(2) `ALIAS(3)"),
            "2 + 1 3 + 1"
        );
        assert_eq!(
            words("`define A `B\n`define B 4\n`A\n`define B 7\n`A"),
            "4 7"
        );
        for code in [
            "`define A `A\n`A",
            "`define A `B\n`define B `A\n`A",
            "`MISSING",
            "`define M(x=`M()) x\n`M()",
            "`define M(x=`N()) x\n`define N(x=`M()) x\n`M()",
            "`define Q(x=`Q()) `\"x`\"\n`Q()",
        ] {
            assert!(
                Preprocessor::default().preprocess(code, "bad.sv").is_err(),
                "{code}"
            );
        }
        assert_eq!(words("`define M(x=4) x\n`M(`M())"), "4");
    }

    #[test]
    fn comments_strings_continuations_and_macro_operators() {
        assert_eq!(
            words("`define A 1 /* ignored\n comment */ + \\\n2 // ignored\n`A"),
            "1 + 2"
        );
        assert_eq!(words("`define A 1 // ignored \\\n+2\n`A"), "1 + 2");
        assert_eq!(
            words("`define H(x) \"Hello x `X\"\n`H(world)"),
            "\"Hello x `X\""
        );
        assert_eq!(
            words("`define append(f) f``_primary\n`append(clock)"),
            "clock_primary"
        );
        assert_eq!(
            words("`define QUOTE(x) `\"x`\"\n`QUOTE(hello world)"),
            "\"hello world\""
        );
        assert_eq!(
            words("`define HI hello\n`define Q(x) `\"`HI x`\"\n`Q(world)"),
            "\"hello world\""
        );
        assert_eq!(
            words("`define msg(x,y) `\"x: `\\`\"y`\\`\"`\"\n`msg(left side,right side)"),
            "\"left side: \\\"right side\\\"\""
        );
        assert_eq!(words("`define M(x) x``+\n`M(foo) 1"), "foo + 1");
        assert_eq!(
            words("`define S \"continued\\\nstring\"\n`S"),
            "\"continuedstring\""
        );
        assert_eq!(
            words("`define S \"continued\\\r\nstring\"\r\n`S"),
            "\"continuedstring\""
        );
        let code = "// retained\nmodule/**/Top; /* retained */ endmodule";
        assert_eq!(preprocess(code), code);
        let mut pp = Preprocessor::new(Options {
            strip_comments: true,
            ..Options::default()
        });
        let output = pp.preprocess(code, "comments.sv").unwrap().text();
        assert!(!output.contains("retained"));
        assert!(output.contains("module Top"));
    }

    #[test]
    fn original_spelling_invocation_and_derived_contributors_survive() {
        let code = "`define INNER(x) x``_suffix\n`define OUTER(x) `INNER(x)\nmodule Top; wire `OUTER(clock); endmodule";
        let mut pp = Preprocessor::default();
        let output = pp.preprocess(code, "authored.sv").unwrap();
        let token = output
            .tokens
            .iter()
            .find(|t| t.text == "clock_suffix")
            .unwrap();
        assert_eq!(pp.sources.text(token.origin.spelling), "clock");
        assert!(token.contributors.len() >= 2);
        assert_eq!(pp.sources.text(pp.sources.invocation(token.origin)), "`");
        let inner = pp.sources.expansion(token.origin.expansion.unwrap());
        assert_eq!(inner.name, "INNER");
        let outer = pp.sources.expansion(inner.invocation.expansion.unwrap());
        assert_eq!(outer.name, "OUTER");
        assert_eq!(
            pp.sources.file(token.origin.spelling.file).path,
            Path::new("authored.sv")
        );
    }

    #[test]
    fn line_directives_preserve_physical_spans_and_supply_logical_builtins() {
        let mut pp = Preprocessor::default();
        let output = pp
            .preprocess(
                "`line 90 \"generated.sv\" 0\n`__FILE__ `__LINE__\n",
                "physical.sv",
            )
            .unwrap();
        let tokens = significant(&output.tokens);
        assert_eq!(tokens[tokens.len() - 2].text, "\"generated.sv\"");
        let line = tokens[tokens.len() - 1];
        assert_eq!(line.text, "90");
        assert_eq!(
            pp.sources.file(line.origin.spelling.file).path,
            Path::new("physical.sv")
        );
        assert_eq!(
            pp.sources
                .file(line.origin.spelling.file)
                .line_column(line.origin.spelling.start)
                .0,
            2
        );
        let error = pp
            .preprocess("`line 40 \"logical.sv\" 0\n`MISSING", "physical.sv")
            .unwrap_err();
        assert_eq!((error.path, error.line), (PathBuf::from("logical.sv"), 40));
        let output = pp
            .preprocess("`line 5 \"a\\\\b\\\"c.sv\" 0\n`__FILE__", "physical.sv")
            .unwrap();
        assert_eq!(
            significant(&output.tokens).last().unwrap().text,
            "\"a\\\\b\\\"c.sv\""
        );
        let output = pp
            .preprocess(
                &format!("`line {} \"max.sv\" 0\n`__LINE__", usize::MAX),
                "physical.sv",
            )
            .unwrap();
        assert_eq!(
            significant(&output.tokens).last().unwrap().text,
            usize::MAX.to_string()
        );
        assert!(
            pp.preprocess(
                &format!("`line {} \"overflow.sv\" 0\n\n`__LINE__", usize::MAX),
                "physical.sv"
            )
            .unwrap_err()
            .message
            .contains("supported range")
        );
        for source in [
            "`line 1 \"x.sv\" 0 // comment\n",
            "/* comment */ `line 1 \"x.sv\" 0\n",
            "module Top; `line 1 \"x.sv\" 0\n",
        ] {
            assert!(
                pp.preprocess(source, "physical.sv")
                    .unwrap_err()
                    .message
                    .contains("only whitespace")
            );
        }
    }

    #[test]
    fn malformed_or_unsupported_preprocessing_fails_explicitly() {
        for source in [
            "`ifdef X\n",
            "`endif",
            "`else",
            "`ifdef X\n`else\n`else\n`endif",
            "`ifdef X\n`else\n`elsif Y\n`endif",
            "`define define 1",
            "`define M(x,x) x",
            "`ifdef (X && Y)\n`endif",
            "`define Q(x) `\"x\n`Q(a)",
            "`ifdef UNDEFINED\n/* unterminated",
        ] {
            assert!(
                Preprocessor::default()
                    .preprocess(source, "bad.sv")
                    .is_err(),
                "{source}"
            );
        }
    }

    #[test]
    fn observations_retain_active_definitions_without_parser_tokens() {
        let mut pp = Preprocessor::default();
        let result = pp
            .preprocess(
                "`define UNUSED timeunit 1ns; // retained\nmodule Top; endmodule\n`undef UNUSED\n",
                "observed.sv",
            )
            .unwrap();
        assert!(!result.tokens.iter().any(|t| t.text == "timeunit"));
        assert!(result.observations.iter().any(|t| t.text == "timeunit"));
        assert!(result.inspection_text().contains("// retained"));
        assert!(result.inspection_text().contains("`undef UNUSED"));
        assert_eq!(
            words("`SV_COV_START `SV_COV_HIER `SV_COV_OVERFLOW"),
            "0 11 - 2"
        );
    }

    #[test]
    fn macro_quotes_preserve_string_characters_and_keep_lexical_scope() {
        assert_eq!(words("`define Q(x) `\"λ x`\"\n`Q(world)"), "\"λ world\"");
        assert_eq!(words("`define Q(x) `\"λ``x`\"\n`Q(world)"), "\"λworld\"");
        assert_eq!(
            words("`define Q(x) `\"/*x*/ x`\"\n`Q(world)"),
            "\"/*world*/ world\""
        );
        assert_eq!(
            words("`define Q(x) `\"λ \\\nx`\"\n`Q(world)"),
            "\"λ world\""
        );
        assert_eq!(
            words("`define Q(x) `\"λ \\\r\nx`\"\r\n`Q(world)"),
            "\"λ world\""
        );
        let mut pp = Preprocessor::default();
        assert!(
            pp.preprocess("`define UNUSED `\"unclosed\nmodule λ; endmodule", "bad.sv")
                .is_err()
        );
        assert!(
            pp.preprocess(
                "`define UNUSED `\"unclosed\nmodule Top; endmodule",
                "unused.sv"
            )
            .is_ok()
        );
        assert!(
            pp.preprocess("`define Q(x) `\"unclosed\n`Q(world)", "bad.sv")
                .is_err()
        );
    }

    #[test]
    fn includes_resolve_relative_and_search_paths_with_persistent_macros() {
        let directory = std::env::temp_dir().join(format!(
            "celox-sv-includes-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(directory.join("search")).unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(directory.clone());
        std::fs::write(
            directory.join("first.svh"),
            "`define VALUE 19\n`include <second.svh>\n",
        )
        .unwrap();
        std::fs::write(
            directory.join("search/second.svh"),
            "wire included = `VALUE;\n",
        )
        .unwrap();
        let code = "`define HEADER `\"first.svh`\"\n`include `HEADER\n`VALUE\n";
        let mut pp = Preprocessor::new(Options {
            include_paths: vec![directory.join("search")],
            ..Options::default()
        });
        let output = pp.preprocess(code, directory.join("top.sv")).unwrap();
        assert!(output.text().contains("wire included = 19;"));
        assert_eq!(output.tokens.iter().filter(|t| t.text == "19").count(), 2);
        let wire = output.tokens.iter().find(|t| t.text == "wire").unwrap();
        assert_eq!(
            pp.sources.file(wire.origin.spelling.file).path,
            directory.join("search/second.svh")
        );
        assert!(
            pp.preprocess("`include \"missing.svh\"\n", directory.join("top.sv"))
                .is_err()
        );
        std::fs::write(
            directory.join("recursive.svh"),
            "`include \"recursive.svh\"\n",
        )
        .unwrap();
        assert!(
            pp.preprocess("`include \"recursive.svh\"\n", directory.join("top.sv"))
                .unwrap_err()
                .message
                .contains("nesting limit")
        );
        // Explicit Ignore policy performs no I/O and cannot leak active time
        // directives/macros from a header into either existing consumer.
        std::fs::write(
            directory.join("time.svh"),
            "`timescale 10ns/1ps\n`define HIDDEN 1\n",
        )
        .unwrap();
        let mut ignored = Preprocessor::new(Options {
            includes: IncludePolicy::Ignore,
            ..Options::default()
        });
        let output = ignored
            .preprocess(
                "`include \"time.svh\"\n`include \"missing.svh\"\nmodule Top; endmodule",
                directory.join("top.sv"),
            )
            .unwrap();
        assert!(!output.text().contains("timescale"));
        assert!(!ignored.is_defined("HIDDEN"));
        for source in [
            "`include \"time.svh\" module Hidden; endmodule",
            "`include",
            "`include 1",
            "`include `HEADER extra",
        ] {
            assert!(
                ignored
                    .preprocess(source, directory.join("top.sv"))
                    .is_err(),
                "{source}"
            );
        }
        // Header guards permit including the same physical file recursively
        // when the nested conditional suppresses the repeated include.
        std::fs::write(
            directory.join("guarded.svh"),
            "`ifndef GUARD\n`define GUARD\n`include \"guarded.svh\"\nwire guarded;\n`endif\n",
        )
        .unwrap();
        assert!(
            pp.preprocess("`include \"guarded.svh\"\n", directory.join("top.sv"))
                .unwrap()
                .text()
                .contains("wire guarded;")
        );
    }
}
