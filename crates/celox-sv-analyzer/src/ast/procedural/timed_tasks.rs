//! Propagate suspension through bound task calls before selecting process kernels.
use super::*;

pub(in crate::ast) fn classify<'a>(
    subroutines: impl Iterator<Item = &'a Subroutine>,
    comb: &mut Vec<CombProcess>,
    ff: &mut Vec<FfProcess>,
    processes: &mut Vec<InitialProcess>,
) -> Result<(), AnalyzerError> {
    let subroutines: Vec<_> = subroutines.collect();
    super::final_procedures::validate(&subroutines, processes)?;
    let definitions: HashMap<_, _> = subroutines
        .iter()
        .map(|sub| (sub.name.as_str(), *sub))
        .collect();
    let known = subroutine_effects(&subroutines, &definitions);
    let timed: HashSet<String> = known
        .iter()
        .filter(|(_, effects)| effects.suspends)
        .map(|(name, _)| name.clone())
        .collect();
    // A wait condition only registers the events it reads `triggered` of
    // itself; one read in a called function would miss its wakeup.
    let triggered_readers = callers_closure(&subroutines, |subroutine, readers| {
        reads_triggered(&subroutine.body, readers)
    });
    concurrent_actions(&subroutines, &definitions, &known, comb, ff, processes)?;
    if !triggered_readers.is_empty() {
        for body in bodies(&subroutines, comb, ff, processes) {
            let mut found = false;
            for statement in body {
                statement.walk(&mut |statement| {
                    if let Stmt::Wait(condition) = statement {
                        const_functions::visit_expr_calls(condition, &mut |callee, _| {
                            found |= matches!(callee, Callee::Subroutine(name) if triggered_readers.contains(name));
                        });
                    }
                });
            }
            if found {
                return Err(unsupported(
                    "`triggered` read by a function called in a wait condition",
                ));
            }
        }
    }
    for subroutine in &subroutines {
        let is_timed = timed.contains(&subroutine.name);
        if !subroutine.is_task {
            let (blocking, _, calls, _) = super::final_procedures::effects(
                subroutine.body.iter().chain(&subroutine.sensitivity_body),
                &definitions,
            );
            if blocking
                || calls.iter().any(|name| {
                    definitions
                        .get(name.as_str())
                        .is_some_and(|sub| sub.is_task)
                })
            {
                return Err(unsupported(
                    "timed task call or blocking statement in a function",
                ));
            }
        }
        let mut automatic_nba = false;
        for statement in subroutine.body.iter().chain(&subroutine.sensitivity_body) {
            statement.walk(&mut |statement| match statement {
                Stmt::Assign {
                    lhs,
                    nonblocking: true,
                    ..
                } => {
                    automatic_nba |= subroutine
                        .automatic_locals
                        .iter()
                        .any(|name| name == lhs.name());
                }
                Stmt::AssignConcat {
                    parts,
                    nonblocking: true,
                    ..
                } => {
                    automatic_nba |= parts.iter().any(|part| {
                        subroutine
                            .automatic_locals
                            .iter()
                            .any(|name| name == part.name())
                    });
                }
                _ => {}
            });
        }
        // IEEE 1800-2023 13.3.2 forbids NBAs to automatic task storage,
        // including untimed callees whose caller owns a suspended activation.
        if subroutine.is_task && automatic_nba {
            return Err(unsupported(
                "nonblocking assignment to an automatic task variable",
            ));
        }
        // The static formals of a package or `$unit` task are variables of
        // that scope when the task suspends (see `lower_subroutine`); others
        // would be allocated per consuming module.
        if is_timed
            && subroutine.name.contains("::")
            && subroutine.params.iter().any(|param| {
                !subroutine.automatic_locals.contains(&param.name)
                    && !subroutine.persistent_locals.contains(&param.name)
            })
        {
            return Err(unsupported(
                "static arguments in a package or compilation-unit timed task (declare the task automatic)",
            ));
        }
    }
    for process in processes.iter().filter(|process| process.is_initializer()) {
        if body_effects(&process.body, &known, &definitions).suspends {
            return Err(unsupported(
                "background process scheduled by a declaration initializer; IEEE 1800-2023 13.4.4 requires an initial/always process",
            ));
        }
    }
    let initial_count = processes.len();
    let mut retained = Vec::new();
    for process in std::mem::take(comb) {
        let effects = body_effects(&process.body, &known, &definitions);
        let blocks = effects.suspends;
        if !blocks && !effects.uses_kernel() {
            if process.kind == CombProcessKind::AlwaysLatch && assigns_nonblocking(&process.body) {
                let wait =
                    timing::implicit_event_kind(&process.body, &process.sensitivity_body, true);
                let mut body = process.body;
                // always_latch executes once at time zero, then rearms its
                // implicit sensitivity before the queued NBA updates land.
                body.push(wait);
                processes.push(InitialProcess {
                    creation_order: process.creation_order,
                    site: process.process_site,
                    condition: process.condition,
                    body: vec![Stmt::Loop {
                        kind: LoopKind::Forever,
                        init: Vec::new(),
                        condition: None,
                        step: Vec::new(),
                        body,
                    }],
                    kind: celox_design::ProcessKind::Regular,
                    validation_body: Vec::new(),
                    synthetic: false,
                    repeats: true,
                });
                continue;
            }
            if process.kind == CombProcessKind::AlwaysStar && assigns_nonblocking(&process.body) {
                return Err(unsupported("nonblocking assignment inside always_comb"));
            }
            retained.push(process);
            continue;
        }
        // An `always @*` is an ordinary `always` procedure (IEEE 1800-2023
        // 9.2.2.2.2): calling a timed task makes it a process that waits on
        // its implicit event list before each run of its statement.
        // `always_comb` and `always_latch` may not suspend (9.2.2.2).
        let context = match process.kind {
            CombProcessKind::AlwaysStar => None,
            CombProcessKind::AlwaysLatch => Some("an always_latch body"),
            CombProcessKind::AlwaysComb | CombProcessKind::ContinuousAssign => {
                Some("an always_comb body")
            }
        };
        if let Some(context) = context {
            if !blocks {
                return Err(unsupported(format!(
                    "event trigger or process method in {context}"
                )));
            }
            return Err(AnalyzerError::TimingControlOutsideProcess {
                construct: "timed task call",
                context,
            });
        }
        // The frontend reads the wait's statements as it reads those of a
        // combinational `always @*`, folded branches and the defaults of
        // omitted arguments included.
        let mut body = vec![timing::implicit_event(
            &process.body,
            &process.sensitivity_body,
        )];
        body.extend(process.body);
        processes.push(InitialProcess {
            creation_order: process.creation_order,
            site: process.process_site,
            condition: process.condition,
            body: vec![Stmt::Loop {
                kind: LoopKind::Forever,
                init: Vec::new(),
                condition: None,
                step: Vec::new(),
                body,
            }],
            kind: celox_design::ProcessKind::Regular,
            validation_body: Vec::new(),
            synthetic: false,
            repeats: true,
        });
    }
    *comb = retained;
    let mut retained = Vec::new();
    for process in std::mem::take(ff) {
        let effects = body_effects(&process.body, &known, &definitions);
        let blocks = effects.suspends;
        if !blocks && !effects.uses_kernel() {
            retained.push(process);
            continue;
        }
        // An `always_ff` that triggers an event runs in a process kernel too;
        // it only may not suspend.
        if blocks && !process.suspendable {
            return Err(unsupported("timed task call in always_ff"));
        }
        let mut body = vec![Stmt::WaitEvent(
            process
                .events
                .iter()
                .map(|event| EventItemBase {
                    edge: match event.edge {
                        FfEdge::Pos => EventEdge::Pos,
                        FfEdge::Neg => EventEdge::Neg,
                    },
                    expr: Expr::Ident(event.signal.clone()),
                    iff: None,
                })
                .collect(),
        )];
        body.extend(process.body);
        processes.push(InitialProcess {
            creation_order: process.creation_order,
            site: process.site.clone(),
            condition: None,
            body: vec![Stmt::Loop {
                kind: LoopKind::Forever,
                init: Vec::new(),
                condition: None,
                step: Vec::new(),
                body,
            }],
            kind: celox_design::ProcessKind::Regular,
            validation_body: Vec::new(),
            synthetic: false,
            repeats: true,
        });
    }
    *ff = retained;
    if processes.len() != initial_count {
        processes.sort_by_key(|process| (!process.is_initializer(), process.creation_order));
    }
    Ok(())
}

