//! Lowering scheduled work to execution units.

use super::*;

/// Read-only facts shared by every lowering of one prepared schedule.
pub(super) struct WorkLoweringContext<'a, Addr: Clone + Eq + Hash> {
    pub(super) input: &'a [LogicPath<Addr>],
    pub(super) arena: &'a SLTNodeArena<Addr>,
    pub(super) adj: &'a [Vec<usize>],
    pub(super) component_by_path: &'a [usize],
    pub(super) ignored_loops: &'a HashSet<(Addr, Addr)>,
    pub(super) true_loops: &'a HashMap<(Addr, Addr), usize>,
    pub(super) fold_group_schedule_index: &'a FoldGroupScheduleIndex<Addr>,
    pub(super) unpacked_element_widths: &'a HashMap<Addr, usize>,
    pub(super) direct_ff_writes_by_action: &'a [Vec<VarAtomBase<Addr>>],
    pub(super) four_state: bool,
    /// Start a new execution unit when one grows past [`EU_BLOCK_LIMIT`].
    /// Shared comb/FF lowering keeps one unit because its staged FF state
    /// publishes at the end of the same function.
    pub(super) split_large_units: bool,
}

/// Maximum blocks in a single EU before flushing to a new one.
/// This prevents Cranelift from choking on massive functions.
pub(super) const EU_BLOCK_LIMIT: usize = 20_000;

pub(super) const UNROLL_THRESHOLD: usize = 32;

/// Pairwise joint-fold profitability is intentionally local. Keep all
/// projections of one packed root together, but never build an unbounded
/// compatibility matrix for a long run of independent roots.
pub(super) const MAX_JOINT_FOLD_ROOTS: usize = 16;

/// Lowering state for one sequence of scheduled work emitted through one SIR
/// builder. Register numbers and the expression cache are local to it.
pub(super) struct WorkLowering<'a, Addr: Clone + Eq + Hash> {
    pub(super) context: &'a WorkLoweringContext<'a, Addr>,
    pub(super) lowerer: &'a mut crate::SLTToSIRLowerer,
    pub(super) builder: SIRBuilder<Addr>,
    pub(super) lower_cache: HashMap<NodeId, RegisterId>,
    pub(super) dep_memo: HashMap<NodeId, HashSet<Addr>>,
    pub(super) inverse_dep_memo: HashMap<Addr, HashSet<NodeId>>,
    pub(super) execution_units: Vec<ExecutionUnit<Addr>>,
    pub(super) pending_fold_indices: Vec<usize>,
    pub(super) pending_fold_roots: HashSet<NodeId>,
    /// The processes whose unit an acyclic projection has run (see
    /// [`Self::lower`]).
    pub(super) run_processes: HashSet<Addr>,
    /// Inside a partition of a partitioned settle, which units must not
    /// split.
    pub(super) in_partition: bool,
    /// Where a guarded partition continues when it is skipped.
    pub(super) partition_merge: Option<BlockId>,
    /// Where a group of clean, effect-free partitions can be skipped.
    pub(super) partition_group_merge: Option<BlockId>,
    /// The wait flags of a settle that counts its work, which its run
    /// counters follow ([`SettlePartitioning::profile`]).
    pub(super) settle_profile_waits: Option<usize>,
    /// The partitions of the settle and the byte offset of its run counters
    /// among its flags, once the settle began and when it counts its work.
    pub(super) settle_profile: Option<(usize, usize)>,
    /// The open partition's watched chunks before it ran.
    pub(super) partition_old: Vec<RegisterId>,
}

impl<'a, Addr: Clone + Eq + Ord + Hash + Debug + Copy + Display> WorkLowering<'a, Addr> {
    pub(super) fn new(
        context: &'a WorkLoweringContext<'a, Addr>,
        lowerer: &'a mut crate::SLTToSIRLowerer,
    ) -> Self {
        Self {
            context,
            lowerer,
            builder: SIRBuilder::new(),
            lower_cache: HashMap::default(),
            dep_memo: HashMap::default(),
            inverse_dep_memo: HashMap::default(),
            execution_units: Vec::new(),
            pending_fold_indices: Vec::new(),
            pending_fold_roots: HashSet::default(),
            run_processes: HashSet::default(),
            in_partition: false,
            partition_merge: None,
            partition_group_merge: None,
            settle_profile_waits: None,
            settle_profile: None,
            partition_old: Vec::new(),
        }
    }

