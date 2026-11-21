//! Project a clocked statement tree onto its assignment destinations.
//! A projected assignment retains the predicates at its source position.
//! The procedural remainder executes once in source order.

use super::*;
use sv::ir::Stmt;

/// Keep each case label, including labels with empty bodies: an earlier
/// matching label still prevents a later one from executing.
fn project(body: &[Stmt], keep: &impl Fn(&str) -> bool, effects: bool) -> Vec<Stmt> {
    body.iter()
        .filter_map(|stmt| match stmt {
            Stmt::Block { id, body } => {
                let body = project(body, keep, effects);
                (!body.is_empty()).then_some(Stmt::Block { id: *id, body })
            }
            Stmt::If {
                condition,
                then_body,
                else_body,
            } => {
                let then_body = project(then_body, keep, effects);
                let else_body = project(else_body, keep, effects);
                (!then_body.is_empty() || !else_body.is_empty()).then(|| Stmt::If {
                    condition: condition.clone(),
                    then_body,
                    else_body,
                })
            }
            Stmt::Case {
                kind,
                selector,
                items,
                default,
            } => {
                let mut items = items.clone();
                for item in &mut items {
                    item.body = project(&item.body, keep, effects);
                }
                let default = default.as_ref().map(|body| project(body, keep, effects));
                (items.iter().any(|item| !item.body.is_empty())
                    || default.as_ref().is_some_and(|body| !body.is_empty()))
                .then(|| Stmt::Case {
                    kind: *kind,
                    selector: selector.clone(),
                    items,
                    default,
                })
            }
            Stmt::Assign { lhs, .. } => keep(lhs.name()).then(|| stmt.clone()),
            _ => effects.then(|| stmt.clone()),
        })
        .collect()
}

/// Include the address and predicates sampled at the assignment, as well as its value.
fn read_names(body: &[Stmt]) -> HashSet<String> {
    let mut values = HashSet::default();
    let mut indices = HashSet::default();
    for stmt in body {
        stmt.visit(
            &mut |e| procedural::expr_idents(e, &mut values),
            &mut |lhs| {
                if let sv::ir::LValue::Select {
                    msb,
                    lsb,
                    array_slice_width,
                    window,
                    ..
                } = lhs
                {
                    for expr in [
                        Some(msb),
                        Some(lsb),
                        array_slice_width.as_deref(),
                        window.as_ref().and_then(|w| w.at.as_deref()),
                    ]
                    .into_iter()
                    .flatten()
                    {
                        procedural::const_idents(expr, &mut indices);
                    }
                }
            },
            &mut |_| {},
        );
    }
    values.extend(indices);
    values
}

impl FfGraph {
    /// Return the part still requiring procedural execution. Each accepted
    /// destination is installed in the same arena as the other edge roots.
    pub(in crate::lowering) fn extract(
        &mut self,
        pm: &mut procedural::ProcModule<'_>,
        trigger: &TriggerSet<SourceVarId>,
        body: &[Stmt],
        excluded: &HashSet<String>,
    ) -> Option<Vec<Stmt>> {
        let mut candidates = Vec::new();
        let mut seen = HashSet::default();
        let mut active = HashSet::default();
        let mut opaque = HashSet::default();
        let mut barrier = false;
        for stmt in body {
            let mut calls_expr = false;
            let mut calls_index = false;
            stmt.visit(
                &mut |e| calls_expr |= procedural::expr_calls(e, &mut |_| true),
                &mut |lhs| calls_index |= procedural::lvalue_calls(lhs, &mut |_| true),
                &mut |_| {},
            );
            if calls_expr || calls_index {
                return None;
            }
            stmt.walk(&mut |s| match s {
                Stmt::Assign {
                    lhs, nonblocking, ..
                } => {
                    if seen.insert(lhs.name()) {
                        candidates.push(lhs.name());
                    }
                    if !nonblocking {
                        active.insert(lhs.name());
                    }
                }
                Stmt::AssignConcat {
                    parts, nonblocking, ..
                } => {
                    opaque.extend(parts.iter().map(|lhs| lhs.name()));
                    if !nonblocking {
                        active.extend(parts.iter().map(|lhs| lhs.name()));
                    }
                }
                Stmt::Local { name, .. } => {
                    active.insert(name);
                    opaque.insert(name);
                }
                // A loop stays at its original position and retains all of
                // its destinations. Siblings can still be extracted.
                Stmt::Loop { .. } => s.visit(
                    &mut |_| {},
                    &mut |lhs| {
                        opaque.insert(lhs.name());
                    },
                    &mut |_| {},
                ),
                Stmt::Block { .. }
                | Stmt::If { .. }
                | Stmt::Case { .. }
                | Stmt::Break
                | Stmt::Continue
                | Stmt::SystemTask {
                    task: sv::ir::SystemTask::Display(_) | sv::ir::SystemTask::Write(_),
                    ..
                } => {}
                // A call can change arbitrary inputs. A transfer of control
                // can skip subsequent stores. Both must stay ordered.
                _ => barrier = true,
            });
        }
        if barrier || active.iter().any(|name| excluded.contains(*name)) {
            return None;
        }
        // A blocking writer and its readers need a common symbolic store to
        // distinguish reads before the assignment from reads after it. NBA
        // writers do not impose this dependency. Keep components in source order.
        let mut adjacent = HashMap::<&str, Vec<&str>>::default();
        if !active.is_empty() {
            let effects = project(body, &|_| false, true);
            let effect_reads = read_names(&effects);
            opaque.extend(active.iter().copied().filter(|n| effect_reads.contains(*n)));
            for &target in &candidates {
                let statements = project(body, &|name| name == target, false);
                for read in read_names(&statements) {
                    if let Some(&writer) = active.get(read.as_str()) {
                        adjacent.entry(target).or_default().push(writer);
                        adjacent.entry(writer).or_default().push(target);
                    }
                }
            }
        }
        let mut visited = HashSet::default();
        let mut extracted = HashSet::default();
        for target in candidates {
            if visited.contains(target) {
                continue;
            }
            let mut group = HashSet::default();
            let mut pending = vec![target];
            while let Some(name) = pending.pop() {
                if visited.insert(name) {
                    group.insert(name);
                    pending.extend(adjacent.remove(name).unwrap_or_default());
                }
            }
            if group
                .iter()
                .any(|name| opaque.contains(name) || excluded.contains(*name))
            {
                continue;
            }
            let projection = project(body, &|name| group.contains(name), false);
            let outside = active.difference(&group).filter_map(|n| pm.id(n)).collect();
            if self
                .try_add(pm, trigger, &projection, excluded, &outside)
                .is_some()
            {
                extracted.extend(group);
            }
        }
        (!extracted.is_empty()).then(|| project(body, &|name| !extracted.contains(name), true))
    }
}
