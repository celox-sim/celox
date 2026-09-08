//! Shorten scalar live ranges before deciding which values need spill homes.

use std::cmp::Reverse;
use std::collections::BTreeSet;

use celox_analysis::dependence::MemoryDependencyTracker;
use celox_analysis::memory::{MemoryEffect, MemoryLocation};
use celox_backend_common::regalloc::LiveIntervals;

use crate::HashMap;
use crate::mir::{BaseReg, MFunction, MInst, MemoryAliasRange, VReg};

pub(super) fn run(function: &mut MFunction, intervals: &LiveIntervals<VReg>, capacity: usize) {
    for (block_index, block) in function.blocks.iter_mut().enumerate() {
        let live_out = intervals
            .live_out(block_index)
            .expect("verified block liveness");
        let original = (0..block.insts.len()).collect::<Vec<_>>();
        let original_cost = pressure_cost(&block.insts, &original, live_out, capacity);
        if original_cost.0 == 0 {
            continue;
        }
        let Some(order) = schedule(&block.insts, live_out, capacity) else {
            continue;
        };
        if pressure_cost(&block.insts, &order, live_out, capacity) < original_cost {
            block.insts = order
                .into_iter()
                .map(|index| block.insts[index].clone())
                .collect();
        }
    }
}

fn pressure_cost(
    instructions: &[MInst],
    order: &[usize],
    live_out: &BTreeSet<VReg>,
    capacity: usize,
) -> (usize, usize) {
    let mut live = live_out.clone();
    let mut maximum = live.len();
    let mut excess = live.len().saturating_sub(capacity);
    for &index in order.iter().rev() {
        if let Some(value) = instructions[index].def() {
            live.remove(&value);
        }
        live.extend(instructions[index].uses());
        maximum = maximum.max(live.len());
        excess += live.len().saturating_sub(capacity);
    }
    (excess, maximum)
}

fn object(base: BaseReg) -> u8 {
    match base {
        BaseReg::SimState => 0,
        BaseReg::StackFrame => 1,
    }
}

fn exact(base: BaseReg, offset: i32, byte_len: usize) -> MemoryEffect<u8> {
    MemoryEffect::Exact(MemoryLocation {
        object: object(base),
        offset: i64::from(offset),
        byte_len,
    })
}

fn indexed(base: BaseReg, range: Option<MemoryAliasRange>) -> MemoryEffect<u8> {
    range.map_or(MemoryEffect::UnknownObject(object(base)), |range| {
        exact(base, range.offset(), range.byte_len())
    })
}

/// Pointer operations and control/runtime pseudos stay at their original
/// boundaries. Ordinary state accesses may move only with RAW/WAR/WAW edges.
fn effects(inst: &MInst) -> Option<(Vec<MemoryEffect<u8>>, Vec<MemoryEffect<u8>>)> {
    match *inst {
        MInst::Load {
            base, offset, size, ..
        } => Some((vec![exact(base, offset, size.bytes() as usize)], vec![])),
        MInst::Store {
            base, offset, size, ..
        } => Some((vec![], vec![exact(base, offset, size.bytes() as usize)])),
        MInst::LoadIndexed {
            base, alias_range, ..
        } => Some((vec![indexed(base, alias_range)], vec![])),
        MInst::StoreIndexed {
            base, alias_range, ..
        } => Some((vec![], vec![indexed(base, alias_range)])),
        MInst::OrStoreIndexed {
            base, alias_range, ..
        } => {
            let effect = indexed(base, alias_range);
            Some((vec![effect], vec![effect]))
        }
        MInst::MemFill {
            dst_offset,
            byte_len,
            ..
        } => Some((vec![], vec![exact(BaseReg::SimState, dst_offset, byte_len)])),
        MInst::Mov { .. }
        | MInst::Mov32 { .. }
        | MInst::LoadImm { .. }
        | MInst::LoadConstantTableAddr { .. }
        | MInst::BitExtract { .. }
        | MInst::BitInsert { .. }
        | MInst::OrShifted { .. }
        | MInst::Add { .. }
        | MInst::Add32 { .. }
        | MInst::Sub { .. }
        | MInst::Sub32 { .. }
        | MInst::Mul { .. }
        | MInst::Mul32 { .. }
        | MInst::UMulHi { .. }
        | MInst::And { .. }
        | MInst::And32 { .. }
        | MInst::Or { .. }
        | MInst::Or32 { .. }
        | MInst::Xor { .. }
        | MInst::Xor32 { .. }
        | MInst::Shr { .. }
        | MInst::Shl { .. }
        | MInst::Sar { .. }
        | MInst::AndImm { .. }
        | MInst::AndImm32 { .. }
        | MInst::OrImm { .. }
        | MInst::ShrImm { .. }
        | MInst::ShlImm { .. }
        | MInst::SarImm { .. }
        | MInst::AddImm { .. }
        | MInst::SubImm { .. }
        | MInst::Cmp { .. }
        | MInst::CmpImm { .. }
        | MInst::UDiv { .. }
        | MInst::URem { .. }
        | MInst::SDiv { .. }
        | MInst::SRem { .. }
        | MInst::BitNot { .. }
        | MInst::Neg { .. }
        | MInst::Popcnt { .. }
        | MInst::Bsf { .. }
        | MInst::Bsr { .. }
        | MInst::Select { .. }
        | MInst::CmpSelect { .. }
        | MInst::CmpImmSelect { .. }
        | MInst::GuardedCmpSelect { .. } => Some((vec![], vec![])),
        MInst::KeepAlive { .. }
        | MInst::LoadPtr { .. }
        | MInst::StorePtr { .. }
        | MInst::ReleaseStorePtr { .. }
        | MInst::LoadPtrIndexed { .. }
        | MInst::StorePtrIndexed { .. }
        | MInst::ReleaseStorePtrIndexed { .. }
        | MInst::PackedLaneCompare { .. }
        | MInst::PackedByteAffineCompare { .. }
        | MInst::MemCopy { .. }
        | MInst::SparseCommit { .. }
        | MInst::SparseMarkActive { .. }
        | MInst::SparseCommitWorklist { .. }
        | MInst::Branch { .. }
        | MInst::BranchPred { .. }
        | MInst::JumpTable { .. }
        | MInst::Jump { .. }
        | MInst::Return
        | MInst::ReturnError { .. } => None,
    }
}