/// The subroutines for which `holds` is true, given those already found:
/// a property of a body that also holds of a call of a subroutine having it.
fn callers_closure(
    subroutines: &[&Subroutine],
    mut holds: impl FnMut(&Subroutine, &HashSet<String>) -> bool,
) -> HashSet<String> {
    let mut found = HashSet::default();
    loop {
        let previous = found.len();
        for subroutine in subroutines {
            if !found.contains(&subroutine.name) && holds(subroutine, &found) {
                found.insert(subroutine.name.clone());
            }
        }
        if previous == found.len() {
            return found;
        }
    }
}

/// Every body of statements: those of subroutines and of processes.
fn bodies<'a>(
    subroutines: &'a [&'a Subroutine],
    comb: &'a [CombProcess],
    ff: &'a [FfProcess],
    processes: &'a [InitialProcess],
) -> impl Iterator<Item = &'a Vec<Stmt>> {
    subroutines
        .iter()
        .map(|subroutine| &subroutine.body)
        .chain(comb.iter().map(|process| &process.body))
        .chain(ff.iter().map(|process| &process.body))
        .chain(processes.iter().map(|process| &process.body))
}

/// What statements do to the scheduler, directly or through the subroutines
/// they call.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Effects {
    /// They may suspend the calling process, or need its process kernel as
    /// if they did: a delay, an event control, a wait, a fork.
    suspends: bool,
    /// They wake, schedule or control other processes: an event trigger,
    /// `disable fork`, `kill`, keys returned to a semaphore.
    controls: bool,
    /// They read or update the state of a process or synchronization
    /// object, such as `e.triggered` or `process::self()`, which only a
    /// process kernel can run.
    reads_process: bool,
    nonblocking: bool,
}

