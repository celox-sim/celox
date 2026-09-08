//! Reuse a loop's increasing index for a redundant decreasing trip counter.

use super::*;
use crate::mir::BaseReg;

fn may_write(inst: &MInst, start: i64, end: i64) -> bool {
    let overlaps = |offset: i32, bytes: usize| {
        i64::from(offset) < end
            && i64::try_from(bytes)
                .ok()
                .and_then(|bytes| i64::from(offset).checked_add(bytes))
                .is_none_or(|limit| start < limit)
    };
    match *inst {
        MInst::Store {
            base, offset, size, ..
        } => base == BaseReg::SimState && overlaps(offset, usize::from(size.bytes())),
        MInst::StoreIndexed {
            base, alias_range, ..
        }
        | MInst::OrStoreIndexed {
            base, alias_range, ..
        } => {
            base == BaseReg::SimState
                && alias_range.is_none_or(|range| overlaps(range.offset(), range.byte_len()))
        }
        MInst::MemCopy {
            dst_offset,
            byte_len,
            ..
        }
        | MInst::MemFill {
            dst_offset,
            byte_len,
            ..
        } => overlaps(dst_offset, byte_len),
        MInst::KeepAlive { .. }
        | MInst::Branch { .. }
        | MInst::BranchPred { .. }
        | MInst::Jump { .. }
        | MInst::JumpTable { .. }
        | MInst::Return
        | MInst::ReturnError { .. } => false,
        // Pointer stores and runtime/sparse pseudos may change state memory.
        _ => inst.def().is_none(),
    }
}

/// A header load is already guaranteed to execute from its sole jumping
/// predecessor. Hoist it only if no iteration can modify its physical bytes.
pub(super) fn hoist_invariant_loads(func: &mut MFunction) {
    let positions = func
        .blocks
        .iter()
        .enumerate()
        .map(|(index, block)| (block.id, index))
        .collect::<HashMap<_, _>>();
    let Some(successors) = func
        .blocks
        .iter()
        .map(|block| {
            block
                .successors()
                .into_iter()
                .map(|id| positions.get(&id).copied())
                .collect::<Option<Vec<_>>>()
        })
        .collect::<Option<Vec<_>>>()
    else {
        return;
    };
    let Ok(cfg) = celox_analysis::cfg::ForwardControlFlowGraph::analyze_structure(successors, 0)
    else {
        return;
    };
    for region in &cfg.loops {
        let mut entries = cfg.predecessors[region.header]
            .iter()
            .copied()
            .filter(|pred| !region.blocks.contains(pred));
        let Some(entry) = entries.next() else {
            continue;
        };
        if entries.next().is_some()
            || !matches!(func.blocks[entry].terminator(), Some(MInst::Jump { target }) if *target == func.blocks[region.header].id)
        {
            continue;
        }
        let writes = region
            .blocks
            .iter()
            .flat_map(|&block| &func.blocks[block].insts)
            .filter(|inst| inst.def().is_none())
            .cloned()
            .collect::<Vec<_>>();
        let mut moved = Vec::new();
        func.blocks[region.header].insts.retain(|inst| {
            let MInst::Load {
                base: BaseReg::SimState,
                offset,
                size,
                ..
            } = *inst
            else {
                return true;
            };
            let start = i64::from(offset);
            let end = start + i64::from(size.bytes());
            if writes.iter().any(|write| may_write(write, start, end)) {
                return true;
            }
            moved.push(inst.clone());
            false
        });
        let at = func.blocks[entry].insts.len() - 1;
        func.blocks[entry].insts.splice(at..at, moved);
    }
}

