//! Function-statement restrictions across calls from final procedures (9.2.3).
use super::*;
use const_functions::CallArguments;

pub(super) fn validate(subroutines: &[&Subroutine], processes: &[InitialProcess]) -> Converted<()> {
    if !processes.iter().any(InitialProcess::is_final) {
        return Ok(());
    }
    let definitions = subroutines
        .iter()
        .map(|subroutine| (subroutine.name.as_str(), *subroutine))
        .collect();
    let mut forbidden = HashSet::default();
    let mut calls = Vec::new();
    let mut schedules = HashSet::default();
    let mut runtime_calls = Vec::new();
    for subroutine in subroutines {
        let (invalid, _, callees) = effects(
            subroutine.body.iter().chain(&subroutine.sensitivity_body),
            &definitions,
        );
        // Functions cannot enable even an untimed task (13.4 b).
        if subroutine.is_task || invalid {
            forbidden.insert(subroutine.name.clone());
        }
        calls.push((&subroutine.name, callees));
        // Unlike the function-statement restrictions, 13.4.4 concerns an
        // executed event scheduling operation. A folded-away NBA is harmless.
        let (_, schedules_events, callees) = effects(subroutine.body.iter(), &definitions);
        if schedules_events {
            schedules.insert(subroutine.name.clone());
        }
        runtime_calls.push((&subroutine.name, callees));
    }
    loop {
        let previous = (forbidden.len(), schedules.len());
        for (name, callees) in &calls {
            if callees.iter().any(|callee| forbidden.contains(callee)) {
                forbidden.insert((*name).clone());
            }
        }
        for (name, callees) in &runtime_calls {
            if callees.iter().any(|callee| schedules.contains(callee)) {
                schedules.insert((*name).clone());
            }
        }
        if previous == (forbidden.len(), schedules.len()) {
            break;
        }
    }
    for process in processes.iter().filter(|process| process.is_final()) {
        let (invalid, _, callees) = effects(
            process.body.iter().chain(&process.validation_body),
            &definitions,
        );
        if invalid {
            return Err(unsupported("timing control in a final procedure"));
        }
        if let Some(callee) = callees
            .iter()
            .filter(|callee| forbidden.contains(*callee))
            .min()
        {
            return Err(unsupported(format!(
                "call to `{callee}` in a final procedure does not obey function-statement restrictions"
            )));
        }
        let (_, _, callees) = effects(process.body.iter(), &definitions);
        if let Some(callee) = callees
            .iter()
            .filter(|callee| schedules.contains(*callee))
            .min()
        {
            return Err(unsupported(format!(
                "call to `{callee}` from a final procedure schedules an event after the function returns; IEEE 1800-2023 13.4.4 requires an initial/always process"
            )));
        }
    }
    Ok(())
}

/// Include discarded branches, expression calls and lvalue selectors: legality
/// is a property of the authored procedure, not just the path executed now.
pub(super) fn effects<'a>(
    body: impl Iterator<Item = &'a Stmt>,
    definitions: &HashMap<&str, &Subroutine>,
) -> (bool, bool, HashSet<String>) {
    let mut invalid = false;
    let mut schedules_events = false;
    let mut calls = Calls {
        definitions,
        names: HashSet::default(),
        defaults: Vec::new(),
        expanded: HashSet::default(),
    };
    for statement in body {
        statement.walk_without_detached_children(&mut |statement| {
            invalid |= events::blocks(statement)
                || matches!(
                    statement,
                    Stmt::Delay { .. }
                        | Stmt::WaitEvent(_)
                        | Stmt::WaitImplicit(_)
                        | Stmt::Wait(_)
                        | Stmt::AssertionRegion { .. }
                        | Stmt::WaitFork
                        | Stmt::Fork {
                            join: celox_design::ForkJoin::All | celox_design::ForkJoin::Any,
                            ..
                        }
                );
            schedules_events |= matches!(
                statement,
                Stmt::Fork {
                    join: celox_design::ForkJoin::None,
                    ..
                } | Stmt::Assign {
                    nonblocking: true,
                    ..
                } | Stmt::AssignConcat {
                    nonblocking: true,
                    ..
                }
            );
            if let Stmt::Call { name, args, .. } = statement {
                calls.record(name, CallArguments::Expressions(args));
            }
            let mut call = |callee: &Callee, args| {
                if let Callee::Subroutine(name) = callee {
                    calls.record(name, args);
                }
            };
            for expr in lifetimes::stmt_exprs(statement) {
                const_functions::visit_expr_calls(expr, &mut call);
            }
            let targets = match statement {
                Stmt::Assign { lhs, .. } => std::slice::from_ref(lhs),
                Stmt::AssignConcat { parts, .. } => parts.as_slice(),
                _ => &[],
            };
            for target in targets {
                if let LValue::Select {
                    msb,
                    lsb,
                    array_slice_width,
                    ..
                } = target
                {
                    for index in [Some(msb), Some(lsb), array_slice_width.as_ref()]
                        .into_iter()
                        .flatten()
                    {
                        const_functions::visit_const_calls(index, &mut call);
                    }
                }
            }
        });
    }
    calls.expand_defaults();
    (invalid, schedules_events, calls.names)
}

struct Calls<'map, 'definition> {
    definitions: &'map HashMap<&'definition str, &'definition Subroutine>,
    names: HashSet<String>,
    defaults: Vec<&'definition Expr>,
    expanded: HashSet<(String, usize)>,
}

impl Calls<'_, '_> {
    fn record(&mut self, name: &str, args: CallArguments<'_>) {
        self.names.insert(name.to_owned());
        let Some(&subroutine) = self.definitions.get(name) else {
            return;
        };
        for (index, parameter) in subroutine.params.iter().enumerate() {
            if !args.is_present(index)
                && let Some(default) = &parameter.default
                && self.expanded.insert((name.to_owned(), index))
            {
                self.defaults.push(default);
            }
        }
    }

    fn expand_defaults(&mut self) {
        while let Some(default) = self.defaults.pop() {
            const_functions::visit_expr_calls(default, &mut |callee, args| {
                if let Callee::Subroutine(name) = callee {
                    self.record(name, args);
                }
            });
        }
    }
}