impl Effects {
    fn union(self, other: Self) -> Self {
        Self {
            suspends: self.suspends || other.suspends,
            controls: self.controls || other.controls,
            reads_process: self.reads_process || other.reads_process,
            nonblocking: self.nonblocking || other.nonblocking,
        }
    }

    /// Whether only a process kernel can run them without suspending.
    fn uses_kernel(self) -> bool {
        self.controls || self.reads_process
    }
}

/// The effects of `statement` itself, without its child statements and
/// the subroutines it calls.
fn statement_effects(statement: &Stmt) -> Effects {
    let mut effects = Effects::default();
    match statement {
        Stmt::Assign { nonblocking, .. } | Stmt::AssignConcat { nonblocking, .. } => {
            effects.nonblocking = *nonblocking;
        }
        // Its delay control is a child statement.
        Stmt::DelayedNonblocking { .. } => effects.nonblocking = true,
        // A fork needs a process kernel even when it does not wait.
        Stmt::Delay { .. }
        | Stmt::WaitEvent(_)
        | Stmt::WaitImplicit { .. }
        | Stmt::Wait(_)
        | Stmt::WaitFork
        | Stmt::Fork { .. }
        | Stmt::DeferActive
        | Stmt::AssertionRegion { .. } => effects.suspends = true,
        Stmt::DisableFork => effects.controls = true,
        Stmt::If { .. }
        | Stmt::Case { .. }
        | Stmt::Loop { .. }
        | Stmt::Break
        | Stmt::Continue
        | Stmt::Return(_)
        | Stmt::Call { .. }
        | Stmt::Eval(_)
        | Stmt::SystemTask { .. }
        | Stmt::Local { .. } => {}
    }
    for expr in lifetimes::stmt_exprs(statement) {
        effects = effects.union(expression_effects(expr));
    }
    effects
}

