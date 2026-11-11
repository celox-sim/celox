//! Formal port names and connections of external modules.

use super::*;

pub fn external_port_formal_names(
    decl: &InstDeclaration,
) -> Result<(bool, Vec<String>), ParserError> {
    if let Component::SystemVerilog(component) = decl.component.as_ref()
        && component.connects.is_empty()
    {
        return Ok((false, Vec::new()));
    }
    if decl.token.beg.source == veryl_parser::veryl_token::TokenSource::External {
        let Component::SystemVerilog(component) = decl.component.as_ref() else {
            unreachable!();
        };
        return Ok((false, vec![String::new(); component.connects.len()]));
    }
    let source = decl.token.source().get_text();
    let start = decl.token.beg.pos as usize;
    let source = source.get(start..).ok_or_else(|| {
        ParserError::illegal_context(
            "external module port connections",
            "instance source range is unavailable",
            Some(&decl.token),
        )
    })?;
    let source = without_comments(source);
    let declaration = source
        .split_once(';')
        .map(|(declaration, _)| declaration)
        .ok_or_else(|| {
            ParserError::illegal_context(
                "external module port connections",
                "instance source range is unavailable",
                Some(&decl.token),
            )
        })?;
    let port_list = last_outer_parenthesized(declaration).ok_or_else(|| {
        ParserError::illegal_context(
            "external module port connections",
            "instance port list is unavailable",
            Some(&decl.token),
        )
    })?;

    let items = split_top_level(port_list, ',');
    let uses_named_associations = uses_named_external_port_associations(&items);
    if !uses_named_associations {
        return Ok((false, vec![String::new(); items.len()]));
    }
    let formal_names = items
        .into_iter()
        .map(|item| {
            let item = item.trim();
            let formal = find_top_level_connection_colon(item)
                .map_or(item, |colon| &item[..colon])
                .trim();
            if formal.is_empty()
                || !formal
                    .chars()
                    .all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
            {
                Err(ParserError::illegal_context(
                    "external module port connections",
                    format!("unsupported named port association `{item}`"),
                    Some(&decl.token),
                ))
            } else {
                Ok(formal.to_string())
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut unique_formals = HashSet::default();
    if let Some(formal) = formal_names
        .iter()
        .find(|formal| !unique_formals.insert(formal.as_str()))
    {
        return Err(ParserError::illegal_context(
            "external module port connections",
            format!("duplicate named SystemVerilog port association `{formal}`"),
            Some(&decl.token),
        ));
    }
    Ok((uses_named_associations, formal_names))
}

pub(super) fn without_comments(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut string = false;
    let mut escaped = false;
    let mut line_comment = false;
    let mut block_comment = false;

    while let Some(ch) = chars.next() {
        if line_comment {
            if ch == '\n' {
                line_comment = false;
                result.push(ch);
            } else {
                result.push(' ');
            }
            continue;
        }
        if block_comment {
            if ch == '*' && chars.peek() == Some(&'/') {
                chars.next();
                result.push_str("  ");
                block_comment = false;
            } else if ch == '\n' {
                result.push(ch);
            } else {
                result.push(' ');
            }
            continue;
        }
        if string {
            result.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                string = false;
            }
            continue;
        }
        match (ch, chars.peek().copied()) {
            ('"', _) => {
                string = true;
                result.push(ch);
            }
            ('/', Some('/')) => {
                chars.next();
                result.push_str("  ");
                line_comment = true;
            }
            ('/', Some('*')) => {
                chars.next();
                result.push_str("  ");
                block_comment = true;
            }
            _ => result.push(ch),
        }
    }
    result
}

pub(super) fn uses_named_external_port_associations(items: &[&str]) -> bool {
    items.iter().any(|item| {
        let Some(colon) = find_top_level_connection_colon(item) else {
            return false;
        };
        let formal = item[..colon].trim();
        !formal.is_empty()
            && formal
                .chars()
                .all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
    })
}

pub(super) fn last_outer_parenthesized(text: &str) -> Option<&str> {
    let mut depth = 0usize;
    let mut start = None;
    let mut last = None;
    let mut string = false;
    let mut escaped = false;
    for (index, ch) in text.char_indices() {
        if string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                string = false;
            }
            continue;
        }
        if ch == '"' {
            string = true;
        } else if ch == '(' {
            if depth == 0 {
                start = Some(index + ch.len_utf8());
            }
            depth += 1;
        } else if ch == ')' {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                last = Some((start?, index));
            }
        }
    }
    let (start, end) = last?;
    text.get(start..end)
}

pub(super) fn split_top_level(text: &str, separator: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut depths = [0usize; 3];
    let mut string = false;
    let mut escaped = false;
    for (index, ch) in text.char_indices() {
        if string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                string = false;
            }
            continue;
        }
        match ch {
            '"' => string = true,
            '(' => depths[0] += 1,
            ')' => depths[0] = depths[0].saturating_sub(1),
            '[' => depths[1] += 1,
            ']' => depths[1] = depths[1].saturating_sub(1),
            '{' => depths[2] += 1,
            '}' => depths[2] = depths[2].saturating_sub(1),
            _ if ch == separator && depths == [0, 0, 0] => {
                parts.push(&text[start..index]);
                start = index + ch.len_utf8();
            }
            _ => {}
        }
    }
    if !text[start..].trim().is_empty() {
        parts.push(&text[start..]);
    }
    parts
}

pub(super) fn find_top_level_connection_colon(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depths = [0usize; 3];
    for (index, byte) in bytes.iter().copied().enumerate() {
        match byte {
            b'(' => depths[0] += 1,
            b')' => depths[0] = depths[0].saturating_sub(1),
            b'[' => depths[1] += 1,
            b']' => depths[1] = depths[1].saturating_sub(1),
            b'{' => depths[2] += 1,
            b'}' => depths[2] = depths[2].saturating_sub(1),
            b':' if depths == [0, 0, 0]
                && bytes.get(index.wrapping_sub(1)) != Some(&b':')
                && bytes.get(index + 1) != Some(&b':') =>
            {
                return Some(index);
            }
            _ => {}
        }
    }
    None
}