fn schedule(
    instructions: &[MInst],
    live_out: &BTreeSet<VReg>,
    capacity: usize,
) -> Option<Vec<usize>> {
    let definitions = instructions
        .iter()
        .enumerate()
        .filter_map(|(index, inst)| inst.def().map(|value| (value, index)))
        .collect::<HashMap<_, _>>();
    let uses = instructions
        .iter()
        .map(|inst| {
            let mut values = inst.uses();
            values.sort_unstable();
            values.dedup();
            values
        })
        .collect::<Vec<_>>();
    let mut users = HashMap::<VReg, Vec<usize>>::default();
    let mut dependencies = vec![Vec::new(); instructions.len()];
    let mut dependents = vec![Vec::new(); instructions.len()];
    let mut memory = MemoryDependencyTracker::<u8, usize>::default();
    let mut since_barrier = Vec::new();
    let mut last_barrier = None;
    for (index, inst) in instructions.iter().enumerate() {
        let mut inputs = BTreeSet::new();
        for &value in &uses[index] {
            users.entry(value).or_default().push(index);
            inputs.extend(definitions.get(&value).copied());
        }
        inputs.extend(last_barrier);
        if let Some((reads, writes)) = effects(inst) {
            memory.add_event(index, reads, writes, &mut inputs);
            since_barrier.push(index);
        } else {
            inputs.extend(since_barrier.drain(..));
            last_barrier = Some(index);
            memory = MemoryDependencyTracker::default();
        }
        inputs.remove(&index);
        for dependency in inputs {
            if dependency >= index {
                return None;
            }
            dependencies[index].push(dependency);
            dependents[dependency].push(index);
        }
    }
    let mut entry = vec![1usize; instructions.len()];
    for (index, inputs) in dependencies.iter().enumerate() {
        for &input in inputs {
            entry[index] = entry[index].max(entry[input] + 1);
        }
    }
    let mut exit = vec![1usize; instructions.len()];
    for (index, inputs) in dependencies.iter().enumerate().rev() {
        for &input in inputs {
            exit[input] = exit[input].max(exit[index] + 1);
        }
    }
    let mut queue = ReadyQueue::new(exit.into_iter().zip(entry).collect());
    let mut live = live_out.clone();
    let mut remaining = dependents.iter().map(Vec::len).collect::<Vec<_>>();
    let delta = |index: usize, live: &BTreeSet<VReg>| {
        uses[index].iter().filter(|v| !live.contains(v)).count() as isize
            - isize::from(instructions[index].def().is_some_and(|v| live.contains(&v)))
    };
    for (index, &count) in remaining.iter().enumerate() {
        if count == 0 {
            queue.insert(index, delta(index, &live));
        }
    }
    let mut order = Vec::with_capacity(instructions.len());
    while order.len() < instructions.len() {
        let index = queue.pop(live.len(), capacity)?;
        let mut change = |value, adjustment| {
            for &user in users.get(&value).into_iter().flatten() {
                queue.adjust(user, adjustment);
            }
            if let Some(&definition) = definitions.get(&value) {
                queue.adjust(definition, adjustment);
            }
        };
        if let Some(value) = instructions[index].def()
            && live.remove(&value)
        {
            change(value, 1);
        }
        for &value in &uses[index] {
            if live.insert(value) {
                change(value, -1);
            }
        }
        for &dependency in &dependencies[index] {
            remaining[dependency] -= 1;
            if remaining[dependency] == 0 {
                queue.insert(dependency, delta(dependency, &live));
            }
        }
        order.push(index);
    }
    order.reverse();
    Some(order)
}

