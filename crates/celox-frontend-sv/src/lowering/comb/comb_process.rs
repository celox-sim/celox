//! The execution of a combinational process into the logic it defines, and
//! the variables its statements may write.

use super::*;

impl Comb<'_, '_> {
    /// The bits `target` of variable `id` that `value` defines (from bit
    /// `origin` of the value), when the value reads bits of `id` itself:
    /// one piece per bit, with the sources of that bit. A continuous
    /// assignment whose bits each read only other bits of its net, such as a
    /// Gray-code decoder `assign b = {1'b0, b[N-1:1]} ^ g;`, settles as those
    /// bits do (IEEE 1800-2023 10.3.2). Any other process, or a bit that
    /// reads itself, would hold its previous value: a latch.
    fn own_bit_pieces(
        &mut self,
        id: SourceVarId,
        value: NodeId,
        sources: &Sources,
        target: BitAccess,
        origin: usize,
    ) -> Result<Vec<(BitAccess, NodeId, Sources)>, sv::AnalyzerError> {
        let message = format!(
            "latch inference inside always_comb for `{}` (self dependency at bits {}:{})",
            self.m.var(id).path.join("."),
            target.msb,
            target.lsb
        );
        let latch = || unsupported(message.clone());
        if !self.continuous {
            return Err(latch());
        }
        let others: Sources = sources
            .iter()
            .copied()
            .filter(|source| source.id != id)
            .collect();
        let mut pieces = Vec::new();
        for bit in target.lsb..=target.msb {
            let relative = BitAccess::new(bit - origin, bit - origin);
            let own = bit_sources(self.arena, value, relative).ok_or_else(latch)?;
            let own: Sources = own.into_iter().filter(|source| source.id == id).collect();
            if own
                .iter()
                .any(|source| source.access.lsb <= bit && bit <= source.access.msb)
            {
                return Err(latch());
            }
            let mut sources = others.clone();
            sources.extend(own);
            pieces.push((
                BitAccess::new(bit, bit),
                self.slice(value, relative)?,
                sources,
            ));
        }
        Ok(pieces)
    }

