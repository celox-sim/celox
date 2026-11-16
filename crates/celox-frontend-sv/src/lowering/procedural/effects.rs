//! What a body or an expression does besides computing its writes: host
//! requests, reads of the simulation state, task calls and suspension.

use super::*;

impl ProcModule<'_> {
    /// Whether evaluating `expr` can make a host request, which suspends
    /// the process: it calls a function that makes one directly, in the
    /// defaults of omitted arguments, or through the subroutines it calls.
    pub fn makes_host_request(&mut self, expr: &sv::ir::Expr) -> bool {
        let mut calls = Vec::new();
        collect_calls(expr, &mut calls);
        let mut seen_defaults = SeenDefaults::default();
        while let Some((callee, args)) = calls.pop() {
            match callee {
                sv::ir::Callee::System(function) if function.is_host_request() => return true,
                sv::ir::Callee::String(sv::ir::StringOp::Format { .. }) => return true,
                sv::ir::Callee::Subroutine(name) => {
                    let Some(subroutine) = self.subroutine(&name) else {
                        continue;
                    };
                    // An omitted argument evaluates its default at the call.
                    default_calls(subroutine, &args, &mut calls, &mut seen_defaults);
                    if self.subroutine_makes_host_request(&name) {
                        return true;
                    }
                }
                _ => {}
            }
        }
        false
    }

    /// Whether the body of subroutine `name` can make a host request,
    /// including through the calls it makes and their omitted defaults.
    pub(in crate::lowering) fn subroutine_makes_host_request(&mut self, name: &str) -> bool {
        if let Some(&known) = self.requesting_subroutines.get(name) {
            return known;
        }
        let mut pending = vec![name.to_string()];
        let mut visited = HashSet::default();
        let mut found = false;
        while let Some(name) = pending.pop() {
            if found {
                break;
            }
            if !visited.insert(name.clone()) {
                continue;
            }
            let Some(subroutine) = self.subroutine(&name) else {
                continue;
            };
            let mut calls = Vec::new();
            for stmt in &subroutine.body {
                stmt.walk(&mut |stmt| {
                    if let sv::ir::Stmt::SystemTask { task, .. } = stmt {
                        found |= task.is_host_request();
                    }
                    stmt_calls(stmt, &mut calls);
                });
            }
            let mut seen_defaults = SeenDefaults::default();
            while let Some((callee, args)) = calls.pop() {
                match callee {
                    sv::ir::Callee::System(function) => found |= function.is_host_request(),
                    sv::ir::Callee::String(op) => {
                        found |= matches!(op, sv::ir::StringOp::Format { .. })
                    }
                    sv::ir::Callee::Subroutine(callee) => {
                        if let Some(subroutine) = self.subroutine(&callee) {
                            default_calls(subroutine, &args, &mut calls, &mut seen_defaults);
                        }
                        pending.push(callee);
                    }
                    sv::ir::Callee::Object(_)
                    | sv::ir::Callee::Checked { .. }
                    | sv::ir::Callee::Lookup { .. }
                    | sv::ir::Callee::Stream(_) => {}
                }
            }
        }
        self.requesting_subroutines.insert(name.to_string(), found);
        found
    }

    /// Whether a process calls a system function that reads the state of
    /// the simulation, such as `$urandom` or `$time`, including through user
    /// subroutines and omitted argument defaults.
    /// Whether `body`, or a subroutine it calls, runs `$readmemh` or
    /// `$readmemb`, which reads its file when it executes (IEEE 1800-2023
    /// 21.4): imperative code runs it.
    pub fn loads_memory_file(&self, body: &[sv::ir::Stmt]) -> bool {
        let mut pending: Vec<&[sv::ir::Stmt]> = vec![body];
        let mut visited = HashSet::default();
        while let Some(statements) = pending.pop() {
            let mut loads = false;
            let mut calls = Vec::new();
            for stmt in statements {
                stmt.walk(&mut |stmt| {
                    loads |= matches!(
                        stmt,
                        sv::ir::Stmt::SystemTask {
                            task: sv::ir::SystemTask::Readmem(_),
                            ..
                        }
                    );
                    stmt_calls(stmt, &mut calls);
                });
            }
            if loads {
                return true;
            }
            for (callee, _) in calls {
                if let sv::ir::Callee::Subroutine(name) = callee
                    && let Some(subroutine) = self.subroutine(&name)
                    && visited.insert(name)
                {
                    pending.push(&subroutine.body);
                }
            }
        }
        false
    }

    pub fn reads_simulation_state(&mut self, body: &[sv::ir::Stmt]) -> bool {
        let mut calls = Vec::new();
        for stmt in body {
            stmt.walk(&mut |stmt| stmt_calls(stmt, &mut calls));
        }
        let mut visited = HashSet::default();
        let mut seen_defaults = SeenDefaults::default();
        while let Some((callee, args)) = calls.pop() {
            let name = match callee {
                sv::ir::Callee::System(function) => {
                    if function.reads_simulation_state() {
                        return true;
                    }
                    continue;
                }
                // A lookup or a stream is a value of its operands.
                sv::ir::Callee::Lookup { .. } | sv::ir::Callee::Stream(_) => continue,
                // String operations use the simulator's heap.
                sv::ir::Callee::String(_)
                | sv::ir::Callee::Object(_)
                | sv::ir::Callee::Checked { .. } => return true,
                sv::ir::Callee::Subroutine(name) => name,
            };
            // A call lowered as its constant value runs nothing.
            if args.is_empty() && self.constant_functions.value(&name).is_some() {
                continue;
            }
            if let Some(subroutine) = self.subroutine(&name) {
                if !subroutine.persistent_locals.is_empty() {
                    return true;
                }
                default_calls(subroutine, &args, &mut calls, &mut seen_defaults);
                if visited.insert(name) {
                    for stmt in &subroutine.body {
                        stmt.walk(&mut |stmt| stmt_calls(stmt, &mut calls));
                    }
                }
            }
        }
        false
    }

    /// Task bodies do not contribute to an implicit always_comb/latch
    /// sensitivity list. Preserve their procedural activation instead of
    /// turning their body reads into symbolic combinational dependencies.
    pub fn calls_task(&self, body: &[sv::ir::Stmt]) -> bool {
        let mut calls = Vec::new();
        for stmt in body {
            stmt.walk(&mut |stmt| stmt_calls(stmt, &mut calls));
        }
        let mut visited = HashSet::default();
        let mut seen_defaults = SeenDefaults::default();
        while let Some((callee, args)) = calls.pop() {
            let sv::ir::Callee::Subroutine(name) = callee else {
                continue;
            };
            if let Some(subroutine) = self.subroutine(&name) {
                if subroutine.is_task {
                    return true;
                }
                default_calls(subroutine, &args, &mut calls, &mut seen_defaults);
                if visited.insert(name) {
                    for stmt in &subroutine.body {
                        stmt.walk(&mut |stmt| stmt_calls(stmt, &mut calls));
                    }
                }
            }
        }
        false
    }

    /// Whether `body` has no effect but its writes: no system task, no
    /// subroutine call, no system function that reads the simulation.
    pub(super) fn is_pure(&self, body: &[sv::ir::Stmt]) -> bool {
        let mut pure = true;
        let mut calls = Vec::new();
        for stmt in body {
            stmt.walk(&mut |stmt| {
                pure &= !matches!(
                    stmt,
                    sv::ir::Stmt::SystemTask { .. }
                        | sv::ir::Stmt::Eval(_)
                        | sv::ir::Stmt::Call { .. }
                        | sv::ir::Stmt::Return(_)
                );
                stmt_calls(stmt, &mut calls);
            });
        }
        pure && calls.iter().all(|(callee, _)| {
            matches!(callee, sv::ir::Callee::System(function) if !function.reads_simulation_state())
        })
    }

    pub(super) fn suspends_or_defers(&self, body: &[sv::ir::Stmt]) -> bool {
        let mut found = false;
        let mut calls = Vec::new();
        let mut visit = |stmt: &sv::ir::Stmt, calls: &mut Vec<_>| {
            found |= matches!(
                stmt,
                sv::ir::Stmt::Delay { .. }
                    | sv::ir::Stmt::WaitEvent(_)
                    | sv::ir::Stmt::WaitImplicit { .. }
                    | sv::ir::Stmt::Wait(_)
                    | sv::ir::Stmt::Assign {
                        nonblocking: true,
                        ..
                    }
                    | sv::ir::Stmt::AssignConcat {
                        nonblocking: true,
                        ..
                    }
            );
            stmt_calls(stmt, calls);
        };
        for stmt in body {
            stmt.walk(&mut |stmt| visit(stmt, &mut calls));
        }
        let mut visited = HashSet::default();
        let mut seen_defaults = SeenDefaults::default();
        while let Some((callee, args)) = calls.pop() {
            let sv::ir::Callee::Subroutine(name) = callee else {
                continue;
            };
            if let Some(subroutine) = self.subroutine(&name) {
                default_calls(subroutine, &args, &mut calls, &mut seen_defaults);
                if visited.insert(name) {
                    for stmt in &subroutine.body {
                        stmt.walk(&mut |stmt| visit(stmt, &mut calls));
                    }
                }
            }
        }
        found
    }
}
