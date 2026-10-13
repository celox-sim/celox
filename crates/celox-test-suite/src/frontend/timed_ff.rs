//! Keep FF listeners armed during setup, deferring their actions until startup.
use super::*;

fn trivia(source: &str, at: usize) -> usize {
    source.len() - after_trivia(&source[at..]).len()
}
fn skip_token(source: &str, at: &mut usize) -> bool {
    let bytes = source.as_bytes();
    if bytes.get(*at..*at + 2) == Some(b"//") {
        *at = source[*at..]
            .find('\n')
            .map_or(bytes.len(), |end| *at + end);
    } else if bytes.get(*at..*at + 2) == Some(b"/*") {
        *at = source[*at + 2..]
            .find("*/")
            .map_or(bytes.len(), |end| *at + end + 4);
    } else if bytes.get(*at) == Some(&b'"') {
        *at += 1;
        while *at < bytes.len() {
            match bytes[*at] {
                b'\\' => *at = (*at + 2).min(bytes.len()),
                b'"' => {
                    *at += 1;
                    break;
                }
                _ => *at += 1,
            }
        }
    } else if bytes.get(*at) == Some(&b'\\') {
        while *at < bytes.len() && !bytes[*at].is_ascii_whitespace() {
            *at += 1;
        }
    } else {
        return false;
    }
    true
}
fn parens(source: &str, begin: usize) -> Option<usize> {
    if source.as_bytes().get(begin) != Some(&b'(') {
        return None;
    }
    let mut at = begin;
    let mut depth = 0;
    while at < source.len() {
        if skip_token(source, &mut at) {
            continue;
        }
        match source.as_bytes()[at] {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(at + 1);
                }
            }
            _ => {}
        }
        at += 1;
    }
    None
}
fn statement_end(source: &str, at: usize) -> Option<usize> {
    let at = trivia(source, at);
    if source[at..].starts_with("(*") {
        return statement_end(source, parens(source, at)?);
    }
    let words = sv_words(&source[at..]);
    let keyword = words.first().filter(|word| word.0 == 0).map(|word| word.2);
    match keyword {
        Some("unique" | "unique0" | "priority") => statement_end(source, at + words.first()?.1),
        Some("begin" | "case" | "casez" | "casex") => {
            let is_block = keyword == Some("begin");
            let mut depth = 0;
            for (_, end, word) in &words {
                if (is_block && *word == "begin")
                    || (!is_block && matches!(*word, "case" | "casez" | "casex"))
                {
                    depth += 1;
                }
                if (is_block && *word == "end") || (!is_block && *word == "endcase") {
                    depth -= 1;
                    if depth == 0 {
                        let end = at + end;
                        let label = trivia(source, end);
                        if source.as_bytes().get(label) == Some(&b':') {
                            let start = trivia(source, label + 1);
                            let name = sv_words(&source[start..]).first().copied()?;
                            return Some(start + name.1);
                        }
                        return Some(end);
                    }
                }
            }
            None
        }
        Some("if" | "for" | "foreach" | "while" | "repeat" | "assert" | "assume" | "cover") => {
            let word = words.first()?;
            let mut control = trivia(source, at + word.1);
            if matches!(keyword, Some("assert" | "assume" | "cover")) {
                if source[control..].starts_with("#0") {
                    control = trivia(source, control + 2);
                } else if sv_words(&source[control..])
                    .first()
                    .is_some_and(|word| word.0 == 0 && word.2 == "final")
                {
                    control = trivia(source, control + 5);
                }
            }
            let end = parens(source, control)?;
            let body = statement_end(source, end)?;
            let next = trivia(source, body);
            if matches!(keyword, Some("if" | "assert" | "assume"))
                && sv_words(&source[next..])
                    .first()
                    .is_some_and(|word| word.0 == 0 && word.2 == "else")
            {
                statement_end(source, next + 4)
            } else {
                Some(body)
            }
        }
        Some("do") => {
            let body = statement_end(source, at + words.first()?.1)?;
            let next = trivia(source, body);
            let word = sv_words(&source[next..]).first().copied()?;
            if word.0 != 0 || word.2 != "while" {
                return None;
            }
            let end = parens(source, trivia(source, next + word.1))?;
            let semicolon = trivia(source, end);
            (source.as_bytes().get(semicolon) == Some(&b';')).then_some(semicolon + 1)
        }
        Some("forever") => statement_end(source, at + words.first()?.1),
        _ => {
            let mut cursor = at;
            let mut depth = 0;
            while cursor < source.len() {
                if skip_token(source, &mut cursor) {
                    continue;
                }
                match source.as_bytes()[cursor] {
                    b'(' | b'[' | b'{' => depth += 1,
                    b')' | b']' | b'}' => depth -= 1,
                    b';' if depth == 0 => return Some(cursor + 1),
                    _ => {}
                }
                cursor += 1;
            }
            None
        }
    }
}

