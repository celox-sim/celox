//! The scan of a process body before it is lowered: which variables it
//! writes, how (blocking or nonblocking, a constant range or a run-time
//! element), also through the subroutines it calls.

use super::*;

/// What a process writes and how: the classes its caller passes in, and
/// those the scan of its body records. They decide where a variable is read
/// and written while the process is evaluated, and what its units commit.
#[derive(Default)]
pub(super) struct Writes {
    pub(super) sparse_memories: HashSet<SourceVarId>,
    /// Arrays only edge-triggered processes drive. One this process writes
    /// only by nonblocking element writes, at least one at a run-time
    /// element, joins `sparse_memories`.
    pub(super) sparse_candidates: HashSet<SourceVarId>,
    /// Variables written at a run-time element of an unpacked array.
    pub(super) element_writes: HashSet<SourceVarId>,
    /// Variables written other than at a constant range or one element of
    /// an unpacked array, which a sparse publication does not support.
    pub(super) other_writes: HashSet<SourceVarId>,
    /// Variables a process kernel may write between the evaluation of this
    /// process and the NBA region (see `kernel_window_writes`).
    pub(super) window_writes: HashSet<SourceVarId>,
    /// Variables several edge processes write by blocking assignments (see
    /// `with_shared_variables`).
    pub(super) shared: HashSet<SourceVarId>,
    /// Variables read from the working region.
    pub(super) working: HashSet<SourceVarId>,
    /// Variables a nonblocking assignment writes.
    pub(super) nonblocking: HashSet<SourceVarId>,
    /// The bits whose working value is the process result.
    pub(super) targets: Vec<(SourceVarId, BitAccess)>,
}

impl Ff<'_, '_> {
    /// Record a written lvalue: its constant bit range, or the whole variable.
    pub(super) fn record_target(&mut self, lvalue: &sv::ir::LValue, blocking: bool) {
        let Some(id) = self.m.id(lvalue.name()) else {
            return;
        };
        let variable = self.m.var(id);
        let constant = match lvalue {
            sv::ir::LValue::Ident(_) => None,
            sv::ir::LValue::Select { msb, lsb, .. } => sv::typecheck::eval_const_expr_with_types(
                msb,
                self.m.constants,
                self.m.parameter_types,
            )
            .zip(sv::typecheck::eval_const_expr_with_types(
                lsb,
                self.m.constants,
                self.m.parameter_types,
            ))
            .and_then(|(msb, lsb)| constant_select_write(variable, msb, lsb)),
        };
        // An empty constant write is not a dynamic write of the whole
        // variable. Keep it out of publication and latch-coverage targets.
        if matches!(constant, Some((None, _))) {
            return;
        }
        let constant = constant.and_then(|(access, _)| access);
        if blocking {
            self.writes.working.insert(id);
        } else {
            self.writes.nonblocking.insert(id);
        }
        if self.m.is_hidden(id) {
            return;
        }
        // A run-time bit select within one array element writes only that
        // element (see `write`); a run-time element may be any of them.
        let element_write = dynamic_array_element_lvalue(
            lvalue,
            self.m.variables,
            self.m.name_to_id,
            self.m.constants,
            self.m.parameter_types,
        )
        .is_some();
        let whole = (!element_write)
            .then(|| {
                dynamic_packed_write(
                    lvalue,
                    self.m.variables,
                    self.m.name_to_id,
                    self.m.constants,
                    self.m.parameter_types,
                )
            })
            .flatten()
            .and_then(|write| write.window)
            .unwrap_or(BitAccess::new(0, variable.width - 1));
        if constant.is_none() {
            // `write` takes the same order: a constant range, then one
            // run-time element, then a read-modify-write of the variable.
            if element_write {
                self.writes.element_writes.insert(id);
            } else {
                self.writes.other_writes.insert(id);
            }
        }
        let access = match lvalue {
            sv::ir::LValue::Ident(_) => BitAccess::new(0, variable.width - 1),
            sv::ir::LValue::Select { .. } => constant.unwrap_or(whole),
        };
        self.writes.targets.push((id, access));
    }

    /// The variables the subroutines `body` calls assign, directly or
    /// through further calls.
    pub(super) fn subroutine_assignments(&self, body: &[sv::ir::Stmt]) -> HashSet<SourceVarId> {
        let mut calls = Vec::new();
        for stmt in body {
            stmt.walk(&mut |stmt| stmt_calls(stmt, &mut calls));
        }
        let mut assigned = HashSet::default();
        let mut visited = HashSet::default();
        let mut seen_defaults = super::super::procedural::SeenDefaults::default();
        while let Some((callee, args)) = calls.pop() {
            if let sv::ir::Callee::Object(op) = &callee {
                assigned.extend(
                    written_object_arguments(op, &args)
                        .iter()
                        .filter_map(|lhs| self.m.id(lhs.name())),
                );
            }
            let sv::ir::Callee::Subroutine(name) = callee else {
                continue;
            };
            let Some(subroutine) = self.m.subroutine(&name) else {
                continue;
            };
            default_calls(subroutine, &args, &mut calls, &mut seen_defaults);
            if !visited.insert(name) {
                continue;
            }
            for stmt in &subroutine.body {
                stmt.walk(&mut |stmt| {
                    match stmt {
                        sv::ir::Stmt::Assign { lhs, .. } => {
                            assigned.extend(self.m.id(lhs.name()));
                        }
                        sv::ir::Stmt::AssignConcat { parts, .. } => {
                            assigned.extend(parts.iter().filter_map(|part| self.m.id(part.name())));
                        }
                        _ => {}
                    }
                    stmt_calls(stmt, &mut calls);
                });
            }
        }
        assigned
    }