    /// Start a settle of `count` partitions. An untracked raw view requires
    /// every partition to run: set every flag, so that the guards test
    /// nothing but their own flag.
    pub(super) fn begin_settle(&mut self, flags: Addr, count: usize) {
        if let Some(waits) = self.settle_profile_waits {
            self.settle_profile = Some((count, celox_design::settle_profile_offset(count, waits)));
            let one = self.builder.alloc_bit(64, false);
            self.builder
                .emit(SIRInstruction::Imm(one, SIRValue::new(1u64)));
            self.count_work(flags, 2 * count, one);
        }
        let untracked = self.builder.alloc_bit(8, false);
        self.builder.emit(SIRInstruction::Load(
            untracked,
            flags,
            SIROffset::Static(celox_design::settle_untracked_flag(count) * 8),
            8,
        ));
        let zero = self.builder.alloc_bit(8, false);
        self.builder
            .emit(SIRInstruction::Imm(zero, SIRValue::new(0u64)));
        let raw = self.builder.alloc_bit(1, false);
        self.builder
            .emit(SIRInstruction::Binary(raw, untracked, BinaryOp::Ne, zero));
        let fill = self.builder.new_block();
        let start = self.builder.new_block();
        self.builder.seal_block(SIRTerminator::Branch {
            cond: raw,
            true_block: (fill, Vec::new()),
            false_block: (start, Vec::new()),
        });
        self.builder.switch_to_block(fill);
        for first in (0..count).step_by(8) {
            let bytes = (count - first).min(8);
            let ones = self.builder.alloc_bit(bytes * 8, false);
            self.builder.emit(SIRInstruction::Imm(
                ones,
                SIRValue::new(0x0101_0101_0101_0101u64 & (u64::MAX >> (64 - bytes * 8))),
            ));
            self.builder.emit(SIRInstruction::Store(
                flags,
                SIROffset::Static(first * 8),
                bytes * 8,
                ones,
                Vec::new(),
                Vec::new(),
            ));
        }
        self.builder
            .seal_block(SIRTerminator::Jump(start, Vec::new()));
        self.builder.switch_to_block(start);
    }

    /// Add `amount` (64 bits) to run counter `word` of a settle that counts
    /// its work ([`SettlePartitioning::profile`]).
    pub(super) fn count_work(&mut self, flags: Addr, word: usize, amount: RegisterId) {
        let Some((_, start)) = self.settle_profile else {
            return;
        };
        let offset = SIROffset::Static((start + word * 8) * 8);
        let [old, new] = std::array::from_fn(|_| self.builder.alloc_bit(64, false));
        self.builder
            .emit(SIRInstruction::Load(old, flags, offset.clone(), 64));
        self.builder
            .emit(SIRInstruction::Binary(new, old, BinaryOp::Add, amount));
        self.builder.emit(SIRInstruction::Store(
            flags,
            offset,
            64,
            new,
            Vec::new(),
            Vec::new(),
        ));
    }