fn single_generate_body(source: &str, start: usize) -> bool {
    let mut at = 0;
    let mut last_word = "";
    let mut control_end = None;
    while at < start {
        if skip_token(source, &mut at) {
            continue;
        }
        if source.as_bytes()[at] == b'(' {
            if let Some(end) = parens(source, at) {
                if source[at..].starts_with("(*") {
                    at = end;
                    continue;
                }
                control_end = matches!(last_word, "if" | "for").then_some(end);
                at = end;
                continue;
            }
        }
        if source.as_bytes()[at].is_ascii_alphabetic() || source.as_bytes()[at] == b'_' {
            let begin = at;
            at += 1;
            while at < start
                && (source.as_bytes()[at].is_ascii_alphanumeric() || source.as_bytes()[at] == b'_')
            {
                at += 1;
            }
            last_word = &source[begin..at];
            control_end = (last_word == "else").then_some(at);
        } else {
            if source.as_bytes()[at] == b':' {
                control_end = Some(at + 1);
            } else if !source.as_bytes()[at].is_ascii_whitespace() {
                control_end = None;
                last_word = "";
            }
            at += 1;
        }
    }
    control_end.is_some_and(|end| {
        let mut at = trivia(source, end);
        while source[at..].starts_with("(*") {
            let Some(end) = parens(source, at) else {
                return false;
            };
            at = trivia(source, end);
        }
        at == start
    })
}

// Keep attributes on the generated always item, inside a generated block
// when the original listener was a single generate arm.
fn attributes_start(source: &str, start: usize) -> usize {
    let mut at = 0;
    let mut attributes = start;
    while at < start {
        at = trivia(source, at);
        if at >= start {
            break;
        }
        if source[at..].starts_with("(*") {
            attributes = attributes.min(at);
            at = parens(source, at).expect("parsed attribute");
        } else {
            attributes = start;
            if !skip_token(source, &mut at) {
                at += 1;
            }
        }
    }
    attributes
}

pub(super) fn gated(
    source: &str,
    start: usize,
    end: usize,
    names: &TimedNames,
    tasks: &TaskTiming,
) -> Result<(usize, usize, String)> {
    let at = trivia(source, end);
    if source.as_bytes().get(at) != Some(&b'@') {
        return Err("timed always_ff requires an explicit event control".into());
    }
    let begin = trivia(source, at + 1);
    let event_end = parens(source, begin).ok_or("unsupported timed always_ff event control")?;
    let body_start = trivia(source, event_end);
    let body_end =
        statement_end(source, body_start).ok_or("unsupported timed always_ff statement")?;
    let body = timed_content(&source[body_start..body_end], names, tasks)?;
    let identifiers = sv_words(source)
        .into_iter()
        .map(|word| word.2)
        .collect::<std::collections::BTreeSet<_>>();
    let fresh = |kind: &str| {
        for suffix in 0.. {
            let name = format!("celox_suite_ff_{kind}_{start}_{suffix}");
            if !identifiers.contains(name.as_str()) {
                return name;
            }
        }
        unreachable!()
    };
    let armed = fresh("armed");
    let pending = fresh("pending");
    let package = &names.package;
    let event = &source[begin + 1..event_end - 1];
    let replacement_start = attributes_start(source, start);
    let attributes = &source[replacement_start..start];
    let staged = format!(
        "bit {armed}=0;\nbit {pending}=0;\n{attributes}always @({event} or posedge {package}::started) begin\n  if (!{package}::started) {pending}=1;\n  else if ({armed} || {pending}) begin\n    {pending}=0;\n    {body}\n  end\n  {armed}={package}::started;\nend"
    );
    // A single generate item needs a block for the additional declarations.
    // Reuse that construct's unnamed body rather than inserting another if,
    // which would change the user's implicit genblkN numbering.
    let staged = if single_generate_body(source, start) {
        format!("begin\n{staged}\nend")
    } else {
        staged
    };
    Ok((replacement_start, body_end, format!("{staged}\n")))
}