    /// Collect the variables a body writes, following calls into subroutines.
    pub(super) fn scan(&mut self, stmts: &[sv::ir::Stmt], visited: &mut HashSet<String>) {
        let mut writes: Vec<(sv::ir::LValue, bool)> = Vec::new();
        for stmt in stmts {
            stmt.walk(&mut |stmt| match stmt {
                sv::ir::Stmt::Assign {
                    lhs, nonblocking, ..
                } => writes.push((lhs.clone(), !nonblocking)),
                sv::ir::Stmt::AssignConcat {
                    parts, nonblocking, ..
                } => {
                    writes.extend(parts.iter().map(|part| (part.clone(), !nonblocking)));
                }
                // `$readmemh` writes its destination immediately, and
                // `$timeformat` the design's format of `%t`.
                sv::ir::Stmt::SystemTask { task, args, .. } => {
                    if let Some(destination) = readmem_destination(*task, args) {
                        writes.push((sv::ir::LValue::Ident(destination.to_string()), true));
                    }
                    if *task == sv::ir::SystemTask::Timeformat {
                        writes.push((sv::ir::LValue::Ident(TIME_FORMAT.to_string()), true));
                    }
                }
                _ => {}
            });
        }
        for (lvalue, blocking) in writes {
            self.record_target(&lvalue, blocking);
        }
        // Calls: output arguments and the subroutine bodies, including the
        // calls in the select positions of assignment targets.
        let mut calls = Vec::new();
        for stmt in stmts {
            stmt.walk(&mut |stmt| stmt_calls(stmt, &mut calls));
        }
        let mut seen_defaults = super::super::procedural::SeenDefaults::default();
        while let Some((callee, args)) = calls.pop() {
            if let sv::ir::Callee::Object(op) = &callee {
                for lhs in written_object_arguments(op, &args) {
                    self.record_target(&lhs, true);
                }
            }
            let name = match callee {
                sv::ir::Callee::System(function) => {
                    use sv::ir::SystemFunction;
                    match function {
                        SystemFunction::ScopeRandomize
                        | SystemFunction::ScopeRandomizeEnum
                        | SystemFunction::Urandom
                        | SystemFunction::UrandomRange => {
                            self.urandom_state();
                        }
                        SystemFunction::Process(_) => {
                            self.urandom_state();
                        }
                        SystemFunction::Random if args.is_empty() => {
                            self.dist_seed();
                        }
                        _ => {}
                    }
                    for lvalue in written_system_arguments(function, &args) {
                        self.record_target(&lvalue, true);
                    }
                    continue;
                }
                sv::ir::Callee::Object(op)
                    if matches!(
                        *op,
                        sv::ir::ObjectOp::New { .. }
                            | sv::ir::ObjectOp::Container {
                                op: sv::ir::ContainerOp::Shuffle,
                                ..
                            }
                    ) =>
                {
                    // Construction and shuffling consume the process RNG. Register it
                    // before lower_process freezes the persistent target ranges.
                    self.urandom_state();
                    continue;
                }
                sv::ir::Callee::String(_)
                | sv::ir::Callee::Object(_)
                | sv::ir::Callee::Checked { .. }
                | sv::ir::Callee::Lookup { .. }
                | sv::ir::Callee::Stream(_) => continue,
                sv::ir::Callee::Subroutine(name) => name,
            };
            if let Some(import) = self.m.dpi_imports.get(&name) {
                for lvalue in written_dpi_arguments(import, &args) {
                    self.record_target(&lvalue, true);
                }
                continue;
            }
            let Some(subroutine) = self.m.subroutine(&name).cloned() else {
                continue;
            };
            default_calls(&subroutine, &args, &mut calls, &mut seen_defaults);
            for (param, arg) in subroutine.params.iter().zip(args.iter()) {
                if param.direction.is_written()
                    && let Some(lvalues) = arg.as_ref().and_then(lvalue_from_expr)
                {
                    for lvalue in lvalues {
                        self.record_target(&lvalue, true);
                    }
                }
            }
            if visited.insert(name) {
                self.scan(&subroutine.body, visited);
            }
        }
    }
}
