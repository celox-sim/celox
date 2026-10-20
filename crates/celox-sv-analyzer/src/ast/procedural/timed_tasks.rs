//! Propagate suspension through bound task calls before selecting process kernels.
use super::*;

pub(in crate::ast) fn classify<'a>(
    subroutines: impl Iterator<Item = &'a Subroutine>,
    comb: &[CombProcess],
    ff: &mut Vec<FfProcess>,
    processes: &mut Vec<InitialProcess>,
) -> Result<(), AnalyzerError> {
    let subroutines: Vec<_> = subroutines.collect();
    let mut timed = HashSet::default();
    loop {
        let previous = timed.len();
        for subroutine in &subroutines {
            if suspends(&subroutine.body, &timed) {
                timed.insert(subroutine.name.clone());
            }
        }
        if previous == timed.len() {
            break;
        }
    }
    for subroutine in &subroutines {
        let is_timed = timed.contains(&subroutine.name);
        if is_timed && !subroutine.is_task {
            return Err(unsupported("timed task call in a function"));
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
    if comb.iter().any(|process| suspends(&process.body, &timed)) {
        return Err(unsupported("timed task call in a combinational process"));
    }
    let initial_count = processes.len();
    let mut retained = Vec::new();
    for process in std::mem::take(ff) {
        if !suspends(&process.body, &timed) {
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
                })
                .collect(),
        )];
        body.extend(process.body);
        processes.push(InitialProcess {
            creation_order: process.creation_order,
            condition: None,
            body: vec![Stmt::Loop {
                kind: LoopKind::Forever,
                init: Vec::new(),
                condition: None,
                step: Vec::new(),
                body,
            }],
            initializer: false,
            synthetic: false,
            repeats: true,
        });
    }
    *ff = retained;
    if processes.len() != initial_count {
        processes.sort_by_key(|process| (!process.initializer, process.creation_order));
    }
    Ok(())
}

fn suspends(body: &[Stmt], timed: &HashSet<String>) -> bool {
    let mut result = false;
    for statement in body {
        statement.walk(&mut |statement| {
            result |= match statement {
                Stmt::Delay { .. } | Stmt::WaitEvent(_) | Stmt::Wait(_) => true,
                Stmt::Call { name, .. } => timed.contains(name),
                _ => false,
            };
        });
    }
    result
}