/// The effects of the process operations in `expr`.
fn expression_effects(expr: &Expr) -> Effects {
    let mut effects = Effects::default();
    const_functions::visit_expr_calls(expr, &mut |callee, _| {
        if let Callee::System(SystemFunction::Process(op)) = callee {
            match events::process_effect(*op) {
                events::ProcessEffect::Blocks => effects.suspends = true,
                events::ProcessEffect::Schedules => effects.controls = true,
                events::ProcessEffect::Reads => effects.reads_process = true,
            }
        }
    });
    effects
}

/// The effects of `body` itself, with the defaults of the omitted arguments
/// its calls evaluate, and the subroutines it calls.
fn own_effects(
    body: &[Stmt],
    definitions: &HashMap<&str, &Subroutine>,
) -> (Effects, HashSet<String>) {
    #[cfg(test)]
    OWN_EFFECT_WALKS.with(|count| count.set(count.get() + 1));
    let mut effects = Effects::default();
    for statement in body {
        statement.walk(&mut |statement| {
            effects = effects.union(statement_effects(statement));
        });
    }
    // A call evaluates the defaults of its omitted arguments.
    let (_, _, calls, defaults) = super::final_procedures::effects(body.iter(), definitions);
    let effects = defaults.into_iter().fold(effects, |effects, default| {
        effects.union(expression_effects(default))
    });
    (effects, calls)
}

/// `effects` with those `known` of the subroutines `calls` names.
fn with_callees(
    effects: Effects,
    calls: &HashSet<String>,
    known: &HashMap<String, Effects>,
) -> Effects {
    calls
        .iter()
        .filter_map(|name| known.get(name))
        .fold(effects, |effects, callee| effects.union(*callee))
}

/// The effects of `body`, with those `known` of the subroutines it calls.
fn body_effects(
    body: &[Stmt],
    known: &HashMap<String, Effects>,
    definitions: &HashMap<&str, &Subroutine>,
) -> Effects {
    let (effects, calls) = own_effects(body, definitions);
    with_callees(effects, &calls, known)
}

/// The effects of every subroutine, through the subroutines it calls. Each
/// body is walked once; the effects then propagate through the calls until
/// they settle, one pass per step of the longest call chain.
fn subroutine_effects(
    subroutines: &[&Subroutine],
    definitions: &HashMap<&str, &Subroutine>,
) -> HashMap<String, Effects> {
    let own = subroutines
        .iter()
        .map(|subroutine| own_effects(&subroutine.body, definitions))
        .collect::<Vec<_>>();
    let mut known = HashMap::default();
    loop {
        let mut changed = false;
        for (subroutine, (effects, calls)) in subroutines.iter().zip(&own) {
            let effects = with_callees(*effects, calls, &known);
            if known.get(&subroutine.name) != Some(&effects) {
                known.insert(subroutine.name.clone(), effects);
                changed = true;
            }
        }
        if !changed {
            return known;
        }
    }
}

