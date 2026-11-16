//! The execution of a combinational process into the logic it defines, and
//! the variables its statements may write.

use super::*;

impl Comb<'_, '_> {
    /// Execute one combinational process and return the logic it defines.
    pub fn lower_process(
        &mut self,
        body: &[sv::ir::Stmt],
    ) -> Result<Vec<LogicPath<SourceVarId>>, sv::AnalyzerError> {
        let store = self.exec_block(Store::default(), &[], body)?;
        // Every bit a process may write must be defined on every path;
        // otherwise it would hold its previous value (a latch).
        let mut visited = HashSet::default();
        let mut writes = Vec::new();
        let mut unrolled = HashSet::default();
        static_writes(
            self.m,
            body,
            &mut visited,
            &mut writes,
            &mut unrolled,
            false,
        );
        // The drivers of a variable count the bits a select in loops writes
        // in their iterations (see `ProcModule::unrolled_select_writes`).
        // A loop folded rather than unrolled writes the whole variable or
        // element at each run-time position instead; the process then runs
        // as a guarded unit, which writes only the positions the select
        // takes.
        for &id in &unrolled {
            let mut counted: Vec<_> = writes
                .iter()
                .filter(|(written, _)| *written == id)
                .map(|(_, access)| (access.lsb, access.msb))
                .collect();
            counted.sort_unstable();
            let beyond = store.get(&id).is_some_and(|range| {
                range.ranges.iter().any(|(&lsb, (value, width, _))| {
                    // Whether the counted bits cover `lsb` up to its top.
                    let msb = lsb + width - 1;
                    let mut next = lsb;
                    for &(low, high) in &counted {
                        if low <= next && next <= high {
                            next = high + 1;
                        }
                    }
                    value.is_some() && next <= msb
                })
            });
            if beyond {
                return Err(unsupported(RUNTIME_LOOP_CONDITION));
            }
        }
        for &(id, access) in &writes {
            let defined = store.get(&id).is_some_and(|range| {
                range
                    .get_parts_ref(access)
                    .is_ok_and(|parts| parts.iter().all(|(value, _)| value.is_some()))
            });
            if !defined {
                return Err(unsupported(format!(
                    "latch inference inside always_comb for `{}` (bits {}:{})",
                    self.m.var(id).path.join("."),
                    access.msb,
                    access.lsb
                )));
            }
        }
        let mut written: Vec<SourceVarId> = store
            .keys()
            .copied()
            .filter(|id| !self.m.is_hidden(*id))
            .collect();
        written.sort_by_key(|id| id.0);
        let mut written_atoms = Vec::new();
        for id in &written {
            if let Some(range) = store.get(id) {
                for (&lsb, (value, width, _)) in &range.ranges {
                    if value.is_some() {
                        written_atoms.push(VarAtomBase::new(*id, lsb, lsb + width - 1));
                    }
                }
            }
        }
        let mut paths = Vec::new();
        for id in written {
            let Some(range) = store.get(&id) else {
                continue;
            };
            let ranges: Vec<_> = range
                .ranges
                .iter()
                .map(|(&lsb, (value, width, origin))| (lsb, value.clone(), *width, *origin))
                .collect();
            for (lsb, value, width, origin) in ranges {
                let Some((expr, sources)) = value else {
                    continue;
                };
                let msb = lsb + width - 1;
                let relative = BitAccess::new(lsb - origin, msb - origin);
                let expr = self.slice(expr, relative)?;
                let target = BitAccess::new(lsb, msb);
                if sources
                    .iter()
                    .any(|source| source.id == id && source.access.overlaps(&target))
                {
                    return Err(unsupported(format!(
                        "latch inference inside always_comb for `{}` (self dependency at bits {msb}:{lsb})",
                        self.m.var(id).path.join(".")
                    )));
                }
                let previous_sources = sources
                    .iter()
                    .copied()
                    .filter(|source| source.id != id || !source.access.overlaps(&target))
                    .filter(|source| {
                        written_atoms.iter().any(|written| {
                            written.id == source.id && written.access.overlaps(&source.access)
                        })
                    })
                    .collect();
                paths.push(LogicPath {
                    transparent_copy: self.continuous,
                    target: LogicPathTarget::Var(VarAtomBase::new(id, lsb, msb)),
                    sources: sources
                        .into_iter()
                        .filter(|source| !self.m.is_hidden(source.id))
                        .collect(),
                    previous_sources,
                    address_sources: HashSet::default(),
                    local_inputs: Vec::new(),
                    order_before: HashSet::default(),
                    comb_capture_enable_sites: Vec::new(),
                    comb_capture_enable_always: false,
                    pre_lower_nodes: Vec::new(),
                    expr,
                });
            }
        }
        if !self.events.observers.is_empty() {
            let mut sensitivity: Sources = std::mem::take(&mut self.events.effect_sensitivity);
            for path in &paths {
                sensitivity.extend(path.sources.iter().copied());
            }
            // The assertion control state guards the reports but is not
            // read by the source, so `$asserton` does not rerun the process
            // (IEEE 1800-2023 9.2.2.2.1, 20.11).
            let sensitivity: Vec<_> = subtract_written_sensitivity(sensitivity, &written_atoms)
                .into_iter()
                .filter(|atom| !self.m.is_hidden(atom.id) && !self.m.is_assertion_control(atom.id))
                .collect();
            for observer in &mut self.events.observers {
                observer.sensitivity = sensitivity.clone();
                observer.written_input_atoms = observer
                    .observed_inputs
                    .iter()
                    .chain(observer.position_inputs.iter())
                    .copied()
                    .filter(|atom| {
                        written_atoms.iter().any(|written| {
                            written.id == atom.id && written.access.overlaps(&atom.access)
                        })
                    })
                    .collect();
                let mut written_inputs: Vec<_> = observer
                    .written_input_atoms
                    .iter()
                    .map(|atom| atom.id)
                    .collect();
                written_inputs.sort_by_key(|id| id.0);
                written_inputs.dedup();
                observer.written_inputs = written_inputs;
            }
        }
        Ok(paths)
    }
}

