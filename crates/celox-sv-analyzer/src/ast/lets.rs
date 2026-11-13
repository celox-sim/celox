//! `let` declarations (IEEE 1800-2023 11.12).
//!
//! A `let` among the items of a module, interface or program is expanded
//! where the element calls it, before the element is analyzed: the call
//! becomes the parenthesized body with each formal replaced by its actual,
//! cast to the formal's type where it has one, or by its default. A call is
//! one whose name the scoped lookup at the call resolves to the `let`, and
//! its actuals bind to the formals as a subroutine's do (13.5.4). The body's
//! other names resolve where the `let` is declared: the element's items. A
//! call in a nested scope that declares one of them again would resolve it
//! otherwise, and is rejected. A `let` declared elsewhere is rejected.

use super::*;
use celox_sv_syntax::{
    BodyItemKind, Declarations, Expr, ExprId, ExprKind, LetDeclaration, TokenRange, TypeId,
    UnitKind,
};

/// A call to expand: the `let` it calls and the actual of each formal,
/// `None` for one that takes its default.
struct Expansion {
    call: ExprId,
    declaration: usize,
    actuals: Vec<Option<ExprId>>,
}

/// Expand the calls of the `let` declarations of `tree`.
pub(crate) fn expand(tree: &mut SyntaxTree) -> Converted<()> {
    if tree.declarations().lets.is_empty() {
        return Ok(());
    }
    let mut claimed = HashSet::default();
    let mut expansions = Vec::new();
    for handle in tree.root_scopes()? {
        let scoped = tree.scoped(handle)?;
        if scoped.root_unit()?.kind == UnitKind::Package {
            continue;
        }
        let context = AnalysisContext::unspecialized(&scoped);
        plan(&context, &mut claimed, &mut expansions)?;
    }
    let native = tree.declarations();
    if let Some(declaration) = (0..native.lets.len())
        .find(|index| !claimed.contains(index))
        .map(|index| &native.lets[index])
    {
        return Err(unsupported(format!(
            "`let` `{}` declared other than among the items of a module, interface or program",
            declaration.name.spelling
        )));
    }
    // A call in a body is expanded before the calls of that body copy it,
    // as the body comes first.
    expansions.sort_by_key(|expansion| native.expression(expansion.call).tokens.start);
    let native = tree.declarations_mut();
    for expansion in &expansions {
        apply(native, expansion);
    }
    Ok(())
}

fn name(name: &celox_sv_syntax::Name) -> String {
    instances::normalize_identifier(&name.spelling)
}

fn contains(outer: TokenRange, inner: TokenRange) -> bool {
    outer.start <= inner.start && inner.end <= outer.end
}

