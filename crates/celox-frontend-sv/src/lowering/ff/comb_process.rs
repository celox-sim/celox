//! The lowering of a stateful combinational process.

use super::*;

impl Ff<'_, '_> {
    /// Lower a stateful combinational process. Its memory guard records the
    /// implicit sensitivity values, independently of internal settling calls.
    /// `explicit_targets` are hidden buffers consumed outside this process,
    /// such as child-port actuals. They retain their module-wide addresses.
    pub fn lower_comb_process(
        mut self,
        body: &[sv::ir::Stmt],
        sensitivity_body: &[sv::ir::Stmt],
        kind: sv::ir::CombProcessKind,
        allow_inferred_latches: bool,
        site: Option<&sv::ir::SourceSpan>,
        explicit_targets: &[SourceVarId],
        arena: &mut SLTNodeArena<SourceVarId>,
    ) -> Result<Vec<LogicPath<SourceVarId>>, sv::AnalyzerError> {
        let always_comb = kind != sv::ir::CombProcessKind::AlwaysStar;
        // `always @*` and a sensitivity list wait for a change (IEEE
        // 1800-2023 9.2.2.2). In two-state mode their outputs would then
        // start as a real 0, so they run once at time zero, after the
        // `initial` blocks, like `always_comb`, as Verilator settles them.
        let runs_at_time_zero = always_comb || !self.m.four_state;
        self.direct = true;
        let mut reads = self.m.implicit_reads(body, always_comb);
        reads.extend(self.m.implicit_reads(sensitivity_body, always_comb));
        for stmt in body {
            let mut nonblocking = false;
            stmt.walk(&mut |stmt| {
                nonblocking |= matches!(
                    stmt,
                    sv::ir::Stmt::Assign {
                        nonblocking: true,
                        ..
                    } | sv::ir::Stmt::AssignConcat {
                        nonblocking: true,
                        ..
                    }
                )
            });
            if nonblocking {
                return Err(unsupported("nonblocking assignment inside always_comb"));
            }
        }
        self.scan(body, &mut HashSet::default());
        for &id in explicit_targets {
            let width = self.m.var(id).width;
            self.writes.targets.push((id, BitAccess::new(0, width - 1)));
        }
        let flush_site = self.deferred_flush_site(body);
        if self.exec_block(body)? {
            self.b.seal_block(SIRTerminator::Return);
        }
        let body_unit = seal_drained(std::mem::replace(&mut self.b, SIRBuilder::new()));
        // The analyzer promotes static locals whose entry value is observable
        // to module signals (lifetimes::keep_static_locals). Remaining hidden
        // locals are initialized per activation; they cannot retain old state.
        let mut targets: Vec<_> = self
            .writes
            .targets
            .iter()
            .filter(|(id, _)| !self.m.is_hidden(*id) || explicit_targets.contains(id))
            .map(|(id, access)| VarAtomBase::new(*id, access.lsb, access.msb))
            .collect();
        targets.sort_by_key(|atom| (atom.id.0, atom.access.lsb, atom.access.msb));
        let mut merged: Vec<VarAtomBase<SourceVarId>> = Vec::new();
        for target in targets {
            match merged.last_mut() {
                Some(last) if last.id == target.id && target.access.lsb <= last.access.msb + 1 => {
                    last.access.msb = last.access.msb.max(target.access.msb)
                }
                _ => merged.push(target),
            }
        }
        let mut targets = merged;
        if kind == sv::ir::CombProcessKind::AlwaysComb
            && super::super::comb_retention::retains_state(
                &body_unit,
                &targets,
                &targets
                    .iter()
                    .map(|target| (target.id, self.m.var(target.id).width))
                    .collect(),
            )
        {
            if !allow_inferred_latches {
                return Err(unsupported("latch inference inside always_comb"));
            }
            self.m.warn(crate::FrontendWarningKind::InferredLatch, site);
        }
        let mut dependencies =
            super::super::comb::subtract_written_sensitivity(reads.clone(), &targets);
        let mut sources: Vec<_> = if always_comb {
            dependencies.clone()
        } else {
            reads
        }
        .into_iter()
        .collect();
        // A force or a release of a target writes it again (IEEE 1800-2023
        // 10.6.2): the unit runs when the force controls change.
        for target in &targets {
            if !self.m.procedurally_forced.contains(&target.id) {
                continue;
            }
            let Some((enabled, _)) = self.m.force_controls(target.id) else {
                continue;
            };
            let controls = [Some(enabled), self.m.force_holds.get(&target.id).copied()];
            let width = self.m.var(target.id).width;
            for control in controls.into_iter().flatten() {
                let atom = VarAtomBase::new(control, 0, width - 1);
                if dependencies.insert(atom) {
                    sources.push(atom);
                }
            }
        }
        sources.sort_by_key(|atom| (atom.id.0, atom.access.lsb, atom.access.msb));
        let (initialized, _) = self.m.temp("comb_initialized", 1, false, false);
        let init = self.b.alloc_bit(1, false);
        self.b.emit(SIRInstruction::Load(
            init,
            stable(initialized),
            SIROffset::Static(0),
            1,
        ));
        let mut changed = self.b.alloc_bit(1, false);
        self.b
            .emit(SIRInstruction::Imm(changed, SIRValue::new(0u8)));
        // A large sensitivity list need not retain every old and current
        // value until the final comparison. Check bounded groups, stop at
        // the first change, and refresh the snapshots only when needed.
        // Occurrence counters also observe transient changes; retain their
        // existing path until they can share the same grouped check.
        let grouped = sources.len() >= 4 && !self.input_occurrences;
        let capture = grouped.then(|| self.b.new_block());
        let execute = self.b.new_block();
        let done = self.b.new_block();
        if let Some(capture) = capture {
            let check = self.b.new_block();
            self.b.seal_block(SIRTerminator::Branch {
                cond: init,
                true_block: (check, vec![]),
                false_block: (capture, vec![]),
            });
            self.b.switch_to_block(check);
        }
        let mut snapshots = Vec::new();
        let mut sensitivity_counters = Vec::new();
        let mut counter_snapshots = Vec::new();
        for (ordinal, source) in sources.iter().enumerate() {
            let width = source.access.msb - source.access.lsb + 1;
            let (saved, _) = self.m.temp(
                "comb_input",
                width,
                false,
                self.m.four_state && self.m.var(source.id).is_4state,
            );
            let current = self.b.alloc_logic(width);
            let previous = self.b.alloc_logic(width);
            let offset = sv_memory_offset(self.m.var(source.id), source.access.lsb, width);
            self.b.emit(SIRInstruction::Load(
                current,
                stable(source.id),
                offset,
                width,
            ));
            self.b.emit(SIRInstruction::Load(
                previous,
                stable(saved),
                SIROffset::Static(0),
                width,
            ));
            let differs = self.b.alloc_bit(1, false);
            self.b.emit(SIRInstruction::Binary(
                differs,
                current,
                BinaryOp::NeCase,
                previous,
            ));
            let next = self.b.alloc_bit(1, false);
            self.b
                .emit(SIRInstruction::Binary(next, changed, BinaryOp::Or, differs));
            changed = next;
            snapshots.push((saved, width, current));
            if let Some(capture) = capture
                && ((ordinal + 1).is_multiple_of(4) || ordinal + 1 == sources.len())
            {
                let next = if ordinal + 1 == sources.len() {
                    done
                } else {
                    self.b.new_block()
                };
                self.b.seal_block(SIRTerminator::Branch {
                    cond: changed,
                    true_block: (capture, vec![]),
                    false_block: (next, vec![]),
                });
                if next != done {
                    self.b.switch_to_block(next);
                    changed = self.b.alloc_bit(1, false);
                    self.b
                        .emit(SIRInstruction::Imm(changed, SIRValue::new(0u8)));
                }
            }
            if !self.input_occurrences {
                continue;
            }
            // A source can change and return to its saved value inside one
            // initial activation. Count occurrences around stores before
            // optimization, rather than relying only on the settled value.
            let bits = celox_design::EVENT_COUNTER_WIDTH;
            let (counter, _) = self.m.temp("comb_occurrences", bits, false, false);
            let (saved_counter, _) = self.m.temp("comb_seen", bits, false, false);
            let current_counter = self.b.alloc_bit(bits, false);
            let previous_counter = self.b.alloc_bit(bits, false);
            self.b.emit(SIRInstruction::Load(
                current_counter,
                stable(counter),
                SIROffset::Static(0),
                bits,
            ));
            self.b.emit(SIRInstruction::Load(
                previous_counter,
                stable(saved_counter),
                SIROffset::Static(0),
                bits,
            ));
            let differs = self.b.alloc_bit(1, false);
            self.b.emit(SIRInstruction::Binary(
                differs,
                current_counter,
                BinaryOp::NeCase,
                previous_counter,
            ));
            let next = self.b.alloc_bit(1, false);
            self.b
                .emit(SIRInstruction::Binary(next, changed, BinaryOp::Or, differs));
            changed = next;
            self.store(saved_counter, SIROffset::Static(0), bits, current_counter);
            sensitivity_counters.push((*source, counter));
            counter_snapshots.push((counter, saved_counter));
        }
        if let Some(capture) = capture {
            self.b.switch_to_block(capture);
        }
        let run = self.b.alloc_bit(1, false);
        if grouped && runs_at_time_zero {
            self.b.emit(SIRInstruction::Imm(run, SIRValue::new(1u8)));
        } else if grouped {
            // Ordinary event controls establish their initial baseline
            // without executing; always_comb executes on that first visit.
            self.b
                .emit(SIRInstruction::Binary(run, init, BinaryOp::Or, init));
        } else if runs_at_time_zero {
            let first = self.b.alloc_bit(1, false);
            self.b
                .emit(SIRInstruction::Unary(first, UnaryOp::LogicNot, init));
            self.b
                .emit(SIRInstruction::Binary(run, first, BinaryOp::Or, changed));
        } else {
            self.b
                .emit(SIRInstruction::Binary(run, init, BinaryOp::And, changed));
        }
        // Snapshot before executing: writes in the body do not retrigger it.
        for (source, &(saved, width, current)) in sources.iter().zip(&snapshots) {
            let current = if grouped {
                // The checks do not mutate input state. Load each value at
                // its store, avoiding a live snapshot set spanning the list.
                let current = self.b.alloc_logic(width);
                let offset = sv_memory_offset(self.m.var(source.id), source.access.lsb, width);
                self.b.emit(SIRInstruction::Load(
                    current,
                    stable(source.id),
                    offset,
                    width,
                ));
                current
            } else {
                current
            };
            self.store(saved, SIROffset::Static(0), width, current);
        }
        let one = self.b.alloc_bit(1, false);
        self.b.emit(SIRInstruction::Imm(one, SIRValue::new(1u8)));
        self.store(initialized, SIROffset::Static(0), 1, one);
        self.b.seal_block(SIRTerminator::Branch {
            cond: run,
            true_block: (execute, vec![]),
            false_block: (done, vec![]),
        });
        self.b.switch_to_block(execute);
        // Each run is a resume of the process (IEEE 1800-2023 16.4.2).
        if let Some(site_id) = flush_site {
            self.b.emit(SIRInstruction::RuntimeEvent {
                site_id,
                args: Vec::new(),
            });
        }
        self.b.inline_execution_unit(&body_unit);
        if !always_comb {
            // @* is not waiting during its body. Its own writes cannot wake
            // the same activation, including when another output is published.
            for (source, &(saved, width, _)) in sources.iter().zip(&snapshots) {
                let current = self.b.alloc_logic(width);
                let offset = sv_memory_offset(self.m.var(source.id), source.access.lsb, width);
                self.b.emit(SIRInstruction::Load(
                    current,
                    stable(source.id),
                    offset,
                    width,
                ));
                self.store(saved, SIROffset::Static(0), width, current);
            }
        }
        if !always_comb {
            // The event control is rearmed after its body, so occurrences
            // caused by this activation itself are already consumed.
            for &(counter, saved) in &counter_snapshots {
                let bits = celox_design::EVENT_COUNTER_WIDTH;
                let current = self.b.alloc_bit(bits, false);
                self.b.emit(SIRInstruction::Load(
                    current,
                    stable(counter),
                    SIROffset::Static(0),
                    bits,
                ));
                self.store(saved, SIROffset::Static(0), bits, current);
            }
        }
        self.jump(done);
        self.b.switch_to_block(done);
        let mut private_locals = HashMap::default();
        let unit = seal_builder(self.b).into_map_addr(&mut |addr| {
            let id = addr.var_id;
            if !self
                .m
                .variables
                .get(&id)
                .is_some_and(SvVariable::is_scratch)
                || self.m.created.contains(&id)
                || explicit_targets.contains(&id)
            {
                return id;
            }
            *private_locals.entry(id).or_insert_with(|| {
                let variable = self.m.var(id).clone();
                let (copy, name) = self.m.temp(
                    "comb_local",
                    variable.width,
                    variable.signed,
                    variable.is_4state,
                );
                self.m.variables.insert(
                    copy,
                    SvVariable {
                        path: vec![name],
                        activation_scratch: true,
                        ..variable
                    },
                );
                copy
            })
        });
        if targets.is_empty() {
            targets.push(VarAtomBase::new(initialized, 0, 0));
        }
        let mut paths = Vec::new();
        for target in targets {
            let expr = arena
                .alloc(SLTNode::Input {
                    variable: target.id,
                    signed: false,
                    index: Vec::new(),
                    access: target.access,
                })
                .map_err(slt_error)?;
            paths.push(LogicPath {
                transparent_copy: false,
                target: LogicPathTarget::Process {
                    process: initialized,
                    sensitivity_counters: sensitivity_counters.clone(),
                    startup: (!runs_at_time_zero).then(|| celox_slt::CombStartup {
                        initialized,
                        counters: counter_snapshots.clone(),
                        inputs: sources
                            .iter()
                            .cloned()
                            .zip(snapshots.iter().map(|&(saved, _, _)| saved))
                            .collect(),
                    }),
                    after_initials: matches!(
                        kind,
                        sv::ir::CombProcessKind::AlwaysComb | sv::ir::CombProcessKind::AlwaysLatch
                    ) || (kind == sv::ir::CombProcessKind::AlwaysStar
                        && !self.m.four_state),
                    target,
                    unit: Box::new(unit.clone()),
                },
                sources: dependencies.clone(),
                previous_sources: HashSet::default(),
                address_sources: HashSet::default(),
                local_inputs: Vec::new(),
                order_before: HashSet::default(),
                comb_capture_enable_sites: Vec::new(),
                comb_capture_enable_always: false,
                pre_lower_nodes: Vec::new(),
                expr,
            });
        }
        Ok(paths)
    }
}
