//! Publish changes from independent, adjacent registers using their laid-out words.
use super::*;
use crate::settle::CommitRange;
use celox_sir::native_memory::NativeMemoryOp;

struct Copy<'a> {
    instruction: &'a SIRInstruction<RegionedAbsoluteAddr>,
    range: CommitRange,
    readers: u64,
}

// One (reader mask, significant bits) pair per byte of a state word.
type Lanes = [(u64, u8); 8];

pub(super) fn dense_commits(layout: &crate::backend::MemoryLayout, unit: &Unit) -> bool {
    let block = &unit.blocks[&unit.entry_block_id];
    unit.blocks.len() == 1
        && block.params.is_empty()
        && matches!(block.terminator, SIRTerminator::Return)
        && !block.instructions.is_empty()
        && block
            .instructions
            .iter()
            .all(|i| CommitRange::of(layout, i).is_some_and(|range| range.width <= 64))
}

fn constant(b: &mut Builder, value: u64) -> RegisterId {
    let reg = b.alloc_bit(64, false);
    b.emit(SIRInstruction::Imm(reg, SIRValue::new(value)));
    reg
}

fn binary(b: &mut Builder, left: RegisterId, op: BinaryOp, right: RegisterId) -> RegisterId {
    let dst = b.alloc_bit(64, false);
    b.emit(SIRInstruction::Binary(dst, left, op, right));
    dst
}

fn read(b: &mut Builder, offset: RegisterId) -> RegisterId {
    let dst = b.alloc_bit(64, false);
    b.emit(SIRInstruction::ExternCall {
        dst: Some(dst),
        func: NativeMemoryOp::ReadState.function(),
        args: vec![offset],
    });
    dst
}

fn accumulate<K: Ord>(
    b: &mut Builder,
    map: &mut BTreeMap<K, RegisterId>,
    key: K,
    value: RegisterId,
) {
    map.entry(key)
        .and_modify(|old| *old = binary(b, *old, BinaryOp::Or, value))
        .or_insert(value);
}

fn copy_words(
    b: &mut Builder,
    patterns: &mut BTreeMap<Lanes, RegisterId>,
    run: &[Copy<'_>],
    len: usize,
) {
    let mut bytes = run.iter().flat_map(|c| {
        (0..c.range.bytes()).map(move |byte| {
            let significant = (c.range.width - byte * 8).min(8);
            (c.readers, u8::MAX >> (8 - significant))
        })
    });
    for start in (0..len).step_by(8) {
        // Overlap the preceding word for a short tail. Only newly covered
        // bytes contribute to the change mask, and both accesses stay in bounds.
        let offset = start.min(len - 8);
        let mut lanes = [(0, 0); 8];
        for lane in &mut lanes[start - offset..] {
            *lane = bytes.next().expect("one lane for each byte in the run");
        }
        let src = constant(b, (run[0].range.working + offset) as u64);
        let dst = constant(b, (run[0].range.stable + offset) as u64);
        let value = read(b, src);
        if lanes.iter().any(|(readers, _)| *readers != 0) {
            let old = read(b, dst);
            let diff = binary(b, old, BinaryOp::Xor, value);
            accumulate(b, patterns, lanes, diff);
        }
        b.emit(SIRInstruction::ExternCall {
            dst: None,
            func: NativeMemoryOp::WriteState.function(),
            args: vec![dst, value],
        });
    }
}

fn publish(
    b: &mut Builder,
    patterns: BTreeMap<Lanes, RegisterId>,
    slot: RegionedAbsoluteAddr,
    offset: usize,
) {
    if patterns.is_empty() {
        return;
    }
    // Words with identical lane ownership already share their XOR reduction.
    // Combine all the remaining differing bits of each reader before testing it.
    let mut changes = BTreeMap::new();
    for (lanes, diff) in patterns {
        let mut groups = BTreeMap::new();
        for (i, (readers, bits)) in lanes.into_iter().enumerate().filter(|(_, (r, _))| *r != 0) {
            *groups.entry(readers).or_default() |= u64::from(bits) << (8 * i);
        }
        for (readers, mask) in groups {
            let part = if mask == u64::MAX {
                diff
            } else {
                let mask = constant(b, mask);
                binary(b, diff, BinaryOp::And, mask)
            };
            accumulate(b, &mut changes, readers, part);
        }
    }
    let offset = SIROffset::Static(offset);
    let mut pending = b.alloc_bit(64, false);
    b.emit(SIRInstruction::Load(pending, slot, offset.clone(), 64));
    let zero = constant(b, 0);
    for (readers, diff) in changes {
        let changed = b.alloc_bit(1, false);
        b.emit(SIRInstruction::Binary(changed, diff, BinaryOp::Ne, zero));
        let readers = constant(b, readers);
        let bits = b.alloc_bit(64, false);
        b.emit(SIRInstruction::Mux(bits, changed, readers, zero));
        pending = binary(b, pending, BinaryOp::Or, bits);
    }
    b.emit(SIRInstruction::Store(
        slot,
        offset,
        64,
        pending,
        vec![],
        vec![],
    ));
}

impl Generator<'_> {
    /// A batch of triggerless working-to-stable copies. Stable destinations
    /// must be disjoint, so independent copies can be visited in address order.
    pub(super) fn packed_publications(&self, units: &[Unit]) -> Option<Unit> {
        let layout = self.program.layout();
        let schedule = self.incremental?.events.as_ref()?;
        let mut copies = Vec::new();
        for instruction in units.iter().flat_map(instructions) {
            let range = CommitRange::of(layout, instruction)?;
            let addr = range.destination.absolute_addr();
            let readers = if self.settle_reads(addr) {
                self.publication(addr)?.1
            } else {
                0
            };
            copies.push(Copy {
                instruction,
                range,
                readers,
            });
        }
        if copies.len() < 8 || copies.iter().all(|c| c.readers == 0) {
            return None;
        }
        copies.sort_unstable_by_key(|c| c.range.stable);
        if copies
            .windows(2)
            .any(|p| p[0].range.stable + p[0].range.bytes() > p[1].range.stable)
        {
            return None;
        }

        let mut scalar = Builder::new();
        let mut words = Builder::new();
        let mut patterns = BTreeMap::new();
        for run in copies.chunk_by(|a, b| {
            a.range.stable + a.range.bytes() == b.range.stable
                && a.range.working + a.range.bytes() == b.range.working
        }) {
            let last = &run.last()?.range;
            let len = last.stable + last.bytes() - run[0].range.stable;
            if len >= 8 && len.div_ceil(8) < run.len() {
                copy_words(&mut words, &mut patterns, run, len);
            } else {
                for copy in run {
                    scalar.emit(copy.instruction.clone());
                }
            }
        }
        publish(
            &mut words,
            patterns,
            stable(self.slots.settle_events),
            schedule.publication_offset,
        );
        let words = words.flush_eu()?;
        if let Some(rest) = scalar.flush_eu() {
            let mut body = Builder::new();
            body.inline_execution_unit(&self.clock_publications(&rest)?);
            body.inline_execution_unit(&words);
            body.flush_eu()
        } else {
            Some(words)
        }
    }
}