/// The frontend classifies an ordinary always as FF only when every leaf
/// of its event list is explicitly edge-qualified.
pub(super) fn edge_control(source: &str, end: usize, tasks: &TaskTiming) -> bool {
    let at = trivia(source, end);
    if source.as_bytes().get(at) != Some(&b'@') {
        return false;
    }
    let begin = trivia(source, at + 1);
    let Some(end) = parens(source, begin) else {
        return false;
    };
    let Some(body_end) = statement_end(source, end) else {
        return false;
    };
    let body = &source[end..body_end];
    if has_timing(body) || tasks.has_timed_call(source, end, body_end) {
        return false;
    }
    let event = &source[begin + 1..end - 1];
    let ors = sv_words(event)
        .into_iter()
        .filter_map(|(start, _, word)| (word == "or").then_some(start))
        .collect::<std::collections::BTreeSet<_>>();
    let mut at = 0;
    let mut start = 0;
    let mut depth = 0;
    let mut leaves = Vec::new();
    while at < event.len() {
        if skip_token(event, &mut at) {
            continue;
        }
        if depth == 0 && (event.as_bytes()[at] == b',' || ors.contains(&at)) {
            leaves.push(&event[start..at]);
            at += if ors.contains(&at) { 2 } else { 1 };
            start = at;
            continue;
        }
        match event.as_bytes()[at] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            _ => {}
        }
        at += 1;
    }
    leaves.push(&event[start..]);
    leaves.into_iter().all(|leaf| {
        let leaf = after_trivia(leaf);
        matches!(sv_words(leaf).first(), Some((0, _, "posedge" | "negedge")))
    })
}

/// Multiline macro replacements must keep every generated line in the
/// original definition. Existing continuation lines already have a slash.
pub(super) fn in_macro(source: &str, start: usize, replacement: String) -> String {
    let lines = source[..start].split('\n').collect::<Vec<_>>();
    let mut first = lines.len() - 1;
    while first > 0 && lines[first - 1].trim_end().ends_with('\\') {
        first -= 1;
    }
    if !lines[first].trim_start().starts_with("`define") {
        return replacement;
    }
    let mut output = String::new();
    for line in replacement.split_inclusive('\n') {
        if let Some(content) = line.strip_suffix('\n') {
            output.push_str(content);
            if !content.trim_end().ends_with('\\') {
                output.push_str(" \\");
            }
            output.push('\n');
        } else {
            output.push_str(line);
        }
    }
    output
}

pub(super) fn wildcard_control(source: &str, end: usize) -> bool {
    let at = trivia(source, end);
    if source.as_bytes().get(at) != Some(&b'@') {
        return false;
    }
    let begin = trivia(source, at + 1);
    if source.as_bytes().get(begin) == Some(&b'*') {
        return true;
    }
    parens(source, begin).is_some_and(|end| after_trivia(&source[begin + 1..end - 1]).trim() == "*")
}

fn has_timing(source: &str) -> bool {
    if sv_words(source).iter().any(|word| word.2 == "wait") {
        return true;
    }
    let mut at = 0;
    while at < source.len() {
        if skip_token(source, &mut at) {
            continue;
        }
        if source.as_bytes()[at] == b'#'
            && sv_words(&source[..at])
                .last()
                .is_some_and(|word| matches!(word.2, "assert" | "assume" | "cover"))
            && source[at..].starts_with("#0")
            && source.as_bytes().get(trivia(source, at + 2)) == Some(&b'(')
        {
            at += 2;
            continue;
        }
        if matches!(source.as_bytes()[at], b'#' | b'@') {
            return true;
        }
        at += 1;
    }
    false
}

