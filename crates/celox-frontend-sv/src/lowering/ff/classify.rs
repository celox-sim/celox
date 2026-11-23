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
    /// Groups of variables that `ref` ports alias within one instance (see
    /// `with_alias_groups`).
    pub(super) alias_groups: Vec<Vec<SourceVarId>>,
    /// Per-process private `WORKING` shadows for variables in an aliased group.
    pub(super) alias_work: HashMap<SourceVarId, SourceVarId>,
    /// Variables read from the working region.
    pub(super) working: HashSet<SourceVarId>,
    /// Variables a nonblocking assignment writes.
    pub(super) nonblocking: HashSet<SourceVarId>,
    /// The bits whose working value is the process result.
    pub(super) targets: Vec<(SourceVarId, BitAccess)>,
    /// The words `$readmemh`/`$readmemb` calls may load, which the process
    /// assigns by the call although the file gives only some of them.
    pub(super) memory_loads: Vec<(SourceVarId, BitAccess)>,
}

impl Ff<'_, '_> {
    /// Record a written lvalue: its constant bit range, or the whole variable.
    pub(super) fn record_target(&mut self, lvalue: &sv::ir::LValue, blocking: bool) {
        self.record_target_bits(lvalue, blocking, None);
    }

    /// Record a written lvalue as [`Self::record_target`] does, the bits
    /// `unrolled` of a select in loops it writes in their iterations (see
    /// `ProcModule::unrolled_select_writes`) its targets.
    fn record_target_bits(
        &mut self,
        lvalue: &sv::ir::LValue,
        blocking: bool,
        unrolled: Option<Vec<BitAccess>>,
    ) {
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
        let element_lvalue = dynamic_array_element_lvalue(
            lvalue,
            self.m.variables,
            self.m.name_to_id,
            self.m.constants,
            self.m.parameter_types,
        )
        .is_some();
        let bit_lvalue = !element_lvalue
            && dynamic_array_bit_lvalue(
                lvalue,
                self.m.variables,
                self.m.name_to_id,
                self.m.constants,
                self.m.parameter_types,
            )
            .is_some();
        let window = (!element_lvalue)
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
            .and_then(|write| write.window);
        let whole = window.unwrap_or(BitAccess::new(0, variable.width - 1));
        if constant.is_none() {
            // `write` takes the same order: a constant range, then one
            // run-time element, then a read-modify-write of the variable.
            // A run-time bit select inside a single constant element has a
            // narrow `window` and does not need sparse promotion by itself,
            // yet remains compatible with sparse publication when another
            // write in the same process addresses a run-time element.
            if element_lvalue || (bit_lvalue && window.is_none()) {
                self.writes.element_writes.insert(id);
            } else if !bit_lvalue {
                self.writes.other_writes.insert(id);
            }
        }
        if let Some(unrolled) = unrolled.filter(|_| constant.is_none()) {
            self.writes
                .targets
                .extend(unrolled.into_iter().map(|access| (id, access)));
            return;
        }
        let access = match lvalue {
            sv::ir::LValue::Ident(_) => BitAccess::new(0, variable.width - 1),
            sv::ir::LValue::Select { .. } => constant.unwrap_or(whole),
        };
        self.writes.targets.push((id, access));
    }

