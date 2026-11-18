//! The lowering of an edge-triggered process into its evaluation and
//! commit units.

use super::*;

impl Ff<'_, '_> {
    /// Lower one `always_ff` body into its evaluation and commit units.
    pub fn lower_process(
        mut self,
        body: &[sv::ir::Stmt],
    ) -> Result<FfProcessUnits, sv::AnalyzerError> {
        self.declare_history(body)?;
        let mut visited = HashSet::default();
        self.scan(body, &mut visited);
        // A variable only this process's nonblocking assignments write here,
        // and that a process kernel may write before they are published,
        // takes only the bits they wrote: the kernel's write stays in the
        // others, even when no assignment executed (IEEE 1800-2023 10.4.2).
        // Staging rewrites the assignments of the body; a variable a called
        // subroutine assigns keeps its whole next state.
        let called = self.subroutine_assignments(body);
        let exact: HashSet<SourceVarId> = self
            .writes
            .nonblocking
            .iter()
            .copied()
            .filter(|id| {
                self.writes.window_writes.contains(id)
                    && !called.contains(id)
                    && !self.writes.working.contains(id)
                    && !self.m.is_hidden(*id)
                    && !self.writes.sparse_memories.contains(id)
            })
            .collect();
        // Publish only the written elements of an array this process alone
        // updates by nonblocking element writes, rather than seeding and
        // committing all of it on every edge. The element stores keep
        // statement order, so the last write to an element wins, and reads
        // see stable state (IEEE 1800-2023 4.6, 10.4.2).
        let sparse: Vec<_> = self
            .writes
            .sparse_candidates
            .iter()
            .copied()
            .filter(|id| {
                self.writes.element_writes.contains(id)
                    && !exact.contains(id)
                    && !self.writes.working.contains(id)
                    && !self.writes.other_writes.contains(id)
                    && !self.m.is_hidden(*id)
                    // A forced array commits through the force controls.
                    && !self.m.procedurally_forced.contains(id)
            })
            .collect();
        self.writes.sparse_memories.extend(sparse);
        if let Some(&forced) = self
            .writes
            .sparse_memories
            .iter()
            .find(|id| self.m.procedurally_forced.contains(id))
        {
            return Err(unsupported(format!(
                "force or release of `{}`, an unpacked array several edge-triggered processes write",
                self.m.var(forced).path.join(".")
            )));
        }
        if self
            .writes
            .nonblocking
            .iter()
            .any(|id| self.writes.shared.contains(id))
        {
            return Err(unsupported(
                "nonblocking writes to a variable several edge processes write",
            ));
        }
        if self
            .writes
            .working
            .iter()
            .any(|id| self.writes.sparse_memories.contains(id))
        {
            return Err(unsupported(
                "blocking writes to a shared edge-triggered memory",
            ));
        }
        // Merge the written ranges of each variable.
        let mut ranges: HashMap<SourceVarId, Vec<BitAccess>> = HashMap::default();
        for (id, access) in std::mem::take(&mut self.writes.targets) {
            ranges.entry(id).or_default().push(access);
        }
        let mut targets = Vec::new();
        let mut ids: Vec<_> = ranges.keys().copied().collect();
        ids.sort_by_key(|id| id.0);
        for id in ids {
            let mut accesses = ranges.remove(&id).unwrap_or_default();
            accesses.sort_by_key(|access| access.lsb);
            let mut merged: Vec<BitAccess> = Vec::new();
            for access in accesses {
                match merged.last_mut() {
                    Some(last) if access.lsb <= last.msb + 1 => last.msb = last.msb.max(access.msb),
                    _ => merged.push(access),
                }
            }
            targets.extend(
                merged
                    .into_iter()
                    .map(|access| VarAtomBase::new(id, access.lsb, access.msb)),
            );
        }
        // A variable the process writes both ways keeps its nonblocking
        // values apart: blocking reads see the blocking value, and the
        // nonblocking update lands after it (IEEE 1800-2023 10.4).
        let mut mixed: Vec<SourceVarId> = self
            .writes
            .nonblocking
            .intersection(&self.writes.working)
            .copied()
            .filter(|&id| !self.m.is_hidden(id) && self.m.var(id).array_dims.is_empty())
            .chain(exact.iter().copied())
            .collect();
        mixed.sort_by_key(|id| id.0);
        // The intra-assignment delays of this body's delayed nonblocking
        // assignments (IEEE 1800-2023 9.4.5): each distinct delay of a
        // variable gets a staging pair of its own, so an update is
        // published at its own due time rather than at the edge.
        let mut delayed: HashMap<SourceVarId, Vec<u64>> = HashMap::default();
        let mut delayed_assignments: HashSet<usize> = HashSet::default();
        let mut stmt_delays: HashMap<usize, u64> = HashMap::default();
        let mut pending_delays: Vec<(usize, sv::ir::Expr, u64, SourceVarId)> = Vec::new();
        for stmt in body {
            stmt.walk(&mut |stmt| {
                let sv::ir::Stmt::DelayedNonblocking {
                    control,
                    assignment,
                } = stmt
                else {
                    return;
                };
                let sv::ir::Stmt::Delay {
                    amount,
                    ticks_per_unit,
                    ..
                } = control.as_ref()
                else {
                    return;
                };
                let (target, assignment_ptr) = match assignment.as_ref() {
                    sv::ir::Stmt::Assign { lhs, .. } => (
                        self.m.id(lhs.name()),
                        std::ptr::from_ref(assignment.as_ref()),
                    ),
                    sv::ir::Stmt::AssignConcat { parts, .. } => (
                        parts.first().and_then(|part| self.m.id(part.name())),
                        std::ptr::from_ref(assignment.as_ref()),
                    ),
                    _ => (None, std::ptr::from_ref(assignment.as_ref())),
                };
                delayed_assignments.insert(assignment_ptr as usize);
                if let Some(target) = target {
                    pending_delays.push((
                        std::ptr::from_ref(stmt) as usize,
                        amount.clone(),
                        *ticks_per_unit,
                        target,
                    ));
                }
            });
        }
        for (pointer, amount, ticks_per_unit, target) in pending_delays {
            let delay = self
                .eval_const_with_width(&amount, Some(celox_design::PROCESS_DELAY_WIDTH))?
                .and_then(|(value, _)| u64::try_from(value).ok())
                .and_then(|value| value.checked_mul(ticks_per_unit))
                .ok_or_else(|| {
                    unsupported("nonconstant or overflowing nonblocking assignment delay")
                })?;
            stmt_delays.insert(pointer, delay);
            delayed.entry(target).or_default().push(delay);
        }
        // Variables an ordinary (undelayed) nonblocking assignment writes.
        let mut plain: HashSet<SourceVarId> = HashSet::default();
        for stmt in body {
            stmt.walk(&mut |stmt| {
                if delayed_assignments.contains(&(std::ptr::from_ref(stmt) as usize)) {
                    return;
                }
                match stmt {
                    sv::ir::Stmt::Assign {
                        lhs,
                        nonblocking: true,
                        ..
                    } => {
                        if let Some(id) = self.m.id(lhs.name()) {
                            plain.insert(id);
                        }
                    }
                    sv::ir::Stmt::AssignConcat {
                        parts,
                        nonblocking: true,
                        ..
                    } => {
                        for part in parts {
                            if let Some(id) = self.m.id(part.name()) {
                                plain.insert(id);
                            }
                        }
                    }
                    _ => {}
                }
            });
        }
        // Every variable a delayed assignment writes gets a stage, even
        // when its undelayed assignments would write the working state
        // directly: the delayed value must be captured at the edge and
        // published at its due time instead.
        let mut stage_ids: Vec<SourceVarId> = mixed;
        for id in delayed.keys() {
            if !stage_ids.contains(id) {
                stage_ids.push(*id);
            }
        }
        stage_ids.sort_by_key(|id| id.0);
        let staged: Vec<NbaStage> = stage_ids
            .into_iter()
            .flat_map(|id| {
                let mut delays: Vec<u64> = delayed.get(&id).cloned().unwrap_or_default();
                delays.sort_unstable();
                delays.dedup();
                if (plain.contains(&id) || delays.is_empty()) && !delays.contains(&0) {
                    delays.insert(0, 0);
                }
                delays.into_iter().map(move |delay| (id, delay))
            })
            .map(|(id, delay)| {
                let (value, value_name) = self.m.shadow(id, "nba_value", false);
                let (mask, mask_name) = self.m.shadow(id, "nba_mask", true);
                // Staging of one activation's update.
                for staging in [value, mask] {
                    self.m
                        .variables
                        .get_mut(&staging)
                        .expect("a new staging copy")
                        .activation_scratch = true;
                }
                let site = (delay > 0).then(|| {
                    let variable = self.m.var(id);
                    let (site_value, _) = self.m.module_temp(
                        "ff_nba_value",
                        variable.width,
                        false,
                        variable.is_4state,
                    );
                    let (pending, _) = self.m.module_temp("ff_nba_pending", 1, false, false);
                    (site_value, pending)
                });
                NbaStage {
                    id,
                    name: self.m.var(id).path.join("."),
                    value,
                    value_name,
                    mask,
                    mask_name,
                    delay,
                    site,
                    sites: None,
                }
            })
            .collect();
        // Publish only the elements an array's nonblocking writes wrote, so
        // an edge costs its writes rather than the whole array.
        let mut staged = staged;
        if self.kernel.is_none() && !self.direct {
            for stage in &mut staged {
                if stage.delay > 0 || self.m.var(stage.id).array_dims.is_empty() {
                    continue;
                }
                let Some(count) = array_nba_site_count(body, stage.id, self.m) else {
                    continue;
                };
                let sites = (0..count)
                    .map(|_| {
                        let (lsb, lsb_name) = self.m.temp("nba_lsb", 64, false, false);
                        let (msb, msb_name) = self.m.temp("nba_msb", 64, false, false);
                        let (hit, hit_name) = self.m.temp("nba_hit", 1, false, false);
                        NbaArraySite {
                            lsb,
                            lsb_name,
                            msb,
                            msb_name,
                            hit,
                            hit_name,
                        }
                    })
                    .collect();
                stage.sites = Some(sites);
            }
        }
        let body = if staged.is_empty() {
            body.to_vec()
        } else {
            stage_nonblocking(body, &staged, self.m, &mut HashMap::default(), &stmt_delays)
        };
        // A variable staged for an exact publication has no next state of
        // its own: only its merge publishes it. A shared variable is written
        // in stable state.
        let published: Vec<_> = targets
            .iter()
            .copied()
            .filter(|target| {
                !exact.contains(&target.id) && !self.writes.shared.contains(&target.id)
            })
            .collect();
        let ordinary_targets: Vec<_> = published
            .iter()
            .copied()
            .filter(|target| !self.writes.sparse_memories.contains(&target.id))
            .collect();
        emit_ff_seeds(&mut self.b, &ordinary_targets);
        for stage in &staged {
            let width = self.m.var(stage.mask).width;
            if self.m.var(stage.id).array_dims.is_empty() {
                let zero = self.b.alloc_bit(width, false);
                self.b.emit(SIRInstruction::Imm(zero, SIRValue::new(0u8)));
                self.store(stage.mask, SIROffset::Static(0), width, zero);
            } else if stage.sites.is_some() {
                // The merge clears the mask bits it published.
            } else {
                // Nothing writes the mask's stable state, which stays zero;
                // a store may not span the elements of an array.
                self.b.emit(SIRInstruction::Commit(
                    stable(stage.mask),
                    working(stage.mask),
                    SIROffset::Static(0),
                    width,
                    Vec::new(),
                ));
            }
        }
        // Each evaluation resumes the process from its event control.
        if let Some(site_id) = self.deferred_flush_site(&body) {
            self.b.emit(SIRInstruction::RuntimeEvent {
                site_id,
                args: Vec::new(),
            });
        }
        self.capture_history_gates()?;
        if self.exec_block(&body)? && self.update_history()? {
            self.b.seal_block(SIRTerminator::Return);
        }
        let mut eval = seal_drained(self.b);
        if eval.blocks.values().flat_map(|block| &block.instructions).any(|instruction| {
            matches!(instruction, SIRInstruction::Load(_, address, ..)
                if address.region == WORKING_REGION && self.writes.sparse_memories.contains(&address.var_id))
        }) {
            return Err(unsupported("read-modify-write destination in a shared FF memory"));
        }
        for block in eval.blocks.values_mut() {
            for instruction in &mut block.instructions {
                if let SIRInstruction::Store(address, ..) = instruction
                    && address.region == WORKING_REGION
                    && self.writes.sparse_memories.contains(&address.var_id)
                {
                    address.region = SPARSE_WORKING_REGION;
                }
            }
        }
        let (active_targets, nba_targets): (Vec<_>, Vec<_>) = published
            .iter()
            .copied()
            .partition(|target| self.writes.working.contains(&target.id));
        let mut apply = SIRBuilder::new();
        emit_sparse_memory_commits(&mut apply, &published, &self.writes.sparse_memories, self.m);
        emit_nba_merges(&mut apply, &staged, self.m);
        emit_delayed_publications(&mut apply, &staged, self.m);
        let mut active = SIRBuilder::new();
        emit_ff_commits(&mut active, &active_targets, self.m);
        let mut nba = SIRBuilder::new();
        emit_sparse_memory_commits(&mut nba, &nba_targets, &self.writes.sparse_memories, self.m);
        emit_nba_merges(&mut nba, &staged, self.m);
        emit_delayed_publications(&mut nba, &staged, self.m);
        Ok(FfProcessUnits {
            eval,
            apply: seal_builder(apply),
            active: seal_builder(active),
            nba: seal_builder(nba),
            targets,
            staged: staged
                .iter()
                .map(|stage| (stage.id, stage.value, stage.mask))
                .collect(),
            delayed_sites: staged
                .iter()
                .filter(|stage| stage.delay > 0)
                .filter_map(|stage| {
                    stage.site.map(|(value, pending)| NbaSite {
                        delay: stage.delay,
                        process: 0,
                        target: stage.id,
                        lsb: 0,
                        width: self.m.var(stage.id).width,
                        value,
                        position: None,
                        pending: Some(pending),
                        unit: true,
                    })
                })
                .collect(),
        })
    }
}