    /// Whether `node`, the value of bits `window` of variable `id`, reads
    /// those bits only where it keeps them: an arm of a multiplexer that
    /// is the read of the very bits it defines.
    fn holds_only(&self, node: NodeId, id: SourceVarId, window: BitAccess) -> bool {
        match self.arena.get(node) {
            SLTNode::Input {
                variable,
                index,
                access,
                ..
            } if *variable == id && index.is_empty() && *access == window => true,
            SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } => {
                !self.reads(*cond, id, window)
                    && self.holds_only(*then_expr, id, window)
                    && self.holds_only(*else_expr, id, window)
            }
            // A part of a wider value of the variable's bits.
            SLTNode::Slice { expr, access } if access.lsb <= window.lsb => {
                let lsb = window.lsb - access.lsb;
                let inner = BitAccess::new(lsb, lsb + self.width(*expr) - 1);
                self.holds_only(*expr, id, inner)
            }
            SLTNode::Concat(parts) => {
                let mut lsb = window.lsb;
                parts.iter().rev().all(|&(part, width)| {
                    let part_window = BitAccess::new(lsb, lsb + width - 1);
                    lsb += width;
                    self.holds_only(part, id, part_window)
                })
            }
            _ => !self.reads(node, id, window),
        }
    }

    /// Whether `node` may read bits `window` of variable `id`.
    fn reads(&self, node: NodeId, id: SourceVarId, window: BitAccess) -> bool {
        self.node_sources(node).is_none_or(|sources| {
            sources
                .iter()
                .any(|source| source.id == id && source.access.overlaps(&window))
        })
    }
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
                // Bits that no path the process takes writes, such as
                // those of a branch a parameter excludes, keep their value.
                if self.hold_latches {
                    self.held = true;
                    continue;
                }
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
                let Some((value, sources)) = value else {
                    continue;
                };
                let msb = lsb + width - 1;
                let target = BitAccess::new(lsb, msb);
                // A value that reads other bits of its own net defines each
                // bit apart, from the current values of those bits.
                let self_read = sources
                    .iter()
                    .any(|source| source.id == id && source.access.overlaps(&target));
                // A value that keeps the bits on some paths, and reads them
                // nowhere else, is a latch (IEEE 1800-2023 9.2.2.2,
                // 9.2.2.3): it reads the bits' previous value, and is the
                // same however often it is evaluated.
                let held = self_read && !self.continuous && self.hold_latches && {
                    let relative = BitAccess::new(lsb - origin, msb - origin);
                    let sliced = self.slice(value, relative)?;
                    self.holds_only(sliced, id, target)
                };
                self.held |= held;
                let split = self_read && !held;
                let pieces = if split {
                    self.own_bit_pieces(id, value, &sources, target, origin)?
                } else {
                    let relative = BitAccess::new(lsb - origin, msb - origin);
                    vec![(target, self.slice(value, relative)?, sources)]
                };
                for (target, expr, sources) in pieces {
                    let previous_sources = sources
                        .iter()
                        .copied()
                        // A source the process writes is read before the write,
                        // the bits a latch keeps included.
                        .filter(|source| {
                            held || source.id != id || !source.access.overlaps(&target)
                        })
                        .filter(|source| !split || source.id != id)
                        .filter(|source| {
                            written_atoms.iter().any(|written| {
                                written.id == source.id && written.access.overlaps(&source.access)
                            })
                        })
                        .collect();
                    paths.push(LogicPath {
                        transparent_copy: self.continuous,
                        target: LogicPathTarget::Var(VarAtomBase::new(id, target.lsb, target.msb)),
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
        calls: m,
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

/// The bits of the variables that bits `bits` of `node` are computed from,
/// through operators that take each result bit from the same bits of their
/// operands (bitwise operators, concatenations and selects); `None` where
/// the expression mixes bits otherwise.
fn bit_sources(
    arena: &SLTNodeArena<SourceVarId>,
    node: NodeId,
    bits: BitAccess,
) -> Option<Sources> {
    let width = |node| celox_slt::get_width(node, arena);
    let whole = |node| BitAccess::new(0, width(node) - 1);
    Some(match arena.get(node) {
        SLTNode::Input {
            variable,
            index,
            access,
            ..
        } if index.is_empty() => std::iter::once(VarAtomBase::new(
            *variable,
            access.lsb + bits.lsb,
            access.lsb + bits.msb,
        ))
        .collect(),
        SLTNode::Constant(..) => Sources::default(),
        SLTNode::Binary(
            left,
            celox_design::BinaryOp::And | celox_design::BinaryOp::Or | celox_design::BinaryOp::Xor,
            right,
        ) if width(*left) == width(node) && width(*right) == width(node) => {
            let mut sources = bit_sources(arena, *left, bits)?;
            sources.extend(bit_sources(arena, *right, bits)?);
            sources
        }
        SLTNode::Unary(
            celox_design::UnaryOp::Ident
            | celox_design::UnaryOp::BitNot
            | celox_design::UnaryOp::ToTwoState,
            operand,
        ) if width(*operand) == width(node) => bit_sources(arena, *operand, bits)?,
        SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        } => {
            let mut sources = bit_sources(arena, *cond, whole(*cond))?;
            sources.extend(bit_sources(arena, *then_expr, bits)?);
            sources.extend(bit_sources(arena, *else_expr, bits)?);
            sources
        }
        SLTNode::Slice { expr, access } => bit_sources(
            arena,
            *expr,
            BitAccess::new(access.lsb + bits.lsb, access.lsb + bits.msb),
        )?,
        SLTNode::Capture { expr, .. } => bit_sources(arena, *expr, bits)?,
        SLTNode::Concat(parts) => {
            // Most significant part first.
            let mut sources = Sources::default();
            let mut top = width(node);
            for &(part, part_width) in parts {
                let low = top - part_width;
                let (from, to) = (bits.lsb.max(low), bits.msb.min(top - 1));
                if from <= to {
                    sources.extend(bit_sources(
                        arena,
                        part,
                        BitAccess::new(from - low, to - low),
                    )?);
                }
                top = low;
            }
            sources
        }
        _ => return None,
    })
}
