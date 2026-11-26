//! Bind directives (IEEE 1800-2023 23.11). The instantiation of a bind
//! directive is in effect at the very end of its target scope, so each one
//! is moved there on the tokens, before the source is parsed again: its
//! actuals name the objects of the target, and no consumer of the tree
//! sees the directive itself.

use fxhash::FxHashSet as HashSet;

use celox_sv_syntax::{
    BodyItemKind, Declarations, PortKind, Token, TokenKind, TokenRange, Unit, UnitKind,
};

use crate::AnalyzerError;

fn invalid(rule: impl Into<String>) -> AnalyzerError {
    AnalyzerError::InvalidBind(rule.into())
}

fn unsupported(construct: impl Into<String>) -> AnalyzerError {
    AnalyzerError::Unsupported(construct.into())
}

/// Whether `tokens` hold a bind directive: `bind` is a keyword, so a token
/// of that text is one.
pub(super) fn any(tokens: &[Token]) -> bool {
    tokens.iter().any(|token| token.text == "bind")
}

fn contains(outer: TokenRange, inner: TokenRange) -> bool {
    outer.start <= inner.start && inner.end <= outer.end
}

/// The tokens of `declarations` with the instantiation of each of its bind
/// directives moved before the end keyword of the module or interface it
/// binds into, and the directives left out.
pub(super) fn spliced(declarations: &Declarations) -> Result<Vec<Token>, AnalyzerError> {
    let tokens = &declarations.tokens;
    let units = || declarations.units.iter().chain(&declarations.nested_units);
    // The innermost design element whose tokens hold `range`.
    let owner = |range: TokenRange| {
        units()
            .filter(|unit| contains(unit.tokens, range))
            .min_by_key(|unit| unit.tokens.end - unit.tokens.start)
    };
    // The design element `name` that the scope of `range` sees: one nested
    // in a design element holding `range`, the innermost, or else one of the
    // compilation unit.
    let declared = |name: &str, range: TokenRange| {
        let named = |unit: &&Unit| crate::normalize_identifier(&unit.name.spelling) == name;
        declarations
            .nested_units
            .iter()
            .filter(named)
            .filter_map(|nested| {
                // The design element that declares `nested`, which sees it.
                let scope = units()
                    .filter(|outer| {
                        !std::ptr::eq(*outer, nested) && contains(outer.tokens, nested.tokens)
                    })
                    .min_by_key(|outer| outer.tokens.end - outer.tokens.start)?;
                contains(scope.tokens, range)
                    .then(|| (scope.tokens.end - scope.tokens.start, nested))
            })
            .min_by_key(|(size, _)| *size)
            .map(|(_, nested)| nested)
            .or_else(|| declarations.units.iter().find(named))
    };
    let generate_blocks: Vec<TokenRange> = declarations
        .body_items
        .iter()
        .filter(|item| {
            matches!(
                item.kind,
                BodyItemKind::Block { .. }
                    | BodyItemKind::Conditional { .. }
                    | BodyItemKind::Case { .. }
                    | BodyItemKind::Loop { .. }
            )
        })
        .map(|item| item.tokens)
        .collect();
    // The instantiations to insert before each end keyword, and the token
    // ranges of the directives.
    let mut inserted: Vec<(usize, TokenRange)> = Vec::new();
    let mut removed: Vec<TokenRange> = Vec::new();
    for item in &declarations.body_items {
        if !matches!(&item.kind, BodyItemKind::Unconverted { category, .. } if category == "bind") {
            continue;
        }
        let range = item.tokens;
        let significant: Vec<usize> = (range.start..range.end)
            .filter(|&index| !tokens[index].is_trivia())
            .collect();
        let text = |position: usize| {
            significant
                .get(position)
                .map_or("", |&index| tokens[index].text.as_str())
        };
        if text(0) != "bind" {
            continue;
        }
        let container = owner(range);
        if generate_blocks.iter().any(|block| contains(*block, range)) {
            return Err(unsupported("bind directive in a generate block"));
        }
        if let Some(unit) = container
            && !matches!(unit.kind, UnitKind::Module | UnitKind::Interface)
        {
            return Err(invalid(format!(
                "a bind directive is specified in a module, an interface or the compilation unit, not in `{}` (23.11)",
                unit.name.spelling
            )));
        }
        // The target: a hierarchical identifier with constant bit selects.
        let mut position = 1;
        let mut segments = Vec::new();
        let mut selected = false;
        loop {
            segments.push(text(position).to_string());
            position += 1;
            while text(position) == "[" {
                selected = true;
                let mut depth = 0usize;
                loop {
                    match text(position) {
                        "[" => depth += 1,
                        "]" => depth -= 1,
                        "" => return Err(invalid("a bind target has an unclosed select")),
                        _ => {}
                    }
                    position += 1;
                    if depth == 0 {
                        break;
                    }
                }
            }
            if text(position) != "." {
                break;
            }
            position += 1;
        }
        let target = segments.join(".");
        if text(position) == ":" {
            return Err(unsupported(format!(
                "bind directive to a list of instances of `{target}`"
            )));
        }
        let name = crate::normalize_identifier(&target);
        let instance_here = container
            .is_some_and(|unit| declares_instance(declarations, unit, &generate_blocks, &name));
        if segments.len() > 1 || selected || instance_here {
            return Err(unsupported(format!(
                "bind directive to the instance `{target}`"
            )));
        }
        let Some(scope) = declared(&name, range) else {
            return Err(unsupported(format!(
                "bind directive whose target `{target}` is not a module or an interface of its compilation unit"
            )));
        };
        let end_keyword = match scope.kind {
            UnitKind::Module => "endmodule",
            UnitKind::Interface => "endinterface",
            _ => {
                return Err(invalid(format!(
                    "the target scope `{target}` of a bind directive is a module or an interface (23.11)"
                )));
            }
        };
        let bound = crate::normalize_identifier(text(position));
        if scope.kind == UnitKind::Interface
            && let Some(unit) = declared(&bound, range)
            && unit.kind != UnitKind::Interface
        {
            return Err(invalid(format!(
                "a bind directive into the interface `{target}` instantiates an interface or a checker, not `{bound}` (23.11)"
            )));
        }
        let Some(end) = (scope.tokens.start..scope.tokens.end)
            .rev()
            .find(|&index| tokens[index].text == end_keyword)
        else {
            return Err(invalid(format!("`{target}` has no `{end_keyword}`")));
        };
        if !instantiates(&text, &significant, tokens, position) {
            return Err(invalid(format!(
                "a bind directive into `{target}` gives an instantiation of a module, an interface, a program or a checker (23.11)"
            )));
        }
        let start = significant[position];
        // The interface pass places a reference by its offset in the source,
        // where the instantiation still is in its container: an actual that
        // names an interface port or instance there would be taken for one of
        // the container's.
        let handles: HashSet<String> = std::iter::once(scope)
            .chain(container)
            .flat_map(|unit| interface_handles(declarations, unit))
            .collect();
        if let Some(handle) = actual_names(&text, position)
            .map(crate::normalize_identifier)
            .find(|text| handles.contains(text))
        {
            return Err(unsupported(format!(
                "bind directive into `{target}` whose instantiation names the interface port or instance `{handle}`"
            )));
        }
        inserted.push((
            end,
            TokenRange {
                start,
                end: range.end,
            },
        ));
        removed.push(range);
    }
    inserted.sort_by_key(|(end, range)| (*end, range.start));
    removed.sort_by_key(|range| range.start);
    let mut spliced = Vec::with_capacity(tokens.len());
    let mut insertions = inserted.iter().peekable();
    let mut removals = removed.iter().peekable();
    for (index, token) in tokens.iter().enumerate() {
        while let Some((_, range)) = insertions.next_if(|(end, _)| *end == index) {
            spliced.extend_from_slice(&tokens[range.start..range.end]);
        }
        while removals.next_if(|range| range.end <= index).is_some() {}
        if removals.peek().is_some_and(|range| range.start <= index) {
            continue;
        }
        spliced.push(token.clone());
    }
    Ok(spliced)
}