/// A concurrent assertion action runs in the Reactive region of the
/// assertion's process (IEEE 1800-2023 16.14.1), which must not suspend
/// before the next clocking event or schedule work after the action: no
/// nonblocking assignment, process, event trigger or other process control.
/// The action, with the subroutines it calls, is judged by the effects that
/// select process kernels; reading process or event state is allowed.
fn concurrent_actions(
    subroutines: &[&Subroutine],
    definitions: &HashMap<&str, &Subroutine>,
    known: &HashMap<String, Effects>,
    comb: &[CombProcess],
    ff: &[FfProcess],
    processes: &[InitialProcess],
) -> Result<(), AnalyzerError> {
    let mut actions = Vec::new();
    for body in bodies(subroutines, comb, ff, processes) {
        for statement in body {
            statement.walk(&mut |statement| {
                if let Stmt::If {
                    then_body,
                    else_body,
                    ..
                } = statement
                {
                    for branch in [then_body, else_body] {
                        if let [Stmt::AssertionRegion { reactive: true }, action @ ..] =
                            branch.as_slice()
                        {
                            actions.push(action);
                        }
                    }
                }
            });
        }
    }
    for action in actions {
        let effects = body_effects(action, known, definitions);
        let (_, _, calls, _) = super::final_procedures::effects(action.iter(), definitions);
        let callee = |has: fn(&Effects) -> bool| {
            calls
                .iter()
                .filter(|name| known.get(*name).is_some_and(has))
                .min()
                .map_or(String::new(), |name| format!(" (through `{name}`)"))
        };
        if effects.suspends {
            return Err(unsupported(format!(
                "concurrent assertion action that may suspend{}",
                callee(|effects| effects.suspends)
            )));
        }
        if effects.controls {
            return Err(unsupported(format!(
                "concurrent assertion action that triggers an event or controls a process{}",
                callee(|effects| effects.controls)
            )));
        }
        if effects.nonblocking {
            return Err(unsupported(format!(
                "concurrent assertion action that performs a nonblocking assignment{}",
                callee(|effects| effects.nonblocking)
            )));
        }
    }
    Ok(())
}

/// Whether `body` reads the `triggered` state of an event, directly or
/// through a call of one of `readers`.
fn reads_triggered(body: &[Stmt], readers: &HashSet<String>) -> bool {
    let mut result = false;
    for statement in body {
        statement.walk(&mut |statement| {
            if let Stmt::Call { name, .. } = statement {
                result |= readers.contains(name);
            }
            for expr in lifetimes::stmt_exprs(statement) {
                result |= events::reads_triggered(expr);
                const_functions::visit_expr_calls(expr, &mut |callee, _| {
                    result |= matches!(callee, Callee::Subroutine(name) if readers.contains(name));
                });
            }
        });
    }
    result
}

fn assigns_nonblocking(body: &[Stmt]) -> bool {
    let mut result = false;
    for statement in body {
        statement.walk(&mut |statement| {
            result |= matches!(
                statement,
                Stmt::Assign {
                    nonblocking: true,
                    ..
                } | Stmt::AssignConcat {
                    nonblocking: true,
                    ..
                }
            );
        });
    }
    result
}

#[cfg(test)]
thread_local! {
    /// Walks of a body for its own effects.
    static OWN_EFFECT_WALKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
mod tests {
    #[test]
    fn subroutine_effects_walk_each_body_once() {
        // Each task of a chain calls the next, and the last one waits: the
        // suspension takes a pass per call to reach the first task, which
        // walks each body once, not once per pass.
        let walks = |chain: usize| {
            let tasks = (0..chain)
                .map(|i| {
                    if i + 1 < chain {
                        format!("task automatic t{i}(); t{}(); endtask\n", i + 1)
                    } else {
                        format!("task automatic t{i}(); #1; endtask\n")
                    }
                })
                .collect::<String>();
            let source = format!("module Top;\n{tasks}initial t0();\nendmodule\n");
            super::OWN_EFFECT_WALKS.with(|count| count.set(0));
            crate::analyze_source(&source, std::path::Path::new("effect-walks.sv"))
                .expect("the module analyzes");
            super::OWN_EFFECT_WALKS.with(|count| count.get())
        };
        let step = walks(3) - walks(2);
        assert_eq!(walks(18) - walks(2), 16 * step);
    }
}
