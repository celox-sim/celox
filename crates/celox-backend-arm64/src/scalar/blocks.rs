//! Resolve empty jump chains after physical edge copies are known.

use super::*;

pub(super) fn forwarded_blocks(
    function: &MFunction,
    plan: &EdgeCopyPlan<BlockId>,
) -> HashMap<BlockId, BlockId> {
    let entry = function.blocks.first().map(|block| block.id);
    let jumps = function
        .blocks
        .iter()
        .filter_map(|block| {
            let Some(MInst::Jump { target }) = block.insts.last() else {
                return None;
            };
            (Some(block.id) != entry
                && block.insts[..block.insts.len() - 1]
                    .iter()
                    .all(|inst| matches!(inst, MInst::KeepAlive { .. }))
                && plan.edge(block.id, *target).is_none_or(<[_]>::is_empty))
            .then_some((block.id, *target))
        })
        .collect::<HashMap<_, _>>();
    let mut resolved = HashMap::default();
    for block in &function.blocks {
        let mut path = Vec::new();
        let mut positions = HashMap::default();
        let mut target = block.id;
        loop {
            if let Some(&canonical) = resolved.get(&target) {
                target = canonical;
                break;
            }
            let Some(&next) = jumps.get(&target) else {
                break;
            };
            if let Some(&cycle_start) = positions.get(&target) {
                // An empty cycle is still an infinite loop. Keep its blocks
                // instead of removing every possible branch destination.
                for member in path.drain(cycle_start..) {
                    resolved.insert(member, member);
                }
                break;
            }
            positions.insert(target, path.len());
            path.push(target);
            target = next;
        }
        resolved.extend(path.into_iter().map(|block| (block, target)));
    }
    resolved.retain(|block, target| block != target);
    resolved
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mir::MBlock;

    #[test]
    fn keeps_empty_cycles_and_edges_with_copies() {
        let blocks = [(0, 1), (1, 2), (2, 3), (3, 2), (4, 5), (5, 2)]
            .into_iter()
            .map(|(id, target)| {
                let mut block = MBlock::new(BlockId(id));
                block.push(MInst::Jump {
                    target: BlockId(target),
                });
                block
            })
            .collect();
        let function = MFunction::new(blocks, vec![]);
        let mut plan = EdgeCopyPlan::default();
        plan.insert(
            BlockId(5),
            BlockId(2),
            vec![CopyOperation::Move {
                destination: CopyDestination::Stack(0),
                source: CopySource::Immediate(42),
            }],
        );
        let redirects = forwarded_blocks(&function, &plan);
        assert_eq!(redirects.len(), 2);
        assert_eq!(redirects[&BlockId(1)], BlockId(2));
        assert_eq!(redirects[&BlockId(4)], BlockId(5));
    }
}
