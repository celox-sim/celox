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
fn semicolon(source: &str, mut at: usize) -> Option<usize> {
    while at < source.len() {
        if skip_token(source, &mut at) {
            continue;
        }
        if source.as_bytes()[at] == b';' {
            return Some(at);
        }
        at += 1;
    }
    None
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
// Resolve callable operands while preserving ordinary value references.
pub(super) fn gated_rhs(
    source: &str,
    begin: usize,
    end: usize,
    tasks: &TaskTiming,
) -> Option<String> {
    let expression = &source[begin..end];
    let mut calls = false;
    let mut bare_calls = Vec::new();
    for (offset, end, word) in sv_words(expression) {
        if after_trivia(&expression[end..]).starts_with('(') {
            calls = true;
        } else if tasks.bare_callable(source, begin + offset, word) {
            calls = true;
            bare_calls.push(end);
        }
    }
    if !calls {
        return None;
    }
    let mut expression = expression.to_owned();
    for end in bare_calls.into_iter().rev() {
        expression.insert_str(end, " ()");
    }
    Some(expression)
}

// Icarus evaluates function operands in a continuous ternary eagerly. Use
// a procedural gate and retain the original net driver (and its delays and
// strengths). Named destinations retain their declared assignment type.
pub(super) fn gated_continuous(
    source: &str,
    start: usize,
    end: usize,
    names: &TimedNames,
    tasks: &TaskTiming,
) -> Option<(usize, String)> {
    let net_declaration = &source[start..end] != "assign";
    let mut at = end;
    let mut depth = 0usize;
    let mut lhs_start = end;
    let mut lhs_name = None;
    let mut rhs = None;
    let mut previous = start;
    let mut output = String::new();
    let mut drivers = String::new();
    while at < source.len() {
        if skip_token(source, &mut at) {
            continue;
        }
        match source.as_bytes()[at] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth = depth.checked_sub(1)?,
            b'=' if depth == 0 && rhs.is_none() => {
                let left = source[lhs_start..at].trim();
                lhs_name = Some(if net_declaration {
                    // Net declaration assignments name an entire net.
                    tasks
                        .driver_types
                        .get(source)
                        .into_iter()
                        .flatten()
                        .find(|(offset, _, _, _)| lhs_start <= *offset && *offset < at)
                        .map(|(_, id, _, _)| id.clone())
                        .or_else(|| sv_words(left).last().map(|word| word.2.to_owned()))?
                } else {
                    // Skip optional drive strength and delay controls.
                    let mut begin = trivia(left, 0);
                    if left.as_bytes().get(begin) == Some(&b'(') {
                        begin = trivia(left, parens(left, begin)?);
                    }
                    if left.as_bytes().get(begin) == Some(&b'#') {
                        begin = trivia(left, begin + 1);
                        begin = if left.as_bytes().get(begin) == Some(&b'(') {
                            trivia(left, parens(left, begin)?)
                        } else {
                            let tail = &left[begin..];
                            begin + tail.find(char::is_whitespace)?
                        };
                    }
                    left[begin..].trim().to_owned()
                });
                rhs = Some(at + 1);
            }
            b',' | b';' if depth == 0 => {
                if let Some(begin) = rhs.take() {
                    if let Some(expression) = gated_rhs(source, begin, at, tasks) {
                        let lhs = lhs_name.take()?;
                        let mut suffix = at;
                        let name = loop {
                            let name = format!("{}_driver_{suffix}", names.package);
                            if !sv_words(source).iter().any(|(_, _, word)| *word == name) {
                                break name;
                            }
                            suffix += 1;
                        };
                        let declaration = tasks.driver_declaration(source, start, &lhs, &name);
                        drivers.push_str(&format!(
                            "{declaration}\nalways_comb begin if ({}::started) {name} = {expression}; end\n", names.package
                        ));
                        output.push_str(&source[previous..begin]);
                        output.push_str(&format!(" {name}"));
                        previous = at;
                    }
                }
                lhs_start = at + 1;
                if source.as_bytes()[at] == b';' {
                    if previous == start {
                        return None;
                    }
                    output.push_str(&source[previous..=at]);
                    let replacement = format!("{drivers}{output}");
                    return Some((
                        at + 1,
                        if single_generate_body(source, start) {
                            format!("begin\n{replacement}\nend")
                        } else {
                            replacement
                        },
                    ));
                }
            }
            _ => {}
        }
        at += 1;
    }
    None
}