    /// Compare each chunk with its value before the partition ran (`old`),
    /// and set the flags of its `readers` when it changed. Readers with the
    /// same dependencies share a change test; adjacent readers share a
    /// constant flag store.
    pub(super) fn mark_changed_readers(
        &mut self,
        flags: Addr,
        one: RegisterId,
        chunks: Vec<(&WatchedChunk<Addr>, &[usize], RegisterId)>,
    ) -> Option<RegisterId> {
        let mut set_changes: Vec<(&[usize], RegisterId)> = Vec::new();
        for (chunk, readers, old) in chunks {
            let new = self.builder.alloc_logic(chunk.width);
            self.builder.emit(SIRInstruction::Load(
                new,
                chunk.object,
                chunk.offset.clone(),
                chunk.width,
            ));
            // Both planes: an X turning into a known value is a change.
            let changed = self.builder.alloc_bit(1, false);
            self.builder
                .emit(SIRInstruction::Binary(changed, old, BinaryOp::NeCase, new));
            match set_changes.iter_mut().find(|(set, _)| *set == readers) {
                Some((_, any)) => {
                    let both = self.builder.alloc_bit(1, false);
                    self.builder
                        .emit(SIRInstruction::Binary(both, *any, BinaryOp::Or, changed));
                    *any = both;
                }
                None => set_changes.push((readers, changed)),
            }
        }
        // Whether anything compared changed, for the run counters.
        let mut any_changed = None;
        if self.settle_profile.is_some() {
            for &(_, changed) in &set_changes {
                any_changed = Some(match any_changed {
                    None => changed,
                    Some(any) => {
                        let both = self.builder.alloc_bit(1, false);
                        self.builder
                            .emit(SIRInstruction::Binary(both, any, BinaryOp::Or, changed));
                        both
                    }
                });
            }
        }
        for (readers, changed) in set_changes {
            let publish = self.builder.new_block();
            let done = self.builder.new_block();
            self.builder.seal_block(SIRTerminator::Branch {
                cond: changed,
                true_block: (publish, Vec::new()),
                false_block: (done, Vec::new()),
            });
            self.builder.switch_to_block(publish);
            // Adjacent flags are set by one constant store of 1, 2, 4 or 8
            // bytes; a run of another length splits into those. Flags are
            // never read back and ORed in: a word of another width costs the
            // backend a masked read-modify-write for every flag it sets.
            let mut bytes: Vec<usize> = readers.to_vec();
            bytes.sort_unstable();
            bytes.dedup();
            let mut index = 0;
            while index < bytes.len() {
                let first = bytes[index];
                let mut run = 1;
                while index + run < bytes.len() && bytes[index + run] == first + run {
                    run += 1;
                }
                index += run;
                let mut at = first;
                let mut left = run;
                while left > 0 {
                    let piece = [8, 4, 2, 1]
                        .into_iter()
                        .find(|&piece| piece <= left)
                        .unwrap();
                    let marked = if piece == 1 {
                        one
                    } else {
                        let constant = self.builder.alloc_bit(piece * 8, false);
                        let ones = 0x0101_0101_0101_0101u64 & (u64::MAX >> (64 - piece * 8));
                        self.builder
                            .emit(SIRInstruction::Imm(constant, SIRValue::new(ones)));
                        constant
                    };
                    self.builder.emit(SIRInstruction::Store(
                        flags,
                        SIROffset::Static(at * 8),
                        piece * 8,
                        marked,
                        Vec::new(),
                        Vec::new(),
                    ));
                    at += piece;
                    left -= piece;
                }
            }
            self.builder
                .seal_block(SIRTerminator::Jump(done, Vec::new()));
            self.builder.switch_to_block(done);
        }
        any_changed
    }

    /// Skip a consecutive group without testing every individual clean flag.
    /// A pinned partition can have effects even without an input change.
    pub(super) fn begin_partition_group(
        &mut self,
        flags: Addr,
        first: usize,
        partitions: &[PlannedPartition<Addr>],
    ) {
        if partitions.len() < 4 || partitions.iter().any(|partition| partition.pinned) {
            return;
        }
        if self.context.split_large_units && self.builder.block_count() >= EU_BLOCK_LIMIT {
            if let Some(eu) = self.builder.flush_eu() {
                self.execution_units.push(eu);
            }
        }
        let width = partitions.len() * 8;
        let word = self.builder.alloc_bit(width, false);
        self.builder.emit(SIRInstruction::Load(
            word,
            flags,
            SIROffset::Static(first * 8),
            width,
        ));
        let zero = self.builder.alloc_bit(width, false);
        self.builder
            .emit(SIRInstruction::Imm(zero, SIRValue::new(0u64)));
        let dirty = self.builder.alloc_bit(1, false);
        self.builder
            .emit(SIRInstruction::Binary(dirty, word, BinaryOp::Ne, zero));
        let body = self.builder.new_block();
        let done = self.builder.new_block();
        self.builder.seal_block(SIRTerminator::Branch {
            cond: dirty,
            true_block: (body, Vec::new()),
            false_block: (done, Vec::new()),
        });
        self.builder.switch_to_block(body);
        self.partition_group_merge = Some(done);
    }

