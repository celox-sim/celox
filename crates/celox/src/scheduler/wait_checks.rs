//! Registering the waits of the kernels and checking them for their edges.
use super::*;

/// The waits of the waiting kernels of `kernels`.
pub(super) fn waits_of<'a>(
    kernels: &'a [Option<Kernel>],
    ordinals: &[usize],
    schema: &celox_design::RuntimeSchema<AbsoluteAddr>,
) -> Vec<WaitCheck<'a>> {
    kernels
        .iter()
        .enumerate()
        .filter_map(|(index, kernel)| {
            let kernel = kernel.as_ref()?;
            Some(WaitCheck {
                index,
                delay: schema.processes[index].delay,
                ordinal: ordinals[index],
                wait: kernel.wait.as_ref()?,
                status: kernel.delays.then_some(schema.processes[index].status),
            })
        })
        .collect()
}

/// The place of each kernel among those the scheduler runs, in kernel
/// order; unused for the others.
pub(super) fn ordinals(kernels: &[Option<Kernel>]) -> Vec<usize> {
    let mut next = 0;
    kernels
        .iter()
        .map(|kernel| {
            let ordinal = next;
            next += usize::from(kernel.is_some());
            ordinal
        })
        .collect()
}

impl Generator<'_> {
    /// Store the baseline of kernel `index`, which suspended at the wait
    /// site in its `delay` slot, then continue at `target`: the level of
    /// the site's edge's signal now, with the edge's index when the kernel
    /// waits for several.
    pub(super) fn register_wait(
        &mut self,
        index: usize,
        delay: AbsoluteAddr,
        wait: &Wait,
        target: celox_sir::BlockId,
        internal_exit: celox_sir::BlockId,
    ) {
        if !wait.alternatives.is_empty() {
            let site = self.load(delay, SCHEDULER_TIME_WIDTH);
            let blocks: Vec<_> = wait
                .alternatives
                .iter()
                .map(|_| self.b.new_block())
                .collect();
            let sites: Vec<_> = wait.site_terms.iter().map(|&index| blocks[index]).collect();
            self.dispatch_sites(site, &sites, internal_exit);
            for (alternative, block) in wait.alternatives.iter().zip(blocks) {
                self.b.switch_to_block(block);
                self.capture_raw_wait(index, alternative);
                self.b.seal_block(SIRTerminator::Jump(target, Vec::new()));
            }
            return;
        }
        if wait.raw.is_some() {
            self.capture_raw_wait(index, wait);
            self.b.seal_block(SIRTerminator::Jump(target, Vec::new()));
            return;
        }
        if let [(signal, _)] = wait.terms[..] {
            self.capture_compact_counter(index, wait.counters[0]);
            let level = self.signal_level(signal);
            let level = self.baseline_byte(level);
            self.store(self.slots.baseline[index], SCHEDULER_FLAG_WIDTH, level);
            self.b.seal_block(SIRTerminator::Jump(target, Vec::new()));
            return;
        }
        // The site index, which `Plan::new` bounds by a switch selector's
        // width.
        let site = self.load(delay, SITE_BITS);
        let blocks: Vec<celox_sir::BlockId> =
            wait.terms.iter().map(|_| self.b.new_block()).collect();
        let cases = wait
            .site_terms
            .iter()
            .enumerate()
            .map(|(site, &term)| SIRSwitchCase {
                value: site.into(),
                target: blocks[term],
            })
            .collect();
        self.b.seal_block(SIRTerminator::Switch {
            selector: site,
            cases,
            default: internal_exit,
        });
        for (term, (&(signal, _), block)) in wait.terms.iter().zip(blocks).enumerate() {
            self.b.switch_to_block(block);
            self.capture_compact_counter(index, wait.counters[term]);
            let level = self.signal_level(signal);
            let level = self.baseline_byte(level);
            let index_bits =
                self.constant((term as u64) << BASELINE_TERM_SHIFT, SCHEDULER_FLAG_WIDTH);
            let byte = self.binary(SCHEDULER_FLAG_WIDTH, level, BinaryOp::Or, index_bits);
            self.store(self.slots.baseline[index], SCHEDULER_FLAG_WIDTH, byte);
            self.b.seal_block(SIRTerminator::Jump(target, Vec::new()));
        }
    }

    /// The preceding check established every active wait's baseline. Between
    /// it and this boundary only this unit ran, apart from the time slots
    /// updated at slot entry. Host writes are observed before native entry.
    /// Opaque calls and host-service processes keep a full scan. Buffered
    /// event notifications do not change simulation state at this boundary;
    /// cyclic SCHEDULER_RUN calls only charge the work budget.
    pub(super) fn check_waits_after_unit(
        &mut self,
        waits: &[WaitCheck],
        unit: &Unit,
        time_slots: &[AbsoluteAddr],
    ) {
        if instructions(unit).any(makes_host_calls) {
            self.check_waits_at(waits, false);
            return;
        }
        let storage = |address| self.shared.get(&address).copied().unwrap_or(address);
        let writes: HashSet<_> = instructions(unit)
            .flat_map(written_storage)
            .filter(|write| write.region == STABLE_REGION)
            .map(|write| storage(write.absolute_addr()))
            .chain(time_slots.iter().copied().map(storage))
            .collect();
        let affected: Vec<_> = waits
            .iter()
            .copied()
            .filter(|check| {
                check
                    .wait
                    .terms
                    .iter()
                    .any(|(signal, _)| writes.contains(&storage(*signal)))
                    || check
                        .wait
                        .counters
                        .iter()
                        .flatten()
                        .any(|counter| writes.contains(&storage(*counter)))
            })
            .collect();
        tracing::debug!(target: "celox::scheduler", total=waits.len(), affected=affected.len(), "filtered post-kernel wait checks");
        self.check_waits_at(&affected, false);
    }

    /// Test the waits of `waits` against their signals now
    /// (`check_waits`): a wait whose edge occurred since its baseline wakes
    /// its kernel; every changed signal becomes the baseline.
    pub(super) fn check_waits(&mut self, waits: &[WaitCheck]) {
        self.check_waits_at(waits, true);
    }

    pub(super) fn check_waits_at(&mut self, waits: &[WaitCheck], settled: bool) {
        if waits.is_empty() {
            return;
        }
        // A process's wait flag is set when a signal its wait watches
        // changes (`SettlePartitions::waits`): a wait checked since can wake
        // only once the flag is set again, so a clear flag skips the check.
        // The untracked byte stands for changes no one reports.
        let gate = self
            .program
            .runtime()
            .runtime_schema
            .settle_partitions
            .as_ref()
            .filter(|settle| settle.waits > 0)
            .map(|settle| {
                (
                    settle.flags,
                    settle.partitions as usize,
                    settle.waits as usize,
                )
            });
        let untracked = gate.map(|(flags, partitions, _)| {
            let byte = self.load_at(
                flags,
                celox_design::settle_untracked_flag(partitions) * 8,
                SCHEDULER_FLAG_WIDTH,
            );
            let zero = self.constant(0, SCHEDULER_FLAG_WIDTH);
            self.binary(1, byte, BinaryOp::Ne, zero)
        });
        let mut woken = self.constant(0, self.ready_bits);
        // The flagged checks, by the word of up to eight flag bytes holding
        // theirs: one load tells whether any of them is set, and a clear
        // word skips its checks together.
        let mut words: BTreeMap<usize, Vec<&WaitCheck>> = BTreeMap::new();
        for check in waits {
            match gate {
                Some((_, _, waits)) if check.index < waits => {
                    words.entry(check.index / 8).or_default().push(check);
                }
                _ => {
                    let wake = self.check_wait(check, settled);
                    let bit = self.bit(wake, check.ordinal);
                    woken = self.binary(self.ready_bits, woken, BinaryOp::Or, bit);
                }
            }
        }
        if let (Some((flags, partitions, waits)), Some(untracked)) = (gate, untracked) {
            for (word, checks) in words {
                let first = word * 8;
                let count = (waits - first).min(8);
                let width = count * SCHEDULER_FLAG_WIDTH;
                let bytes = self.load_at(
                    flags,
                    celox_design::settle_wait_flag(partitions, first) * 8,
                    width,
                );
                let zero = self.constant(0, width);
                let any = self.binary(1, bytes, BinaryOp::Ne, zero);
                let dirty = self.binary(1, any, BinaryOp::Or, untracked);
                let group = self.b.alloc_bit(self.ready_bits, false);
                let join = self.b.new_block_with(vec![group]);
                let run = self.b.new_block();
                let none = self.constant(0, self.ready_bits);
                self.b.seal_block(SIRTerminator::Branch {
                    cond: dirty,
                    true_block: (run, Vec::new()),
                    false_block: (join, vec![none]),
                });
                self.b.switch_to_block(run);
                let mut partial = self.constant(0, self.ready_bits);
                for check in checks {
                    let offset = celox_design::settle_wait_flag(partitions, check.index) * 8;
                    let byte = self.b.alloc_bit(SCHEDULER_FLAG_WIDTH, false);
                    self.b.emit(SIRInstruction::Slice(
                        byte,
                        bytes,
                        (check.index - first) * SCHEDULER_FLAG_WIDTH,
                        SCHEDULER_FLAG_WIDTH,
                    ));
                    let zero = self.constant(0, SCHEDULER_FLAG_WIDTH);
                    let dirty = self.binary(1, byte, BinaryOp::Ne, zero);
                    let dirty = self.binary(1, dirty, BinaryOp::Or, untracked);
                    let wake = self.b.alloc_bit(1, false);
                    let join = self.b.new_block_with(vec![wake]);
                    let run = self.b.new_block();
                    let clean = self.constant(0, 1);
                    self.b.seal_block(SIRTerminator::Branch {
                        cond: dirty,
                        true_block: (run, Vec::new()),
                        false_block: (join, vec![clean]),
                    });
                    self.b.switch_to_block(run);
                    self.store_at(flags, offset, SCHEDULER_FLAG_WIDTH, zero);
                    let woke = self.check_wait(check, settled);
                    self.b.seal_block(SIRTerminator::Jump(join, vec![woke]));
                    self.b.switch_to_block(join);
                    let bit = self.bit(wake, check.ordinal);
                    partial = self.binary(self.ready_bits, partial, BinaryOp::Or, bit);
                }
                self.b.seal_block(SIRTerminator::Jump(join, vec![partial]));
                self.b.switch_to_block(join);
                woken = self.binary(self.ready_bits, woken, BinaryOp::Or, group);
            }
        }
        let ready = self.load(self.slots.ready, self.ready_bits);
        let ready = self.binary(self.ready_bits, ready, BinaryOp::Or, woken);
        self.store(self.slots.ready, self.ready_bits, ready);
    }

    /// Check the wait of `check`, advance its baseline and clear its waiting
    /// flag when it woke; whether it woke.
    fn check_wait(&mut self, check: &WaitCheck, settled: bool) -> RegisterId {
        let wait = check.wait;
        let flag = self.waiting(check.index);
        let mut waiting = flag;
        if let Some(status) = check.status {
            let edge = self.equals_at(status, ProcessStatus::Wait);
            let pending = self.equals_at(status, ProcessStatus::Pending);
            let edge = self.binary(1, edge, BinaryOp::Or, pending);
            waiting = self.binary(1, waiting, BinaryOp::And, edge);
        }
        let wake = if !wait.alternatives.is_empty() {
            self.check_wait_site(check.index, check.delay, wait, waiting, settled)
        } else if wait.raw.is_some() {
            self.check_raw_wait(check.index, wait, waiting, settled)
        } else {
            let baseline = self.load(self.slots.baseline[check.index], SCHEDULER_FLAG_WIDTH);
            let level_width = if self.four_state.is_none() { 1 } else { 2 };
            // With several edges, the byte holds the edge's index above the
            // level (`BASELINE_TERM_SHIFT`).
            let (stored, term) = if wait.terms.len() == 1 {
                (baseline, None)
            } else {
                let level = self.b.alloc_bit(level_width, false);
                self.b
                    .emit(SIRInstruction::Slice(level, baseline, 0, level_width));
                let level = self.widen_to(level, level_width, SCHEDULER_FLAG_WIDTH);
                let shift = self.constant(u64::from(BASELINE_TERM_SHIFT), SCHEDULER_FLAG_WIDTH);
                let term = self.binary(SCHEDULER_FLAG_WIDTH, baseline, BinaryOp::Shr, shift);
                (level, Some(term))
            };
            let mut occurred = self.constant(0, 1);
            let mut current = None;
            let mut levels: Vec<(AbsoluteAddr, RegisterId)> = Vec::new();
            for (ordinal, &(signal, kind)) in wait.terms.iter().enumerate() {
                let is = match levels.iter().find(|(known, _)| *known == signal) {
                    Some(&(_, level)) => level,
                    None => {
                        let level = self.signal_level(signal);
                        levels.push((signal, level));
                        level
                    }
                };
                let mut edge = self.occurred(stored, is, kind);
                if let Some(counter) = wait.counters[ordinal] {
                    let previous =
                        self.load_at(self.slots.baseline[check.index], SCHEDULER_FLAG_WIDTH, 32);
                    let current = self.load(counter, 32);
                    let counted = self.binary(1, previous, BinaryOp::Ne, current);
                    edge = self.binary(1, edge, BinaryOp::Or, counted);
                }
                let (edge, is) = match term {
                    None => (edge, is),
                    Some(term) => {
                        let selected = self.equals(term, ordinal as u64);
                        let edge = self.binary(1, edge, BinaryOp::And, selected);
                        let is = match current {
                            None => is,
                            Some(other) => {
                                let level = self.b.alloc_bit(level_width, false);
                                self.b.emit(SIRInstruction::Mux(level, selected, is, other));
                                level
                            }
                        };
                        (edge, is)
                    }
                };
                occurred = self.binary(1, occurred, BinaryOp::Or, edge);
                current = Some(is);
            }
            let is = current.expect("a wait has an edge");
            let wake = self.binary(1, occurred, BinaryOp::And, waiting);
            let mut level = self.baseline_byte(is);
            if term.is_some() {
                // Keep the edge's index.
                let mask = self.constant(
                    (1 << SCHEDULER_FLAG_WIDTH) - (1 << BASELINE_TERM_SHIFT),
                    SCHEDULER_FLAG_WIDTH,
                );
                let index = self.binary(SCHEDULER_FLAG_WIDTH, baseline, BinaryOp::And, mask);
                level = self.binary(SCHEDULER_FLAG_WIDTH, level, BinaryOp::Or, index);
            }
            let next = self.b.alloc_bit(SCHEDULER_FLAG_WIDTH, false);
            self.b
                .emit(SIRInstruction::Mux(next, waiting, level, baseline));
            self.store(self.slots.baseline[check.index], SCHEDULER_FLAG_WIDTH, next);
            wake
        };
        let one = self.constant(1, 1);
        let stays = self.binary(1, wake, BinaryOp::Xor, one);
        let still = self.binary(1, flag, BinaryOp::And, stays);
        let flag = self.widen(still, SCHEDULER_FLAG_WIDTH);
        self.store(self.slots.waiting[check.index], SCHEDULER_FLAG_WIDTH, flag);
        wake
    }

    /// Whether the edge `kind` occurred from the level of the baseline byte
    /// `baseline`, its edge index cleared, to the level `is`.
    fn occurred(&mut self, baseline: RegisterId, is: RegisterId, kind: CounterKind) -> RegisterId {
        if self.four_state.is_none() {
            let zero = self.constant(0, SCHEDULER_FLAG_WIDTH);
            let was = self.binary(1, baseline, BinaryOp::Ne, zero);
            let changed = self.binary(1, was, BinaryOp::Ne, is);
            match kind {
                CounterKind::Changes => changed,
                CounterKind::Rises => self.binary(1, changed, BinaryOp::And, is),
                CounterKind::Falls => self.binary(1, changed, BinaryOp::And, was),
            }
        } else {
            // The baseline byte holds the level (`baseline_byte`); the
            // edges are those of `timing::occurs`, Table 9-2's.
            let was = self.b.alloc_bit(2, false);
            self.b.emit(SIRInstruction::Slice(was, baseline, 0, 2));
            match kind {
                CounterKind::Changes => self.binary(1, was, BinaryOp::Ne, is),
                CounterKind::Rises => self.level_edge(true, was, is),
                CounterKind::Falls => self.level_edge(false, was, is),
            }
        }
    }
}

