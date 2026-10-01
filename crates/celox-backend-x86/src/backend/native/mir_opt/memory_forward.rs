//! Local memory forwarding, partial-store promotion, and dead stores.

use super::*;

type LocalMemoryByte = (BaseReg, i64);

#[derive(Clone, Copy, PartialEq, Eq)]
enum LocalMemoryEvent {
    Load(usize),
    Store(usize),
}

#[derive(Clone)]
struct PartialStoreEvent {
    slot: MemorySlot,
    source: VReg,
    insert: Option<StateInsertDesc>,
    prior_events: [Option<LocalMemoryEvent>; 8],
    prior_writes: [Option<usize>; 8],
}

struct PartialRoundTripPlan {
    load_instruction: usize,
    insertion_instruction: usize,
    load_slot: MemorySlot,
    destination: VReg,
    stores: Vec<usize>,
}

fn slot_bytes(slot: MemorySlot) -> impl Iterator<Item = LocalMemoryByte> {
    let start = i64::from(slot.offset);
    (0..slot.size.bytes() as usize).map(move |byte| (slot.base, start + byte as i64))
}

fn direct_memory_barrier(inst: &MInst) -> bool {
    let reads = memory_effect::reads(inst);
    let writes = memory_effect::writes(inst);
    [reads, writes].into_iter().any(|effects| {
        matches!(
            effects.unknown_memory(),
            Some(memory_effect::UnknownMemory::Direct(_))
        ) || effects.ranges().next().is_some()
    })
}

fn contained_slot(inner: MemorySlot, outer: MemorySlot) -> bool {
    if inner.base != outer.base {
        return false;
    }
    let inner_start = i64::from(inner.offset);
    let outer_start = i64::from(outer.offset);
    let inner_end = inner_start + i64::from(inner.size.bytes());
    let outer_end = outer_start + i64::from(outer.size.bytes());
    outer_start <= inner_start && inner_end <= outer_end
}

fn recover_partial_store_insert(
    definitions: &HashMap<VReg, &MInst>,
    slot: MemorySlot,
    source: VReg,
    possible_ones: &[u64],
    constants: &HashMap<VReg, u64>,
) -> Option<StateInsertDesc> {
    let MInst::Or { lhs, rhs, .. } = definitions.get(&source)? else {
        return None;
    };
    let storage_mask = machine_width_mask(slot.size);

    let cleared = |value: VReg| -> Option<(VReg, u64)> {
        match definitions.get(&value)? {
            MInst::AndImm { src, imm, .. } => Some((*src, *imm & storage_mask)),
            MInst::AndImm32 { src, imm, .. } => Some((*src, u64::from(*imm) & storage_mask)),
            MInst::And { lhs, rhs, .. } | MInst::And32 { lhs, rhs, .. } => constants
                .get(rhs)
                .map(|mask| (*lhs, *mask & storage_mask))
                .or_else(|| constants.get(lhs).map(|mask| (*rhs, *mask & storage_mask))),
            _ => None,
        }
    };
    let ((old, clear_mask), inserted) = cleared(*lhs)
        .map(|clear| (clear, *rhs))
        .or_else(|| cleared(*rhs).map(|clear| (clear, *lhs)))?;
    if !matches!(
        definitions.get(&old),
        Some(MInst::Load {
            base,
            offset,
            size,
            ..
        }) if *base == slot.base && *offset == slot.offset && *size == slot.size
    ) {
        return None;
    }

    let changed_mask = storage_mask & !clear_mask;
    let bit_offset = changed_mask.trailing_zeros() as usize;
    let width_bits = changed_mask.count_ones() as usize;
    let field_mask = if width_bits == 64 {
        u64::MAX
    } else {
        (1_u64 << width_bits) - 1
    };
    if width_bits == 0
        || changed_mask != field_mask.checked_shl(bit_offset as u32).unwrap_or(0)
        || possible_bits(inserted, possible_ones, constants) & !changed_mask != 0
    {
        return None;
    }

    let (value, value_bit_offset) = match definitions.get(&inserted) {
        Some(MInst::ShlImm { src, imm, .. })
            if usize::from(*imm) == bit_offset
                && possible_bits(*src, possible_ones, constants) & !field_mask == 0 =>
        {
            (*src, 0)
        }
        _ => (inserted, bit_offset),
    };
    Some(StateInsertDesc {
        value,
        value_bit_offset,
        bit_offset,
        width_bits,
        complete_value: false,
    })
}