/// Whether the significant tokens from `position` start an instantiation:
/// a type name, parameter values, an instance name with unpacked
/// dimensions, and the parenthesized port connections every instance has
/// (IEEE 1800-2023 A.4.1.1). A declaration does not.
fn instantiates<'t>(
    text: &impl Fn(usize) -> &'t str,
    significant: &[usize],
    tokens: &[Token],
    mut position: usize,
) -> bool {
    let name = |position: usize| {
        significant.get(position).is_some_and(|&index| {
            matches!(
                tokens[index].kind,
                TokenKind::Identifier | TokenKind::EscapedIdentifier
            )
        })
    };
    // Past the group opened at `position`, if it is `open`.
    let group = |mut position: usize, open: &str, close: &str| {
        let mut depth = 0usize;
        loop {
            match text(position) {
                "" => return None,
                t if t == open => depth += 1,
                t if t == close => depth -= 1,
                _ => {}
            }
            position += 1;
            if depth == 0 {
                return Some(position);
            }
        }
    };
    if !name(position) {
        return false;
    }
    position += 1;
    if text(position) == "#" {
        if text(position + 1) != "(" {
            return false;
        }
        let Some(next) = group(position + 1, "(", ")") else {
            return false;
        };
        position = next;
    }
    if !name(position) {
        return false;
    }
    position += 1;
    while text(position) == "[" {
        let Some(next) = group(position, "[", "]") else {
            return false;
        };
        position = next;
    }
    text(position) == "("
}