    pub(super) fn end_partition_group(&mut self) {
        if let Some(done) = self.partition_group_merge.take() {
            self.builder
                .seal_block(SIRTerminator::Jump(done, Vec::new()));
            self.builder.switch_to_block(done);
        }
        self.lower_cache.clear();
    }

    /// Open partition `index`: run it only when its flag is set, and clear
    /// its flag. Values computed before it may be reused inside, but not the
    /// other way round, so the expression cache starts over.
    pub(super) fn begin_partition(
        &mut self,
        flags: Addr,
        index: usize,
        partition: &PlannedPartition<Addr>,
    ) {
        self.flush_pending_folds();
        if self.partition_group_merge.is_none()
            && self.context.split_large_units
            && self.builder.block_count() >= EU_BLOCK_LIMIT
        {
            if let Some(eu) = self.builder.flush_eu() {
                self.execution_units.push(eu);
            }
        }
        self.lower_cache.clear();
        self.in_partition = true;
        let offset = SIROffset::Static(index * 8);
        if !partition.pinned {
            let own = self.builder.alloc_bit(8, false);
            self.builder
                .emit(SIRInstruction::Load(own, flags, offset.clone(), 8));
            let zero = self.builder.alloc_bit(8, false);
            self.builder
                .emit(SIRInstruction::Imm(zero, SIRValue::new(0u64)));
            let dirty = self.builder.alloc_bit(1, false);
            self.builder
                .emit(SIRInstruction::Binary(dirty, own, BinaryOp::Ne, zero));
            let body = self.builder.new_block();
            let merge = self.builder.new_block();
            self.builder.seal_block(SIRTerminator::Branch {
                cond: dirty,
                true_block: (body, Vec::new()),
                false_block: (merge, Vec::new()),
            });
            self.builder.switch_to_block(body);
            self.partition_merge = Some(merge);
        }
        let clean = self.builder.alloc_bit(8, false);
        self.builder
            .emit(SIRInstruction::Imm(clean, SIRValue::new(0u64)));
        self.builder.emit(SIRInstruction::Store(
            flags,
            offset,
            8,
            clean,
            Vec::new(),
            Vec::new(),
        ));
        if self.settle_profile.is_some() {
            let one = self.builder.alloc_bit(64, false);
            self.builder
                .emit(SIRInstruction::Imm(one, SIRValue::new(1u64)));
            self.count_work(flags, index, one);
        }
        self.partition_old.clear();
        for chunk in &partition.watched {
            let old = self.builder.alloc_logic(chunk.width);
            self.builder.emit(SIRInstruction::Load(
                old,
                chunk.object,
                chunk.offset.clone(),
                chunk.width,
            ));
            self.partition_old.push(old);
        }
    }