/// The calls of the `let` declarations among the items of the element of
/// `tree` to expand, into `expansions`; `claimed` receives those
/// declarations.
fn plan(
    tree: &AnalysisContext<'_>,
    claimed: &mut HashSet<usize>,
    expansions: &mut Vec<Expansion>,
) -> Converted<()> {
    let native = tree.declarations();
    let unit = tree.root_unit()?;
    // The element's own items, through the regions that group them.
    let mut items = HashSet::default();
    let mut pending = unit.items.clone();
    while let Some(id) = pending.pop() {
        let item = native.body_item(id);
        match &item.kind {
            BodyItemKind::Region(nested) => pending.extend(nested.iter().copied()),
            BodyItemKind::Unconverted { category, .. } if category == "let" => {
                items.insert(item.tokens.start);
            }
            _ => {}
        }
    }
    let mut lets: Vec<usize> = (0..native.lets.len())
        .filter(|index| items.contains(&native.lets[*index].tokens.start))
        .collect();
    if lets.is_empty() {
        return Ok(());
    }
    lets.sort_by_key(|index| native.lets[*index].tokens.start);
    let mut named: HashMap<String, usize> = HashMap::default();
    for &index in &lets {
        claimed.insert(index);
        let declared = name(&native.lets[index].name);
        if named.insert(declared.clone(), index).is_some() {
            return Err(AnalyzerError::Parse(format!(
                "`let` `{declared}` is declared twice in `{}`",
                unit.name.spelling
            )));
        }
    }
    // The names each body resolves where it is declared, through the
    // bodies of the earlier `let` declarations it calls.
    let mut free: HashMap<usize, Vec<String>> = HashMap::default();
    for &index in &lets {
        let names = free_names(native, &native.lets[index], &named, &free);
        free.insert(index, names);
    }
    // A nested element has a scope of its own.
    let children = unit
        .children
        .iter()
        .map(|id| native.unit(*id).tokens)
        .collect::<Vec<_>>();
    let callees = native
        .expression_nodes()
        .filter_map(|(_, node)| match &node.kind {
            ExprKind::Call { expression, .. } => Some(*expression),
            _ => None,
        })
        .collect::<HashSet<_>>();
    for (id, node) in native.expression_nodes() {
        let at = node.tokens;
        if !contains(unit.tokens, at) || children.iter().any(|child| contains(*child, at)) {
            continue;
        }
        let (callee, arguments) = match &node.kind {
            ExprKind::Call {
                expression,
                arguments,
            } => match &native.expression(*expression).kind {
                ExprKind::Name(callee) => (callee, arguments.as_slice()),
                _ => continue,
            },
            ExprKind::Name(callee) if !callees.contains(&id) => (callee, &[][..]),
            _ => continue,
        };
        let called = name(callee);
        let Some(&index) = named.get(&called) else {
            continue;
        };
        let declaration = &native.lets[index];
        // In a body, a formal of that `let` is no call.
        if lets.iter().any(|other| {
            let other = &native.lets[*other];
            contains(other.tokens, at) && other.ports.iter().any(|port| name(&port.name) == called)
        }) {
            continue;
        }
        // A nested scope that declares the name again hides the `let`.
        if dimensions::hides_scope_item(tree, at, &called)? {
            continue;
        }
        if at.start < declaration.tokens.end {
            return Err(AnalyzerError::Parse(
                if declaration.tokens.start <= at.start {
                    format!("`let` `{called}` calls itself (IEEE 1800-2023 11.12)")
                } else {
                    format!("`let` `{called}` used before its declaration (IEEE 1800-2023 11.12)")
                },
            ));
        }
        let formals = declaration
            .ports
            .iter()
            .map(|port| Some(name(&port.name)))
            .collect::<Vec<_>>();
        let (actuals, _) = expression_binding::formal_arguments(&called, arguments, &formals)?;
        for (actual, port) in actuals.iter().zip(&declaration.ports) {
            if actual.is_none() && port.default.is_none() {
                return Err(AnalyzerError::InvalidCallArguments {
                    subroutine: called.clone(),
                    detail: format!(
                        "no actual for formal `{}`, which has no default (IEEE 1800-2023 11.12)",
                        name(&port.name)
                    ),
                });
            }
        }
        for hidden in &free[&index] {
            if dimensions::hides_scope_item(tree, at, hidden)? {
                return Err(unsupported(format!(
                    "call of `let` `{called}`, whose body names `{hidden}`, which a scope around \
                     the call declares again"
                )));
            }
        }
        expansions.push(Expansion {
            call: id,
            declaration: index,
            actuals,
        });
    }
    Ok(())
}

/// The names `declaration` refers to other than its formals, in its body,
/// in the defaults and in the types of its formals; for a call of another
/// `let` of `named`, the names of that one, from `free`.
fn free_names(
    native: &Declarations,
    declaration: &LetDeclaration,
    named: &HashMap<String, usize>,
    free: &HashMap<usize, Vec<String>>,
) -> Vec<String> {
    enum Part {
        Expression(ExprId),
        Type(TypeId),
    }
    let formals = declaration
        .ports
        .iter()
        .map(|port| name(&port.name))
        .collect::<HashSet<_>>();
    let mut pending = vec![Part::Expression(declaration.body)];
    for port in &declaration.ports {
        pending.extend(port.default.map(Part::Expression));
        pending.extend(port.data_type.map(Part::Type));
    }
    let mut names = Vec::new();
    while let Some(part) = pending.pop() {
        match part {
            Part::Expression(id) => {
                let kind = &native.expression(id).kind;
                match kind {
                    ExprKind::Name(used) => {
                        let used = name(used);
                        if formals.contains(&used) {
                            continue;
                        }
                        match named.get(&used).and_then(|index| free.get(index)) {
                            Some(called) => names.extend(called.iter().cloned()),
                            None => names.push(used),
                        }
                    }
                    ExprKind::Type(data_type) => pending.push(Part::Type(*data_type)),
                    _ => {}
                }
                kind.map_expressions(|child| {
                    pending.push(Part::Expression(child));
                    child
                });
            }
            Part::Type(id) => {
                let data_type = native.data_type(id);
                if let celox_sv_syntax::DataTypeKind::Named(segments) = &data_type.kind
                    && let Some(segment) = segments.first()
                {
                    names.push(name(&segment.name));
                }
                let mut types = Vec::new();
                data_type.map_parts(
                    &mut |child| {
                        pending.push(Part::Expression(child));
                        child
                    },
                    &mut |child| {
                        types.push(child);
                        child
                    },
                );
                pending.extend(types.into_iter().map(Part::Type));
            }
        }
    }
    names.sort();
    names.dedup();
    names
}