/// The identifiers of the parameter values and port connections of the
/// instantiation whose significant tokens start at `position`, up to its
/// `;`: not its type and instance names, the names of named connections
/// (`.b(q)`), or the names after a `.` of hierarchical names and members or
/// after a `::` of scope-qualified names (`P::b`), and the scopes before it.
fn actual_names<'t>(
    text: &impl Fn(usize) -> &'t str,
    mut position: usize,
) -> impl Iterator<Item = &'t str> {
    let mut names = Vec::new();
    let mut depth = 0usize;
    let mut previous = "";
    loop {
        let token = text(position);
        match token {
            "" | ";" if depth == 0 => break,
            "(" | "[" | "{" => depth += 1,
            ")" | "]" | "}" => depth = depth.saturating_sub(1),
            _ if depth > 0
                && !matches!(previous, "." | "::")
                && text(position + 1) != "::"
                && token
                    .starts_with(|c: char| c.is_ascii_alphabetic() || c == '_' || c == '\\') =>
            {
                names.push(token);
            }
            _ => {}
        }
        previous = token;
        position += 1;
    }
    names.into_iter()
}

/// Whether `unit` declares an instance named `name` outside its generate
/// blocks, whose instances a bind target does not name unqualified.
fn declares_instance(
    declarations: &Declarations,
    unit: &Unit,
    generate_blocks: &[TokenRange],
    name: &str,
) -> bool {
    unit.instantiations.iter().any(|&id| {
        let instantiation = declarations.instantiation(id);
        !generate_blocks
            .iter()
            .any(|block| contains(*block, instantiation.tokens))
            && instantiation
                .instances
                .iter()
                .any(|instance| crate::normalize_identifier(&instance.name.spelling) == name)
    })
}

/// The names of the interface ports and interface instances of `unit`.
fn interface_handles(declarations: &Declarations, unit: &Unit) -> Vec<String> {
    let interface = |name: &str| {
        let name = crate::normalize_identifier(name);
        name == "interface"
            || declarations.units.iter().any(|unit| {
                unit.kind == UnitKind::Interface
                    && crate::normalize_identifier(&unit.name.spelling) == name
            })
    };
    let ports = unit.ports.iter().filter_map(|port| {
        let name = match &port.kind {
            PortKind::Named(name) | PortKind::Explicit { name, .. } => name,
            PortKind::NonAnsi(_) | PortKind::Wildcard => return None,
        };
        let first = (port.tokens.start..port.tokens.end)
            .map(|index| &declarations.tokens[index])
            .find(|token| !token.is_trivia())?;
        interface(&first.text).then(|| crate::normalize_identifier(&name.spelling))
    });
    let instances = unit.instantiations.iter().flat_map(|&id| {
        let instantiation = declarations.instantiation(id);
        interface(&instantiation.name.spelling)
            .then(|| {
                instantiation
                    .instances
                    .iter()
                    .map(|instance| crate::normalize_identifier(&instance.name.spelling))
            })
            .into_iter()
            .flatten()
    });
    ports.chain(instances).collect()
}