    /// Close a partition: set the flags of the partitions reading what it
    /// changed, then join the path that skipped it.
    pub(super) fn end_partition(
        &mut self,
        flags: Addr,
        index: usize,
        partition: &PlannedPartition<Addr>,
    ) {
        self.flush_pending_folds();
        if !partition.watched.is_empty() {
            let one = self.builder.alloc_bit(8, false);
            self.builder
                .emit(SIRInstruction::Imm(one, SIRValue::new(1u64)));
            let old = std::mem::take(&mut self.partition_old);
            // A full settle also clears each later reader's flag when it
            // runs. Publishing to those readers is harmless and shares the
            // normal path, avoiding a duplicate comparison/notification CFG.
            let all = partition
                .watched
                .iter()
                .zip(&old)
                .map(|(chunk, &old)| (chunk, chunk.readers.as_slice(), old))
                .collect();
            let changed = self.mark_changed_readers(flags, one, all);
            if let (Some((count, _)), Some(changed)) = (self.settle_profile, changed) {
                let [one, zero, amount] =
                    std::array::from_fn(|_| self.builder.alloc_bit(64, false));
                self.builder
                    .emit(SIRInstruction::Imm(one, SIRValue::new(1u64)));
                self.builder
                    .emit(SIRInstruction::Imm(zero, SIRValue::new(0u64)));
                self.builder
                    .emit(SIRInstruction::Mux(amount, changed, one, zero));
                self.count_work(flags, count + index, amount);
            }
        } else if let Some((count, _)) = self.settle_profile {
            // Nothing compared: every run counts as changing something.
            let one = self.builder.alloc_bit(64, false);
            self.builder
                .emit(SIRInstruction::Imm(one, SIRValue::new(1u64)));
            self.count_work(flags, count + index, one);
        }
        if !partition.marks.is_empty() {
            let one = self.builder.alloc_bit(8, false);
            self.builder
                .emit(SIRInstruction::Imm(one, SIRValue::new(1u64)));
            for &reader in &partition.marks {
                self.builder.emit(SIRInstruction::Store(
                    flags,
                    SIROffset::Static(reader * 8),
                    8,
                    one,
                    Vec::new(),
                    Vec::new(),
                ));
            }
        }
        if let Some(merge) = self.partition_merge.take() {
            self.builder
                .seal_block(SIRTerminator::Jump(merge, Vec::new()));
            self.builder.switch_to_block(merge);
        }
        self.lower_cache.clear();
        self.in_partition = false;
    }

    /// Lower the buffered run of exact grouped-fold paths.
    pub(super) fn flush_pending_folds(&mut self) {
        let context = self.context;
        flush_pending_fold_paths(
            &mut self.pending_fold_indices,
            context.input,
            context.fold_group_schedule_index,
            self.lowerer,
            &mut self.builder,
            context.arena,
            &mut self.lower_cache,
            &mut self.dep_memo,
            &mut self.inverse_dep_memo,
            context.unpacked_element_widths,
            context.four_state,
        );
        self.pending_fold_roots.clear();
    }

    /// Emit SIR for one logic path and keep the lowering cache coherent.
    pub(super) fn emit_node(&mut self, idx: usize) {
        let context = self.context;
        let path = &context.input[idx];
        collect_logic_path_input_deps(
            path,
            context.arena,
            &mut self.dep_memo,
            &mut self.inverse_dep_memo,
        );
        emit_logic_path_store(
            self.lowerer,
            &mut self.builder,
            path,
            context.arena,
            &mut self.lower_cache,
            context.unpacked_element_widths,
        );
        invalidate_logic_path_target(path, &self.inverse_dep_memo, &mut self.lower_cache);
    }

