//! Reuse a loop's increasing index for a redundant decreasing trip counter.

use super::*;

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
pub(super) fn index_zeros(func: &MFunction) -> Vec<(VReg, u64)> {
    let mut definitions = vec![None; func.vregs.count() as usize];
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
            let Some(&MInst::Branch {
                cond,
                true_bb,
                false_bb,
            }) = func.blocks[positions[&latch]].terminator()
            else {
                continue;
            };
            let Some(MInst::CmpImm { lhs, imm, kind, .. }) = definitions[cond.0 as usize] else {
                continue;
            };
            if *lhs != next
                || *imm <= 0
                || *imm as u64 > mask
                || !matches!(
                    (*kind, true_bb == header.id, false_bb == header.id),
                    (CmpKind::Ne, true, false) | (CmpKind::Eq, false, true)
                )
            {
                continue;
            }
            let maximum = (*imm - 1) as u64;
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

/// A load at a loop header is executed on entry. Move it to a unique jumping
/// predecessor when no loop write can change any of its physical bytes.
pub(super) fn hoist_invariant_loads(func: &mut MFunction) {
    let positions = func
        .blocks
        .iter()
        .enumerate()
        .map(|(index, block)| (block.id, index))
        .collect::<HashMap<_, _>>();
    let successors = func
        .blocks
        .iter()
        .map(|block| {
            block
                .successors()
                .into_iter()
                .map(|target| positions[&target])
                .collect()
        })
        .collect();
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
            .flat_map(|&block| func.blocks[block].insts.iter().map(memory_effect::writes))
            .filter(|effect| effect.has_effect())
            .collect::<Vec<_>>();
        let mut moved = Vec::new();
        func.blocks[region.header].insts.retain(|inst| {
            let MInst::Load {
                base, offset, size, ..
            } = *inst
            else {
                return true;
            };
            // State memory is valid for the whole native invocation. Stack
            // lifetime and runtime-owned pointer accesses stay at their sites.
            if base != BaseReg::SimState {
                return true;
            }
            let start = i64::from(offset);
            let end = start + i64::from(size.bytes());
            if writes.iter().any(|effect| {
                effect.unknown_memory() == Some(memory_effect::UnknownMemory::Direct(base))
                    || effect.ranges().any(|range| {
                        range.base == base
                            && range.offset < end
                            && range.end().is_none_or(|limit| start < limit)
                    })
            }) {
                return true;
            }
            moved.push(inst.clone());
            false
        });
        let at = func.blocks[entry].insts.len() - 1;
        func.blocks[entry].insts.splice(at..at, moved);
    }
}

/// A trip counter to replace: the latch, its exit condition, the index
/// update to compare instead, the trip count, the comparison and the
/// counter's update chain.
type Plan = (usize, VReg, VReg, i32, CmpKind, Vec<VReg>);

pub(super) fn run(func: &mut MFunction) {
    let plans = plans(func);
    apply(func, &plans);
}

fn plans(func: &MFunction) -> Vec<Plan> {
    let positions = func
        .blocks
        .iter()
        .enumerate()
        .map(|(index, block)| (block.id, index))
        .collect::<HashMap<_, _>>();
    let successors = func
        .blocks
        .iter()
        .map(|block| {
            block
                .successors()
                .into_iter()
                .map(|target| positions[&target])
                .collect()
        })
        .collect();
    let Ok(cfg) = celox_analysis::cfg::ForwardControlFlowGraph::analyze_structure(successors, 0)
    else {
        return Vec::new();
    };
    let mut definitions = vec![None; func.vregs.count() as usize];
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
    plans
}

fn apply(func: &mut MFunction, plans: &[Plan]) {
    if plans.is_empty() {
        return;
    }
    // Rewrite every plan in one pass over the function. The chains are
    // disjoint, each ending at its own counter phi, and a rewritten
    // condition or comparison never reads another plan's chain, so this
    // equals applying the plans one after another.
    let conditions = plans
        .iter()
        .map(|&(_, condition, ..)| condition)
        .collect::<HashSet<_>>();
    for block in &mut func.blocks {
        block
            .insts
            .retain(|inst| inst.def().is_none_or(|dst| !conditions.contains(&dst)));
    }
    let mut chain_of = HashMap::<VReg, usize>::default();
    for (plan, (latch, condition, next_index, trips, kind, chain)) in plans.iter().enumerate() {
        let before_branch = func.blocks[*latch].insts.len() - 1;
        func.blocks[*latch].insts.insert(
            before_branch,
            MInst::CmpImm {
                dst: *condition,
                lhs: *next_index,
                imm: *trips,
                kind: *kind,
            },
        );
        for &value in chain {
            let previous = chain_of.insert(value, plan);
            debug_assert_eq!(previous, None, "trip counter chains overlap at {value}");
        }
    }
    // The old induction variable is a dead SSA cycle. Ordinary local DCE
    // cannot collect it, so remove it only if its entire use set is private.
    let mut private = vec![true; plans.len()];
    let mut escape = |owner: Option<usize>, source: VReg| {
        if let Some(&plan) = chain_of.get(&source)
            && owner != Some(plan)
        {
            private[plan] = false;
        }
    };
    for block in &func.blocks {
        for phi in &block.phis {
            let owner = chain_of.get(&phi.dst).copied();
            for &(_, source) in &phi.sources {
                escape(owner, source);
            }
        }
        for inst in &block.insts {
            let owner = inst.def().and_then(|dst| chain_of.get(&dst).copied());
            for source in inst.uses() {
                escape(owner, source);
            }
        }
    }
    let removed = |value: VReg| chain_of.get(&value).is_some_and(|&plan| private[plan]);
    if private.contains(&true) {
        for block in &mut func.blocks {
            block.phis.retain(|phi| !removed(phi.dst));
            block
                .insts
                .retain(|inst| inst.def().is_none_or(|dst| !removed(dst)));
        }
    }
}

#[cfg(test)]
mod tests;