struct ReadyQueue {
    pressure: BTreeSet<(isize, Reverse<usize>, Reverse<usize>, Reverse<usize>)>,
    depth: BTreeSet<(Reverse<usize>, Reverse<usize>, isize, Reverse<usize>)>,
    ranks: Vec<(usize, usize)>,
    deltas: Vec<Option<isize>>,
}

impl ReadyQueue {
    fn new(ranks: Vec<(usize, usize)>) -> Self {
        Self {
            deltas: vec![None; ranks.len()],
            ranks,
            pressure: BTreeSet::new(),
            depth: BTreeSet::new(),
        }
    }

    fn insert(&mut self, index: usize, delta: isize) {
        let (exit, entry) = self.ranks[index];
        self.pressure
            .insert((delta, Reverse(exit), Reverse(entry), Reverse(index)));
        self.depth
            .insert((Reverse(exit), Reverse(entry), delta, Reverse(index)));
        self.deltas[index] = Some(delta);
    }

    fn remove(&mut self, index: usize) -> isize {
        let delta = self.deltas[index].take().expect("queued instruction");
        let (exit, entry) = self.ranks[index];
        self.pressure
            .remove(&(delta, Reverse(exit), Reverse(entry), Reverse(index)));
        self.depth
            .remove(&(Reverse(exit), Reverse(entry), delta, Reverse(index)));
        delta
    }

    fn adjust(&mut self, index: usize, adjustment: isize) {
        if self.deltas[index].is_some() {
            let delta = self.remove(index);
            self.insert(index, delta + adjustment);
        }
    }

    fn pop(&mut self, live: usize, capacity: usize) -> Option<usize> {
        let &(_, _, delta, Reverse(deepest)) = self.depth.first()?;
        let index = if live <= capacity && live.saturating_add_signed(delta) <= capacity {
            deepest
        } else {
            self.pressure.first()?.3.0
        };
        self.remove(index);
        Some(index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mir::{BlockId, MBlock, OpSize};

    #[test]
    fn independent_snapshots_do_not_require_spill_homes() {
        let mut block = MBlock::new(BlockId(0));
        for index in 0..32 {
            block.push(MInst::Load {
                dst: VReg(index),
                base: BaseReg::SimState,
                offset: (index * 8) as i32,
                size: OpSize::S64,
            });
        }
        for index in 0..32 {
            block.push(MInst::AddImm {
                dst: VReg(32 + index),
                src: VReg(index),
                imm: 1,
            });
            block.push(MInst::Store {
                base: BaseReg::SimState,
                offset: ((32 + index) * 8) as i32,
                src: VReg(32 + index),
                size: OpSize::S64,
            });
        }
        block.push(MInst::Return);
        let allocation =
            super::super::allocate_with_spills(MFunction::new(vec![block], vec![]), || false)
                .unwrap();
        assert!(allocation.allocated.function.spill_homes.is_empty());
        assert_eq!(allocation.spill_frame_size, 0);
        super::super::verify_allocated(&allocation.allocated).unwrap();
    }

    #[test]
    fn overlap_and_unknown_index_preserve_memory_versions() {
        for range in [None, MemoryAliasRange::new(0, 16)] {
            let instructions = vec![
                MInst::LoadImm {
                    dst: VReg(0),
                    value: 3,
                },
                MInst::Load {
                    dst: VReg(1),
                    base: BaseReg::SimState,
                    offset: 0,
                    size: OpSize::S64,
                },
                MInst::StoreIndexed {
                    base: BaseReg::SimState,
                    offset: 0,
                    index: VReg(0),
                    src: VReg(0),
                    size: OpSize::S8,
                    alias_range: range,
                },
                MInst::Load {
                    dst: VReg(2),
                    base: BaseReg::SimState,
                    offset: 0,
                    size: OpSize::S64,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 0,
                    src: VReg(0),
                    size: OpSize::S64,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 16,
                    src: VReg(1),
                    size: OpSize::S64,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 24,
                    src: VReg(2),
                    size: OpSize::S64,
                },
                MInst::Return,
            ];
            let order = schedule(&instructions, &BTreeSet::new(), 2).unwrap();
            let position = |index| order.iter().position(|&value| value == index).unwrap();
            // The first snapshot predates the byte update; the second one
            // observes it, and both must precede the covering eight-byte write.
            assert!(position(1) < position(2));
            assert!(position(2) < position(3));
            assert!(position(3) < position(4));
        }
    }
}