    /// Lower one scheduled component, selecting static unrolling or dynamic
    /// convergence for cyclic SCCs.
    pub(super) fn lower<E>(
        &mut self,
        work: &ScheduledWork,
        ff: Option<&mut (dyn ClockFfLowering<Addr, Error = E> + '_)>,
        runtime_errors: &mut HashMap<i64, RuntimeErrorInfo<Addr>>,
        next_runtime_error_code: &mut i64,
    ) -> Result<(), ClockSortError<Addr, E>> {
        let context = self.context;
        let input = context.input;
        let adj = context.adj;
        let component_by_path = context.component_by_path;
        let singleton;
        let scc = match work {
            ScheduledWork::CombPath(path) => {
                singleton = [*path];
                singleton.as_slice()
            }
            ScheduledWork::CombScc(scc) => scc.as_slice(),
            ScheduledWork::GuardedComb { condition, paths } => {
                self.flush_pending_folds();
                emit_scheduled_guard_region(
                    self.lowerer,
                    &mut self.builder,
                    *condition,
                    paths,
                    input,
                    context.arena,
                    &mut self.lower_cache,
                    &mut self.dep_memo,
                    &mut self.inverse_dep_memo,
                    context.unpacked_element_widths,
                );
                return Ok(());
            }
            ScheduledWork::Ff(index) => {
                self.flush_pending_folds();
                ff.ok_or(ClockSortError::Scheduler(
                    SchedulerError::InvalidDependencyGraph,
                ))?
                .lower(
                    *index,
                    &context.direct_ff_writes_by_action[*index],
                    &mut self.builder,
                )
                .map_err(ClockSortError::Lowering)?;
                // Any directly published range is ordered after all of its
                // old-state readers. Other FF state stays invisible in
                // WORKING/SPARSE_WORKING until the final publish, so values
                // computed before the FF CFG keep the ordinary lowering cache
                // valid.
                return Ok(());
            }
        };
        let component = component_by_path[scc[0]];
        let mut user_safety_limit = None;
        for &v_idx in scc {
            for &u_idx in &adj[v_idx] {
                if component_by_path[u_idx] == component {
                    if let (Some(v_target), Some(u_target)) =
                        (input[v_idx].target.var(), input[u_idx].target.var())
                    {
                        let edge = (v_target.id, u_target.id);
                        if let Some(&limit) = context.true_loops.get(&edge) {
                            user_safety_limit =
                                Some(user_safety_limit.map_or(limit, |l: usize| l.max(limit)));
                        }
                    }
                }
            }
        }
        let is_loop = scc.len() > 1 || (scc.len() == 1 && adj[scc[0]].contains(&scc[0]));

        if is_loop {
            // Exact grouped folds are atomic with respect to a loop SCC.
            self.flush_pending_folds();
            let mut authorized = user_safety_limit.is_some();
            'check_scc: for &v_idx in scc {
                for &u_idx in &adj[v_idx] {
                    if component_by_path[u_idx] == component
                        && input[v_idx]
                            .target
                            .var()
                            .zip(input[u_idx].target.var())
                            .is_some_and(|(v, u)| context.ignored_loops.contains(&(v.id, u.id)))
                    {
                        // Some loops are explicitly allowed by the user (e.g., false loops).
                        authorized = true;
                        break 'check_scc;
                    }
                }
            }

            if !authorized {
                return Err(ClockSortError::Scheduler(
                    SchedulerError::CombinationalLoop {
                        blocks: scc.iter().map(|idx| input[*idx].clone()).collect(),
                    },
                ));
            }

            // FAS Sort
            let optimized_scc_order = greedy_fas_sort(scc, adj);
            let force_strategy_b = user_safety_limit.is_some();
            let iterations = calculate_required_iterations(adj, &optimized_scc_order);
            let total_ops_estimate = optimized_scc_order.len().saturating_mul(iterations);
            if !force_strategy_b && total_ops_estimate <= UNROLL_THRESHOLD {
                // Strategy A: Static Unrolling
                // The loop is unrolled a fixed number of times based on structural dependency depth (iterations).
                for _ in 0..iterations {
                    for &idx in &optimized_scc_order {
                        self.emit_node(idx);
                    }
                }
            } else {
                let runtime_error_code = *next_runtime_error_code;
                *next_runtime_error_code += 1;
                let mut seen = HashSet::default();
                let sources = scc
                    .iter()
                    .filter_map(|idx| {
                        let addr = input[*idx].target.var()?.id;
                        seen.insert(addr).then_some(addr)
                    })
                    .collect::<Vec<_>>();
                runtime_errors.insert(
                    runtime_error_code,
                    RuntimeErrorInfo {
                        message: "Detected True Loop".to_string(),
                        signals: sources,
                    },
                );

                // Strategy B: Dynamic Convergence
                // Implements a runtime loop that continues executing the SCC until all signals converge (dirty flag is false).
                // Includes a safety limit to detect non-converging "True Loops" and avoid infinite hang.

                // 1. Determine the runtime repetition limit.
                let safety_limit = user_safety_limit.unwrap_or(iterations + 1);

                // 2. Prepare Constants and Counters
                let zero_reg = self.builder.alloc_bit(64, false);
                self.builder
                    .emit(SIRInstruction::Imm(zero_reg, SIRValue::new(0u64)));

                let limit_reg = self.builder.alloc_bit(64, false);
                self.builder.emit(SIRInstruction::Imm(
                    limit_reg,
                    SIRValue::new(safety_limit as u64),
                ));

                // 3. Blocks
                let current_counter = self.builder.alloc_bit(64, false);
                let header_block = self.builder.new_block_with(vec![current_counter]); // [counter]
                let body_block = self.builder.new_block();
                let exit_block = self.builder.new_block();
                let error_block = self.builder.new_block(); // For True Loop detection

                // Start: Jump to header with counter = 0
                self.builder
                    .seal_block(SIRTerminator::Jump(header_block, vec![zero_reg]));

                // --- Header Block ---
                self.builder.switch_to_block(header_block);

                // Check: counter < safety_limit
                let can_continue_reg = self.builder.alloc_bit(1, false);
                self.builder.emit(SIRInstruction::Binary(
                    can_continue_reg,
                    current_counter,
                    BinaryOp::LtU,
                    limit_reg,
                ));

                // If counter exceeded limit, we might have an oscillating True Loop
                self.builder.seal_block(SIRTerminator::Branch {
                    cond: can_continue_reg,
                    true_block: (body_block, vec![]),
                    false_block: (error_block, vec![]),
                });
                self.builder.switch_to_block(body_block);
                let mut current_dirty_reg = self.builder.alloc_bit(1, false);
                self.builder
                    .emit(SIRInstruction::Imm(current_dirty_reg, SIRValue::new(0u32)));
                for &idx in &optimized_scc_order {
                    let path = &input[idx];
                    let Some(target) = path.target.var() else {
                        emit_logic_path_store(
                            self.lowerer,
                            &mut self.builder,
                            path,
                            context.arena,
                            &mut self.lower_cache,
                            context.unpacked_element_widths,
                        );
                        continue;
                    };
                    let width = 1 + target.access.msb - target.access.lsb;
                    let addr = target.id;
                    let offset = static_access_offset(
                        &target.id,
                        target.access,
                        context.unpacked_element_widths,
                    );

                    // --- Dynamic Convergence Check Logic ---
                    // For each node in the SCC, we verify if its value changed after this iteration.
                    //
                    // a. Load the current value (pre-update benchmark)
                    let old_val_reg = self.builder.alloc_bit(width, false);
                    self.builder.emit(SIRInstruction::Load(
                        old_val_reg,
                        addr,
                        offset.clone(),
                        width,
                    ));
                    collect_logic_path_input_deps(
                        path,
                        context.arena,
                        &mut self.dep_memo,
                        &mut self.inverse_dep_memo,
                    );
                    // b. Compute the new value
                    let new_val_reg = lower_logic_path_expr(
                        self.lowerer,
                        &mut self.builder,
                        path,
                        context.arena,
                        &mut self.lower_cache,
                    );

                    // c. Compare: changed = (old != new)
                    let is_changed_reg = self.builder.alloc_bit(1, false);
                    self.builder.emit(SIRInstruction::Binary(
                        is_changed_reg,
                        old_val_reg,
                        BinaryOp::Ne, // Not Equal
                        new_val_reg,
                    ));
                    let new_dirty_reg = self.builder.alloc_bit(1, false);

                    // d. Accumulate dirty flag: dirty = dirty | is_changed
                    // If any signal in the SCC changes, the entire SCC requires another iteration.
                    self.builder.emit(SIRInstruction::Binary(
                        new_dirty_reg,
                        current_dirty_reg,
                        BinaryOp::Or,
                        is_changed_reg,
                    ));
                    current_dirty_reg = new_dirty_reg;
                    // e. Store the new value
                    self.builder.emit(SIRInstruction::Store(
                        addr,
                        offset,
                        width,
                        new_val_reg,
                        Vec::new(),
                        Vec::new(),
                    ));
                    if !path.comb_capture_enable_sites.is_empty() {
                        let (old, new) = if path.comb_capture_enable_always {
                            let old = self.builder.alloc_bit(1, false);
                            let new = self.builder.alloc_bit(1, false);
                            self.builder
                                .emit(SIRInstruction::Imm(old, SIRValue::new(0u8)));
                            self.builder
                                .emit(SIRInstruction::Imm(new, SIRValue::new(1u8)));
                            (old, new)
                        } else {
                            (old_val_reg, new_val_reg)
                        };
                        self.builder
                            .emit(SIRInstruction::CombCaptureEnableIfChanged {
                                old,
                                new,
                                sites: path.comb_capture_enable_sites.clone(),
                            });
                    }
                    if let Some(to_remove) = self.inverse_dep_memo.get(&addr) {
                        for node in to_remove {
                            self.lower_cache.remove(node);
                        }
                    }
                    // -------------------------------
                }

                // 4. Branch: Loop if dirty
                let one_reg = self.builder.alloc_bit(64, false);
                self.builder
                    .emit(SIRInstruction::Imm(one_reg, SIRValue::new(1u64)));
                let next_counter = self.builder.alloc_bit(64, false);
                self.builder.emit(SIRInstruction::Binary(
                    next_counter,
                    current_counter,
                    BinaryOp::Add,
                    one_reg,
                ));

                // Increment the iteration counter and branch.
                // If 'dirty' is true, return to the header block; otherwise, exit the loop.
                self.builder.seal_block(SIRTerminator::Branch {
                    cond: current_dirty_reg,
                    true_block: (header_block, vec![next_counter]),
                    false_block: (exit_block, vec![]),
                });

                // --- Error/Exit Blocks ---
                self.builder.switch_to_block(error_block);
                // Emit a trap or special instruction to indicate "Combinational Loop Oscillation"
                // self.builder.emit(SIRInstruction::Trap(1));
                self.builder
                    .seal_block(SIRTerminator::Error(runtime_error_code));

                // 5. Exit Block
                self.builder.switch_to_block(exit_block);
            }
        } else {
            // DAG Part — flush before emitting if the EU has grown too large
            if context.split_large_units
                && !self.in_partition
                && self.builder.block_count() >= EU_BLOCK_LIMIT
            {
                self.flush_pending_folds();
                if let Some(eu) = self.builder.flush_eu() {
                    self.execution_units.push(eu);
                    // Clear the lowering cache — register IDs are EU-scoped
                    self.lower_cache.clear();
                }
            }

            let idx = scc[0];
            let exact_fold = is_exact_fold_path(idx, context.fold_group_schedule_index);
            let fold_root = exact_fold
                .then_some(context.fold_group_schedule_index.direct_group_by_path[idx])
                .flatten();
            let depends_on_pending = self
                .pending_fold_indices
                .iter()
                .any(|pending| adj[*pending].binary_search(&idx).is_ok());
            let starts_new_root =
                fold_root.is_some_and(|root| !self.pending_fold_roots.contains(&root));
            let window_full =
                starts_new_root && self.pending_fold_roots.len() >= MAX_JOINT_FOLD_ROOTS;
            if exact_fold && !depends_on_pending && !window_full {
                self.pending_fold_indices.push(idx);
                self.pending_fold_roots.extend(fold_root);
            } else {
                self.flush_pending_folds();
                if exact_fold {
                    self.pending_fold_indices.push(idx);
                    self.pending_fold_roots.extend(fold_root);
                } else if let LogicPathTarget::Process { process, .. } = &input[idx].target
                    && !self.run_processes.insert(*process)
                {
                    // The unit writes every output of its process. Each
                    // projection depends on all its sources, so they were
                    // all written before the first projection ran the unit,
                    // and this projection's activation guard would find them
                    // unchanged.
                } else {
                    self.emit_node(idx);
                }
            }
        }
        Ok(())
    }

    /// Terminate the current unit and return every unit emitted so far.
    pub(super) fn finish(mut self) -> Vec<ExecutionUnit<Addr>> {
        self.builder.seal_block(SIRTerminator::Return);
        let (blocks, register_map, _) = self.builder.drain();
        self.execution_units.push(ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        });
        self.execution_units
    }
}