/// Replace the call of `expansion` with the parenthesized body of its `let`.
fn apply(native: &mut Declarations, expansion: &Expansion) {
    let declaration = native.lets[expansion.declaration].clone();
    let mut replacements = HashMap::default();
    for (port, actual) in declaration.ports.iter().zip(&expansion.actuals) {
        let actual = actual.or(port.default).expect("the call is checked");
        // A typed formal converts its actual as an assignment would; an
        // untyped one is the parenthesized actual (11.12).
        let tokens = native.expression(actual).tokens;
        let replacement = match port.data_type {
            Some(data_type) => {
                let target = native.push_expression(Expr {
                    kind: ExprKind::Type(data_type),
                    tokens,
                });
                native.push_expression(Expr {
                    kind: ExprKind::Cast {
                        target,
                        expression: actual,
                    },
                    tokens,
                })
            }
            None => native.push_expression(Expr {
                kind: ExprKind::Group(actual),
                tokens,
            }),
        };
        replacements.insert(name(&port.name), replacement);
    }
    let body = substitute(native, declaration.body, &replacements);
    native.expression_mut(expansion.call).kind = ExprKind::Group(body);
}

/// `id` with each name of `replacements` replaced, also in the types it
/// holds, such as the operand of `type(...)`, copying only the nodes above
/// a replaced name. The traversal keeps its own stack, as the parser does,
/// so that a deep body cannot exhaust the thread's.
fn substitute(
    native: &mut Declarations,
    id: ExprId,
    replacements: &HashMap<String, ExprId>,
) -> ExprId {
    enum Step {
        Enter(ExprId),
        Leave(ExprId, Vec<ExprId>),
    }
    let mut steps = vec![Step::Enter(id)];
    let mut results: Vec<ExprId> = Vec::new();
    while let Some(step) = steps.pop() {
        match step {
            Step::Enter(id) => {
                let node = native.expression(id);
                match &node.kind {
                    ExprKind::Name(used) => {
                        results.push(replacements.get(&name(used)).copied().unwrap_or(id));
                        continue;
                    }
                    ExprKind::Type(data_type) => {
                        let (data_type, tokens) = (*data_type, node.tokens);
                        let substituted = substitute_type(native, data_type, replacements);
                        results.push(if substituted == data_type {
                            id
                        } else {
                            native.push_expression(Expr {
                                kind: ExprKind::Type(substituted),
                                tokens,
                            })
                        });
                        continue;
                    }
                    _ => {}
                }
                let mut children = Vec::new();
                node.kind.map_expressions(|child| {
                    children.push(child);
                    child
                });
                steps.push(Step::Leave(id, children.clone()));
                steps.extend(children.into_iter().rev().map(Step::Enter));
            }
            Step::Leave(id, children) => {
                let substituted = results.split_off(results.len() - children.len());
                if substituted == children {
                    results.push(id);
                    continue;
                }
                let node = native.expression(id);
                let mut next = substituted.into_iter();
                let kind = node
                    .kind
                    .map_expressions(|_| next.next().expect("one replacement per child"));
                let tokens = node.tokens;
                results.push(native.push_expression(Expr { kind, tokens }));
            }
        }
    }
    results.pop().expect("the substituted root")
}

/// Type `id` with each name of `replacements` replaced in the expressions
/// it holds, or `id` itself when none is.
fn substitute_type(
    native: &mut Declarations,
    id: TypeId,
    replacements: &HashMap<String, ExprId>,
) -> TypeId {
    let data_type = native.data_type(id).clone();
    let (mut expressions, mut types) = (Vec::new(), Vec::new());
    data_type.map_parts(
        &mut |child| {
            expressions.push(child);
            child
        },
        &mut |child| {
            types.push(child);
            child
        },
    );
    let substituted_expressions = expressions
        .iter()
        .map(|child| substitute(native, *child, replacements))
        .collect::<Vec<_>>();
    let substituted_types = types
        .iter()
        .map(|child| substitute_type(native, *child, replacements))
        .collect::<Vec<_>>();
    if substituted_expressions == expressions && substituted_types == types {
        return id;
    }
    let (mut expressions, mut types) = (
        substituted_expressions.into_iter(),
        substituted_types.into_iter(),
    );
    let copy = data_type.map_parts(
        &mut |_| expressions.next().expect("one replacement per expression"),
        &mut |_| types.next().expect("one replacement per type"),
    );
    native.push_data_type(copy)
}

#[cfg(test)]
mod tests;