/// Reuse the sparse next-state protocol used by the Veryl frontend: no whole
/// memory seed, and publication touches only the dirty ranges.
pub(super) fn emit_sparse_memory_commits(
    builder: &mut SIRBuilder<Addr>,
    targets: &[VarAtomBase<SourceVarId>],
    sparse: &HashSet<SourceVarId>,
    pm: &ProcModule<'_>,
) {
    let mut committed = HashSet::default();
    for target in targets {
        if sparse.contains(&target.id) {
            if !committed.insert(target.id) {
                continue;
            }
            // Sparse publication consumes this memory's complete dirty set,
            // including updates from other processes evaluated before commit.
            // Describe the full object, and publish it once even if this
            // process writes several disjoint static ranges.
            builder.emit(SIRInstruction::Commit(
                Addr {
                    region: SPARSE_WORKING_REGION,
                    var_id: target.id,
                },
                stable(target.id),
                SIROffset::Static(0),
                pm.var(target.id).width,
                Vec::new(),
            ));
        } else {
            emit_ff_commits(builder, std::slice::from_ref(target), pm);
        }
    }
}

/// The units of an edge-triggered process: its evaluation, the commit of
/// all its results, and that commit split into the blocking results (the
/// active region) and the nonblocking ones (the NBA region).
pub(in crate::lowering) struct FfProcessUnits {
    pub eval: ExecutionUnit<Addr>,
    pub apply: ExecutionUnit<Addr>,
    pub active: ExecutionUnit<Addr>,
    pub nba: ExecutionUnit<Addr>,
    pub targets: Vec<VarAtomBase<SourceVarId>>,
    /// The staged variables, as (variable, staging value, staging mask).
    pub staged: Vec<(SourceVarId, SourceVarId, SourceVarId)>,
    /// Timed nonblocking sites of the delayed assignments, grouped later
    /// under a synthetic publication process.
    pub delayed_sites: Vec<NbaSite<SourceVarId>>,
}