fn discover_partial_round_trips(
    block: &MBlock,
    spill_descs: &[SpillDesc],
    defined_values: &HashSet<VReg>,
    possible_ones: &[u64],
    constants: &HashMap<VReg, u64>,
) -> (Vec<PartialRoundTripPlan>, Vec<Option<PartialStoreEvent>>) {
    let mut last_events = HashMap::<LocalMemoryByte, LocalMemoryEvent>::default();
    let mut last_writes = HashMap::<LocalMemoryByte, usize>::default();
    let mut stores = vec![None; block.insts.len()];
    let mut plans = Vec::new();
    let mut region_start = 0usize;
    let mut definitions = HashMap::<VReg, &MInst>::default();

    for (instruction, inst) in block.insts.iter().enumerate() {
        match *inst {
            MInst::Store {
                base,
                offset,
                src,
                size,
            } => {
                let slot = MemorySlot { base, offset, size };
                let mut prior_events = [None; 8];
                let mut prior_writes = [None; 8];
                for (byte, key) in slot_bytes(slot).enumerate() {
                    prior_events[byte] = last_events.get(&key).copied();
                    prior_writes[byte] = last_writes.get(&key).copied();
                    last_events.insert(key, LocalMemoryEvent::Store(instruction));
                    last_writes.insert(key, instruction);
                }
                let insert = spill_descs
                    .get(src.0 as usize)
                    .and_then(|descriptor| descriptor.state_insert)
                    .filter(|insert| {
                        defined_values.contains(&insert.value)
                            && insert.width_bits != 0
                            && insert
                                .bit_offset
                                .checked_add(insert.width_bits)
                                .is_some_and(|end| end <= size.bytes() as usize * 8)
                            && insert
                                .value_bit_offset
                                .checked_add(insert.width_bits)
                                .is_some_and(|end| end <= 64)
                    })
                    .or_else(|| {
                        recover_partial_store_insert(
                            &definitions,
                            slot,
                            src,
                            possible_ones,
                            constants,
                        )
                        .filter(|insert| defined_values.contains(&insert.value))
                    });
                stores[instruction] = Some(PartialStoreEvent {
                    slot,
                    source: src,
                    insert,
                    prior_events,
                    prior_writes,
                });
            }
            MInst::Load {
                dst,
                base,
                offset,
                size,
            } => {
                let load_slot = MemorySlot { base, offset, size };
                let mut candidates = slot_bytes(load_slot)
                    .filter_map(|byte| match last_events.get(&byte) {
                        Some(LocalMemoryEvent::Store(store)) => Some(*store),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                candidates.sort_unstable();
                candidates.dedup();

                let valid = !candidates.is_empty()
                    && candidates.iter().all(|&store| {
                        let Some(event) = stores[store].as_ref() else {
                            return false;
                        };
                        event.slot.size.bytes() < size.bytes()
                            && contained_slot(event.slot, load_slot)
                            && slot_bytes(event.slot).all(|byte| {
                                last_events.get(&byte) == Some(&LocalMemoryEvent::Store(store))
                            })
                    });
                if valid {
                    let mut barrier = region_start;
                    for byte in slot_bytes(load_slot) {
                        let mut write = last_writes.get(&byte).copied();
                        while let Some(candidate) = write.filter(|write| candidates.contains(write))
                        {
                            let event = stores[candidate]
                                .as_ref()
                                .expect("candidate store has an event");
                            let byte_index = usize::try_from(byte.1 - i64::from(event.slot.offset))
                                .expect("candidate contains queried byte");
                            write = event.prior_writes[byte_index];
                        }
                        if let Some(write) = write {
                            barrier = barrier.max(write.saturating_add(1));
                        }
                    }

                    let first_store = candidates[0];
                    let mut insertion = first_store;
                    for &store in &candidates {
                        let event = stores[store]
                            .as_ref()
                            .expect("candidate store has an event");
                        for prior in event.prior_events.into_iter().flatten() {
                            if let LocalMemoryEvent::Load(load) = prior
                                && load >= barrier
                            {
                                insertion = insertion.min(load);
                            }
                        }
                    }
                    insertion = insertion.max(barrier);
                    if insertion <= first_store {
                        plans.push(PartialRoundTripPlan {
                            load_instruction: instruction,
                            insertion_instruction: insertion,
                            load_slot,
                            destination: dst,
                            stores: candidates,
                        });
                    }
                }

                for byte in slot_bytes(load_slot) {
                    last_events.insert(byte, LocalMemoryEvent::Load(instruction));
                }
            }
            _ if direct_memory_barrier(inst) => {
                last_events.clear();
                last_writes.clear();
                region_start = instruction.saturating_add(1);
            }
            _ => {}
        }
        if let Some(destination) = inst.def() {
            definitions.insert(destination, inst);
        }
    }

    (plans, stores)
}

fn retain_dead_partial_store_plans(
    block: &MBlock,
    plans: Vec<PartialRoundTripPlan>,
) -> Vec<PartialRoundTripPlan> {
    let mut plans_by_load = plans
        .iter()
        .enumerate()
        .map(|(plan, candidate)| (candidate.load_instruction, plan))
        .collect::<HashMap<_, _>>();
    let mut accepted = vec![false; plans.len()];
    let mut removed_stores = HashSet::<usize>::default();
    let mut next_events = HashMap::<LocalMemoryByte, LocalMemoryEvent>::default();

    for (instruction, inst) in block.insts.iter().enumerate().rev() {
        if let Some(plan) = plans_by_load.remove(&instruction) {
            let candidate = &plans[plan];
            let all_overwritten = candidate.stores.iter().all(|&store| {
                let MInst::Store {
                    base, offset, size, ..
                } = block.insts[store]
                else {
                    return false;
                };
                slot_bytes(MemorySlot { base, offset, size }).all(|byte| {
                    matches!(
                        next_events.get(&byte),
                        Some(LocalMemoryEvent::Store(next)) if !removed_stores.contains(next)
                    )
                })
            });
            if all_overwritten {
                accepted[plan] = true;
                removed_stores.extend(candidate.stores.iter().copied());
            }
        }

        match *inst {
            MInst::Store {
                base, offset, size, ..
            } if !removed_stores.contains(&instruction) => {
                for byte in slot_bytes(MemorySlot { base, offset, size }) {
                    next_events.insert(byte, LocalMemoryEvent::Store(instruction));
                }
            }
            MInst::Store { .. } => {}
            MInst::Load {
                base, offset, size, ..
            } => {
                for byte in slot_bytes(MemorySlot { base, offset, size }) {
                    next_events.insert(byte, LocalMemoryEvent::Load(instruction));
                }
            }
            _ if direct_memory_barrier(inst) => next_events.clear(),
            _ => {}
        }
    }

    plans
        .into_iter()
        .zip(accepted)
        .filter_map(|(plan, accepted)| accepted.then_some(plan))
        .collect()
}

fn emit_partial_store_overlay(
    instructions: &mut Vec<MInst>,
    vregs: &mut VRegAllocator,
    spill_descs: &mut Vec<SpillDesc>,
    mut current: VReg,
    destination: VReg,
    load_slot: MemorySlot,
    stores: &[PartialStoreEvent],
) {
    let load_mask = match load_slot.size {
        OpSize::S8 => u8::MAX as u64,
        OpSize::S16 => u16::MAX as u64,
        OpSize::S32 => u32::MAX as u64,
        OpSize::S64 => u64::MAX,
    };

    for (index, store) in stores.iter().enumerate() {
        let (source, source_bit_offset, width_bits, store_bit_offset) =
            if let Some(insert) = store.insert {
                (
                    insert.value,
                    insert.value_bit_offset,
                    insert.width_bits,
                    insert.bit_offset,
                )
            } else {
                (store.source, 0, store.slot.size.bytes() as usize * 8, 0)
            };
        let stored_mask = match width_bits {
            64 => u64::MAX,
            width => (1_u64 << width) - 1,
        };
        let shift =
            u8::try_from((store.slot.offset - load_slot.offset) * 8 + store_bit_offset as i32)
                .expect("contained scalar store shift fits one word");
        let source = if source_bit_offset == 0 {
            source
        } else {
            let shifted = alloc_transient_vreg(vregs, spill_descs);
            instructions.push(MInst::ShrImm {
                dst: shifted,
                src: source,
                imm: source_bit_offset as u8,
            });
            shifted
        };
        let normalized = if width_bits == 64 {
            source
        } else {
            let normalized = alloc_transient_vreg(vregs, spill_descs);
            if width_bits <= 32 {
                instructions.push(MInst::AndImm32 {
                    dst: normalized,
                    src: source,
                    imm: stored_mask as u32,
                });
            } else if and_imm_ok(stored_mask) {
                instructions.push(MInst::AndImm {
                    dst: normalized,
                    src: source,
                    imm: stored_mask,
                });
            } else {
                let mask = alloc_transient_vreg(vregs, spill_descs);
                instructions.push(MInst::LoadImm {
                    dst: mask,
                    value: stored_mask,
                });
                instructions.push(MInst::And {
                    dst: normalized,
                    lhs: source,
                    rhs: mask,
                });
            }
            normalized
        };

        let shifted = if shift == 0 {
            normalized
        } else {
            let shifted = alloc_transient_vreg(vregs, spill_descs);
            instructions.push(MInst::ShlImm {
                dst: shifted,
                src: normalized,
                imm: shift,
            });
            shifted
        };

        let clear_mask = load_mask & !(stored_mask << shift);
        let cleared = alloc_transient_vreg(vregs, spill_descs);
        if load_slot.size != OpSize::S64 {
            instructions.push(MInst::AndImm32 {
                dst: cleared,
                src: current,
                imm: clear_mask as u32,
            });
        } else if and_imm_ok(clear_mask) {
            instructions.push(MInst::AndImm {
                dst: cleared,
                src: current,
                imm: clear_mask,
            });
        } else {
            let mask = alloc_transient_vreg(vregs, spill_descs);
            instructions.push(MInst::LoadImm {
                dst: mask,
                value: clear_mask,
            });
            instructions.push(MInst::And {
                dst: cleared,
                lhs: current,
                rhs: mask,
            });
        }

        let merged = if index + 1 == stores.len() {
            destination
        } else {
            alloc_transient_vreg(vregs, spill_descs)
        };
        instructions.push(MInst::Or {
            dst: merged,
            lhs: cleared,
            rhs: shifted,
        });
        current = merged;
    }
}

/// Promote a local sequence of partial state writes through its sole wide
/// observation.
///
/// For every physical byte, the forward scan proves that the wide load is the
/// first observer of each selected store. The reverse scan proves that every
/// selected byte is overwritten before another observer. The load can
/// therefore move to the preceding memory version, while the removed stores
/// are represented as ordinary SSA inserts at the original load point.
///
/// Direct scalar accesses touch at most eight byte facts. Both scans and the
/// rewrite are linear in block instructions and use storage proportional to
/// the direct bytes referenced by one block. Insert recovery additionally
/// reuses one sparse whole-function possible-bit solve.
pub(super) fn promote_partial_store_round_trips(func: &mut MFunction) {
    promote_partial_store_round_trips_impl(func, false);
}

pub(super) fn forward_live_partial_stores(func: &mut MFunction) {
    promote_partial_store_round_trips_impl(func, true);
}

fn discover_live_partial_stores(
    block: &MBlock,
) -> (Vec<PartialRoundTripPlan>, Vec<Option<PartialStoreEvent>>) {
    let mut plans = Vec::new();
    let mut stores = vec![None; block.insts.len()];
    for (instruction, inst) in block.insts.iter().enumerate() {
        let MInst::Load {
            dst,
            base: BaseReg::SimState,
            offset,
            size,
        } = *inst
        else {
            continue;
        };
        if size == OpSize::S8 {
            continue;
        }
        let load_slot = MemorySlot {
            base: BaseReg::SimState,
            offset,
            size,
        };
        let start = i64::from(offset);
        let end = start + i64::from(size.bytes());
        let mut selected = Vec::new();
        let mut covered = 0u16;
        for previous in (instruction.saturating_sub(32)..instruction).rev() {
            let write = &block.insts[previous];
            let effects = memory_effect::writes(write);
            if effects.unknown_memory()
                == Some(memory_effect::UnknownMemory::Direct(BaseReg::SimState))
            {
                break;
            }
            if !effects.ranges().any(|range| {
                range.base == BaseReg::SimState
                    && range.offset < end
                    && range.end().is_none_or(|limit| start < limit)
            }) {
                continue;
            }
            let MInst::Store {
                base,
                offset,
                src,
                size,
            } = *write
            else {
                break;
            };
            let slot = MemorySlot { base, offset, size };
            if size.bytes() >= load_slot.size.bytes() || !contained_slot(slot, load_slot) {
                break;
            }
            let mask = ((1u16 << size.bytes()) - 1) << (offset - load_slot.offset);
            if mask & covered != 0 {
                break;
            }
            covered |= mask;
            selected.push(previous);
            stores[previous] = Some(PartialStoreEvent {
                slot,
                source: src,
                insert: None,
                prior_events: [None; 8],
                prior_writes: [None; 8],
            });
            if selected.len() == 2 {
                break;
            }
        }
        if selected.is_empty() {
            continue;
        }
        selected.reverse();
        plans.push(PartialRoundTripPlan {
            load_instruction: instruction,
            insertion_instruction: selected[0],
            load_slot,
            destination: dst,
            stores: selected,
        });
    }
    (plans, stores)
}

fn promote_partial_store_round_trips_impl(func: &mut MFunction, retain_stores: bool) {
    let constants = func
        .blocks
        .iter()
        .flat_map(|block| &block.insts)
        .filter_map(|inst| match inst {
            MInst::LoadImm { dst, value } => Some((*dst, *value)),
            _ => None,
        })
        .collect::<HashMap<_, _>>();
    let possible_ones = global_possible_one_bits(func, &constants);
    let defined_values = func
        .blocks
        .iter()
        .flat_map(|block| {
            block
                .phis
                .iter()
                .map(|phi| phi.dst)
                .chain(block.insts.iter().filter_map(MInst::def))
        })
        .collect::<HashSet<_>>();
    let (vregs, spill_descs, blocks) = (&mut func.vregs, &mut func.spill_descs, &mut func.blocks);
    for block in blocks {
        let (plans, store_events) = if retain_stores {
            discover_live_partial_stores(block)
        } else {
            discover_partial_round_trips(
                block,
                spill_descs,
                &defined_values,
                &possible_ones,
                &constants,
            )
        };
        let plans = if retain_stores {
            plans
        } else {
            retain_dead_partial_store_plans(block, plans)
        };
        if plans.is_empty() {
            continue;
        }

        let mut insertions = HashMap::<usize, Vec<MInst>>::default();
        let mut replacements = HashMap::<usize, Vec<MInst>>::default();
        let mut removals = HashSet::<usize>::default();
        for plan in plans {
            let old = alloc_transient_vreg(vregs, spill_descs);
            insertions
                .entry(plan.insertion_instruction)
                .or_default()
                .push(MInst::Load {
                    dst: old,
                    base: plan.load_slot.base,
                    offset: plan.load_slot.offset,
                    size: plan.load_slot.size,
                });
            let stores = plan
                .stores
                .iter()
                .map(|&store| {
                    store_events[store]
                        .as_ref()
                        .expect("planned store retains its event")
                        .clone()
                })
                .collect::<Vec<_>>();
            let mut replacement = Vec::new();
            emit_partial_store_overlay(
                &mut replacement,
                vregs,
                spill_descs,
                old,
                plan.destination,
                plan.load_slot,
                &stores,
            );
            replacements.insert(plan.load_instruction, replacement);
            if !retain_stores {
                removals.extend(plan.stores);
            }
            spill_descs[plan.destination.0 as usize] = SpillDesc::transient();
        }

        let original = std::mem::take(&mut block.insts);
        let mut rewritten = Vec::with_capacity(original.len());
        for (instruction, inst) in original.into_iter().enumerate() {
            if let Some(mut inserted) = insertions.remove(&instruction) {
                rewritten.append(&mut inserted);
            }
            if removals.contains(&instruction) {
                continue;
            }
            if let Some(mut replacement) = replacements.remove(&instruction) {
                rewritten.append(&mut replacement);
            } else {
                rewritten.push(inst);
            }
        }
        block.insts = rewritten;
    }
}

pub(super) fn forward_local_store_loads(func: &mut MFunction) {
    let (vregs, spill_descs, blocks) = (&mut func.vregs, &mut func.spill_descs, &mut func.blocks);
    for block in blocks {
        let mut available = AvailableStores::default();
        let mut rewritten = Vec::with_capacity(block.insts.len());

        for inst in block.insts.drain(..) {
            match inst {
                MInst::Store {
                    base,
                    offset,
                    src,
                    size,
                } => {
                    available.invalidate_slot(base, offset, size);
                    available.insert(base, offset, size, src);
                    rewritten.push(MInst::Store {
                        base,
                        offset,
                        src,
                        size,
                    });
                }
                MInst::Load {
                    dst,
                    base,
                    offset,
                    size,
                } => {
                    if let Some(src) = available.get(base, offset, size) {
                        emit_partial_load_forward(
                            &mut rewritten,
                            vregs,
                            spill_descs,
                            dst,
                            src,
                            offset,
                            size,
                            offset,
                            size,
                        );
                        continue;
                    }
                    if let Some((covering_slot, src)) =
                        find_best_covering_value(&available, base, offset, size)
                    {
                        emit_partial_load_forward(
                            &mut rewritten,
                            vregs,
                            spill_descs,
                            dst,
                            src,
                            covering_slot.offset,
                            covering_slot.size,
                            offset,
                            size,
                        );
                        continue;
                    }
                    available.insert(base, offset, size, dst);
                    rewritten.push(MInst::Load {
                        dst,
                        base,
                        offset,
                        size,
                    });
                }
                other => {
                    let writes = memory_effect::writes(&other);
                    if let Some(memory_effect::UnknownMemory::Direct(base)) =
                        writes.unknown_memory()
                    {
                        available.invalidate_base(base);
                    }
                    for range in writes.ranges() {
                        if let Some(end) = range.end() {
                            available.invalidate_range(range.base, range.offset, end);
                        } else {
                            available.invalidate_base(range.base);
                        }
                    }
                    rewritten.push(other);
                }
            }
        }

        block.insts = rewritten;
    }
}

/// Values still visible in memory at the current program point of a block,
/// indexed by `(base, size)` and then start offset so coverage queries only
/// inspect the few offsets a load can overlap instead of every tracked slot.
#[derive(Default)]
pub(super) struct AvailableStores {
    pub(super) slots: HashMap<(BaseReg, OpSize), BTreeMap<i32, VReg>>,
}

impl AvailableStores {
    #[cfg(test)]
    pub(super) fn clear(&mut self) {
        self.slots.clear();
    }

    pub(super) fn get(&self, base: BaseReg, offset: i32, size: OpSize) -> Option<VReg> {
        self.slots.get(&(base, size))?.get(&offset).copied()
    }

    pub(super) fn insert(&mut self, base: BaseReg, offset: i32, size: OpSize, value: VReg) {
        self.slots
            .entry((base, size))
            .or_default()
            .insert(offset, value);
    }

    pub(super) fn invalidate_slot(&mut self, base: BaseReg, offset: i32, size: OpSize) {
        let Some((start, end)) = byte_range(offset, size.bytes() as usize) else {
            self.invalidate_base(base);
            return;
        };
        self.invalidate_range(base, start, end);
    }

    #[cfg(test)]
    pub(super) fn invalidate_byte_range(&mut self, base: BaseReg, offset: i32, byte_len: usize) {
        let Some((start, end)) = byte_range(offset, byte_len) else {
            self.invalidate_base(base);
            return;
        };
        self.invalidate_range(base, start, end);
    }

    pub(super) fn invalidate_base(&mut self, base: BaseReg) {
        self.slots.retain(|(slot_base, _), _| *slot_base != base);
    }

    /// Drop every tracked value on `base` overlapping the byte range
    /// `[start, end)`.
    pub(super) fn invalidate_range(&mut self, base: BaseReg, start: i64, end: i64) {
        for ((slot_base, slot_size), slots) in self.slots.iter_mut() {
            if *slot_base != base {
                continue;
            }
            let width = i64::from(slot_size.bytes());
            // An entry starting at `o` overlaps when `o + width > start` and
            // `o < end`, i.e. `start - width + 1 <= o <= end - 1`.
            let Some((lo, hi)) = clamped_key_range(start - width + 1, end - 1) else {
                continue;
            };
            let overlapping: Vec<i32> = slots.range(lo..=hi).map(|(offset, _)| *offset).collect();
            for offset in overlapping {
                slots.remove(&offset);
            }
        }
    }
}

/// Clamp an inclusive i64 key window into the i32 domain of stored offsets.
pub(super) fn clamped_key_range(lo: i64, hi: i64) -> Option<(i32, i32)> {
    if lo > hi || lo > i64::from(i32::MAX) || hi < i64::from(i32::MIN) {
        return None;
    }
    let lo = lo.clamp(i64::from(i32::MIN), i64::from(i32::MAX));
    let hi = hi.clamp(i64::from(i32::MIN), i64::from(i32::MAX));
    Some((lo as i32, hi as i32))
}

pub(super) fn find_best_covering_value(
    available: &AvailableStores,
    base: BaseReg,
    offset: i32,
    size: OpSize,
) -> Option<(MemorySlot, VReg)> {
    let load_start = i64::from(offset);
    let load_end = load_start + i64::from(size.bytes());
    let mut best: Option<((MemorySlot, VReg), (u32, i64, i32, u32))> = None;
    for ((slot_base, slot_size), slots) in &available.slots {
        if *slot_base != base {
            continue;
        }
        let width = i64::from(slot_size.bytes());
        // A stored value covers the load when its range
        // `[value_start, value_start + width)` contains
        // `[load_start, load_end)`: `load_end - width <= o <= load_start`.
        let Some((lo, hi)) = clamped_key_range(load_end - width, load_start) else {
            continue;
        };
        for (value_offset, &src) in slots.range(lo..=hi) {
            let slot = MemorySlot {
                base,
                offset: *value_offset,
                size: *slot_size,
            };
            let selection_key = (
                slot_size.bytes(),
                load_start - i64::from(*value_offset),
                *value_offset,
                src.0,
            );
            if best
                .as_ref()
                .is_none_or(|(_, best_key)| selection_key < *best_key)
            {
                best = Some(((slot, src), selection_key));
            }
        }
    }
    best.map(|(found, _)| found)
}

fn emit_partial_load_forward(
    rewritten: &mut Vec<MInst>,
    vregs: &mut VRegAllocator,
    spill_descs: &mut Vec<SpillDesc>,
    dst: VReg,
    src: VReg,
    store_offset: i32,
    _store_size: OpSize,
    load_offset: i32,
    load_size: OpSize,
) {
    let shift_bytes = (load_offset - store_offset) as u8;
    let shift_bits = shift_bytes * 8;
    let mut current = src;

    if shift_bits != 0 {
        let shifted = alloc_transient_vreg(vregs, spill_descs);
        rewritten.push(MInst::ShrImm {
            dst: shifted,
            src: current,
            imm: shift_bits,
        });
        current = shifted;
    }

    let mask = match load_size {
        OpSize::S8 => Some(0xff),
        OpSize::S16 => Some(0xffff),
        OpSize::S32 => Some(0xffff_ffff),
        OpSize::S64 => None,
    };

    if let Some(mask) = mask {
        rewritten.push(MInst::AndImm {
            dst,
            src: current,
            imm: mask,
        });
    } else {
        rewritten.push(MInst::Mov { dst, src: current });
    }
}

pub(super) fn alloc_transient_vreg(
    vregs: &mut VRegAllocator,
    spill_descs: &mut Vec<SpillDesc>,
) -> VReg {
    let vreg = vregs.alloc();
    while spill_descs.len() <= vreg.0 as usize {
        spill_descs.push(SpillDesc::transient());
    }
    vreg
}

#[derive(Default)]
struct LaterDirectStores {
    sim_state: BTreeMap<i32, u8>,
    stack_frame: BTreeMap<i32, u8>,
}

impl LaterDirectStores {
    const SIZES: [(OpSize, u8); 4] = [
        (OpSize::S8, 1 << 0),
        (OpSize::S16, 1 << 1),
        (OpSize::S32, 1 << 2),
        (OpSize::S64, 1 << 3),
    ];

    fn slots(&self, base: BaseReg) -> &BTreeMap<i32, u8> {
        match base {
            BaseReg::SimState => &self.sim_state,
            BaseReg::StackFrame => &self.stack_frame,
        }
    }

    fn slots_mut(&mut self, base: BaseReg) -> &mut BTreeMap<i32, u8> {
        match base {
            BaseReg::SimState => &mut self.sim_state,
            BaseReg::StackFrame => &mut self.stack_frame,
        }
    }

    fn size_bit(size: OpSize) -> u8 {
        match size {
            OpSize::S8 => 1 << 0,
            OpSize::S16 => 1 << 1,
            OpSize::S32 => 1 << 2,
            OpSize::S64 => 1 << 3,
        }
    }

    fn contains(&self, slot: MemorySlot) -> bool {
        self.slots(slot.base)
            .get(&slot.offset)
            .is_some_and(|sizes| sizes & Self::size_bit(slot.size) != 0)
    }

    fn insert(&mut self, slot: MemorySlot) {
        *self.slots_mut(slot.base).entry(slot.offset).or_default() |= Self::size_bit(slot.size);
    }

    fn clear(&mut self, base: BaseReg) {
        self.slots_mut(base).clear();
    }

    /// Forget later stores whose values can be observed by `range`.
    ///
    /// Direct stores are at most eight bytes wide, so an overlapping store
    /// starts no earlier than seven bytes before the read.  Indexing by start
    /// offset finds narrow-read candidates in O(log n + overlap) instead of
    /// retaining over every store in the block; removing those candidates is
    /// O(overlap * log n).  A wide bounded indexed read visits only the
    /// tracked starts in its alias envelope.
    fn invalidate_range(
        &mut self,
        range: memory_effect::MemoryRange,
        scratch: &mut Vec<(i32, u8)>,
    ) {
        let Some(read_end) = range.end() else {
            self.clear(range.base);
            return;
        };
        let read_start = range.offset;
        if read_end <= read_start {
            return;
        }

        const MAX_STORE_BYTES: i64 = 8;
        let first_candidate = read_start.saturating_sub(MAX_STORE_BYTES - 1);
        let last_candidate = read_end - 1;
        if last_candidate < i64::from(i32::MIN) || first_candidate > i64::from(i32::MAX) {
            return;
        }
        let first_candidate =
            first_candidate.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
        let last_candidate = last_candidate.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
        if first_candidate > last_candidate {
            return;
        }

        scratch.clear();
        scratch.extend(
            self.slots(range.base)
                .range(first_candidate..=last_candidate)
                .filter_map(|(&offset, &sizes)| {
                    let mut retained = 0u8;
                    for (size, bit) in Self::SIZES {
                        if sizes & bit == 0 {
                            continue;
                        }
                        let slot_start = i64::from(offset);
                        let slot_end = slot_start + i64::from(size.bytes());
                        if slot_end <= read_start || read_end <= slot_start {
                            retained |= bit;
                        }
                    }
                    (retained != sizes).then_some((offset, retained))
                }),
        );
        let slots = self.slots_mut(range.base);
        for &(offset, retained) in scratch.iter() {
            if retained == 0 {
                slots.remove(&offset);
            } else {
                slots.insert(offset, retained);
            }
        }
    }
}

fn invalidate_stores_observed_by(
    later_stores: &mut LaterDirectStores,
    inst: &MInst,
    scratch: &mut Vec<(i32, u8)>,
) {
    let reads = memory_effect::reads(inst);
    if let Some(memory) = reads.unknown_memory() {
        match memory {
            memory_effect::UnknownMemory::Direct(base) => later_stores.clear(base),
            // Runtime-owned pointer memory is explicitly disjoint from both
            // direct-addressed bases tracked by this local DSE.
            memory_effect::UnknownMemory::Indirect => {}
        }
    }
    for range in reads.ranges() {
        later_stores.invalidate_range(range, scratch);
    }
}

pub(in super::super) fn eliminate_redundant_local_stores(func: &mut MFunction) {
    for block in &mut func.blocks {
        let mut later_stores = LaterDirectStores::default();
        let mut indexed_stores =
            Vec::<(BaseReg, i32, VReg, OpSize, Option<MemoryAliasRange>)>::new();
        let mut invalidation_scratch = Vec::new();
        let mut reversed = Vec::with_capacity(block.insts.len());

        for inst in block.insts.drain(..).rev() {
            invalidate_stores_observed_by(&mut later_stores, &inst, &mut invalidation_scratch);
            let reads = memory_effect::reads(&inst);
            indexed_stores.retain(|&(base, _, _, _, envelope)| {
                if reads.unknown_memory() == Some(memory_effect::UnknownMemory::Direct(base)) {
                    return false;
                }
                !reads.ranges().any(|read| {
                    read.base == base
                        && envelope.is_none_or(|envelope| {
                            read.end()
                                .is_none_or(|end| i64::from(envelope.offset()) < end)
                                && read.offset < envelope.end()
                        })
                })
            });
            if let MInst::StoreIndexed {
                base,
                offset,
                index,
                size,
                alias_range,
                ..
            } = &inst
            {
                if indexed_stores.iter().any(
                    |&(later_base, later_offset, later_index, later_size, later_envelope)| {
                        (
                            later_base,
                            later_offset,
                            later_index,
                            later_size,
                            later_envelope,
                        ) == (*base, *offset, *index, *size, *alias_range)
                    },
                ) {
                    continue;
                }
                // SSA index identity and the same semantic envelope prove an
                // exact overwrite. Reads invalidate through that envelope;
                // intervening writes cannot observe the discarded value.
                // Bound compile-time work for unusually long store-only blocks.
                if indexed_stores.len() == 64 {
                    indexed_stores.remove(0);
                }
                indexed_stores.push((*base, *offset, *index, *size, *alias_range));
            }
            if let MInst::Store {
                base, offset, size, ..
            } = &inst
            {
                let slot = MemorySlot {
                    base: *base,
                    offset: *offset,
                    size: *size,
                };
                if later_stores.contains(slot) {
                    continue;
                }
                // Writes do not observe an earlier value. Keep every exact
                // later overwrite candidate, including overlapping widths;
                // intervening reads invalidate precisely the candidates they
                // can observe.
                later_stores.insert(slot);
            }
            reversed.push(inst);
        }

        reversed.reverse();
        block.insts = reversed;
    }
}

/// Reuse indexed reads before allocation can overwrite their result registers.
/// Track at most 16 reads within 32 instructions to keep compile work bounded
/// and avoid distant reuse. Unknown or overlapping writes invalidate the
/// corresponding base/envelope as in direct forwarding.
pub(super) fn forward_local_indexed_loads(func: &mut MFunction) {
    const MAX_LOADS: usize = 16;
    const MAX_DISTANCE: usize = 32;
    #[derive(Clone, Copy)]
    struct Available {
        base: BaseReg,
        offset: i32,
        index: VReg,
        scale: u8,
        size: OpSize,
        alias: Option<MemoryAliasRange>,
        value: VReg,
        position: usize,
    }
    let mut reused = 0usize;
    for block in &mut func.blocks {
        let mut available = Vec::<Available>::with_capacity(MAX_LOADS);
        for (position, inst) in block.insts.iter_mut().enumerate() {
            let writes = memory_effect::writes(inst);
            available.retain(|load| {
                if position - load.position > MAX_DISTANCE {
                    return false;
                }
                if let Some(memory_effect::UnknownMemory::Direct(base)) = writes.unknown_memory()
                    && load.base == base
                {
                    return false;
                }
                for range in writes.ranges() {
                    if load.base != range.base {
                        continue;
                    }
                    let Some(alias) = load.alias else {
                        return false;
                    };
                    let Some(end) = range.end() else {
                        return false;
                    };
                    if i64::from(alias.offset()) < end && range.offset < alias.end() {
                        return false;
                    }
                }
                true
            });
            let load = match inst {
                MInst::LoadIndexed {
                    dst,
                    base,
                    offset,
                    index,
                    scale,
                    size,
                    alias_range,
                } => Some(Available {
                    base: *base,
                    offset: *offset,
                    index: *index,
                    scale: *scale,
                    size: *size,
                    alias: *alias_range,
                    value: *dst,
                    position,
                }),
                _ => None,
            };
            let source = load.and_then(|load| {
                available
                    .iter()
                    .find(|old| {
                        (old.base, old.offset, old.index, old.scale, old.size)
                            == (load.base, load.offset, load.index, load.scale, load.size)
                    })
                    .map(|old| old.value)
            });
            if let Some(definition) = inst.def() {
                available.retain(|old| old.index != definition && old.value != definition);
            }
            let Some(load) = load else {
                continue;
            };
            if let Some(source) = source {
                *inst = MInst::Mov {
                    dst: load.value,
                    src: source,
                };
                reused += 1;
            }
            if load.value != load.index {
                if available.len() == MAX_LOADS {
                    available.remove(0);
                }
                available.push(load);
            }
        }
    }
    tracing::debug!(reused, "local indexed load reuse");
}