pub(super) fn step(
    value: VReg,
    source: VReg,
    increasing: bool,
    definitions: &[Option<&MInst>],
) -> Option<(u64, Vec<VReg>)> {
    let mut value = value;
    let mut mask = u64::MAX;
    let mut chain = Vec::new();
    for _ in 0..8 {
        chain.push(value);
        let inst = *definitions.get(value.0 as usize)?.as_ref()?;
        let constant_one = |value: VReg| {
            matches!(
                definitions[value.0 as usize],
                Some(MInst::LoadImm { value: 1, .. })
            )
        };
        match *inst {
            MInst::Mov { src, .. } => value = src,
            MInst::Mov32 { src, .. } => {
                mask &= u32::MAX as u64;
                value = src;
            }
            MInst::AndImm { src, imm, .. } => {
                mask &= imm;
                value = src;
            }
            MInst::AndImm32 { src, imm, .. } => {
                mask &= u64::from(imm);
                value = src;
            }
            MInst::AddImm { src, imm: 1, .. } if increasing && src == source => break,
            MInst::SubImm { src, imm: 1, .. } if !increasing && src == source => break,
            MInst::Add { lhs, rhs, .. } | MInst::Add32 { lhs, rhs, .. }
                if increasing
                    && (lhs == source && constant_one(rhs)
                        || rhs == source && constant_one(lhs)) =>
            {
                if matches!(inst, MInst::Add32 { .. }) {
                    mask &= u32::MAX as u64;
                }
                break;
            }
            MInst::Sub { lhs, rhs, .. } | MInst::Sub32 { lhs, rhs, .. }
                if !increasing && lhs == source && constant_one(rhs) =>
            {
                if matches!(inst, MInst::Sub32 { .. }) {
                    mask &= u32::MAX as u64;
                }
                break;
            }
            _ => return None,
        }
    }
    // A truncation must preserve a contiguous low part. Confirm that the walk
    // ended on the arithmetic operation, rather than exhausting its budget.
    if value != *chain.last()? || mask == 0 || mask & mask.wrapping_add(1) != 0 {
        return None;
    }
    Some((mask, chain))
}

/// A zero-based, unit-step phi cannot reach its limit on a backedge guarded by
/// `next != limit`. Seed that inductive invariant before the bit-fact solve.
pub(crate) fn index_zeros(func: &MFunction) -> Vec<(VReg, u64)> {
    let mut definitions = vec![None; func.value_count()];
    let positions = func
        .blocks
        .iter()
        .enumerate()
        .map(|(index, block)| (block.id, index))
        .collect::<HashMap<_, _>>();
    for block in &func.blocks {
        for inst in &block.insts {
            if let Some(dst) = inst.def() {
                definitions[dst.0 as usize] = Some(inst);
            }
        }
    }
    let mut facts = Vec::new();
    for header in &func.blocks {
        for phi in &header.phis {
            if phi.sources.len() != 2 {
                continue;
            }
            let Some(&(entry, _)) = phi.sources.iter().find(|(_, source)| {
                matches!(
                    definitions[source.0 as usize],
                    Some(MInst::LoadImm { value: 0, .. })
                )
            }) else {
                continue;
            };
            let Some(&(latch, next)) = phi.sources.iter().find(|(pred, _)| *pred != entry) else {
                continue;
            };
            let Some((mask, _)) = step(next, phi.dst, true, &definitions) else {
                continue;
            };
            let (lhs, limit, kind, true_bb, false_bb) = match func.blocks[positions[&latch]]
                .terminator()
            {
                Some(&MInst::Branch {
                    cond,
                    true_bb,
                    false_bb,
                }) => {
                    let Some(&MInst::CmpImm { lhs, imm, kind, .. }) = definitions[cond.0 as usize]
                    else {
                        continue;
                    };
                    (lhs, imm, kind, true_bb, false_bb)
                }
                Some(&MInst::BranchPred {
                    predicate: BranchPredicate::CompareImm { lhs, imm, kind },
                    true_bb,
                    false_bb,
                }) => (lhs, imm, kind, true_bb, false_bb),
                _ => continue,
            };
            if lhs != next
                || limit <= 0
                || limit as u64 > mask
                || !matches!(
                    (kind, true_bb == header.id, false_bb == header.id),
                    (CmpKind::Ne, true, false) | (CmpKind::Eq, false, true)
                )
            {
                continue;
            }
            let maximum = (limit - 1) as u64;
            facts.push((
                phi.dst,
                u64::MAX
                    .checked_shl(64 - maximum.leading_zeros())
                    .unwrap_or(0),
            ));
        }
    }
    facts
}