/// `atoms` without the bits in `written`.
pub(in crate::lowering) fn subtract_written_sensitivity(
    atoms: Sources,
    written: &[VarAtomBase<SourceVarId>],
) -> Sources {
    let mut result = Sources::default();
    for atom in atoms {
        let mut ranges = vec![(atom.access.lsb, atom.access.msb)];
        for written in written.iter().filter(|written| written.id == atom.id) {
            ranges = ranges
                .into_iter()
                .flat_map(|(lsb, msb)| {
                    if written.access.msb < lsb || written.access.lsb > msb {
                        return vec![(lsb, msb)];
                    }
                    let mut kept = Vec::new();
                    if lsb < written.access.lsb {
                        kept.push((lsb, written.access.lsb - 1));
                    }
                    if written.access.msb < msb {
                        kept.push((written.access.msb + 1, msb));
                    }
                    kept
                })
                .collect();
        }
        for (lsb, msb) in ranges {
            result.insert(VarAtomBase::new(atom.id, lsb, msb));
        }
    }
    result
}

/// The module variables (and constant bit ranges) a statement list may
/// write, following subroutine calls. The variables of `unrolled` are
/// those of selects in loops whose bits are counted in each iteration (see
/// `ProcModule::unrolled_select_writes`).
pub(in crate::lowering) fn static_writes(
    m: &ProcModule<'_>,
    stmts: &[sv::ir::Stmt],
    visited: &mut HashSet<String>,
    writes: &mut Vec<(SourceVarId, BitAccess)>,
    unrolled: &mut HashSet<SourceVarId>,
    functions_only: bool,
) {
    let mut calls = Vec::new();
    let record = |this: &ProcModule<'_>,
                  lvalue: &sv::ir::LValue,
                  loops: &LoopValues,
                  writes: &mut Vec<(SourceVarId, BitAccess)>,
                  unrolled: &mut HashSet<SourceVarId>| {
        let Some(id) = this.id(lvalue.name()) else {
            return;
        };
        if this.is_hidden(id) {
            return;
        }
        let variable = this.var(id);
        let sv::ir::LValue::Select { msb, lsb, .. } = lvalue else {
            writes.push((id, full(variable.width)));
            return;
        };
        // A select at positions only the variables of the loops around it
        // make run-time values writes the bits it selects in their
        // iterations, as the unrolled loops write them.
        if let Some(accesses) = this.unrolled_select_writes(lvalue, loops) {
            writes.extend(accesses.into_iter().map(|access| (id, access)));
            unrolled.insert(id);
            return;
        }
        let bounds =
            sv::typecheck::eval_const_expr_with_types(msb, this.constants, this.parameter_types)
                .zip(sv::typecheck::eval_const_expr_with_types(
                    lsb,
                    this.constants,
                    this.parameter_types,
                ))
                .and_then(|(msb, lsb)| constant_select_write(variable, msb, lsb));
        let access = match bounds {
            Some((Some(access), _)) => access,
            // A constant select entirely outside the vector writes
            // nothing.
            Some((None, _)) => return,
            // A run-time select may write any bit; the written value keeps
            // the others, so it defines the whole vector, or the one
            // element its constant part names.
            None => this
                .dynamic_select_access(lvalue)
                .unwrap_or(full(variable.width)),
        };
        writes.push((id, access));
    };
    let variable_type = |name: &str| m.integral_variable_type(name);
    let bounds = LoopBounds {
        constants: m.constants,
        parameter_types: m.parameter_types,
        variable_type: &variable_type,
    };
    bounds.walk(stmts, &mut LoopValues::new(), &mut |stmt, loops| {
        match stmt {
            sv::ir::Stmt::Assign { lhs, .. } => record(m, lhs, loops, writes, unrolled),
            sv::ir::Stmt::AssignConcat { parts, .. } => {
                for part in parts {
                    record(m, part, loops, writes, unrolled);
                }
            }
            sv::ir::Stmt::Call { name, args, .. } => calls.push((name.clone(), args.clone())),
            _ => {}
        }
        if functions_only {
            let mut expressions = Vec::new();
            stmt_calls(stmt, &mut expressions);
            calls.extend(expressions.into_iter().filter_map(|(callee, args)| {
                if let sv::ir::Callee::Subroutine(name) = callee {
                    Some((name, args))
                } else {
                    None
                }
            }));
        }
    });
    let mut index = 0;
    let mut seen_defaults = super::super::procedural::SeenDefaults::default();
    while index < calls.len() {
        let (name, args) = calls[index].clone();
        index += 1;
        let Some(subroutine) = m.subroutine(&name).cloned() else {
            continue;
        };
        if functions_only {
            // Defaults execute at each call that omits an argument, even if
            // this subroutine's body has already been visited at another call.
            let mut defaults = Vec::new();
            default_calls(&subroutine, &args, &mut defaults, &mut seen_defaults);
            calls.extend(defaults.into_iter().filter_map(|(callee, args)| {
                if let sv::ir::Callee::Subroutine(name) = callee {
                    Some((name, args))
                } else {
                    None
                }
            }));
        }
        for (param, arg) in subroutine.params.iter().zip(&args) {
            if param.direction.is_written()
                && let Some(lvalues) = arg.as_ref().and_then(lvalue_from_expr)
            {
                for lvalue in &lvalues {
                    record(m, lvalue, &LoopValues::new(), writes, unrolled);
                }
            }
        }
        if visited.insert(name) && (!functions_only || !subroutine.is_task) {
            static_writes(
                m,
                &subroutine.body,
                visited,
                writes,
                unrolled,
                functions_only,
            );
            if functions_only {
                // Sensitivity is syntactic: folded branches still exclude
                // their written expressions, including writes in callees.
                static_writes(
                    m,
                    &subroutine.sensitivity_body,
                    visited,
                    writes,
                    unrolled,
                    true,
                );
            }
        }
    }
}