#[derive(Default)]
pub(super) struct TaskTiming {
    tasks: BTreeMap<(String, Vec<usize>, String), bool>,
}

fn container_at(source: &str, at: usize) -> String {
    let words = sv_words(&source[..at]);
    let mut scope = String::new();
    for (index, (_, _, word)) in words.iter().enumerate() {
        match *word {
            "module" | "interface" | "package" => {
                if let Some((_, _, name)) = words[index + 1..]
                    .iter()
                    .find(|word| !matches!(word.2, "static" | "automatic"))
                {
                    scope = name.to_string();
                }
            }
            "endmodule" | "endinterface" | "endpackage" => scope.clear(),
            _ => {}
        }
    }
    scope
}

// Scope identities include their source offsets so closed sibling blocks
// cannot make their imports visible at a later module item.
fn lexical_scope_at(source: &str, at: usize) -> Vec<usize> {
    let mut scopes = Vec::new();
    for (start, _, word) in sv_words(&source[..at]) {
        match word {
            "module" | "interface" | "package" | "begin" | "fork" | "task" | "function" => {
                scopes.push(start)
            }
            "endmodule" | "endinterface" | "endpackage" | "end" | "join" | "join_any"
            | "join_none" | "endtask" | "endfunction" => {
                scopes.pop();
            }
            _ => {}
        }
    }
    scopes
}

fn callable_key(source: &str, at: usize, name: &str) -> (String, Vec<usize>, String) {
    let container = container_at(source, at);
    let mut scope = lexical_scope_at(source, at);
    if !container.is_empty() && !scope.is_empty() {
        scope.remove(0);
    }
    (container, scope, name.to_owned())
}

impl TaskTiming {
    pub(super) fn new(sources: &[String]) -> Self {
        let mut result = Self::default();
        let mut records = Vec::new();
        for source in sources {
            let words = sv_words(source);
            for (index, (start, end, word)) in words.iter().enumerate() {
                if *word == "function" {
                    let header_end = source[*end..]
                        .find(';')
                        .map(|offset| end + offset)
                        .unwrap_or(source.len());
                    let header = &source[*end..header_end];
                    let name_end = header.find('(').unwrap_or(header.len());
                    if let Some((_, _, name)) = sv_words(&header[..name_end]).last() {
                        result
                            .tasks
                            .insert(callable_key(source, *start, name), false);
                    }
                    continue;
                }
                if *word != "task" {
                    continue;
                }
                let Some((_, body_end, _)) =
                    words[index + 1..].iter().find(|word| word.2 == "endtask")
                else {
                    continue;
                };
                let Some((_, _, name)) = words[index + 1..]
                    .iter()
                    .find(|word| !matches!(word.2, "automatic" | "static"))
                else {
                    continue;
                };
                let key = callable_key(source, *start, name);
                let header_end = source[*end..*body_end]
                    .find(';')
                    .map_or(*end, |offset| end + offset + 1);
                result
                    .tasks
                    .insert(key.clone(), has_timing(&source[header_end..*body_end]));
                records.push((key, source.as_str(), header_end, *body_end));
            }
        }
        loop {
            let mut changed = false;
            for (key, source, start, end) in &records {
                if !result.tasks[key] && result.has_timed_call(source, *start, *end) {
                    result.tasks.insert(key.clone(), true);
                    changed = true;
                }
            }
            if !changed {
                return result;
            }
        }
    }