pub(super) fn statement_end(source: &str, at: usize) -> Option<usize> {
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

pub(super) fn single_generate_body(source: &str, start: usize) -> bool {
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
        .map(|word| word.2.strip_prefix('\\').unwrap_or(word.2))
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
    let captured = fresh("captured");
    let package = &names.package;
    let original_event = &source[begin + 1..event_end - 1];
    let event = event_leaves(original_event).join(" or ");
    let replacement_start = attributes_start(source, start);
    let attributes = &source[replacement_start..start];
    let mut declarations = String::new();
    let mut samples = Vec::new();
    let mut active = Vec::new();
    for (index, leaf) in event_leaves(&event).into_iter().enumerate() {
        let leaf = after_trivia(leaf);
        let (_, end, edge) = *sv_words(leaf).first().ok_or("empty FF event")?;
        if !matches!(edge, "posedge" | "negedge") {
            return Err("FF staging requires edge-qualified events".into());
        }
        let sample = format!("1'({})", &leaf[end..]);
        let baseline = fresh(&format!("baseline_{index}"));
        declarations.push_str(&format!("logic {baseline};\n"));
        samples.push(format!("{baseline}={sample};"));
        let (from, to) = if edge == "posedge" { (0, 1) } else { (1, 0) };
        active.push(format!("(({baseline}===1'b{from} && {sample}!==1'b{from}) || ({baseline}!==1'b{to} && {sample}===1'b{to}))"));
    }
    let samples = samples.join(" ");
    let active = active.join(" || ");
    let staged = format!(
        "bit {armed}=0;\nbit {captured}=0;\n{declarations}{attributes}always @({event} or posedge {package}::setup_started or posedge {package}::started) begin\n  if (!{package}::started) begin\n    if ({package}::setup_started && !{captured}) begin {samples} {captured}=1; end\n  end\n  else if ({armed} || ({active})) begin\n    {body}\n  end\n  {armed}={package}::started;\nend"
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
    if tasks
        .ff_offsets
        .get(source)
        .is_some_and(|offsets| !offsets.contains(&end.saturating_sub("always".len())))
    {
        return false;
    }
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
    event_leaves(event).into_iter().all(|leaf| {
        let leaf = after_trivia(leaf);
        matches!(sv_words(leaf).first(), Some((0, _, "posedge" | "negedge")))
    })
}

fn event_leaves(event: &str) -> Vec<&str> {
    let event = unwrap_event_leaf(event);
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
    leaves.into_iter().map(unwrap_event_leaf).collect()
}

fn unwrap_event_leaf(mut event: &str) -> &str {
    event = after_trivia(event).trim();
    while let Some(end) = parens(event, 0) {
        if !after_trivia(&event[end..]).is_empty() {
            break;
        }
        event = after_trivia(&event[1..end - 1]).trim();
    }
    event
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

pub(super) fn wildcard_body(source: &str, end: usize, tasks: &TaskTiming) -> Option<usize> {
    let at = trivia(source, end);
    if source.as_bytes().get(at) != Some(&b'@') {
        return None;
    }
    let begin = trivia(source, at + 1);
    let event_end = if source.as_bytes().get(begin) == Some(&b'*') {
        begin + 1
    } else if let Some(end) = parens(source, begin)
        && after_trivia(&source[begin + 1..end - 1]).trim() == "*"
    {
        end
    } else {
        return None;
    };
    let body_end = statement_end(source, event_end)?;
    (!has_timing(&source[event_end..body_end])
        && !tasks.has_timed_call(source, event_end, body_end))
    .then_some(event_end)
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

type CallableKey = (String, Option<usize>, Vec<usize>, String);

#[derive(Default)]
pub(super) struct TaskTiming {
    pub(super) packages: Option<celox_sv_analyzer::Packages>,
    tasks: BTreeMap<CallableKey, bool>,
    pub(super) ports: BTreeMap<String, Vec<String>>,
    pub(super) inputs: BTreeMap<String, std::collections::BTreeSet<String>>,
    values: BTreeMap<String, Vec<(usize, String)>>,
    driver_types: BTreeMap<String, Vec<(usize, String, String, String)>>,
    units: BTreeMap<String, usize>,
    import_clauses: BTreeMap<String, BTreeMap<usize, Vec<(Option<String>, Option<String>)>>>,
    generate_scopes: BTreeMap<String, Vec<(usize, usize)>>,
    ff_offsets: BTreeMap<String, Vec<usize>>,
    calls: BTreeMap<String, Vec<(usize, String)>>,
    instance_types: BTreeMap<String, Vec<(usize, String, String)>>,
    generic_bindings: BTreeMap<(String, String), std::collections::BTreeSet<String>>,
    imports: BTreeMap<String, Vec<(String, Option<String>)>>,
    exports: BTreeMap<String, Vec<(Option<String>, Option<String>)>>,
}

// `interface` inside an ANSI header is a generic formal, not a container.
fn generic_interface_keyword(source: &str, start: usize) -> bool {
    let mut at = 0;
    let mut depth = 0usize;
    while at < start {
        if skip_token(source, &mut at) {
            continue;
        }
        match source.as_bytes()[at] {
            b'(' => depth += 1,
            b')' => depth = depth.saturating_sub(1),
            _ => {}
        }
        at += 1;
    }
    depth > 0
}

fn container_at(source: &str, at: usize) -> String {
    let words = sv_words(&source[..at]);
    let mut scope = String::new();
    for (index, (start, _, word)) in words.iter().enumerate() {
        if *word == "interface" && generic_interface_keyword(source, *start) {
            continue;
        }
        match *word {
            "module" | "interface" | "package" => {
                if let Some((_, _, name)) = words[index + 1..]
                    .iter()
                    .find(|word| !matches!(word.2, "static" | "automatic"))
                {
                    scope = name.trim_start_matches('\\').to_owned();
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
fn lexical_scope_at(source: &str, at: usize, generates: &[(usize, usize)]) -> Vec<usize> {
    let mut scopes = Vec::new();
    for (start, _, word) in sv_words(&source[..at]) {
        if word == "interface" && generic_interface_keyword(source, start) {
            continue;
        }
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
    scopes.extend(
        generates
            .iter()
            .filter_map(|(start, end)| (*start <= at && at < *end).then_some(*start)),
    );
    scopes.sort_unstable();
    scopes.dedup();
    scopes
}

impl TaskTiming {
    fn driver_declaration(&self, source: &str, at: usize, lhs: &str, name: &str) -> String {
        let scope = self.lexical_scope(source, at);
        let declaration = self
            .driver_types
            .get(source)
            .into_iter()
            .flatten()
            .filter(|(offset, id, _, _)| {
                id.trim().trim_start_matches('\\') == lhs.trim().trim_start_matches('\\')
                    && scope.starts_with(&self.lexical_scope(source, *offset))
            })
            .max_by_key(|(offset, _, _, _)| self.lexical_scope(source, *offset).len());
        if let Some((_, _, datatype, dimensions)) = declaration {
            let datatype = datatype.trim();
            let implicit = datatype.is_empty()
                || datatype.starts_with('[')
                || datatype.starts_with("signed")
                || datatype.starts_with("unsigned");
            return format!(
                "{}{datatype} {name} {dimensions};",
                if implicit { "logic " } else { "" }
            );
        }
        // Part-selects and concatenations have their packed assignment type.
        format!("reg [$bits({lhs} )-1:0] {name};")
    }

    pub(super) fn connection_declaration(
        &self,
        module: &str,
        port: &str,
        instance: &str,
        array: bool,
        expression: &str,
        name: &str,
    ) -> Result<String> {
        let declaration = self.driver_types.iter().find_map(|(source, declarations)| {
            declarations.iter().find(|(offset, id, _, _)| {
                id.trim_start_matches('\\') == port.trim().trim_start_matches('\\')
                    && container_at(source, *offset) == module
                    && self.lexical_scope(source, *offset).len() == 1
            })
        });
        if declaration.is_none() {
            return Err(format!(
                "missing input-port type for {module}.{port} in timed callable staging"
            )
            .into());
        }
        if let Some((_, _, datatype, dimensions)) = declaration {
            let datatype = datatype.trim();
            let builtin = datatype.is_empty()
                || datatype.starts_with('[')
                || sv_words(datatype).first().is_some_and(|w| {
                    matches!(
                        w.2,
                        "bit"
                            | "logic"
                            | "reg"
                            | "signed"
                            | "unsigned"
                            | "byte"
                            | "shortint"
                            | "int"
                            | "longint"
                            | "integer"
                            | "time"
                    )
                });
            if !builtin || !dimensions.is_empty() {
                // A package-qualified typedef keeps nominal enum/struct identity,
                // signedness and unpacked shape without relying on type(expr),
                // which the available Icarus adapter does not implement.
                if !array
                    && datatype.contains("::")
                    && !datatype.contains(['[', '{', '('])
                    && dimensions.is_empty()
                {
                    return Ok(format!("{datatype} {name};"));
                }
                return Err(format!("cannot preserve callable input-port type `{datatype} {dimensions}` for {module}.{port} in timed staging").into());
            }
        }
        let width = if array {
            expression.to_owned()
        } else {
            format!("{instance} .{port} ")
        };
        Ok(format!("reg [$bits({width} )-1:0] {name};"))
    }

    fn lexical_scope(&self, source: &str, at: usize) -> Vec<usize> {
        lexical_scope_at(
            source,
            at,
            self.generate_scopes.get(source).map_or(&[], Vec::as_slice),
        )
    }

    fn unit(&self, source: &str) -> Option<usize> {
        self.units.get(source).copied()
    }
    fn clause_items(
        &self,
        source: &str,
        start: usize,
        end: usize,
    ) -> Vec<(Option<String>, Option<String>)> {
        if let Some(clauses) = self.import_clauses.get(source) {
            return clauses.get(&start).cloned().unwrap_or_default();
        }
        // Only malformed sources use the lexical fallback. Split separators
        // outside comments, strings and escaped identifiers.
        let tail = &source[end..];
        let clause = &tail[..semicolon(tail, 0).unwrap_or(tail.len())];
        let mut parts = Vec::new();
        let mut begin = 0;
        let mut at = 0;
        while at < clause.len() {
            if skip_token(clause, &mut at) {
                continue;
            }
            if clause.as_bytes()[at] == b',' {
                parts.push(&clause[begin..at]);
                begin = at + 1;
            }
            at += 1;
        }
        parts.push(&clause[begin..]);
        parts
            .into_iter()
            .map(|item| {
                let words = sv_words(item);
                (
                    words.first().map(|x| x.2.to_owned()),
                    words.get(1).map(|x| x.2.to_owned()),
                )
            })
            .collect()
    }

    fn callable_key(&self, source: &str, at: usize, name: &str) -> CallableKey {
        let container = container_at(source, at);
        let mut scope = self.lexical_scope(source, at);
        if !container.is_empty() && !scope.is_empty() {
            scope.remove(0);
        }
        let unit = container.is_empty().then(|| self.unit(source)).flatten();
        (
            container,
            unit,
            scope,
            name.trim_start_matches('\\').to_owned(),
        )
    }

    fn package_callable(
        &self,
        source: &str,
        package: &str,
        name: &str,
        visiting: &mut std::collections::BTreeSet<String>,
    ) -> Option<bool> {
        let package = package.trim_start_matches('\\');
        let name = name.trim_start_matches('\\');
        let (package, unit) = if package == "$unit" {
            ("", self.unit(source))
        } else {
            (package, None)
        };
        if let Some(value) =
            self.tasks
                .get(&(package.to_owned(), unit, Vec::new(), name.to_owned()))
        {
            return Some(*value);
        }
        if !visiting.insert(package.to_owned()) {
            return None;
        }
        let mut result = None;
        if let (Some(imports), Some(exports)) =
            (self.imports.get(package), self.exports.get(package))
        {
            for (origin, selected) in imports {
                if selected.as_deref().is_some_and(|selected| selected != name) {
                    continue;
                }
                if !exports.iter().any(|(exported, member)| {
                    exported.as_deref().is_none_or(|p| p == origin)
                        && member.as_deref().is_none_or(|n| n == name)
                }) {
                    continue;
                }
                if let Some(timed) = self.package_callable(source, origin, name, visiting) {
                    result = Some(result.unwrap_or(false) || timed);
                }
            }
        }
        visiting.remove(package);
        result
    }
    pub(super) fn new(sources: &[String]) -> Self {
        let mut result = Self::default();
        let paths: Vec<_> = sources
            .iter()
            .map(|s| (s.as_str(), Path::new("timed.sv")))
            .collect();
        if let Ok(elaboration) = celox_sv_analyzer::elaborate_interfaces_with_metadata(&paths, &[])
        {
            result.generic_bindings = elaboration
                .generic_bindings
                .into_iter()
                .map(|(key, types)| (key, types.into_iter().collect()))
                .collect();
        }
        if let Ok(packages) = celox_sv_analyzer::analyze_packages(&paths) {
            for source in sources {
                if let Ok(interfaces) =
                    celox_sv_analyzer::source_module_interfaces(source, Path::new("timed.sv"))
                {
                    for (name, interface) in interfaces {
                        if let Ok(ir) = celox_sv_analyzer::analyze_source_module_with_packages(
                            source,
                            Path::new("timed.sv"),
                            &name,
                            &Default::default(),
                            &packages,
                        ) {
                            for module in ir.modules() {
                                result.inputs.insert(
                                    module.name().to_owned(),
                                    module
                                        .ports()
                                        .iter()
                                        .filter(|port| {
                                            port.direction()
                                                == celox_sv_analyzer::ir::PortDirection::Input
                                        })
                                        .map(|port| port.name().to_owned())
                                        .collect(),
                                );
                            }
                        }
                        result.ports.insert(name, interface.ports);
                    }
                }
            }
            result.packages = Some(packages);
        }
        let mut records = Vec::new();
        for (index, source) in sources.iter().enumerate() {
            result.units.insert(source.clone(), index);
            if let Ok(tree) = celox_sv_analyzer::syntax::parse_source(source, Path::new("timed.sv"))
            {
                // Re-preprocessing can add whitespace after escaped names.
                // All lexical joins use offsets of the source passed here.
                let origin = |offset| {
                    tree.get_origin(&sv_parser::Locate {
                        offset,
                        ..Default::default()
                    })
                    .map_or(offset, |(_, origin)| origin)
                };
                let mut driver_types = Vec::new();
                let mut inherited_port_type = None;
                let text = |node| {
                    timed_initializers::span(&tree, node)
                        .map(|(a, b)| source[a..b].to_owned())
                        .unwrap_or_default()
                };
                let identifier = |offset| {
                    sv_words(&source[offset..])
                        .first()
                        .map(|word| word.2.to_owned())
                        .unwrap_or_default()
                };
                for node in &tree {
                    use sv_parser::RefNode;
                    match node {
                        RefNode::NetDeclaration(sv_parser::NetDeclaration::NetType(n)) => {
                            let datatype = text(RefNode::DataTypeOrImplicit(&n.nodes.3));
                            for declaration in n.nodes.5.nodes.0.contents() {
                                if let Some((offset, _)) = timed_initializers::span(
                                    &tree,
                                    RefNode::NetIdentifier(&declaration.nodes.0),
                                ) {
                                    let id = identifier(offset);
                                    let dimensions = declaration
                                        .nodes
                                        .1
                                        .iter()
                                        .map(|d| text(RefNode::UnpackedDimension(d)))
                                        .collect::<Vec<_>>()
                                        .join(" ");
                                    driver_types.push((offset, id, datatype.clone(), dimensions));
                                }
                            }
                        }
                        RefNode::ModuleDeclaration(_) | RefNode::InterfaceDeclaration(_) => {
                            inherited_port_type = None;
                        }
                        RefNode::AnsiPortDeclarationNet(n) => {
                            if let Some(header) = &n.nodes.0 {
                                inherited_port_type=match header {
                                    sv_parser::NetPortHeaderOrInterfacePortHeader::NetPortHeader(header) => match &header.nodes.1 {
                                        sv_parser::NetPortType::DataType(port) => Some(text(RefNode::DataTypeOrImplicit(&port.nodes.1))),
                                        _ => None,
                                    },
                                    _ => None,
                                };
                            }
                            if let (Some(datatype), Some((offset, _))) = (
                                &inherited_port_type,
                                timed_initializers::span(
                                    &tree,
                                    RefNode::PortIdentifier(&n.nodes.1),
                                ),
                            ) {
                                let dimensions = n
                                    .nodes
                                    .2
                                    .iter()
                                    .map(|d| text(RefNode::UnpackedDimension(d)))
                                    .collect::<Vec<_>>()
                                    .join(" ");
                                driver_types.push((
                                    offset,
                                    identifier(offset),
                                    datatype.clone(),
                                    dimensions,
                                ));
                            }
                        }
                        RefNode::AnsiPortDeclarationVariable(n) => {
                            if let Some(header) = &n.nodes.0 {
                                inherited_port_type = Some(
                                    text(RefNode::VarDataType(&header.nodes.1.nodes.0))
                                        .trim_start_matches("var ")
                                        .to_owned(),
                                );
                            }
                            if let (Some(datatype), Some((offset, _))) = (
                                &inherited_port_type,
                                timed_initializers::span(
                                    &tree,
                                    RefNode::PortIdentifier(&n.nodes.1),
                                ),
                            ) {
                                let dimensions = n
                                    .nodes
                                    .2
                                    .iter()
                                    .map(|d| text(RefNode::VariableDimension(d)))
                                    .collect::<Vec<_>>()
                                    .join(" ");
                                driver_types.push((
                                    offset,
                                    identifier(offset),
                                    datatype.clone(),
                                    dimensions,
                                ));
                            }
                        }
                        RefNode::DataDeclaration(sv_parser::DataDeclaration::Variable(n)) => {
                            let datatype = text(RefNode::DataTypeOrImplicit(&n.nodes.3));
                            for declaration in n.nodes.4.nodes.0.contents() {
                                if let sv_parser::VariableDeclAssignment::Variable(declaration) =
                                    declaration
                                {
                                    if let Some((offset, _)) = timed_initializers::span(
                                        &tree,
                                        RefNode::VariableIdentifier(&declaration.nodes.0),
                                    ) {
                                        let id = identifier(offset);
                                        let dimensions = declaration
                                            .nodes
                                            .1
                                            .iter()
                                            .map(|d| text(RefNode::VariableDimension(d)))
                                            .collect::<Vec<_>>()
                                            .join(" ");
                                        driver_types.push((
                                            offset,
                                            id,
                                            datatype.clone(),
                                            dimensions,
                                        ));
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
                result.driver_types.insert(source.clone(), driver_types);
                let mut values = Vec::new();
                for node in &tree {
                    use sv_parser::RefNode;
                    let identifier = match node {
                        RefNode::VariableDeclAssignmentVariable(n) => {
                            RefNode::VariableIdentifier(&n.nodes.0)
                        }
                        RefNode::NetDeclAssignment(n) => RefNode::NetIdentifier(&n.nodes.0),
                        RefNode::ParamAssignment(n) => RefNode::ParameterIdentifier(&n.nodes.0),
                        RefNode::AnsiPortDeclarationNet(n) => RefNode::PortIdentifier(&n.nodes.1),
                        RefNode::AnsiPortDeclarationVariable(n) => {
                            RefNode::PortIdentifier(&n.nodes.1)
                        }
                        RefNode::AnsiPortDeclarationParen(n) => RefNode::PortIdentifier(&n.nodes.2),
                        RefNode::EnumNameDeclaration(n) => RefNode::EnumIdentifier(&n.nodes.0),
                        RefNode::ListOfPortIdentifiers(n) => {
                            for (name, _) in n.nodes.0.contents() {
                                if let Some(RefNode::Locate(locate)) = RefNode::PortIdentifier(name)
                                    .into_iter()
                                    .find(|n| matches!(n, RefNode::Locate(_)))
                                {
                                    let offset = origin(locate.offset);
                                    if let Some((_, _, name)) = sv_words(&source[offset..]).first()
                                    {
                                        values.push((
                                            offset,
                                            name.trim_start_matches('\\').to_owned(),
                                        ));
                                    }
                                }
                            }
                            continue;
                        }
                        _ => continue,
                    };
                    if let Some(RefNode::Locate(locate)) = identifier
                        .into_iter()
                        .find(|n| matches!(n, RefNode::Locate(_)))
                    {
                        let offset = origin(locate.offset);
                        if let Some((_, _, name)) = sv_words(&source[offset..]).first() {
                            values.push((offset, name.trim_start_matches('\\').to_owned()));
                        }
                    }
                }
                result.values.insert(source.clone(), values);
                result.instance_types.insert(
                    source.clone(),
                    celox_sv_analyzer::ast::instance_type_sites(&tree)
                        .into_iter()
                        .map(|(offset, name, typ)| (origin(offset), name, typ))
                        .collect(),
                );
                result.import_clauses.insert(
                    source.clone(),
                    celox_sv_analyzer::ast::package_import_export_clauses(&tree)
                        .into_iter()
                        .map(|(offset, items)| (origin(offset), items))
                        .collect(),
                );
                result.calls.insert(
                    source.clone(),
                    celox_sv_analyzer::ast::procedural_call_sites(&tree)
                        .into_iter()
                        .map(|(offset, name)| (origin(offset), name))
                        .collect(),
                );
                result.ff_offsets.insert(
                    source.clone(),
                    celox_sv_analyzer::ast::ff_always_offsets(&tree)
                        .into_iter()
                        .map(origin)
                        .collect(),
                );
                result.generate_scopes.insert(
                    source.clone(),
                    celox_sv_analyzer::ast::generate_scope_ranges(&tree)
                        .into_iter()
                        .map(|(start, end)| (origin(start), origin(end.saturating_sub(1)) + 1))
                        .collect(),
                );
            }
            let words = sv_words(source);
            for (start, end, word) in &words {
                if !matches!(*word, "import" | "export")
                    || result.lexical_scope(source, *start).len() != 1
                {
                    continue;
                }
                let container = container_at(source, *start);
                // Only package exports participate in qualified resolution.
                if !sv_words(&source[..*start])
                    .iter()
                    .rev()
                    .find(|(_, _, word)| {
                        matches!(
                            *word,
                            "package"
                                | "module"
                                | "interface"
                                | "endpackage"
                                | "endmodule"
                                | "endinterface"
                        )
                    })
                    .is_some_and(|(_, _, word)| *word == "package")
                {
                    continue;
                }
                for (package, selected) in result.clause_items(source, *start, *end) {
                    if *word == "import" {
                        if let Some(package) = package {
                            result
                                .imports
                                .entry(container.clone())
                                .or_default()
                                .push((package, selected));
                        }
                    } else {
                        result
                            .exports
                            .entry(container.clone())
                            .or_default()
                            .push((package, selected));
                    }
                }
            }
            for (index, (start, end, word)) in words.iter().enumerate() {
                if *word == "function" {
                    let header_end = semicolon(source, *end).unwrap_or(source.len());
                    let header = &source[*end..header_end];
                    let mut name_end = header.len();
                    let mut at = 0;
                    let mut brackets = 0usize;
                    while at < header.len() {
                        if skip_token(header, &mut at) {
                            continue;
                        }
                        match header.as_bytes()[at] {
                            b'[' => brackets += 1,
                            b']' => brackets = brackets.saturating_sub(1),
                            b'(' => {
                                if brackets == 0 {
                                    name_end = at;
                                }
                                if let Some(end) = parens(header, at) {
                                    at = end;
                                    continue;
                                }
                            }
                            _ => {}
                        }
                        at += 1;
                    }
                    if let Some((_, _, name)) = sv_words(&header[..name_end]).last() {
                        result
                            .tasks
                            .insert(result.callable_key(source, *start, name), false);
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
                let key = result.callable_key(source, *start, name);
                let header_end =
                    semicolon(&source[..*body_end], *end).map_or(*end, |offset| offset + 1);
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

    fn bare_callable(&self, source: &str, at: usize, name: &str) -> bool {
        let prefix = source[..at].trim_end();
        if !prefix.ends_with('.') && !prefix.ends_with("::") {
            let scope = self.lexical_scope(source, at);
            let container = container_at(source, at);
            if self.values.get(source).is_some_and(|values| {
                values.iter().any(|(offset, value)| {
                    value == name.trim_start_matches('\\')
                        && container_at(source, *offset) == container
                        && scope.starts_with(&self.lexical_scope(source, *offset))
                })
            }) {
                return false;
            }
        }
        self.resolve(source, at, name).is_some()
    }

    fn resolve(&self, source: &str, at: usize, name: &str) -> Option<bool> {
        let name = name.trim_start_matches('\\');
        let prefix = source[..at].trim_end();
        if let Some(prefix) = prefix.strip_suffix("::") {
            let package = sv_words(prefix).last()?.2;
            return self.package_callable(source, package, name, &mut Default::default());
        }
        if let Some(prefix) = prefix.strip_suffix('.') {
            let instance = sv_words(prefix).last()?.2;
            let scope = container_at(source, at);
            if let Some(types) = self
                .generic_bindings
                .get(&(scope.clone(), instance.to_owned()))
            {
                let mut timed = false;
                for typ in types {
                    timed |= *self
                        .tasks
                        .get(&(typ.clone(), None, Vec::new(), name.to_owned()))?;
                }
                return Some(timed);
            }
            if let Some(instances) = self.instance_types.get(source) {
                let call_scope = self.lexical_scope(source, at);
                let found = instances
                    .iter()
                    .filter(|(offset, n, _)| {
                        n.trim_start_matches('\\') == instance.trim_start_matches('\\')
                            && container_at(source, *offset) == scope
                            && call_scope.starts_with(&self.lexical_scope(source, *offset))
                    })
                    .max_by_key(|(offset, _, _)| self.lexical_scope(source, *offset).len());
                return found.and_then(|(_, _, typ)| {
                    self.tasks
                        .get(&(
                            typ.trim_start_matches('\\').to_owned(),
                            None,
                            Vec::new(),
                            name.to_owned(),
                        ))
                        .copied()
                });
            }
            let words = sv_words(&source[..at]);
            for pair in words.windows(2) {
                if pair[1].2 == instance && container_at(source, pair[0].0) == scope {
                    if let Some(value) = self.tasks.get(&(
                        pair[0].2.trim_start_matches('\\').to_owned(),
                        None,
                        Vec::new(),
                        name.to_owned(),
                    )) {
                        return Some(*value);
                    }
                }
            }
            // An unresolved qualified call cannot use a same-named local task.
            return None;
        }
        let scope = container_at(source, at);
        let mut key = self.callable_key(source, at, name);
        loop {
            if let Some(value) = self.tasks.get(&key) {
                return Some(*value);
            }
            if key.2.pop().is_none() {
                break;
            }
        }
        let call_scope = self.lexical_scope(source, at);
        let mut imports: BTreeMap<usize, (Option<bool>, Option<bool>)> = BTreeMap::new();
        for (start, end, word) in sv_words(&source[..at]) {
            if word != "import" {
                continue;
            }
            let import_scope = container_at(source, start);
            if !import_scope.is_empty() && import_scope != scope {
                continue;
            }
            let lexical = self.lexical_scope(source, start);
            if !call_scope.starts_with(&lexical) {
                continue;
            }
            let (explicit, wildcard) = imports.entry(lexical.len()).or_default();
            for (package, selected) in self.clause_items(source, start, end) {
                let Some(package) = package else {
                    continue;
                };
                if selected.as_deref() == Some(name) || selected.is_none() {
                    if let Some(value) =
                        self.package_callable(source, &package, name, &mut Default::default())
                    {
                        if selected.as_deref() == Some(name) {
                            *explicit = Some(value);
                        } else {
                            *wildcard = Some(wildcard.unwrap_or(false) || value);
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
                    .get(&(
                        String::new(),
                        self.unit(source),
                        Vec::new(),
                        name.to_owned(),
                    ))
                    .copied()
            })
    }

    fn has_timed_call(&self, source: &str, start: usize, end: usize) -> bool {
        if let Some(calls) = self.calls.get(source) {
            return calls.iter().any(|(at, name)| {
                *at >= start
                    && *at < end
                    && self
                        .tasks
                        .keys()
                        .any(|key| key.3 == name.trim_start_matches('\\'))
                    && self.resolve(source, *at, name).unwrap_or(true)
            });
        }
        sv_words(&source[start..end])
            .iter()
            .any(|(at, word_end, name)| {
                if !self
                    .tasks
                    .keys()
                    .any(|key| key.3 == name.trim_start_matches('\\'))
                {
                    return false;
                }
                let tail = after_trivia(&source[start + word_end..end]);
                let prefix = source[start..start + at].trim_end();
                // .formal(expr) is a named actual, not a method call. A
                // qualified call has an identifier before the dot.
                if let Some(before_dot) = prefix.strip_suffix('.')
                    && before_dot.trim_end().ends_with(['(', ','])
                {
                    return false;
                }
                let words = sv_words(prefix);
                let named_block = words.len() >= 2
                    && words[words.len() - 2].2 == "begin"
                    && after_trivia(&prefix[words[words.len() - 2].1..]).starts_with(':')
                    && after_trivia(&prefix[words[words.len() - 1].1..]).is_empty();
                let statement_start = named_block
                    || prefix.is_empty()
                    || prefix.ends_with([';', ')', ':', '.'])
                    || words.last().is_some_and(|word| {
                        matches!(
                            word.2,
                            "begin" | "end" | "else" | "forever" | "do" | "endcase"
                        )
                    });
                (tail.starts_with('(') || (tail.starts_with(';') && statement_start))
                    && self.resolve(source, start + at, name).unwrap_or(true)
            })
    }
}

#[cfg(test)]
mod callable_analysis_tests {
    use super::*;
    #[test]
    fn generic_interface_formals_resolve_from_specialized_bindings() {
        let source="interface Scale;function automatic bit scaled(input bit v);return v;endfunction endinterface\nmodule User(interface s,input bit clk,v,output bit q);always @(posedge clk)q<=s.scaled(v);endmodule\nmodule Top(input bit clk,v,output bit q);Scale s();\nUser u(.s(s),.clk(clk),.v(v),.q(q));endmodule".to_owned();
        let e = celox_sv_analyzer::elaborate_interfaces_with_metadata(
            &[(source.as_str(), Path::new("test.sv"))],
            &[],
        )
        .unwrap();
        assert!(!e.generic_bindings.is_empty());
        let tasks = TaskTiming::new(std::slice::from_ref(&source));
        assert_eq!(
            tasks.resolve(&source, source.find("scaled(v)").unwrap(), "scaled"),
            Some(false)
        );
    }
    #[test]
    fn modport_formals_resolve_their_interface_methods() {
        let source="interface Scale;function automatic bit scaled(input bit v);return v;endfunction modport user(import scaled);endinterface module User(Scale.user s,input bit clk, v,output bit q);always @(posedge clk)q<=s.scaled(v);endmodule".to_owned();
        let tasks = TaskTiming::new(std::slice::from_ref(&source));
        assert_eq!(
            tasks.resolve(&source, source.rfind("scaled(v)").unwrap(), "scaled"),
            Some(false)
        );
    }
    #[test]
    fn parameterized_instances_resolve_their_declared_method_type() {
        let source="interface Scale #(parameter int K=1);function automatic bit scaled(input bit v);return v;endfunction endinterface module Top;Scale #(.K(5)) s();always @(posedge clk)q<=s.scaled(v);endmodule".to_owned();
        let tasks = TaskTiming::new(std::slice::from_ref(&source));
        assert_eq!(
            tasks.resolve(&source, source.rfind("scaled(v)").unwrap(), "scaled"),
            Some(false)
        );
    }
    #[test]
    fn parsed_imports_keep_escaped_names_and_comment_commas_whole() {
        for imports in [
            "import B::*, A /*,*/ :: tick;",
            r"import B::*, \A,items ::tick;",
        ] {
            let package = if imports.contains("items") {
                r"\A,items "
            } else {
                "A"
            };
            let source = format!(
                "package {package};function bit tick();return 1;endfunction endpackage package B;task tick;#1;endtask endpackage module Top;{imports}always @(posedge clk)q<=tick();endmodule"
            );
            let tasks = TaskTiming::new(std::slice::from_ref(&source));
            assert_eq!(
                tasks.resolve(&source, source.rfind("tick()").unwrap(), "tick"),
                Some(false)
            );
            assert!(edge_control(
                &source,
                source.find("always").unwrap() + 6,
                &tasks
            ));
        }
    }
    #[test]
    fn compilation_units_keep_callable_timing_separate() {
        for qualified in [false, true] {
            let first =
                "task tick;#1;endtask module Top;always @(posedge clk)tick();endmodule".to_owned();
            let second =
                "task tick;endtask module Other;always @(posedge clk)tick();endmodule".to_owned();
            let first = if qualified {
                first.replace("tick();endmodule", "$unit::tick();endmodule")
            } else {
                first
            };
            let second = if qualified {
                second.replace("tick();endmodule", "$unit::tick();endmodule")
            } else {
                second
            };
            let timing = TaskTiming::new(&[first.clone(), second.clone()]);
            assert!(timing.has_timed_call(&first, first.find("always").unwrap(), first.len()));
            assert!(!timing.has_timed_call(&second, second.find("always").unwrap(), second.len()));
        }
    }
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
    fn qualified_and_exported_callable_timing_uses_the_declaring_scope() {
        for delay in ["", "#1;"] {
            let source = format!(
                "package A; task tick; {delay} endtask endpackage
                package B; import A::tick; export A::*; endpackage
                package C; import B::*; export B::tick; endpackage
                module Top; import C::*; always @(posedge clk) tick(); endmodule"
            );
            let tasks = TaskTiming::new(std::slice::from_ref(&source));
            assert_eq!(
                tasks.package_callable(&source, "C", "tick", &mut Default::default()),
                Some(!delay.is_empty())
            );
            let at = source.rfind("tick()").unwrap();
            assert_eq!(tasks.resolve(&source, at, "tick"), Some(!delay.is_empty()));
        }
        let source = "task tick; endtask module Top; task tick; #1; endtask always @(posedge clk) $unit::tick(); endmodule".to_owned();
        let tasks = TaskTiming::new(std::slice::from_ref(&source));
        assert_eq!(
            tasks.resolve(&source, source.rfind("tick()").unwrap(), "tick"),
            Some(false)
        );
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
