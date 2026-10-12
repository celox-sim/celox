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
        Some("if" | "for" | "foreach" | "while" | "repeat") => {
            let word = words.first()?;
            let end = parens(source, trivia(source, at + word.1))?;
            let body = statement_end(source, end)?;
            let next = trivia(source, body);
            if keyword == Some("if") && source[next..].starts_with("else") {
                statement_end(source, next + 4)
            } else {
                Some(body)
            }
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

pub(super) fn gated(
    source: &str,
    start: usize,
    end: usize,
    names: &TimedNames,
) -> Result<(usize, String)> {
    let at = trivia(source, end);
    if source.as_bytes().get(at) != Some(&b'@') {
        return Err("timed always_ff requires an explicit event control".into());
    }
    let begin = trivia(source, at + 1);
    let event_end = parens(source, begin).ok_or("unsupported timed always_ff event control")?;
    let body_start = trivia(source, event_end);
    let body_end =
        statement_end(source, body_start).ok_or("unsupported timed always_ff statement")?;
    let body = timed_content(&source[body_start..body_end], names)?;
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
    Ok((
        body_end,
        format!(
            "always @({event} or posedge {package}::started) begin\n  bit {armed}=0;\n  bit {pending}=0;\n  if (!{package}::started) {pending}=1;\n  else if ({armed} || {pending}) begin\n    {pending}=0;\n    {body}\n  end\n  {armed}={package}::started;\nend"
        ),
    ))
}
