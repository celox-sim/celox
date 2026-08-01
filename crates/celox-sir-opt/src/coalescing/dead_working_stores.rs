use std::collections::VecDeque;

use crate::ir::*;
use crate::{HashMap, HashSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct WorkingKey {
    addr: AbsoluteAddr,
    offset: usize,
    bits: usize,
}

impl WorkingKey {
    fn end(self) -> usize {
        self.offset.saturating_add(self.bits)
    }

    fn overlaps(self, other: Self) -> bool {
        self.addr == other.addr && self.offset < other.end() && other.offset < self.end()
    }

    fn covers(self, other: Self) -> bool {
        self.addr == other.addr && self.offset <= other.offset && other.end() <= self.end()
    }
}

fn read_working_key(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> Option<WorkingKey> {
    match inst {
        SIRInstruction::Load(_, addr, SIROffset::Static(offset), bits)
            if addr.region == WORKING_REGION =>
        {
            Some(WorkingKey {
                addr: addr.absolute_addr(),
                offset: *offset,
                bits: *bits,
            })
        }
        SIRInstruction::Commit(src, _, SIROffset::Static(offset), bits, _)
            if src.region == WORKING_REGION =>
        {
            Some(WorkingKey {
                addr: src.absolute_addr(),
                offset: *offset,
                bits: *bits,
            })
        }
        _ => None,
    }
}

fn is_dynamic_working_read(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> bool {
    match inst {
        SIRInstruction::Load(_, addr, SIROffset::Dynamic(_) | SIROffset::Element { .. }, _) => {
            addr.region == WORKING_REGION
        }
        SIRInstruction::Commit(src, _, SIROffset::Dynamic(_) | SIROffset::Element { .. }, _, _) => {
            src.region == WORKING_REGION
        }
        _ => false,
    }
}

fn working_store_key(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> Option<WorkingKey> {
    match inst {
        SIRInstruction::Store(addr, SIROffset::Static(offset), bits, _, _, _)
            if addr.region == WORKING_REGION =>
        {
            Some(WorkingKey {
                addr: addr.absolute_addr(),
                offset: *offset,
                bits: *bits,
            })
        }
        SIRInstruction::Commit(_, dst, SIROffset::Static(offset), bits, _)
            if dst.region == WORKING_REGION =>
        {
            Some(WorkingKey {
                addr: dst.absolute_addr(),
                offset: *offset,
                bits: *bits,
            })
        }
        _ => None,
    }
}

fn successor_blocks(block: &BasicBlock<RegionedAbsoluteAddr>) -> Vec<BlockId> {
    match &block.terminator {
        SIRTerminator::Jump(dst, _) => vec![*dst],
        SIRTerminator::Branch {
            true_block,
            false_block,
            ..
        } => vec![true_block.0, false_block.0],
        SIRTerminator::Switch { cases, default, .. } => cases
            .iter()
            .map(|case| case.target)
            .chain(std::iter::once(*default))
            .collect(),
        SIRTerminator::Return | SIRTerminator::Error(_) => Vec::new(),
    }
}

#[derive(Clone, Default, PartialEq, Eq)]
struct LiveBits {
    words: Vec<u64>,
}

impl LiveBits {
    fn with_len(bits: usize) -> Self {
        Self {
            words: vec![0; bits.div_ceil(64)],
        }
    }

    fn insert(&mut self, bit: usize) {
        self.words[bit / 64] |= 1u64 << (bit % 64);
    }

    fn remove(&mut self, bit: usize) {
        self.words[bit / 64] &= !(1u64 << (bit % 64));
    }

    fn contains(&self, bit: usize) -> bool {
        self.words[bit / 64] & (1u64 << (bit % 64)) != 0
    }

    fn union_with(&mut self, other: &Self) {
        for (word, other) in self.words.iter_mut().zip(&other.words) {
            *word |= *other;
        }
    }
}

#[derive(Default)]
struct StoreTransfer {
    overlapping_reads: Vec<usize>,
    covered_reads: Vec<usize>,
}

fn store_transfer(
    key: WorkingKey,
    reads_by_addr: &HashMap<AbsoluteAddr, Vec<(WorkingKey, usize)>>,
) -> StoreTransfer {
    let mut transfer = StoreTransfer::default();
    if let Some(reads) = reads_by_addr.get(&key.addr) {
        for &(read, index) in reads {
            if key.overlaps(read) {
                transfer.overlapping_reads.push(index);
            }
            if key.covers(read) {
                transfer.covered_reads.push(index);
            }
        }
    }
    transfer
}

fn transfer_block(
    block: &BasicBlock<RegionedAbsoluteAddr>,
    mut live: LiveBits,
    mut unknown: bool,
    read_index: &HashMap<WorkingKey, usize>,
    store_transfers: &HashMap<WorkingKey, StoreTransfer>,
) -> (LiveBits, bool) {
    for inst in block.instructions.iter().rev() {
        if is_dynamic_working_read(inst) {
            unknown = true;
        } else if let Some(key) = read_working_key(inst) {
            live.insert(read_index[&key]);
        } else if let Some(key) = working_store_key(inst)
            && !unknown
        {
            for &read in &store_transfers[&key].covered_reads {
                live.remove(read);
            }
        }
    }
    (live, unknown)
}

pub(super) fn eliminate_dead_working_stores(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) {
    let mut read_keys = Vec::new();
    let mut read_index = HashMap::default();
    let mut store_keys = HashSet::default();
    for block in eu.blocks.values() {
        for inst in &block.instructions {
            if let Some(key) = read_working_key(inst)
                && !read_index.contains_key(&key)
            {
                let index = read_keys.len();
                read_keys.push(key);
                read_index.insert(key, index);
            }
            if let Some(key) = working_store_key(inst) {
                store_keys.insert(key);
            }
        }
    }
    if store_keys.is_empty() {
        return;
    }

    let mut reads_by_addr: HashMap<AbsoluteAddr, Vec<(WorkingKey, usize)>> = HashMap::default();
    for (index, &key) in read_keys.iter().enumerate() {
        reads_by_addr
            .entry(key.addr)
            .or_default()
            .push((key, index));
    }
    let store_transfers = store_keys
        .into_iter()
        .map(|key| (key, store_transfer(key, &reads_by_addr)))
        .collect::<HashMap<_, _>>();

    let mut block_ids = eu.blocks.keys().copied().collect::<Vec<_>>();
    block_ids.sort_unstable();
    let block_index = block_ids
        .iter()
        .enumerate()
        .map(|(index, &block)| (block, index))
        .collect::<HashMap<_, _>>();
    let successors = block_ids
        .iter()
        .map(|block| successor_blocks(&eu.blocks[block]))
        .collect::<Vec<_>>();
    let mut predecessors = vec![Vec::new(); block_ids.len()];
    for (block, targets) in successors.iter().enumerate() {
        for target in targets {
            if let Some(&target) = block_index.get(target) {
                predecessors[target].push(block);
            }
        }
    }

    let mut live_in = vec![LiveBits::with_len(read_keys.len()); block_ids.len()];
    let mut unknown_in = vec![false; block_ids.len()];
    let mut queued = vec![true; block_ids.len()];
    let mut work = (0..block_ids.len()).rev().collect::<VecDeque<_>>();
    while let Some(block) = work.pop_front() {
        queued[block] = false;
        let mut out = LiveBits::with_len(read_keys.len());
        let mut out_unknown = false;
        for successor in &successors[block] {
            let Some(&successor) = block_index.get(successor) else {
                continue;
            };
            out.union_with(&live_in[successor]);
            out_unknown |= unknown_in[successor];
        }
        let (next, next_unknown) = transfer_block(
            &eu.blocks[&block_ids[block]],
            out,
            out_unknown,
            &read_index,
            &store_transfers,
        );
        if next == live_in[block] && next_unknown == unknown_in[block] {
            continue;
        }
        live_in[block] = next;
        unknown_in[block] = next_unknown;
        for &predecessor in &predecessors[block] {
            if !queued[predecessor] {
                queued[predecessor] = true;
                work.push_back(predecessor);
            }
        }
    }

    for (block, block_id) in block_ids.into_iter().enumerate() {
        let mut live = LiveBits::with_len(read_keys.len());
        let mut unknown = false;
        for successor in &successors[block] {
            let Some(&successor) = block_index.get(successor) else {
                continue;
            };
            live.union_with(&live_in[successor]);
            unknown |= unknown_in[successor];
        }
        let body = eu.blocks.get_mut(&block_id).expect("indexed block exists");
        let mut keep = vec![true; body.instructions.len()];
        for index in (0..body.instructions.len()).rev() {
            let inst = &body.instructions[index];
            if is_dynamic_working_read(inst) {
                unknown = true;
            } else if let Some(key) = read_working_key(inst) {
                live.insert(read_index[&key]);
            } else if let Some(key) = working_store_key(inst) {
                let transfer = &store_transfers[&key];
                if !unknown
                    && !transfer
                        .overlapping_reads
                        .iter()
                        .any(|&read| live.contains(read))
                {
                    keep[index] = false;
                }
                if !unknown {
                    for &read in &transfer.covered_reads {
                        live.remove(read);
                    }
                }
            }
        }
        let mut index = 0usize;
        body.instructions.retain(|_| {
            let retain = keep[index];
            index += 1;
            retain
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{InstanceId, RegisterType};
    use celox_design::StateObjectId as VarId;

    fn address(region: u32, instance: usize) -> RegionedAbsoluteAddr {
        RegionedAbsoluteAddr {
            region,
            instance_id: InstanceId(instance),
            var_id: VarId::default(),
        }
    }

    fn store(
        address: RegionedAbsoluteAddr,
        offset: usize,
        bits: usize,
        source: usize,
    ) -> SIRInstruction<RegionedAbsoluteAddr> {
        SIRInstruction::Store(
            address,
            SIROffset::Static(offset),
            bits,
            RegisterId(source),
            Vec::new(),
            Vec::new(),
        )
    }

    fn unit(
        blocks: impl IntoIterator<Item = BasicBlock<RegionedAbsoluteAddr>>,
    ) -> ExecutionUnit<RegionedAbsoluteAddr> {
        ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: blocks.into_iter().map(|block| (block.id, block)).collect(),
            register_map: (0..4)
                .map(|register| {
                    (
                        RegisterId(register),
                        RegisterType::Bit {
                            width: 16,
                            signed: false,
                        },
                    )
                })
                .collect(),
        }
    }

    #[test]
    fn exact_stage_makes_the_older_seed_dead() {
        let stable = address(STABLE_REGION, 0);
        let working = address(WORKING_REGION, 0);
        let stage = store(working, 0, 8, 0);
        let apply = SIRInstruction::Commit(working, stable, SIROffset::Static(0), 8, Vec::new());
        let mut eu = unit([BasicBlock {
            id: BlockId(0),
            params: Vec::new(),
            instructions: vec![
                SIRInstruction::Commit(stable, working, SIROffset::Static(0), 8, Vec::new()),
                stage.clone(),
                apply.clone(),
            ],
            terminator: SIRTerminator::Return,
        }]);

        eliminate_dead_working_stores(&mut eu);

        assert_eq!(eu.blocks[&BlockId(0)].instructions, vec![stage, apply]);
    }

    #[test]
    fn partial_stage_keeps_the_wide_seed_for_uncovered_bits() {
        let stable = address(STABLE_REGION, 0);
        let working = address(WORKING_REGION, 0);
        let seed = SIRInstruction::Commit(stable, working, SIROffset::Static(0), 16, Vec::new());
        let stage = store(working, 0, 8, 0);
        let apply = SIRInstruction::Commit(working, stable, SIROffset::Static(0), 16, Vec::new());
        let mut eu = unit([BasicBlock {
            id: BlockId(0),
            params: Vec::new(),
            instructions: vec![seed.clone(), stage.clone(), apply.clone()],
            terminator: SIRTerminator::Return,
        }]);

        eliminate_dead_working_stores(&mut eu);

        assert_eq!(
            eu.blocks[&BlockId(0)].instructions,
            vec![seed, stage, apply]
        );
    }

    #[test]
    fn seed_stays_live_when_only_one_branch_overwrites_it() {
        let stable = address(STABLE_REGION, 0);
        let working = address(WORKING_REGION, 0);
        let seed = SIRInstruction::Commit(stable, working, SIROffset::Static(0), 8, Vec::new());
        let stage = store(working, 0, 8, 0);
        let apply = SIRInstruction::Commit(working, stable, SIROffset::Static(0), 8, Vec::new());
        let mut eu = unit([
            BasicBlock {
                id: BlockId(0),
                params: Vec::new(),
                instructions: vec![seed.clone()],
                terminator: SIRTerminator::Branch {
                    cond: RegisterId(1),
                    true_block: (BlockId(1), Vec::new()),
                    false_block: (BlockId(2), Vec::new()),
                },
            },
            BasicBlock {
                id: BlockId(1),
                params: Vec::new(),
                instructions: vec![stage.clone()],
                terminator: SIRTerminator::Jump(BlockId(3), Vec::new()),
            },
            BasicBlock {
                id: BlockId(2),
                params: Vec::new(),
                instructions: Vec::new(),
                terminator: SIRTerminator::Jump(BlockId(3), Vec::new()),
            },
            BasicBlock {
                id: BlockId(3),
                params: Vec::new(),
                instructions: vec![apply.clone()],
                terminator: SIRTerminator::Return,
            },
        ]);

        eliminate_dead_working_stores(&mut eu);

        assert_eq!(eu.blocks[&BlockId(0)].instructions, vec![seed]);
        assert_eq!(eu.blocks[&BlockId(1)].instructions, vec![stage]);
        assert_eq!(eu.blocks[&BlockId(3)].instructions, vec![apply]);
    }

    #[test]
    fn dynamic_read_keeps_every_preceding_working_store() {
        let working = address(WORKING_REGION, 0);
        let first = store(working, 0, 8, 0);
        let second = store(working, 8, 8, 1);
        let dynamic =
            SIRInstruction::Load(RegisterId(2), working, SIROffset::Dynamic(RegisterId(3)), 1);
        let mut eu = unit([BasicBlock {
            id: BlockId(0),
            params: Vec::new(),
            instructions: vec![first.clone(), second.clone(), dynamic.clone()],
            terminator: SIRTerminator::Return,
        }]);

        eliminate_dead_working_stores(&mut eu);

        assert_eq!(
            eu.blocks[&BlockId(0)].instructions,
            vec![first, second, dynamic]
        );
    }
}
