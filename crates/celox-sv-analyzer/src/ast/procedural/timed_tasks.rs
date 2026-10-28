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
    let mut timed = HashSet::default();
    loop {
        let previous = timed.len();
        for subroutine in &subroutines {
            if requires_process(&subroutine.body, &timed, &definitions) {
                timed.insert(subroutine.name.clone());
            }
        }
        if previous == timed.len() {
            break;
        }
    }
    for subroutine in &subroutines {
        let is_timed = timed.contains(&subroutine.name);
        if !subroutine.is_task {
            let (blocking, _, calls) = super::final_procedures::effects(
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
        // Retaining static subroutine locals is an existing frontend limit.
        // Do not reinitialize them at each invocation when calls can overlap.
        let mut static_local = false;
        let mut automatic_nba = false;
        for statement in subroutine.body.iter().chain(&subroutine.sensitivity_body) {
            statement.walk(&mut |statement| match statement {
                Stmt::Local { name, .. } => {
                    static_local |= !subroutine.automatic_locals.contains(name);
                }
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
        // Imported package/$unit subroutine locals are currently allocated
        // per consuming module, unlike the scope's actual shared variables.
        // Reject static formals until they can use that shared storage too.
        if is_timed
            && subroutine.name.contains("::")
            && subroutine
                .params
                .iter()
                .any(|param| !subroutine.automatic_locals.contains(&param.name))
        {
            return Err(unsupported(
                "static arguments in a package or compilation-unit timed task (declare the task automatic)",
            ));
        }
        if is_timed && static_local {
            return Err(unsupported(
                "static local in a timed task (declare it automatic)",
            ));
        }
    }
    for process in processes.iter().filter(|process| process.is_initializer()) {
        if requires_process(&process.body, &timed, &definitions) {
            return Err(unsupported(
                "background process scheduled by a declaration initializer; IEEE 1800-2023 13.4.4 requires an initial/always process",
            ));
        }
    }
    let initial_count = processes.len();
    let mut retained = Vec::new();
    for process in std::mem::take(comb) {
        if !requires_process(&process.body, &timed, &definitions) {
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
        if !requires_process(&process.body, &timed, &definitions) {
            retained.push(process);
            continue;
        }
        if !process.suspendable {
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

fn requires_process(
    body: &[Stmt],
    timed: &HashSet<String>,
    definitions: &HashMap<&str, &Subroutine>,
) -> bool {
    let mut result = false;
    for statement in body {
        statement.walk(&mut |statement| {
            result |= matches!(
                statement,
                Stmt::Delay { .. }
                    | Stmt::WaitEvent(_)
                    | Stmt::WaitImplicit(_)
                    | Stmt::Wait(_)
                    | Stmt::WaitFork
                    | Stmt::Fork { .. }
            );
        });
    }
    let (_, _, calls) = super::final_procedures::effects(body.iter(), definitions);
    result || calls.iter().any(|name| timed.contains(name))
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