    fn resolve(&self, source: &str, at: usize, name: &str) -> Option<bool> {
        let prefix = source[..at].trim_end();
        if let Some(prefix) = prefix.strip_suffix("::") {
            let package = sv_words(prefix).last()?.2;
            return self
                .tasks
                .get(&(package.to_string(), Vec::new(), name.to_owned()))
                .copied();
        }
        if let Some(prefix) = prefix.strip_suffix('.') {
            let instance = sv_words(prefix).last()?.2;
            let scope = container_at(source, at);
            let words = sv_words(&source[..at]);
            for pair in words.windows(2) {
                if pair[1].2 == instance && container_at(source, pair[0].0) == scope {
                    if let Some(value) =
                        self.tasks
                            .get(&(pair[0].2.to_owned(), Vec::new(), name.to_owned()))
                    {
                        return Some(*value);
                    }
                }
            }
            // An unresolved qualified call cannot use a same-named local task.
            return None;
        }
        let scope = container_at(source, at);
        let mut key = callable_key(source, at, name);
        loop {
            if let Some(value) = self.tasks.get(&key) {
                return Some(*value);
            }
            if key.1.pop().is_none() {
                break;
            }
        }
        let call_scope = lexical_scope_at(source, at);
        let mut imports: BTreeMap<usize, (Option<bool>, Option<bool>)> = BTreeMap::new();
        for (start, end, word) in sv_words(&source[..at]) {
            if word != "import" {
                continue;
            }
            let import_scope = container_at(source, start);
            if !import_scope.is_empty() && import_scope != scope {
                continue;
            }
            let lexical = lexical_scope_at(source, start);
            if !call_scope.starts_with(&lexical) {
                continue;
            }
            let (explicit, wildcard) = imports.entry(lexical.len()).or_default();
            let tail = &source[end..];
            let clause = &tail[..tail.find(';')?];
            for item in clause.split(',') {
                let words = sv_words(item);
                let Some((_, _, package)) = words.first() else {
                    continue;
                };
                let selected = words.get(1).map(|word| word.2);
                if selected == Some(name) || (selected.is_none() && item.contains('*')) {
                    if let Some(value) =
                        self.tasks
                            .get(&(package.to_string(), Vec::new(), name.to_owned()))
                    {
                        if selected == Some(name) {
                            *explicit = Some(*value);
                        } else {
                            *wildcard = Some(wildcard.unwrap_or(false) || *value);
                        }
                    }
                }
            }
        }
        imports
            .into_values()
            .rev()
            .find_map(|(explicit, wildcard)| explicit.or(wildcard))
            .or_else(|| {
                self.tasks
                    .get(&(String::new(), Vec::new(), name.to_owned()))
                    .copied()
            })
    }

    fn has_timed_call(&self, source: &str, start: usize, end: usize) -> bool {
        sv_words(&source[start..end])
            .iter()
            .any(|(at, word_end, name)| {
                if !self.tasks.keys().any(|key| key.2 == *name) {
                    return false;
                }
                let tail = after_trivia(&source[start + word_end..end]);
                let prefix = source[start..start + at].trim_end();
                let statement_start = prefix.is_empty()
                    || prefix.ends_with([';', ')', ':', '.'])
                    || sv_words(prefix)
                        .last()
                        .is_some_and(|word| matches!(word.2, "begin" | "end" | "else"));
                (tail.starts_with('(') || (tail.starts_with(';') && statement_start))
                    && self.resolve(source, start + at, name).unwrap_or(true)
            })
    }
}

#[cfg(test)]
mod callable_analysis_tests {
    use super::*;
    #[test]
    fn local_declarations_are_not_no_argument_task_calls() {
        for body in ["int tick; q<=1;", "int value, tick; q<=1;"] {
            let source = format!(
                "module Top;task tick;#1;endtask always @(posedge clk)begin {body} end endmodule"
            );
            let tasks = TaskTiming::new(std::slice::from_ref(&source));
            let at = source.find("always").unwrap() + 6;
            assert!(edge_control(&source, at, &tasks), "{body}");
        }
        let source =
            "module Top;task tick;#1;endtask always @(posedge clk)begin tick; end endmodule"
                .to_string();
        let tasks = TaskTiming::new(std::slice::from_ref(&source));
        assert!(!edge_control(
            &source,
            source.find("always").unwrap() + 6,
            &tasks
        ));
    }
    #[test]
    fn escaped_callable_names_remain_whole_tokens() {
        let source = r"task \pause.task ; #1; endtask";
        assert_eq!(
            sv_words(source)
                .iter()
                .filter(|x| x.2.starts_with('\\'))
                .map(|x| x.2)
                .collect::<Vec<_>>(),
            vec![r"\pause.task"]
        );
    }
}
