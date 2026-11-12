//! The lowering of an `initial` body into a process kernel.

use super::*;

/// The activations of a process kernel: whether it runs in several, as one
/// that forks or controls processes does, and what each then keeps in a
/// frame of its own.
#[derive(Default)]
pub(super) struct Activation {
    pub(super) forked: bool,
    /// Whether [`Ff::freeze_lvalue`] keeps its copies in the activation
    /// frame of a forked process.
    pub(super) frame_positions: bool,
    pub(super) process_control: bool,
    pub(super) frame_locals: Vec<SourceVarId>,
}

/// What a process declares to the runtime while it is lowered, which its
/// kernel hands over when it is finished.
#[derive(Default)]
pub(super) struct Sites {
    /// The registers of the kernel holding the number of its own wait
    /// sites, which the sites of the units it calls follow (see
    /// `Ff::function_wait_word`), set when the kernel is finished.
    pub(super) wait_offset_patches: Vec<RegisterId>,
    /// Likewise the number of its own NBA sites.
    pub(super) nba_offset_patches: Vec<RegisterId>,
    pub(super) event_probes: Vec<celox_design::ProcessEventProbe<SourceVarId>>,
    /// The index of the process being lowered among the module's kernels.
    pub(super) process: usize,
    /// The wait sites of the process, by local index.
    pub(super) wait: Vec<WaitSite<SourceVarId>>,
    /// The nonblocking assignment sites of the process, by local index.
    pub(super) nba: Vec<NbaSite<SourceVarId>>,
    pub(super) host_request: Vec<celox_design::HostRequestSite<SourceVarId>>,
    /// The nonblocking sites with a staged and a queued path.
    pub(super) nba_switches: Vec<NbaSwitch<SourceVarId>>,
    /// The calls at which the process kernel can be suspended inside the
    /// callee.
    pub(super) call_resumes: Vec<celox_design::CallResume>,
}

impl Ff<'_, '_> {
    /// The variables the sites of the process name, which the runtime reads
    /// or writes in the frame of a suspended activation.
    fn site_operands(&self) -> HashSet<SourceVarId> {
        let mut operands = HashSet::default();
        let mut add = |id: SourceVarId| {
            operands.insert(id);
            id
        };
        for site in &self.sites.wait {
            site.map(&mut add);
        }
        for site in &self.sites.nba {
            site.map(&mut add);
        }
        for site in &self.sites.host_request {
            site.map(&mut add);
        }
        for switch in &self.sites.nba_switches {
            add(switch.value);
            add(switch.switch);
            add(switch.queued);
            if let Some(position) = &switch.queued_position {
                position.map(&mut add);
            }
        }
        for probe in &self.sites.event_probes {
            add(probe.result);
            for &id in probe.inputs.iter().chain(&probe.scratch) {
                add(id);
            }
        }
        operands
    }