pub(crate) fn run(func: &mut MFunction) {
    let positions = func
        .blocks
        .iter()
        .enumerate()
        .map(|(index, block)| (block.id, index))
        .collect::<HashMap<_, _>>();
    let Some(successors) = func
        .blocks
        .iter()
        .map(|block| {
            block
                .successors()
                .into_iter()
                .map(|target| positions.get(&target).copied())
                .collect::<Option<Vec<_>>>()
        })
        .collect::<Option<Vec<_>>>()
    else {
        return;
    };
    let Ok(cfg) = celox_analysis::cfg::ForwardControlFlowGraph::analyze_structure(successors, 0)
    else {
        return;
    };
    let mut definitions = vec![None; func.value_count()];
    let mut uses = vec![0usize; definitions.len()];
    for block in &func.blocks {
        for phi in &block.phis {
            for &(_, source) in &phi.sources {
                uses[source.0 as usize] += 1;
            }
        }
        for inst in &block.insts {
            for source in inst.uses() {
                uses[source.0 as usize] += 1;
            }
            if let Some(dst) = inst.def() {
                definitions[dst.0 as usize] = Some(inst);
            }
        }
    }
    let constant = |value: VReg| match definitions[value.0 as usize] {
        Some(MInst::LoadImm { value, .. }) => Some(*value),
        _ => None,
    };
    let mut plans = Vec::new();
    for region in &cfg.loops {
        let header = &func.blocks[region.header];
        for counter in &header.phis {
            if counter.sources.len() != 2 {
                continue;
            }
            let Some(&(latch, next)) = counter
                .sources
                .iter()
                .find(|(pred, _)| region.blocks.contains(&positions[pred]))
            else {
                continue;
            };
            let Some(&(entry, initial)) = counter
                .sources
                .iter()
                .find(|(pred, _)| !region.blocks.contains(&positions[pred]))
            else {
                continue;
            };
            let Some(trips) =
                constant(initial).filter(|trips| (1..=i32::MAX as u64).contains(trips))
            else {
                continue;
            };
            let Some((counter_mask, mut chain)) = step(next, counter.dst, false, &definitions)
            else {
                continue;
            };
            if trips > counter_mask {
                continue;
            }
            let latch_index = positions[&latch];
            let Some(&MInst::Branch {
                cond,
                true_bb,
                false_bb,
            }) = func.blocks[latch_index].terminator()
            else {
                continue;
            };
            if true_bb == false_bb || uses[cond.0 as usize] != 1 {
                continue;
            }
            let kind = match definitions[cond.0 as usize] {
                Some(MInst::CmpImm {
                    lhs, imm: 0, kind, ..
                }) if *lhs == next => *kind,
                Some(MInst::Cmp { lhs, rhs, kind, .. })
                    if *lhs == next && constant(*rhs) == Some(0)
                        || *rhs == next && constant(*lhs) == Some(0) =>
                {
                    *kind
                }
                _ => continue,
            };
            if !matches!(
                (kind, true_bb == header.id, false_bb == header.id),
                (CmpKind::Ne, true, false) | (CmpKind::Eq, false, true)
            ) {
                continue;
            }
            for index in &header.phis {
                if index.sources.len() != 2
                    || !index
                        .sources
                        .iter()
                        .any(|&(pred, source)| pred == entry && constant(source) == Some(0))
                {
                    continue;
                }
                let Some(&(_, next_index)) = index.sources.iter().find(|&&(pred, _)| pred == latch)
                else {
                    continue;
                };
                let Some((index_mask, _)) = step(next_index, index.dst, true, &definitions) else {
                    continue;
                };
                if trips > index_mask {
                    continue;
                }
                chain.push(counter.dst);
                plans.push((latch_index, cond, next_index, trips as i32, kind, chain));
                break;
            }
        }
    }
    for (latch, condition, next_index, trips, kind, chain) in plans {
        for block in &mut func.blocks {
            block.insts.retain(|inst| inst.def() != Some(condition));
        }
        let before_branch = func.blocks[latch].insts.len() - 1;
        func.blocks[latch].insts.insert(
            before_branch,
            MInst::CmpImm {
                dst: condition,
                lhs: next_index,
                imm: trips,
                kind,
            },
        );
        // The old induction variable is a dead SSA cycle. Ordinary local DCE
        // cannot collect it, so remove it only if its entire use set is private.
        let private = func.blocks.iter().all(|block| {
            block.phis.iter().all(|phi| {
                chain.contains(&phi.dst)
                    || phi
                        .sources
                        .iter()
                        .all(|(_, source)| !chain.contains(source))
            }) && block.insts.iter().all(|inst| {
                inst.def().is_some_and(|dst| chain.contains(&dst))
                    || inst
                        .uses()
                        .into_iter()
                        .all(|source| !chain.contains(&source))
            })
        });
        if private {
            for block in &mut func.blocks {
                block.phis.retain(|phi| !chain.contains(&phi.dst));
                block
                    .insts
                    .retain(|inst| inst.def().is_none_or(|dst| !chain.contains(&dst)));
            }
        }
    }
}
