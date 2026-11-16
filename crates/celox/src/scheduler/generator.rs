//! The primitives of the [`Generator`]: constants, loads and stores of the
//! scheduler slots and the state, levels and edges of events, service calls,
//! and the finished unit.
use super::*;

impl Generator<'_> {
    pub(super) fn constant(&mut self, value: u64, width: usize) -> RegisterId {
        self.wide_constant(BigUint::from(value), width)
    }

    pub(super) fn wide_constant(&mut self, value: BigUint, width: usize) -> RegisterId {
        let register = self.b.alloc_bit(width, false);
        self.b
            .emit(SIRInstruction::Imm(register, SIRValue::new(value)));
        register
    }

    /// The `width`-bit value with only bit `bit` set.
    pub(super) fn single(&mut self, bit: usize, width: usize) -> RegisterId {
        self.wide_constant(BigUint::from(1u8) << bit, width)
    }

    /// The `width`-bit value with every bit but `bit` set.
    fn all_but(&mut self, bit: usize, width: usize) -> RegisterId {
        let ones = (BigUint::from(1u8) << width) - 1u8;
        self.wide_constant(ones ^ (BigUint::from(1u8) << bit), width)
    }

    /// The `width`-bit value with every bit set.
    pub(super) fn ones(&mut self, width: usize) -> RegisterId {
        self.wide_constant((BigUint::from(1u8) << width) - 1u8, width)
    }

    pub(super) fn binary(
        &mut self,
        width: usize,
        lhs: RegisterId,
        op: BinaryOp,
        rhs: RegisterId,
    ) -> RegisterId {
        let register = self.b.alloc_bit(width, false);
        self.b.emit(SIRInstruction::Binary(register, lhs, op, rhs));
        register
    }

    pub(super) fn publish_service(&mut self, function: Option<u32>, checks: &[WaitCheck<'_>]) {
        if let Some(nbas) = self.nbas {
            nbas.publish(self, checks);
            return;
        }
        let Some(function) = function else {
            return;
        };
        let again = self.b.new_block();
        let done = self.b.new_block();
        let changed = self.b.new_block();
        let failed = self.exit_block(SchedulerExit::Internal);
        self.b.seal_block(SIRTerminator::Jump(again, Vec::new()));
        self.b.switch_to_block(again);
        let context = self.load_at(
            self.slots.defer_limit_settle,
            celox_design::SCHEDULER_HOST_CONTEXT_BIT,
            64,
        );
        let process = self.constant(u64::MAX, 64);
        let request = self.constant(crate::simulator::native_io::NBA_PUBLISH, 64);
        let time = self.load(self.slots.now, 64);
        let first = self.constant(0, 64);
        let result = self.b.alloc_bit(64, false);
        self.b.emit(SIRInstruction::ExternCall {
            dst: Some(result),
            func: function,
            args: vec![context, process, request, time, first],
        });
        let ok = self.equals(result, 0);
        let next = self.b.new_block();
        self.b.seal_block(SIRTerminator::Branch {
            cond: ok,
            true_block: (done, Vec::new()),
            false_block: (next, Vec::new()),
        });
        self.b.switch_to_block(next);
        let update = self.equals(result, 2);
        self.b.seal_block(SIRTerminator::Branch {
            cond: update,
            true_block: (changed, Vec::new()),
            false_block: (failed, Vec::new()),
        });
        self.b.switch_to_block(changed);
        self.mark_dirty();
        self.check_waits_at(checks, false);
        self.b.seal_block(SIRTerminator::Jump(again, Vec::new()));
        self.b.switch_to_block(done);
    }

    pub(super) fn service_call(
        &mut self,
        function: u32,
        process: u64,
        request: u64,
        time: RegisterId,
        failed: celox_sir::BlockId,
    ) {
        let context = self.load_at(
            self.slots.defer_limit_settle,
            celox_design::SCHEDULER_HOST_CONTEXT_BIT,
            64,
        );
        let process = self.constant(process, 64);
        let request = self.constant(request, 64);
        let first = self.constant(0, 64);
        let result = self.b.alloc_bit(64, false);
        self.b.emit(SIRInstruction::ExternCall {
            dst: Some(result),
            func: function,
            args: vec![context, process, request, time, first],
        });
        let ok = self.equals(result, 0);
        let next = self.b.new_block();
        self.b.seal_block(SIRTerminator::Branch {
            cond: ok,
            true_block: (next, Vec::new()),
            false_block: (failed, Vec::new()),
        });
        self.b.switch_to_block(next);
    }

    pub(super) fn equals(&mut self, value: RegisterId, constant: u64) -> RegisterId {
        let width = self.b.register(&value).width();
        let constant = self.constant(constant, width);
        self.binary(1, value, BinaryOp::Eq, constant)
    }

    /// Whether the status in `status` is `expected`.
    pub(super) fn equals_at(
        &mut self,
        status: AbsoluteAddr,
        expected: ProcessStatus,
    ) -> RegisterId {
        let status = self.load(status, celox_design::PROCESS_STATUS_WIDTH);
        self.equals(status, u64::from(expected.code()))
    }

    /// Whether the kernel of `status` suspended for a delay.
    fn delayed_at(&mut self, status: AbsoluteAddr) -> RegisterId {
        let status = self.load(status, celox_design::PROCESS_STATUS_WIDTH);
        let delay = self.equals(status, u64::from(ProcessStatus::Delay.code()));
        let inactive = self.equals(status, u64::from(ProcessStatus::InactiveDelay.code()));
        self.binary(1, delay, BinaryOp::Or, inactive)
    }

    /// The `from`-bit `value` zero-extended to `to` bits.
    pub(super) fn widen_to(&mut self, value: RegisterId, from: usize, to: usize) -> RegisterId {
        let zero = self.constant(0, to - from);
        let wide = self.b.alloc_bit(to, false);
        self.b.emit(SIRInstruction::Concat(wide, vec![zero, value]));
        wide
    }

    /// Whether kernel `index`, with the status slot `status` when it also
    /// waits for edges, waits for its wake time.
    pub(super) fn status_ready(
        &mut self,
        kernels: &[usize],
        ordinals: &[usize],
        schema: &celox_design::RuntimeSchema<AbsoluteAddr>,
        status: ProcessStatus,
    ) -> RegisterId {
        let mut ready = self.constant(0, self.ready_bits);
        for &index in kernels {
            let value = self.load(
                schema.processes[index].status,
                celox_design::PROCESS_STATUS_WIDTH,
            );
            let pending = self.equals(value, u64::from(status.code()));
            let bit = self.bit(pending, ordinals[index]);
            ready = self.binary(self.ready_bits, ready, BinaryOp::Or, bit);
        }
        ready
    }

    pub(super) fn waiting_for_time(
        &mut self,
        index: usize,
        status: Option<AbsoluteAddr>,
    ) -> RegisterId {
        let waiting = self.waiting(index);
        match status {
            None => waiting,
            Some(status) => {
                let delayed = self.delayed_at(status);
                self.binary(1, waiting, BinaryOp::And, delayed)
            }
        }
    }

    /// Whether kernel `index` waits for its wake time, or for its edge.
    pub(super) fn waiting(&mut self, index: usize) -> RegisterId {
        let flag = self.load(self.slots.waiting[index], SCHEDULER_FLAG_WIDTH);
        let zero = self.constant(0, SCHEDULER_FLAG_WIDTH);
        self.binary(1, flag, BinaryOp::Ne, zero)
    }

    /// Record in the timed mask whether the kernel of ready-bit `ordinal`
    /// waits for its wake time.
    pub(super) fn set_timed(&mut self, ordinal: usize, timed: bool) {
        let value = self.constant(u64::from(timed), 1);
        self.store_at(self.slots.timed, ordinal, 1, value);
    }

    /// Fold `group` over the kernels of `kernels` whose bit of the timed
    /// mask `timed` may be set, eight kernels (one byte of the mask) at a
    /// time: a byte with no bit set skips its kernels. `group` gets a
    /// group's kernels with their status slots, the mask restricted to the
    /// group and the accumulator of `width` bits, and returns the new
    /// accumulator. The accumulator is a block parameter of the join after
    /// each group, so callers keep it to one word (the native backend's
    /// branch arguments are single words).
    pub(super) fn scan_timed(
        &mut self,
        timed: RegisterId,
        kernels: &[(usize, Option<AbsoluteAddr>)],
        ordinals: &[usize],
        mut acc: RegisterId,
        width: usize,
        mut group: impl FnMut(
            &mut Self,
            &[(usize, Option<AbsoluteAddr>)],
            RegisterId,
            RegisterId,
        ) -> RegisterId,
    ) -> RegisterId {
        let mut groups: BTreeMap<usize, Vec<(usize, Option<AbsoluteAddr>)>> = BTreeMap::new();
        for &(index, status) in kernels {
            groups
                .entry(ordinals[index] / 8)
                .or_default()
                .push((index, status));
        }
        for kernels in groups.values() {
            let mut mask = BigUint::default();
            for &(index, _) in kernels {
                mask |= BigUint::from(1u8) << ordinals[index];
            }
            let mask = self.wide_constant(mask, self.ready_bits);
            let bits = self.binary(self.ready_bits, timed, BinaryOp::And, mask);
            let zero = self.constant(0, self.ready_bits);
            let some = self.binary(1, bits, BinaryOp::Ne, zero);
            let scan = self.b.new_block();
            let joined = self.b.alloc_bit(width, false);
            let join = self.b.new_block_with(vec![joined]);
            self.b.seal_block(SIRTerminator::Branch {
                cond: some,
                true_block: (scan, Vec::new()),
                false_block: (join, vec![acc]),
            });
            self.b.switch_to_block(scan);
            acc = group(self, kernels, bits, acc);
            self.b.seal_block(SIRTerminator::Jump(join, vec![acc]));
            self.b.switch_to_block(join);
            acc = joined;
        }
        acc
    }

    pub(super) fn load(&mut self, address: AbsoluteAddr, width: usize) -> RegisterId {
        self.load_at(address, 0, width)
    }

    pub(super) fn store(&mut self, address: AbsoluteAddr, width: usize, value: RegisterId) {
        self.store_at(address, 0, width, value);
    }

    pub(super) fn load_at(
        &mut self,
        address: AbsoluteAddr,
        offset: usize,
        width: usize,
    ) -> RegisterId {
        let register = self.b.alloc_bit(width, false);
        self.b.emit(SIRInstruction::Load(
            register,
            stable(address),
            SIROffset::Static(offset),
            width,
        ));
        register
    }

    pub(super) fn store_at(
        &mut self,
        address: AbsoluteAddr,
        offset: usize,
        width: usize,
        value: RegisterId,
    ) {
        if let Some((_, blocks)) = &mut self.partitioned_glue {
            blocks.insert(self.b.current_block());
        }
        self.b.emit(SIRInstruction::Store(
            stable(address),
            SIROffset::Static(offset),
            width,
            value,
            Vec::new(),
            Vec::new(),
        ));
    }

    /// The time of host clock `bit`'s next edge.
    pub(super) fn clock_next(&mut self, bit: usize) -> RegisterId {
        self.load_at(
            self.slots.clock_next,
            bit * SCHEDULER_TIME_WIDTH,
            SCHEDULER_TIME_WIDTH,
        )
    }

    /// Whether bit `bit` of `mask` is set.
    pub(super) fn mask_bit(&mut self, mask: RegisterId, bit: usize) -> RegisterId {
        let width = self.b.register(&mask).width();
        let selector = self.single(bit, width);
        let selected = self.binary(width, mask, BinaryOp::And, selector);
        let zero = self.constant(0, width);
        self.binary(1, selected, BinaryOp::Ne, zero)
    }

    /// The edge level of `event`'s signal, as the runtime reads it: whether
    /// its low byte, within its width, is not zero (see [`Self::level_at`]).
    pub(super) fn level(&mut self, event: &Event) -> RegisterId {
        self.level_at(event.signal, event.width.min(8))
    }

    /// The edge level of the low `width` bits of `address`: one bit, whether
    /// they are not zero, or in a four-state design two, as
    /// `celox_design::edges` encodes it: that bit, and above it whether any
    /// of them is unknown (`edge_level`).
    pub(super) fn level_at(&mut self, address: AbsoluteAddr, width: usize) -> RegisterId {
        if self.four_state.is_none() {
            let value = self.load(address, width);
            let zero = self.constant(0, width);
            return self.binary(1, value, BinaryOp::Ne, zero);
        }
        let (value, unknown) = if self.keeps_unknowns(address) {
            // Both planes, from the 64-bit planes of the bits widened.
            let bits = self.load_state(address, 0, width);
            debug_assert!(width < 64, "an edge level reads the low byte");
            let wide = self.b.alloc_logic(64);
            let zero = self.b.alloc_logic(64 - width);
            self.b.emit(SIRInstruction::Imm(zero, SIRValue::new(0u64)));
            self.b.emit(SIRInstruction::Concat(wide, vec![zero, bits]));
            let planes = self.b.alloc_bit(128, false);
            self.b.emit(SIRInstruction::Unary(
                planes,
                celox_design::UnaryOp::ToPlanes,
                wide,
            ));
            let plane = |g: &mut Self, offset: usize| {
                let bits = g.b.alloc_bit(width, false);
                g.b.emit(SIRInstruction::Slice(bits, planes, offset, width));
                let zero = g.constant(0, width);
                g.binary(1, bits, BinaryOp::Ne, zero)
            };
            (plane(self, 0), plane(self, 64))
        } else {
            let value = self.load(address, width);
            let zero = self.constant(0, width);
            (
                self.binary(1, value, BinaryOp::Ne, zero),
                self.constant(0, 1),
            )
        };
        let level = self.b.alloc_bit(2, false);
        self.b
            .emit(SIRInstruction::Concat(level, vec![unknown, value]));
        level
    }

    /// The width of an edge level (see [`Self::level_at`]).
    pub(super) fn level_width(&self) -> usize {
        if self.four_state.is_some() { 2 } else { 1 }
    }

    /// Whether a change of `event`'s level from `was` to `is` is its active
    /// edge.
    pub(super) fn edge(&mut self, event: &Event, was: RegisterId, is: RegisterId) -> RegisterId {
        self.level_edge(event.active != 0, was, is)
    }

    pub(super) fn settled_edge(
        &mut self,
        event: &Event,
        was: RegisterId,
        is: RegisterId,
    ) -> RegisterId {
        if event.any_settled {
            self.binary(1, was, BinaryOp::Ne, is)
        } else {
            self.edge(event, was, is)
        }
    }

    /// Whether a change of an edge level from `was` to `is` is a rising
    /// (`rising`) or a falling edge, as `celox_design::edges::level_edge`
    /// judges it: in a four-state design, one that leaves the level the edge
    /// starts from or reaches the one it ends at, other than between unknown
    /// levels (IEEE 1800-2023 Table 9-2).
    pub(super) fn level_edge(
        &mut self,
        rising: bool,
        was: RegisterId,
        is: RegisterId,
    ) -> RegisterId {
        let (from, to) = if rising { (0, 1) } else { (1, 0) };
        if self.four_state.is_none() {
            let changed = self.binary(1, was, BinaryOp::Ne, is);
            let active = self.equals(is, to);
            return self.binary(1, changed, BinaryOp::And, active);
        }
        let to = self.constant(to, 2);
        let leaves = self.binary(1, was, BinaryOp::Ne, to);
        let from = self.constant(from, 2);
        let reaches = self.binary(1, is, BinaryOp::Ne, from);
        let edge = self.binary(1, leaves, BinaryOp::And, reaches);
        let both = self.binary(2, was, BinaryOp::And, is);
        let unknown = self.constant(u64::from(LEVEL_UNKNOWN), 2);
        let both = self.binary(2, both, BinaryOp::And, unknown);
        let zero = self.constant(0, 2);
        let known = self.binary(1, both, BinaryOp::Eq, zero);
        self.binary(1, edge, BinaryOp::And, known)
    }

    /// Whether `address` keeps unknown bits: a four-state object of a
    /// four-state design.
    pub(super) fn keeps_unknowns(&self, address: AbsoluteAddr) -> bool {
        self.four_state
            .is_some_and(|objects| objects.get(&address).copied().unwrap_or(false))
    }

    /// Load design state, both planes of an object that keeps unknown bits.
    pub(super) fn load_state(
        &mut self,
        address: AbsoluteAddr,
        offset: usize,
        width: usize,
    ) -> RegisterId {
        if !self.keeps_unknowns(address) {
            return self.load_at(address, offset, width);
        }
        let register = self.b.alloc_logic(width);
        self.b.emit(SIRInstruction::Load(
            register,
            stable(address),
            SIROffset::Static(offset),
            width,
        ));
        register
    }

    /// Store `value` into design state: into an object that keeps unknown
    /// bits as a four-state value, a two-state one with its mask clear;
    /// into one that does not, without its unknown bits.
    pub(super) fn store_state(
        &mut self,
        address: AbsoluteAddr,
        offset: usize,
        width: usize,
        value: RegisterId,
    ) {
        let logic = matches!(
            self.b.register(&value),
            celox_sir::RegisterType::Logic { .. }
        );
        let value = match (self.keeps_unknowns(address), logic) {
            (true, false) => {
                let zero = self.b.alloc_logic(width);
                self.b.emit(SIRInstruction::Imm(zero, SIRValue::new(0u64)));
                let known = self.b.alloc_logic(width);
                self.b
                    .emit(SIRInstruction::Binary(known, value, BinaryOp::Or, zero));
                known
            }
            (false, true) => {
                let known = self.b.alloc_bit(width, false);
                self.b.emit(SIRInstruction::Unary(
                    known,
                    celox_design::UnaryOp::ToTwoState,
                    value,
                ));
                known
            }
            _ => value,
        };
        self.store_at(address, offset, width, value);
    }

    /// The saved level of the event at bit `bit` of `levels`.
    pub(super) fn saved_level(&mut self, levels: Levels, bit: usize) -> RegisterId {
        let value = self.mask_bit(levels.values, bit);
        match levels.unknown {
            None => value,
            Some(unknown) => {
                let unknown = self.mask_bit(unknown, bit);
                let level = self.b.alloc_bit(2, false);
                self.b
                    .emit(SIRInstruction::Concat(level, vec![unknown, value]));
                level
            }
        }
    }

    /// `levels` with `level` at bit `bit`, which was clear.
    pub(super) fn with_level(&mut self, levels: Levels, level: RegisterId, bit: usize) -> Levels {
        let (value, unknown) = self.level_planes(level);
        let set = self.event_bit(value, bit);
        let values = self.binary(self.event_bits, levels.values, BinaryOp::Or, set);
        let unknown = levels.unknown.map(|levels| {
            let set = self.event_bit(unknown.expect("a four-state level"), bit);
            self.binary(self.event_bits, levels, BinaryOp::Or, set)
        });
        Levels { values, unknown }
    }

    /// `levels` with `level` in place of the one at bit `bit`.
    pub(super) fn replace_level(
        &mut self,
        levels: Levels,
        level: RegisterId,
        bit: usize,
    ) -> Levels {
        let keep = self.all_but(bit, self.event_bits);
        let values = self.binary(self.event_bits, levels.values, BinaryOp::And, keep);
        let unknown = levels
            .unknown
            .map(|unknown| self.binary(self.event_bits, unknown, BinaryOp::And, keep));
        self.with_level(Levels { values, unknown }, level, bit)
    }

    /// The value bit of an edge level, and in a four-state design its
    /// unknown bit.
    pub(super) fn level_planes(&mut self, level: RegisterId) -> (RegisterId, Option<RegisterId>) {
        if self.four_state.is_none() {
            return (level, None);
        }
        let plane = |g: &mut Self, offset: usize| {
            let bit = g.b.alloc_bit(1, false);
            g.b.emit(SIRInstruction::Slice(bit, level, offset, 1));
            bit
        };
        (plane(self, 0), Some(plane(self, 1)))
    }

    /// No saved levels: every event at 0.
    pub(super) fn no_levels(&mut self) -> Levels {
        let values = self.constant(0, self.event_bits);
        let unknown = self
            .four_state
            .is_some()
            .then(|| self.constant(0, self.event_bits));
        Levels { values, unknown }
    }

    /// The levels of [`SchedulerSlots::levels`].
    pub(super) fn load_levels(&mut self) -> Levels {
        let values = self.load(self.slots.levels, self.event_bits);
        let unknown = self
            .four_state
            .is_some()
            .then(|| self.load(self.slots.unknown_levels, self.event_bits));
        Levels { values, unknown }
    }

    pub(super) fn store_levels(&mut self, levels: Levels) {
        self.store(self.slots.levels, self.event_bits, levels.values);
        if let Some(unknown) = levels.unknown {
            self.store(self.slots.unknown_levels, self.event_bits, unknown);
        }
    }

    /// Run `body` when bit `index` of `mask` is set.
    pub(super) fn if_bit(&mut self, mask: RegisterId, index: usize, body: impl FnOnce(&mut Self)) {
        let set = self.mask_bit(mask, index);
        let run = self.b.new_block();
        let next = self.b.new_block();
        self.b.seal_block(SIRTerminator::Branch {
            cond: set,
            true_block: (run, Vec::new()),
            false_block: (next, Vec::new()),
        });
        self.b.switch_to_block(run);
        body(self);
        self.b.seal_block(SIRTerminator::Jump(next, Vec::new()));
        self.b.switch_to_block(next);
    }

    /// A block that ends the run with `exit`, emitted by
    /// [`Self::emit_exits`].
    pub(super) fn exit_block(&mut self, exit: SchedulerExit) -> celox_sir::BlockId {
        let block = self.b.new_block();
        self.exits.push((block, exit));
        block
    }

    /// Bit `index` of a kernel or clock mask, set when `condition` holds.
    pub(super) fn bit(&mut self, condition: RegisterId, index: usize) -> RegisterId {
        self.bit_of(condition, index, self.ready_bits)
    }

    /// Bit `index` of an event mask, set when `condition` holds.
    pub(super) fn event_bit(&mut self, condition: RegisterId, index: usize) -> RegisterId {
        self.bit_of(condition, index, self.event_bits)
    }

    pub(super) fn bit_of(
        &mut self,
        condition: RegisterId,
        index: usize,
        width: usize,
    ) -> RegisterId {
        let set = self.single(index, width);
        let unset = self.constant(0, width);
        let bit = self.b.alloc_bit(width, false);
        self.b.emit(SIRInstruction::Mux(bit, condition, set, unset));
        bit
    }

    /// The edge level of the one-bit signal `address` (see
    /// [`Self::level_at`]).
    pub(super) fn signal_level(&mut self, address: AbsoluteAddr) -> RegisterId {
        if self.four_state.is_none() {
            return self.load(address, 1);
        }
        self.level_at(address, 1)
    }

    /// An edge level as the byte of a wait's baseline slot.
    pub(super) fn baseline_byte(&mut self, level: RegisterId) -> RegisterId {
        if self.four_state.is_none() {
            return self.widen(level, SCHEDULER_FLAG_WIDTH);
        }
        let zero = self.constant(0, SCHEDULER_FLAG_WIDTH - 2);
        let byte = self.b.alloc_bit(SCHEDULER_FLAG_WIDTH, false);
        self.b.emit(SIRInstruction::Concat(byte, vec![zero, level]));
        byte
    }

    /// The one-bit `value` as a `width`-bit number.
    pub(super) fn widen(&mut self, value: RegisterId, width: usize) -> RegisterId {
        let set = self.constant(1, width);
        let unset = self.constant(0, width);
        let wide = self.b.alloc_bit(width, false);
        self.b.emit(SIRInstruction::Mux(wide, value, set, unset));
        wide
    }

    /// Publish the staged values of `sites` whose pending flag is set into
    /// their targets, clearing the flags (`publish_nba`).
    pub(super) fn publish(&mut self, sites: &[&Staged]) {
        for site in sites {
            let pending = self.load(site.pending, 1);
            let publish = self.b.new_block();
            let next = self.b.new_block();
            self.b.seal_block(SIRTerminator::Branch {
                cond: pending,
                true_block: (publish, Vec::new()),
                false_block: (next, Vec::new()),
            });
            self.b.switch_to_block(publish);
            let clear = self.constant(0, 1);
            self.store(site.pending, 1, clear);
            let value = self.load_state(site.value, 0, site.width);
            self.store_state(site.target, site.lsb, site.width, value);
            if self.settle_reads(site.target) {
                self.mark_dirty();
            }
            self.b.seal_block(SIRTerminator::Jump(next, Vec::new()));
            self.b.switch_to_block(next);
        }
    }

    /// The unit, once every block is sealed.
    pub(super) fn finish(mut self) -> Unit {
        self.emit_exits();
        let (mut blocks, register_map, _) = self.b.drain();
        // Exits a unit never takes are left out: the `$finish` exit of one
        // without kernels whose domains and combinational logic record no
        // `$finish`, and the exits of the paths it does not generate.
        let mut reached = HashSet::default();
        let mut pending = vec![celox_sir::BlockId(0)];
        while let Some(block) = pending.pop() {
            if !reached.insert(block) {
                continue;
            }
            match &blocks[&block].terminator {
                SIRTerminator::Jump(target, _) => pending.push(*target),
                SIRTerminator::Branch {
                    true_block,
                    false_block,
                    ..
                } => pending.extend([true_block.0, false_block.0]),
                SIRTerminator::Switch { cases, default, .. } => {
                    pending.extend(cases.iter().map(|case| case.target));
                    pending.push(*default);
                }
                SIRTerminator::Return | SIRTerminator::Error(_) => {}
            }
        }
        blocks.retain(|block, _| reached.contains(block));
        let mut unit = Unit {
            entry_block_id: celox_sir::BlockId(0),
            blocks,
            register_map,
        };
        if let Some((program, blocks)) = self.partitioned_glue {
            crate::settle::instrument_generated_glue(program, &mut unit, &blocks);
        }
        unit
    }

    fn emit_exits(&mut self) {
        for (block, exit) in std::mem::take(&mut self.exits) {
            self.b.switch_to_block(block);
            let code = self.constant(exit as u64, SCHEDULER_FLAG_WIDTH);
            self.store(self.slots.exit, SCHEDULER_FLAG_WIDTH, code);
            self.b.seal_block(SIRTerminator::Return);
        }
    }
}