    /// Lower an `initial` body into a process kernel that runs it from time
    /// zero, reading and writing stable state directly.
    pub fn lower_initial_kernel(
        mut self,
        body: &[sv::ir::Stmt],
        slots: ProcessSlots<SourceVarId>,
        process: usize,
        final_process: bool,
    ) -> Result<KernelUnit, sv::AnalyzerError> {
        self.sites.process = process;
        let monitored;
        let no_body: &[sv::ir::Stmt] = &[];
        // Dropping the monitor cannot strand sampled or `$past` readers
        // elsewhere: each process that reads a history declares and
        // advances its own chain (`declare_history` is per process), and
        // sampled copies are refreshed from the module's sample shadows
        // for as long as any unit reads them.
        let body = if !self.m.four_state && vacuous_two_state_monitor(body) {
            no_body
        } else if let Some(clock) = concurrent_monitor_clock(body) {
            self.assertions.inferred_clock = Some(clock);
            self.declare_history(body)?;
            if self.assertions.history.is_empty() {
                body
            } else {
                use sv::ir::{BinaryOp, Expr, LValue, Stmt};
                let mut gates = Vec::new();
                for index in 0..self.assertions.history.len() {
                    let Some(gate) = self.assertions.history[index].gate.clone() else {
                        continue;
                    };
                    let (_, name) = self.m.temp("assertion_history_gate", 1, false, true);
                    gates.push(Stmt::Assign {
                        lhs: LValue::Ident(name.clone()),
                        rhs: Expr::Binary {
                            left: Box::new(gate),
                            op: BinaryOp::Ne,
                            right: Box::new(Expr::Literal("1'b0".into())),
                        },
                        nonblocking: false,
                    });
                    self.assertions.history[index].gate_value = Some(name);
                }
                let mut owned = body.to_vec();
                let [
                    Stmt::Loop {
                        body: repeated,
                        step,
                        ..
                    },
                ] = owned.as_mut_slice()
                else {
                    unreachable!("a canonical monitor loop")
                };
                // Capture gates immediately after the clock wait, in Active,
                // before the predicate yields to Observed and the actions to
                // Reactive. Advance history after either action or disable.
                repeated.splice(1..1, gates);
                step.extend(self.history_updates());
                monitored = owned;
                &monitored[..]
            }
        } else {
            body
        };
        let starts_with_event_wait = slots.requires_timed_simulation;
        let first_created = self.m.created.len();
        fn has_fork(
            m: &ProcModule<'_>,
            body: &[sv::ir::Stmt],
            visited: &mut HashSet<String>,
            defaults: &mut HashSet<(String, usize)>,
        ) -> bool {
            let mut found = false;
            let mut calls = Vec::new();
            for stmt in body {
                stmt.walk(&mut |stmt| {
                    found |= matches!(stmt, sv::ir::Stmt::Fork { .. } | sv::ir::Stmt::WaitFork);
                    stmt_calls(stmt, &mut calls);
                });
            }
            if found {
                return true;
            }
            while let Some((callee, args)) = calls.pop() {
                let sv::ir::Callee::Subroutine(name) = callee else {
                    continue;
                };
                let Some(subroutine) = m.subroutine(&name) else {
                    continue;
                };
                for (position, param) in subroutine.params.iter().enumerate() {
                    if matches!(args.get(position), None | Some(None))
                        && let Some(default) = &param.default
                        && defaults.insert((name.clone(), position))
                    {
                        collect_calls(default, &mut calls);
                    }
                }
                if visited.insert(name) && has_fork(m, &subroutine.body, visited, defaults) {
                    return true;
                }
            }
            false
        }
        // Only kernels containing the new activation-local compound waits
        // need per-assignment observation. Plain fork kernels keep their
        // existing suspension count and execution cost.
        fn has_activation_event(
            m: &ProcModule<'_>,
            body: &[sv::ir::Stmt],
            visited: &mut HashSet<String>,
        ) -> bool {
            let mut found = false;
            let mut calls = Vec::new();
            for stmt in body {
                stmt.walk(&mut |stmt| match stmt {
                    sv::ir::Stmt::WaitEvent(items) => {
                        found |= items.iter().any(|item| {
                            item.iff.is_some() || !matches!(&item.expr, sv::ir::Expr::Ident(_))
                        });
                    }
                    sv::ir::Stmt::Call { name, .. } => calls.push(name.clone()),
                    _ => {}
                });
                stmt.clone().visit_mut(
                    &mut |expr| collect_call_names(expr, &mut calls),
                    &mut |_| {},
                    &mut |_| {},
                );
            }
            if found {
                return true;
            }
            while let Some(name) = calls.pop() {
                if !visited.insert(name.clone()) {
                    continue;
                }
                if let Some(sub) = m.subroutine(&name) {
                    for param in &sub.params {
                        if let Some(default) = &param.default {
                            collect_call_names(default, &mut calls);
                        }
                    }
                    if has_activation_event(m, &sub.body, visited) {
                        return true;
                    }
                }
            }
            false
        }
        self.observation.observe_blocking_assignments =
            has_activation_event(self.m, body, &mut HashSet::default());
        fn has_process(
            m: &ProcModule<'_>,
            body: &[sv::ir::Stmt],
            visited: &mut HashSet<String>,
            defaults: &mut HashSet<(String, usize)>,
        ) -> bool {
            let mut calls = Vec::new();
            for stmt in body {
                stmt.walk(&mut |stmt| {
                    stmt_calls(stmt, &mut calls);
                });
            }
            while let Some((callee, args)) = calls.pop() {
                match callee {
                    sv::ir::Callee::System(sv::ir::SystemFunction::Process(_)) => return true,
                    sv::ir::Callee::Subroutine(name) => {
                        let Some(sub) = m.subroutine(&name) else {
                            continue;
                        };
                        for (position, param) in sub.params.iter().enumerate() {
                            if matches!(args.get(position), None | Some(None))
                                && let Some(default) = &param.default
                                && defaults.insert((name.clone(), position))
                            {
                                collect_calls(default, &mut calls);
                            }
                        }
                        if visited.insert(name) && has_process(m, &sub.body, visited, defaults) {
                            return true;
                        }
                    }
                    _ => {}
                }
            }
            false
        }
        self.activation.process_control = has_process(
            self.m,
            body,
            &mut HashSet::default(),
            &mut HashSet::default(),
        );
        self.activation.forked = has_fork(
            self.m,
            body,
            &mut HashSet::default(),
            &mut HashSet::default(),
        );
        // Finals share one process RNG even when a particular body only
        // draws random values and never exposes process::self().
        self.activation.forked |= self.activation.process_control || final_process;
        // Forks reached through inlined tasks also need per-activation random state.
        if self.activation.forked {
            self.urandom_state();
        }
        let mut kernel = ProcessKernelBuilder::new(slots);
        if let Some(site) = self.deferred_flush_site(body) {
            kernel.flush_deferred_at(site);
        }
        std::mem::swap(&mut self.b, kernel.builder());
        self.kernel = Some(kernel);
        self.exec_block(body)?;
        if !self.observation.observe_blocking_assignments && !self.sites.event_probes.is_empty() {
            return Err(unsupported(
                "activation-local event expression without assignment observation boundaries",
            ));
        }
        let implicit_probe_write = self.sites.event_probes.iter().any(|probe| {
            probe
                .inputs
                .iter()
                .any(|input| self.observation.unobserved_function_writes.contains(input))
        });
        let implicit_gate_write = self.sites.wait.iter().any(|site| {
            site.terms
                .iter()
                .zip(&site.gates)
                .any(|(&(signal, _), gate)| {
                    gate.is_some_and(|gate| {
                        gate.at_write
                            && gate.counters.is_none()
                            && (self
                                .observation
                                .unobserved_function_writes
                                .contains(&signal)
                                || self
                                    .observation
                                    .unobserved_function_writes
                                    .contains(&gate.signal))
                    })
                })
        });
        if implicit_probe_write || implicit_gate_write {
            return Err(unsupported(
                "implicit write to an activation-local event operand without an observation boundary",
            ));
        }
        let mut kernel = self.kernel.take().expect("lowering a process kernel");
        std::mem::swap(&mut self.b, kernel.builder());
        let slots = kernel.slots();
        let resume_count = kernel.resume_count();
        let mut unit = kernel.build();
        if !self.sites.wait_offset_patches.is_empty() || !self.sites.nba_offset_patches.is_empty() {
            let (waits, nbas) = (self.sites.wait.len() as u64, self.sites.nba.len() as u64);
            for block in unit.blocks.values_mut() {
                for instruction in &mut block.instructions {
                    if let SIRInstruction::Imm(dst, value) = instruction {
                        if self.sites.wait_offset_patches.contains(dst) {
                            *value = SIRValue::new(waits);
                        } else if self.sites.nba_offset_patches.contains(dst) {
                            *value = SIRValue::new(nbas);
                        }
                    }
                }
            }
        }
        prune_unreachable_blocks(&mut unit);
        // The runtime reads these in the frame of a suspended activation:
        // the operands of the process's sites and its RNG state.
        let site_operands = if self.activation.forked {
            let mut operands = self.site_operands();
            operands.extend(self.rng.random.as_ref().map(|random| random.0));
            operands
        } else {
            HashSet::default()
        };
        let frame = self.activation.forked.then(|| {
            // Each activation has its own scratch: the values its lowering
            // keeps across its own suspensions. Lexical locals are cells of
            // `locals`, not frame state; every other variable the lowering
            // made, such as the time slot, the held gate, the change counts
            // of boxes, or the sampled copies and `$past` histories the
            // runtime maintains, is the module's.
            let scratch: Vec<_> = self.m.created[first_created..]
                .iter()
                .copied()
                .filter(|id| {
                    self.m.var(*id).activation_scratch && !self.activation.frame_locals.contains(id)
                })
                .collect();
            // Of those, an activation keeps the ones a run may read before it
            // writes them (see `frame_liveness`), and those the runtime reads
            // in its frame.
            let candidates: HashSet<SourceVarId> = scratch.iter().copied().collect();
            let whole = |id: SourceVarId| {
                let variable = self.m.var(id);
                variable.array_dims.is_empty().then_some(variable.width)
            };
            let mut kept = frame_liveness::live_at_entry(&unit, &candidates, &whole);
            kept.extend(site_operands);
            if cfg!(debug_assertions) {
                let excluded: HashSet<SourceVarId> = candidates
                    .iter()
                    .copied()
                    .filter(|id| !kept.contains(id))
                    .collect();
                frame_liveness::assert_written_before_read(&unit, &excluded, &whole);
            }
            let mut private: Vec<_> = scratch.into_iter().filter(|id| kept.contains(id)).collect();
            private.extend([slots.resume, slots.status, slots.delay]);
            celox_design::ProcessFrame {
                resume_count,
                private,
                locals: self.activation.frame_locals,
                event_probes: self.sites.event_probes,
                random: self.rng.random.as_ref().unwrap().0,
                seed: self.rng.random_seed,
                starts_with_event_wait,
            }
        });
        Ok(KernelUnit {
            frame,
            unit,
            slots,
            wait_sites: self.sites.wait,
            nba_sites: self.sites.nba,
            host_request_sites: self.sites.host_request,
            nba_switches: self.sites.nba_switches,
            call_resumes: self.sites.call_resumes,
        })
    }
}

/// The kernel of a process and its wait and nonblocking assignment sites.
pub(in crate::lowering) struct KernelUnit {
    pub unit: ExecutionUnit<Addr>,
    pub slots: ProcessSlots<SourceVarId>,
    pub frame: Option<celox_design::ProcessFrame<SourceVarId>>,
    pub wait_sites: Vec<WaitSite<SourceVarId>>,
    pub nba_sites: Vec<NbaSite<SourceVarId>>,
    pub host_request_sites: Vec<celox_design::HostRequestSite<SourceVarId>>,
    pub nba_switches: Vec<NbaSwitch<SourceVarId>>,
    /// The calls at which the kernel can be suspended inside the callee,
    /// naming module-local functions.
    pub call_resumes: Vec<celox_design::CallResume>,
}
