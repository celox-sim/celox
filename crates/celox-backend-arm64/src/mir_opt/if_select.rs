//! Replace empty selection arms with AArch64 conditional selects.

use super::*;
use crate::mir::{BlockId, MBlock};

struct Plan {
    head: usize,
    join: usize,
    arms: Vec<BlockId>,
    selects: Vec<MInst>,
}

fn plan(
    head: usize,
    blocks: &[MBlock],
    positions: &HashMap<BlockId, usize>,
    predecessors: &[BTreeSet<BlockId>],
) -> Option<Plan> {
    let block = &blocks[head];
    let (true_bb, false_bb) = match *block.terminator()? {
        MInst::Branch {
            true_bb, false_bb, ..
        }
        | MInst::BranchPred {
            true_bb, false_bb, ..
        } => (true_bb, false_bb),
        _ => return None,
    };
    if true_bb == false_bb {
        return None;
    }
    let empty_arm = |id: BlockId| {
        let position = *positions.get(&id)?;
        let arm = &blocks[position];
        if position == 0
            || !arm.phis.is_empty()
            || predecessors[position] != BTreeSet::from([block.id])
        {
            return None;
        }
        match arm.insts.as_slice() {
            [MInst::Jump { target }] => Some(*target),
            _ => None,
        }
    };
    let (join_id, true_pred, false_pred, arms) = match (empty_arm(true_bb), empty_arm(false_bb)) {
        (Some(left), Some(right)) if left == right => {
            (left, true_bb, false_bb, vec![true_bb, false_bb])
        }
        (Some(join), _) if join == false_bb => (join, true_bb, block.id, vec![true_bb]),
        (_, Some(join)) if join == true_bb => (join, block.id, false_bb, vec![false_bb]),
        _ => return None,
    };
    let join = *positions.get(&join_id)?;
    if join == 0
        || join == head
        || arms.contains(&join_id)
        || predecessors[join] != BTreeSet::from([true_pred, false_pred])
    {
        return None;
    }
    let selects = blocks[join]
        .phis
        .iter()
        .map(|phi| {
            if phi.sources.len() != 2 {
                return None;
            }
            let true_val = phi.sources.iter().find(|(pred, _)| *pred == true_pred)?.1;
            let false_val = phi.sources.iter().find(|(pred, _)| *pred == false_pred)?.1;
            let dst = phi.dst;
            if true_val == false_val {
                return Some(MInst::Mov { dst, src: true_val });
            }
            Some(match *block.terminator()? {
                MInst::Branch { cond, .. } => MInst::Select {
                    dst,
                    cond,
                    true_val,
                    false_val,
                },
                MInst::BranchPred {
                    predicate: BranchPredicate::Compare { lhs, rhs, kind },
                    ..
                } => MInst::CmpSelect {
                    dst,
                    lhs,
                    rhs,
                    kind,
                    true_val,
                    false_val,
                },
                MInst::BranchPred {
                    predicate: BranchPredicate::CompareImm { lhs, imm, kind },
                    ..
                } => MInst::CmpImmSelect {
                    dst,
                    lhs,
                    imm,
                    kind,
                    true_val,
                    false_val,
                },
                // A memory predicate must perform its load exactly once even
                // when no phi needs a selection. Keep that branch intact.
                _ => return None,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    if matches!(
        block.terminator(),
        Some(MInst::BranchPred {
            predicate: BranchPredicate::MemoryNonZero { .. },
            ..
        })
    ) {
        return None;
    }
    Some(Plan {
        head,
        join,
        arms,
        selects,
    })
}

pub(crate) fn run(function: &mut MFunction) {
    let positions = function
        .blocks
        .iter()
        .enumerate()
        .map(|(index, block)| (block.id, index))
        .collect::<HashMap<_, _>>();
    let mut predecessors = vec![BTreeSet::new(); function.blocks.len()];
    for block in &function.blocks {
        for successor in block.successors() {
            let Some(&position) = positions.get(&successor) else {
                return;
            };
            predecessors[position].insert(block.id);
        }
    }
    let mut claimed = BTreeSet::new();
    let mut plans = Vec::new();
    for head in 0..function.blocks.len() {
        let Some(plan) = plan(head, &function.blocks, &positions, &predecessors) else {
            continue;
        };
        let ids = [function.blocks[head].id, function.blocks[plan.join].id]
            .into_iter()
            .chain(plan.arms.iter().copied())
            .collect::<Vec<_>>();
        if ids.iter().any(|id| claimed.contains(id)) {
            continue;
        }
        claimed.extend(ids);
        plans.push(plan);
    }
    let mut removed = BTreeSet::new();
    for plan in plans {
        let target = function.blocks[plan.join].id;
        let head = &mut function.blocks[plan.head];
        head.insts.pop();
        head.insts.extend(plan.selects);
        head.push(MInst::Jump { target });
        function.blocks[plan.join].phis.clear();
        removed.extend(plan.arms);
    }
    function.blocks.retain(|block| !removed.contains(&block.id));
}
