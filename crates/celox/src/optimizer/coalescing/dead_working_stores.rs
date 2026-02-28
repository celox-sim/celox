use crate::ir::*;
use crate::{HashMap, HashSet};

pub(super) fn eliminate_dead_working_stores(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    struct WorkingKey {
        addr: AbsoluteAddr,
        offset: usize,
        bits: usize,
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
            SIRInstruction::Load(_, addr, SIROffset::Dynamic(_), _) => {
                addr.region == WORKING_REGION
            }
            SIRInstruction::Commit(src, _, SIROffset::Dynamic(_), _, _) => {
                src.region == WORKING_REGION
            }
            _ => false,
        }
    }

    fn working_store_key(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> Option<WorkingKey> {
        match inst {
            SIRInstruction::Store(addr, SIROffset::Static(offset), bits, _, _)
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
            SIRTerminator::Return => vec![],
            SIRTerminator::Error(_code) => vec![],
        }
    }

    fn ranges_overlap(a: &WorkingKey, b: &WorkingKey) -> bool {
        a.addr == b.addr && a.offset < b.offset + b.bits && b.offset < a.offset + a.bits
    }

    fn store_is_live(key: &WorkingKey, live: &HashSet<WorkingKey>) -> bool {
        live.iter().any(|lk| ranges_overlap(key, lk))
    }

    fn remove_covered(key: &WorkingKey, live: &mut HashSet<WorkingKey>) {
        live.retain(|lk| {
            !(lk.addr == key.addr
                && key.offset <= lk.offset
                && lk.offset + lk.bits <= key.offset + key.bits)
        });
    }

    fn transfer_live(
        block: &BasicBlock<RegionedAbsoluteAddr>,
        mut live: HashSet<WorkingKey>,
        mut unknown: bool,
    ) -> (HashSet<WorkingKey>, bool) {
        for inst in block.instructions.iter().rev() {
            if is_dynamic_working_read(inst) {
                unknown = true;
                continue;
            }

            if let Some(key) = read_working_key(inst) {
                live.insert(key);
                continue;
            }

            if let Some(key) = working_store_key(inst)
                && !unknown
            {
                remove_covered(&key, &mut live);
            }
        }

        (live, unknown)
    }

    let block_ids: Vec<_> = eu.blocks.keys().copied().collect();
    let mut live_in: HashMap<BlockId, HashSet<WorkingKey>> = HashMap::default();
    let mut live_out: HashMap<BlockId, HashSet<WorkingKey>> = HashMap::default();
    let mut unknown_in: HashMap<BlockId, bool> = HashMap::default();
    let mut unknown_out: HashMap<BlockId, bool> = HashMap::default();

    for bid in &block_ids {
        live_in.insert(*bid, HashSet::default());
        live_out.insert(*bid, HashSet::default());
        unknown_in.insert(*bid, false);
        unknown_out.insert(*bid, false);
    }

    let mut changed = true;
    while changed {
        changed = false;

        for bid in block_ids.iter().rev() {
            let Some(block) = eu.blocks.get(bid) else {
                continue;
            };

            let succs = successor_blocks(block);
            let mut out_set: HashSet<WorkingKey> = HashSet::default();
            let mut out_unknown = false;
            for succ in succs {
                if let Some(s) = live_in.get(&succ) {
                    out_set.extend(s.iter().copied());
                }
                if unknown_in.get(&succ).copied().unwrap_or(false) {
                    out_unknown = true;
                }
            }

            if live_out.get(bid) != Some(&out_set) {
                live_out.insert(*bid, out_set.clone());
                changed = true;
            }
            if unknown_out.get(bid).copied().unwrap_or(false) != out_unknown {
                unknown_out.insert(*bid, out_unknown);
                changed = true;
            }

            let (in_set, in_unknown) = transfer_live(block, out_set, out_unknown);
            if live_in.get(bid) != Some(&in_set) {
                live_in.insert(*bid, in_set);
                changed = true;
            }
            if unknown_in.get(bid).copied().unwrap_or(false) != in_unknown {
                unknown_in.insert(*bid, in_unknown);
                changed = true;
            }
        }
    }

    for bid in block_ids {
        let Some(block) = eu.blocks.get_mut(&bid) else {
            continue;
        };

        let mut live = live_out.get(&bid).cloned().unwrap_or_default();
        let mut unknown = unknown_out.get(&bid).copied().unwrap_or(false);
        let mut keep = vec![true; block.instructions.len()];

        for idx in (0..block.instructions.len()).rev() {
            let inst = &block.instructions[idx];

            if is_dynamic_working_read(inst) {
                unknown = true;
                continue;
            }

            if let Some(key) = read_working_key(inst) {
                live.insert(key);
                continue;
            }

            if let Some(key) = working_store_key(inst) {
                if !unknown && !store_is_live(&key, &live) {
                    keep[idx] = false;
                }
                if !unknown {
                    remove_covered(&key, &mut live);
                }
            }
        }

        let mut out = Vec::with_capacity(block.instructions.len());
        for (idx, inst) in block.instructions.iter().enumerate() {
            if keep[idx] {
                out.push(inst.clone());
            }
        }
        block.instructions = out;
    }
}