impl Generator<'_> {
    fn raw_wait_planes(
        &mut self,
        signal: AbsoluteAddr,
        term: &celox_design::schedule::CompiledWaitTerm,
    ) -> RegisterId {
        if term.plane_bits == 1 && term.mask_offset.is_some() {
            return self.signal_level(signal);
        }
        if term.mask_offset.is_none() {
            return self.load(signal, term.width);
        }
        let mut value = self.load_state(signal, 0, term.width);
        if term.width < term.plane_bits {
            let padding = self.b.alloc_logic(term.plane_bits - term.width);
            self.b
                .emit(SIRInstruction::Imm(padding, SIRValue::new(0u64)));
            let padded = self.b.alloc_logic(term.plane_bits);
            self.b
                .emit(SIRInstruction::Concat(padded, vec![padding, value]));
            value = padded;
        }
        let planes = self.b.alloc_bit(term.plane_bits * 2, false);
        self.b.emit(SIRInstruction::Unary(
            planes,
            celox_design::UnaryOp::ToPlanes,
            value,
        ));
        planes
    }

    fn raw_wait_level(
        &mut self,
        planes: RegisterId,
        term: &celox_design::schedule::CompiledWaitTerm,
    ) -> RegisterId {
        let value = self.b.alloc_bit(1, false);
        self.b.emit(SIRInstruction::Slice(value, planes, 0, 1));
        if term.mask_offset.is_none() {
            return value;
        }
        let mask = self.b.alloc_bit(1, false);
        self.b
            .emit(SIRInstruction::Slice(mask, planes, term.plane_bits, 1));
        let level = self.b.alloc_bit(2, false);
        self.b
            .emit(SIRInstruction::Concat(level, vec![mask, value]));
        level
    }

    fn capture_compact_counter(&mut self, process: usize, counter: Option<AbsoluteAddr>) {
        if let Some(counter) = counter {
            let seen = self.load(counter, 32);
            self.store_at(self.slots.baseline[process], SCHEDULER_FLAG_WIDTH, 32, seen);
        }
    }

    fn capture_raw_wait(&mut self, process: usize, wait: &Wait) {
        for (ordinal, ((&(signal, _kind), term), counter)) in wait
            .terms
            .iter()
            .zip(&wait.raw.as_ref().unwrap().terms)
            .zip(&wait.counters)
            .enumerate()
        {
            if let (Some(offset), Some(counter)) = (term.counter_offset, counter) {
                let seen = self.load(*counter, 32);
                self.store_at(self.slots.baseline[process], offset, 32, seen);
            }
            if let Some(offset) = term.gated_counter_offset {
                let counter = wait.gates[ordinal].unwrap().counters.unwrap().gated;
                let seen = self.load(counter, 32);
                self.store_at(self.slots.baseline[process], offset, 32, seen);
            }
            let planes = self.raw_wait_planes(signal, term);
            let width = if term.mask_offset.is_some() {
                term.plane_bits * 2
            } else {
                term.width
            };
            self.store_at(
                self.slots.baseline[process],
                term.value_offset,
                width,
                planes,
            );
        }
    }

    /// A change-only OR wait can stop at its first changed expression.
    /// If none changed, its snapshot already equals the current state. If
    /// any changed, the wait is consumed and capture_raw_wait replaces the
    /// entire snapshot when the process next suspends. Edge-sensitive waits
    /// cannot do this: non-triggering changes must advance their baselines.
    fn check_change_wait(
        &mut self,
        process: usize,
        wait: &Wait,
        waiting: RegisterId,
    ) -> RegisterId {
        let zero = self.constant(0, 1);
        let one = self.constant(1, 1);
        let result = self.b.alloc_bit(1, false);
        let done = self.b.new_block_with(vec![result]);
        let body = self.b.new_block();
        self.b.seal_block(SIRTerminator::Branch {
            cond: waiting,
            true_block: (body, Vec::new()),
            false_block: (done, vec![zero]),
        });
        self.b.switch_to_block(body);
        let raw = wait.raw.as_ref().unwrap();
        // A small batch amortizes branches without extending the lifetime of
        // every expression in a large sensitivity list.
        for (signals, terms) in wait.terms.chunks(4).zip(raw.terms.chunks(4)) {
            let mut differences = zero;
            let mut difference_width = 1;
            for (&(signal, _), term) in signals.iter().zip(terms) {
                let width = if term.mask_offset.is_some() {
                    term.plane_bits * 2
                } else {
                    term.width
                };
                let previous = self.load_at(self.slots.baseline[process], term.value_offset, width);
                let current = self.raw_wait_planes(signal, term);
                // These are raw, two-state planes. Merge differing bits
                // before reducing to a boolean, instead of materializing a
                // comparison result for every value and mask chunk.
                let different = self.binary(width, previous, BinaryOp::Xor, current);
                difference_width = difference_width.max(width);
                differences = self.binary(difference_width, differences, BinaryOp::Or, different);
            }
            let no_difference = self.constant(0, difference_width);
            let changed = self.binary(1, differences, BinaryOp::Ne, no_difference);
            let next = self.b.new_block();
            self.b.seal_block(SIRTerminator::Branch {
                cond: changed,
                true_block: (done, vec![one]),
                false_block: (next, Vec::new()),
            });
            self.b.switch_to_block(next);
        }
        self.b.seal_block(SIRTerminator::Jump(done, vec![zero]));
        self.b.switch_to_block(done);
        result
    }

    /// Test every expression in an OR event list against its own snapshot,
    /// before publishing one ready bit and clearing the process's wait flag.
    pub(super) fn check_raw_wait(
        &mut self,
        process: usize,
        wait: &Wait,
        waiting: RegisterId,
        settled: bool,
    ) -> RegisterId {
        if wait.terms.len() >= 4
            && wait.counters.iter().all(Option::is_none)
            && wait.gates.iter().all(Option::is_none)
            && wait
                .terms
                .iter()
                .all(|(_, kind)| *kind == CounterKind::Changes)
        {
            return self.check_change_wait(process, wait, waiting);
        }
        let mut occurred = self.constant(0, 1);
        for (ordinal, ((&(signal, kind), term), counter)) in wait
            .terms
            .iter()
            .zip(&wait.raw.as_ref().unwrap().terms)
            .zip(&wait.counters)
            .enumerate()
        {
            let gate = wait.gates.get(ordinal).copied().flatten();
            if !settled && gate.is_some_and(|gate| !gate.at_write && gate.counters.is_none()) {
                continue;
            }
            let width = if term.mask_offset.is_some() {
                term.plane_bits * 2
            } else {
                term.width
            };
            let previous = self.load_at(self.slots.baseline[process], term.value_offset, width);
            let current = self.raw_wait_planes(signal, term);
            let changed = self.binary(1, previous, BinaryOp::Ne, current);
            let edge = match kind {
                CounterKind::Changes => changed,
                CounterKind::Rises | CounterKind::Falls if term.mask_offset.is_some() => {
                    let was = self.raw_wait_level(previous, term);
                    let is = self.raw_wait_level(current, term);
                    self.level_edge(kind == CounterKind::Rises, was, is)
                }
                CounterKind::Rises | CounterKind::Falls => {
                    let was = self.raw_wait_level(previous, term);
                    let is = self.raw_wait_level(current, term);
                    let changed = self.binary(1, was, BinaryOp::Ne, is);
                    self.binary(
                        1,
                        changed,
                        BinaryOp::And,
                        if kind == CounterKind::Rises { is } else { was },
                    )
                }
            };
            let gate_holds = gate.map(|gate| {
                let level = self.signal_level(gate.signal);
                self.equals(level, 1)
            });
            let edge = if let (Some(offset), Some(counter)) = (term.counter_offset, counter) {
                let seen = self.load_at(self.slots.baseline[process], offset, 32);
                let current = self.load(*counter, 32);
                let counted = self.binary(1, seen, BinaryOp::Ne, current);
                let next = self.b.alloc_bit(32, false);
                self.b
                    .emit(SIRInstruction::Mux(next, waiting, current, seen));
                self.store_at(self.slots.baseline[process], offset, 32, next);
                if let Some(gated_offset) = term.gated_counter_offset {
                    let gated = gate.unwrap().counters.unwrap().gated;
                    let seen = self.load_at(self.slots.baseline[process], gated_offset, 32);
                    let current = self.load(gated, 32);
                    let accepted = self.binary(1, seen, BinaryOp::Ne, current);
                    let one = self.constant(1, 1);
                    let uncounted = self.binary(1, counted, BinaryOp::Xor, one);
                    let raw = self.binary(1, edge, BinaryOp::And, uncounted);
                    let raw = self.binary(1, raw, BinaryOp::And, gate_holds.unwrap());
                    self.binary(1, accepted, BinaryOp::Or, raw)
                } else {
                    let edge = self.binary(1, edge, BinaryOp::Or, counted);
                    gate_holds.map_or(edge, |gate| self.binary(1, edge, BinaryOp::And, gate))
                }
            } else {
                gate_holds.map_or(edge, |gate| self.binary(1, edge, BinaryOp::And, gate))
            };
            occurred = self.binary(1, occurred, BinaryOp::Or, edge);
            let next = self.b.alloc_bit(width, false);
            self.b
                .emit(SIRInstruction::Mux(next, waiting, current, previous));
            self.store_at(self.slots.baseline[process], term.value_offset, width, next);
        }
        self.binary(1, occurred, BinaryOp::And, waiting)
    }
}