    /// The variables the subroutines `body` calls assign, as `(all_assigned,
    /// nonblocking_assigned)`, directly or through further calls and `ref` formals.
    pub(super) fn subroutine_assignments(
        &self,
        body: &[sv::ir::Stmt],
    ) -> (HashSet<SourceVarId>, HashSet<SourceVarId>) {
        let mut root_calls = Vec::new();
        for stmt in body {
            stmt.walk(&mut |stmt| stmt_calls(stmt, &mut root_calls));
        }
        let mut calls: Vec<(
            sv::ir::Callee,
            Vec<Option<sv::ir::Expr>>,
            HashMap<String, SourceVarId>,
        )> = root_calls
            .into_iter()
            .map(|(callee, args)| (callee, args, HashMap::default()))
            .collect();
        let mut assigned = HashSet::default();
        let mut nonblocking = HashSet::default();
        let mut visited: HashSet<(String, Vec<(String, SourceVarId)>)> = HashSet::default();
        let mut seen_defaults = super::super::procedural::SeenDefaults::default();
        let resolve_id = |name: &str, refs: &HashMap<String, SourceVarId>| {
            refs.get(name).copied().or_else(|| self.m.id(name))
        };
        let bind_arg_id = |expr: &sv::ir::Expr, refs: &HashMap<String, SourceVarId>| {
            lvalue_from_expr(expr).and_then(|lvalues| match lvalues.as_slice() {
                [lvalue] => resolve_id(lvalue.name(), refs),
                _ => None,
            })
        };
        while let Some((callee, args, caller_refs)) = calls.pop() {
            if let sv::ir::Callee::Object(op) = &callee {
                for lhs in written_object_arguments(op, &args) {
                    if let Some(id) = resolve_id(lhs.name(), &caller_refs) {
                        assigned.insert(id);
                    }
                }
            }
            let sv::ir::Callee::Subroutine(name) = callee else {
                continue;
            };
            let Some(subroutine) = self.m.subroutine(&name) else {
                continue;
            };
            let mut sub_calls = Vec::new();
            default_calls(subroutine, &args, &mut sub_calls, &mut seen_defaults);
            for (c, a) in sub_calls {
                calls.push((c, a, caller_refs.clone()));
            }
            let mut child_refs = HashMap::default();
            for (param, arg) in subroutine.params.iter().zip(args.iter()) {
                if !param.direction.is_reference() {
                    continue;
                }
                let Some(arg) = arg else {
                    continue;
                };
                if param.leaves.is_empty() {
                    if let Some(id) = bind_arg_id(arg, &caller_refs) {
                        child_refs.insert(param.name.clone(), id);
                    }
                } else if let sv::ir::Expr::Concat(parts) = arg
                    && parts.len() == param.leaves.len()
                {
                    for (leaf, part) in param.leaves.iter().zip(parts) {
                        if let Some(id) = bind_arg_id(part, &caller_refs) {
                            child_refs.insert(leaf.clone(), id);
                        }
                    }
                }
            }
            let mut binding_key: Vec<(String, SourceVarId)> =
                child_refs.iter().map(|(k, &v)| (k.clone(), v)).collect();
            binding_key.sort();
            if !visited.insert((name, binding_key)) {
                continue;
            }
            for stmt in &subroutine.body {
                stmt.walk(&mut |stmt| {
                    match stmt {
                        sv::ir::Stmt::Assign {
                            lhs,
                            nonblocking: is_nba,
                            ..
                        } => {
                            if let Some(id) = resolve_id(lhs.name(), &child_refs) {
                                assigned.insert(id);
                                if *is_nba {
                                    nonblocking.insert(id);
                                }
                            }
                        }
                        sv::ir::Stmt::AssignConcat {
                            parts,
                            nonblocking: is_nba,
                            ..
                        } => {
                            for part in parts {
                                if let Some(id) = resolve_id(part.name(), &child_refs) {
                                    assigned.insert(id);
                                    if *is_nba {
                                        nonblocking.insert(id);
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                    let mut nested = Vec::new();
                    stmt_calls(stmt, &mut nested);
                    for (c, a) in nested {
                        calls.push((c, a, child_refs.clone()));
                    }
                });
            }
        }
        (assigned, nonblocking)
    }

    /// Collect the variables a body writes, following calls into subroutines.
    pub(super) fn scan(&mut self, stmts: &[sv::ir::Stmt], _visited: &mut HashSet<String>) {
        self.scan_with_refs(
            stmts,
            &HashMap::default(),
            &mut HashSet::default(),
            &mut Vec::new(),
        );
    }

    fn scan_with_refs(
        &mut self,
        stmts: &[sv::ir::Stmt],
        refs: &HashMap<String, sv::ir::LValue>,
        visited: &mut HashSet<(String, Vec<(String, String)>)>,
        active: &mut Vec<String>,
    ) {
        // Each target, whether a blocking assignment writes it, and the
        // bits it writes in the iterations of the loops around it.
        let mut writes: Vec<(sv::ir::LValue, bool, Option<Vec<BitAccess>>)> = Vec::new();
        let m = &*self.m;
        let variable_type = |name: &str| m.integral_variable_type(name);
        let bounds = LoopBounds {
            constants: m.constants,
            parameter_types: m.parameter_types,
            variable_type: &variable_type,
            calls: m,
        };
        bounds.walk(
            stmts,
            &mut LoopValues::new(),
            &mut |stmt, loops| match stmt {
                sv::ir::Stmt::Assign {
                    lhs, nonblocking, ..
                } => {
                    let resolved = resolve_lvalue(lhs, refs);
                    let unrolled = m.unrolled_select_writes(&resolved, loops);
                    writes.push((resolved, !nonblocking, unrolled));
                }
                sv::ir::Stmt::AssignConcat {
                    parts, nonblocking, ..
                } => {
                    writes.extend(parts.iter().map(|part| {
                        let resolved = resolve_lvalue(part, refs);
                        let unrolled = m.unrolled_select_writes(&resolved, loops);
                        (resolved, !nonblocking, unrolled)
                    }));
                }
                // `$readmemh` writes its destination immediately, and
                // `$timeformat` the design's format of `%t`.
                sv::ir::Stmt::SystemTask { task, args, .. } => {
                    if let Some(destination) = readmem_destination(*task, args) {
                        let resolved =
                            resolve_lvalue(&sv::ir::LValue::Ident(destination.to_string()), refs);
                        writes.push((resolved, true, None));
                    }
                    if *task == sv::ir::SystemTask::Timeformat {
                        writes.push((sv::ir::LValue::Ident(TIME_FORMAT.to_string()), true, None));
                    }
                }
                _ => {}
            },
        );
        for (lvalue, blocking, unrolled) in writes {
            self.record_target_bits(&lvalue, blocking, unrolled);
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
                    let resolved = resolve_lvalue(&lhs, refs);
                    self.record_target(&resolved, true);
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
                        let resolved = resolve_lvalue(&lvalue, refs);
                        self.record_target(&resolved, true);
                    }
                    continue;
                }
                sv::ir::Callee::Object(op)
                    if matches!(
                        *op,
                        sv::ir::ObjectOp::New { .. }
                            | sv::ir::ObjectOp::ScopeRandomize(_)
                            | sv::ir::ObjectOp::Container {
                                op: sv::ir::ContainerOp::Shuffle,
                                ..
                            }
                    ) =>
                {
                    // Construction, shuffling and scope randomization, which
                    // constructs its object, consume the process RNG. Register it
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
                    let resolved = resolve_lvalue(&lvalue, refs);
                    self.record_target(&resolved, true);
                }
                continue;
            }
            let Some(subroutine) = self.m.subroutine(&name).cloned() else {
                continue;
            };
            default_calls(&subroutine, &args, &mut calls, &mut seen_defaults);
            let mut child_refs = HashMap::default();
            for (param, arg) in subroutine.params.iter().zip(args.iter()) {
                if param.direction.is_reference() {
                    let Some(arg) = arg else {
                        continue;
                    };
                    if param.leaves.is_empty() {
                        if let Some(lvalues) = lvalue_from_expr(arg)
                            && let [lvalue] = lvalues.as_slice()
                        {
                            child_refs.insert(param.name.clone(), resolve_lvalue(lvalue, refs));
                        }
                    } else if let sv::ir::Expr::Concat(parts) = arg
                        && parts.len() == param.leaves.len()
                    {
                        for (leaf, part) in param.leaves.iter().zip(parts) {
                            if let Some(lvalues) = lvalue_from_expr(part)
                                && let [lvalue] = lvalues.as_slice()
                            {
                                child_refs.insert(leaf.clone(), resolve_lvalue(lvalue, refs));
                            }
                        }
                    }
                    continue;
                }
                if param.direction.is_written()
                    && let Some(lvalues) = arg.as_ref().and_then(lvalue_from_expr)
                {
                    for lvalue in lvalues {
                        let resolved = resolve_lvalue(&lvalue, refs);
                        self.record_target(&resolved, true);
                    }
                }
            }
            if active.contains(&name) {
                continue;
            }
            let mut binding_key: Vec<(String, String)> = child_refs
                .iter()
                .map(|(k, v)| (k.clone(), format!("{v:?}")))
                .collect();
            binding_key.sort();
            if visited.insert((name.clone(), binding_key)) {
                active.push(name);
                self.scan_with_refs(&subroutine.body, &child_refs, visited, active);
                active.pop();
            }
        }
    }
}

fn resolve_lvalue(
    lvalue: &sv::ir::LValue,
    refs: &HashMap<String, sv::ir::LValue>,
) -> sv::ir::LValue {
    match refs.get(lvalue.name()) {
        Some(sv::ir::LValue::Ident(target)) => match lvalue {
            sv::ir::LValue::Ident(_) => sv::ir::LValue::Ident(target.clone()),
            sv::ir::LValue::Select {
                msb,
                lsb,
                array_slice_width,
                array_slice_reversed,
                window,
                ..
            } => sv::ir::LValue::Select {
                name: target.clone(),
                msb: msb.clone(),
                lsb: lsb.clone(),
                array_slice_width: array_slice_width.clone(),
                array_slice_reversed: *array_slice_reversed,
                window: window.clone(),
            },
        },
        Some(actual @ sv::ir::LValue::Select { .. }) => actual.clone(),
        None => lvalue.clone(),
    }
}
