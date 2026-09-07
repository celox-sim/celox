//! Merge adjacent diamonds that branch on the same SSA condition.
//!
//! Move the first diamond's phi nodes to the second diamond's merge, resolving
//! their uses in each arm. This avoids materializing and testing the condition
//! twice while preserving each arm's memory operations and their order.

use super::*;

#[cfg(test)]
mod tests;

struct Plan {
    parent: BlockId,
    edges: [BlockId; 2],
    join: BlockId,
    bodies: [BlockId; 2],
    tail: BlockId,
    aliases: [HashMap<VReg, VReg>; 2],
}

fn find_plan(func: &MFunction) -> Option<Plan> {
    let blocks = func
        .blocks
        .iter()
        .map(|block| (block.id, block))
        .collect::<HashMap<_, _>>();
    let mut predecessors = HashMap::<BlockId, BTreeSet<BlockId>>::default();
    for block in &func.blocks {
        for target in block.successors() {
            predecessors.entry(target).or_default().insert(block.id);
        }
    }
    let has_predecessors = |block, expected: &[BlockId]| {
        predecessors.get(&block).is_some_and(|actual| {
            actual.len() == expected.len() && expected.iter().all(|pred| actual.contains(pred))
        })
    };
    let jump = |id| match blocks[&id].terminator() {
        Some(MInst::Jump { target }) => Some(*target),
        _ => None,
    };
    func.blocks.iter().find_map(|parent| {
        let &MInst::Branch {
            cond,
            true_bb,
            false_bb,
        } = parent.terminator()?
        else {
            return None;
        };
        let edges = [true_bb, false_bb];
        if edges.iter().any(|edge| {
            let block = blocks[edge];
            !has_predecessors(*edge, &[parent.id])
                || !block.phis.is_empty()
                || block.insts.len() != 1
        }) {
            return None;
        }
        let join = jump(edges[0])?;
        if jump(edges[1])? != join || !has_predecessors(join, &edges) {
            return None;
        }
        let block = blocks[&join];
        let &[
            MInst::Branch {
                cond: next,
                true_bb,
                false_bb,
            },
        ] = block.insts.as_slice()
        else {
            return None;
        };
        if next != cond {
            return None;
        }
        let bodies = [true_bb, false_bb];
        if bodies.iter().any(|body| !has_predecessors(*body, &[join])) {
            return None;
        }
        let tail = jump(bodies[0])?;
        if jump(bodies[1])? != tail || !has_predecessors(tail, &bodies) {
            return None;
        }
        if [
            parent.id, edges[0], edges[1], join, bodies[0], bodies[1], tail,
        ]
        .into_iter()
        .collect::<HashSet<_>>()
        .len()
            != 7
        {
            return None;
        }
        let mut aliases = [HashMap::default(), HashMap::default()];
        for phi in &block.phis {
            if phi.sources.len() != 2 {
                return None;
            }
            for (arm, edge) in edges.into_iter().enumerate() {
                let source = phi
                    .sources
                    .iter()
                    .find_map(|&(pred, source)| (pred == edge).then_some(source))?;
                aliases[arm].insert(phi.dst, source);
            }
        }
        Some(Plan {
            parent: parent.id,
            edges,
            join,
            bodies,
            tail,
            aliases,
        })
    })
}

pub(super) fn run(func: &mut MFunction) {
    for _ in 0..256 {
        let Some(plan) = find_plan(func) else { break };
        let mut moved = Vec::new();
        for block in &mut func.blocks {
            if block.id == plan.parent {
                let Some(MInst::Branch {
                    true_bb, false_bb, ..
                }) = block.insts.last_mut()
                else {
                    unreachable!()
                };
                *true_bb = plan.bodies[0];
                *false_bb = plan.bodies[1];
            }
            if block.id == plan.join {
                moved = std::mem::take(&mut block.phis);
                for phi in &mut moved {
                    for (pred, _) in &mut phi.sources {
                        *pred = plan.bodies[usize::from(*pred == plan.edges[1])];
                    }
                }
            }
            if let Some(arm) = plan.bodies.iter().position(|&body| body == block.id) {
                for inst in &mut block.insts {
                    rewrite_uses(inst, &plan.aliases[arm]);
                }
                for phi in &mut block.phis {
                    for (pred, source) in &mut phi.sources {
                        *pred = plan.parent;
                        if let Some(&replacement) = plan.aliases[arm].get(source) {
                            *source = replacement;
                        }
                    }
                }
            }
            if block.id == plan.tail {
                for phi in &mut block.phis {
                    for (pred, source) in &mut phi.sources {
                        let arm = usize::from(*pred == plan.bodies[1]);
                        if let Some(&replacement) = plan.aliases[arm].get(source) {
                            *source = replacement;
                        }
                    }
                }
            }
        }
        func.blocks
            .iter_mut()
            .find(|block| block.id == plan.tail)
            .unwrap()
            .phis
            .extend(moved);
        func.blocks
            .retain(|block| block.id != plan.join && !plan.edges.contains(&block.id));
    }
}
