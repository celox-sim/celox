//! Inlining the units a scheduler runs, or calling them as outlined callees,
//! with the instrumentation of their `$finish` and event records.
use super::*;

impl Generator<'_> {
    /// Inline `unit` as [`Self::inline`] does, and record a change of what
    /// combinational logic reads after it when it may write any.
    pub(super) fn inline_writer(&mut self, unit: &Unit, finish_exit: celox_sir::BlockId) {
        self.inline_writer_with_exit(unit, Some(finish_exit));
    }

    // An execution phase can contain several SIR units. The backend runs
    // all of them before the reference loop observes `$finish`; preserve
    // that boundary, including blocking writes in later units.
    pub(super) fn inline_writers(&mut self, units: &[Unit], finish_exit: celox_sir::BlockId) {
        for unit in units {
            self.inline_writer_with_exit(unit, None);
        }
        if units.iter().any(|unit| self.unit_finishes(unit)) {
            self.check_finish(finish_exit);
        }
    }

    fn inline_writer_with_exit(&mut self, unit: &Unit, finish_exit: Option<celox_sir::BlockId>) {
        if self.partitioned_glue.is_some() {
            // Partition instrumentation already compares every external
            // write to tracked state and publishes that same predicate to
            // slots.dirty. Preserve intermediate changes as well as X/Z-only
            // changes; do not snapshot its notification flags again.
            self.inline_with_exit(unit, finish_exit);
            // The instrumentation marks after a call's writes as well, and
            // after the wide commits it makes itself.
            if instructions(unit).any(makes_host_calls) {
                self.mark_dirty();
            }
            return;
        }
        // Keep small stable write sets precise. Reading a bounded number of
        // words is much cheaper than repeating the whole comb settle when a
        // process merely rewrites the value already present in its outputs.
        let ranges = self.tracked_writer_ranges(unit);
        let before = ranges.as_ref().map(|ranges| {
            ranges
                .iter()
                .map(|&(address, offset, width)| self.load_state(address, offset, width))
                .collect::<Vec<_>>()
        });
        self.inline_with_exit(unit, finish_exit);
        if let (Some(ranges), Some(before)) = (ranges, before) {
            if !ranges.is_empty() {
                let clock_only = self.incremental.is_some_and(|cone| {
                    ranges.iter().all(|&(address, _, _)| {
                        cone.sources
                            .contains(&self.shared.get(&address).copied().unwrap_or(address))
                    })
                });
                let mut dirty = self.load(self.slots.dirty, SCHEDULER_FLAG_WIDTH);
                for ((address, offset, width), before) in ranges.into_iter().zip(before) {
                    let after = self.load_state(address, offset, width);
                    // Case inequality compares both planes, including X/Z-only
                    // changes, and always produces a known boolean.
                    let changed = self.binary(1, before, BinaryOp::NeCase, after);
                    let changed = if clock_only {
                        let two = self.constant(2, SCHEDULER_FLAG_WIDTH);
                        let zero = self.constant(0, SCHEDULER_FLAG_WIDTH);
                        let flag = self.b.alloc_bit(SCHEDULER_FLAG_WIDTH, false);
                        self.b.emit(SIRInstruction::Mux(flag, changed, two, zero));
                        flag
                    } else {
                        changed
                    };
                    dirty = self.binary(SCHEDULER_FLAG_WIDTH, dirty, BinaryOp::Or, changed);
                }
                self.store(self.slots.dirty, SCHEDULER_FLAG_WIDTH, dirty);
            }
        } else if instructions(unit).any(|inst| {
            makes_host_calls(inst)
                || written_storage(inst).any(|target| self.settle_reads(target.absolute_addr()))
        }) {
            self.mark_dirty();
        }
    }

    /// Precise snapshots are bounded independently of design size. Complex
    /// addressing and large FF domains retain the existing conservative flag.
    fn tracked_writer_ranges(&self, unit: &Unit) -> Option<Vec<(AbsoluteAddr, usize, usize)>> {
        const MAX_BITS: usize = 1024;
        const MAX_RANGES: usize = 32;
        let mut ranges = Vec::new();
        let mut bits = 0;
        for instruction in instructions(unit) {
            // An opaque call can change state outside explicit Stores.
            if makes_host_calls(instruction) {
                return None;
            }
            let (target, offset, width) = match instruction.state_writes() {
                StateAccess::Access(access) if self.settle_reads(access.addr.absolute_addr()) => {
                    (access.addr, access.offset, access.width)
                }
                // A call may write any part of these objects, which the
                // bounded snapshots do not cover.
                StateAccess::Objects(objects)
                    if objects
                        .iter()
                        .any(|target| self.settle_reads(target.absolute_addr())) =>
                {
                    return None;
                }
                _ => continue,
            };
            if target.region != STABLE_REGION {
                return None;
            }
            let SIROffset::Static(start) = offset else {
                return None;
            };
            if width > MAX_BITS {
                return None;
            }
            let mut offset = 0;
            while offset < width {
                let width = (width - offset).min(64);
                let range = (target.absolute_addr(), start + offset, width);
                if !ranges.contains(&range) {
                    bits += width;
                    if bits > MAX_BITS || ranges.len() == MAX_RANGES {
                        return None;
                    }
                    ranges.push(range);
                }
                offset += width;
            }
        }
        Some(ranges)
    }

    /// Inline a complete process, checking its finish record after it runs.
    pub(super) fn inline(&mut self, unit: &Unit, finish_exit: celox_sir::BlockId) {
        self.inline_with_exit(unit, Some(finish_exit));
    }

    pub(super) fn unit_finishes(&self, unit: &Unit) -> bool {
        !self.finish_sites.is_empty()
            && (emits_finish(unit, self.finish_sites)
                || self.observers.is_some()
                    && instructions(unit).any(|inst| matches!(inst,
                        SIRInstruction::CombCaptureEvent { site_id, .. } if self.finish_sites.contains(site_id))))
    }

    pub(super) fn inline_with_exit(
        &mut self,
        unit: &Unit,
        finish_exit: Option<celox_sir::BlockId>,
    ) {
        // Compiled code commits wide runs by their raw words alone.
        let unit = &*crate::settle::without_shadow_commits(unit);
        let captures = self.observers.is_some();
        let finishes = self.unit_finishes(unit);
        let counts = self.event_batch.is_some()
            && instructions(unit).any(|inst| {
                matches!(inst, SIRInstruction::RuntimeEvent { .. })
                    || captures && matches!(inst, SIRInstruction::CombCaptureEvent { .. })
            });
        if !finishes && !counts {
            self.inline_body(unit);
            return;
        }
        let instrumented = self.instrument(unit, counts);
        self.inline_body(&instrumented);
        if finishes && let Some(finish_exit) = finish_exit {
            self.check_finish(finish_exit);
        }
    }

    pub(super) fn check_finish(&mut self, finish_exit: celox_sir::BlockId) {
        let finished = self.load(self.slots.finish, SCHEDULER_FLAG_WIDTH);
        let zero = self.constant(0, SCHEDULER_FLAG_WIDTH);
        let set = self.binary(1, finished, BinaryOp::Ne, zero);
        let next = self.b.new_block();
        self.b.seal_block(SIRTerminator::Branch {
            cond: set,
            true_block: (finish_exit, Vec::new()),
            false_block: (next, Vec::new()),
        });
        self.b.switch_to_block(next);
    }

    fn inline_body(&mut self, unit: &Unit) {
        if let Some(outlines) = self.outlined.as_deref_mut()
            && (outlines.cancelled
                || outlines
                    .cancel
                    .as_ref()
                    .is_some_and(|cancel| cancel.is_cancelled()))
        {
            outlines.cancelled = true;
            return;
        }
        let size = instructions(unit).take(OUTLINE_INSTRUCTIONS + 1).count();
        let inlined = self.inlined_instructions;
        let outline = match self.outlined.as_deref_mut() {
            Some(outlines) => {
                // Native allocation grows faster than the function: past a
                // budget, further kernels compile as callees.
                let budgeted = size >= OUTLINE_MIN_INSTRUCTIONS
                    && outlines
                        .inline_budget
                        .is_some_and(|budget| inlined + size > budget);
                outlines.budgeted |= budgeted && size <= OUTLINE_INSTRUCTIONS;
                (size > OUTLINE_INSTRUCTIONS || budgeted).then_some(outlines)
            }
            None => None,
        };
        if let Some(outlines) = outline {
            let outlined = &mut outlines.callees;
            let index = match outlined.iter().position(|previous| previous == unit) {
                Some(index) => index,
                None => {
                    outlined.push(unit.clone());
                    outlined.len() - 1
                }
            };
            self.b.emit(SIRInstruction::ExternCall {
                dst: None,
                func: celox_sir::extern_abi::NATIVE_KERNEL_CALL_TAG
                    | (self.outline_base + index as u32),
                args: Vec::new(),
            });
        } else {
            self.inlined_instructions += size;
            self.b.inline_execution_unit(unit);
        }
    }

    /// A copy of `unit` that sets the finish slot after every `$finish`
    /// record, and when `counts`, counts every record in the events slot.
    fn instrument(&self, unit: &Unit, counts: bool) -> Unit {
        Self::instrument_records(
            unit,
            self.finish_sites,
            self.slots,
            counts,
            self.observers.is_some(),
        )
    }

    /// [`Self::instrument`] of `unit`, for a scheduler of `slots` whose
    /// `$finish` records are `finish_sites`.
    pub(super) fn instrument_records(
        unit: &Unit,
        finish_sites: &HashSet<u32>,
        slots: &SchedulerSlots<AbsoluteAddr>,
        counts: bool,
        captures: bool,
    ) -> Unit {
        let mut unit = unit.clone();
        let mut next = unit
            .register_map
            .keys()
            .map(|register| register.0 + 1)
            .max()
            .unwrap_or(0);
        let mut registers = Vec::new();
        let mut wide = Vec::new();
        for block in unit.blocks.values_mut() {
            let mut instructions = Vec::with_capacity(block.instructions.len());
            for inst in block.instructions.drain(..) {
                let capture = match &inst {
                    SIRInstruction::CombCaptureEvent { site_id, .. } if captures => Some(*site_id),
                    _ => None,
                };
                let enabled = capture.map(|site| {
                    use celox_sir::native_memory::NativeMemoryOp as Memory;
                    let [header, pointer, offset, word, shift, shifted, mask, enabled] =
                        std::array::from_fn(|i| RegisterId(next + i));
                    next += 8;
                    wide.extend([header, pointer, offset, word, shift, shifted, mask, enabled]);
                    instructions.extend([
                        SIRInstruction::Imm(
                            header,
                            SIRValue::new(
                                celox_state_layout::STATE_HEADER_COMB_CAPTURE_ENABLED_ADDR_OFFSET
                                    as u64,
                            ),
                        ),
                        SIRInstruction::ExternCall {
                            dst: Some(pointer),
                            func: Memory::ReadState.function(),
                            args: vec![header],
                        },
                        SIRInstruction::Imm(offset, SIRValue::new(u64::from(site / 8) * 8)),
                        SIRInstruction::ExternCall {
                            dst: Some(word),
                            func: Memory::ReadIndirect.function(),
                            args: vec![pointer, offset],
                        },
                        SIRInstruction::Imm(shift, SIRValue::new(u64::from(site % 8) * 8)),
                        SIRInstruction::Binary(shifted, word, BinaryOp::Shr, shift),
                        SIRInstruction::Imm(mask, SIRValue::new(255u64)),
                        SIRInstruction::Binary(enabled, shifted, BinaryOp::And, mask),
                    ]);
                    enabled
                });
                let finish = matches!(&inst, SIRInstruction::RuntimeEvent { site_id, .. }
                    if finish_sites.contains(site_id))
                    || capture.is_some_and(|site| finish_sites.contains(&site));
                let record =
                    matches!(&inst, SIRInstruction::RuntimeEvent { .. }) || capture.is_some();
                instructions.push(inst);
                if counts && record {
                    let [count, one, sum] = std::array::from_fn(|index| RegisterId(next + index));
                    next += 3;
                    wide.extend([count, one, sum]);
                    let events = stable(slots.events);
                    instructions.extend([
                        SIRInstruction::Load(
                            count,
                            events,
                            SIROffset::Static(0),
                            SCHEDULER_TIME_WIDTH,
                        ),
                        SIRInstruction::Imm(one, SIRValue::new(1u64)),
                        SIRInstruction::Binary(sum, count, BinaryOp::Add, enabled.unwrap_or(one)),
                        SIRInstruction::Store(
                            events,
                            SIROffset::Static(0),
                            SCHEDULER_TIME_WIDTH,
                            sum,
                            Vec::new(),
                            Vec::new(),
                        ),
                    ]);
                }
                if finish {
                    let [one, previous, result] = std::array::from_fn(|i| RegisterId(next + i));
                    next += 3;
                    registers.extend([one, previous, result]);
                    instructions.push(match enabled {
                        Some(enabled) => {
                            SIRInstruction::Slice(one, enabled, 0, SCHEDULER_FLAG_WIDTH)
                        }
                        None => SIRInstruction::Imm(one, SIRValue::new(1u8)),
                    });
                    instructions.extend([
                        SIRInstruction::Load(
                            previous,
                            stable(slots.finish),
                            SIROffset::Static(0),
                            SCHEDULER_FLAG_WIDTH,
                        ),
                        SIRInstruction::Binary(result, previous, BinaryOp::Or, one),
                        SIRInstruction::Store(
                            stable(slots.finish),
                            SIROffset::Static(0),
                            SCHEDULER_FLAG_WIDTH,
                            result,
                            Vec::new(),
                            Vec::new(),
                        ),
                    ]);
                }
            }
            block.instructions = instructions;
        }
        for register in registers {
            unit.register_map.insert(
                register,
                crate::ir::RegisterType::Bit {
                    width: SCHEDULER_FLAG_WIDTH,
                    signed: false,
                },
            );
        }
        for register in wide {
            unit.register_map.insert(
                register,
                crate::ir::RegisterType::Bit {
                    width: SCHEDULER_TIME_WIDTH,
                    signed: false,
                },
            );
        }
        unit
    }
}
