//! Give phi inputs short ranges at their predecessor edges.

use super::*;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum EdgeInput {
    Constant(u64),
    Value(VReg),
}

pub(super) fn localize_phi_inputs(
    function: &mut MFunction,
    next_value: &mut u32,
) -> Result<(), TargetRegallocError> {
    let definitions = function
        .blocks
        .iter()
        .flat_map(|block| {
            block
                .phis
                .iter()
                .map(|phi| {
                    (
                        phi.dst,
                        (EdgeInput::Value(phi.dst), block.id, block.insts.len() + 1),
                    )
                })
                .chain(block.insts.iter().enumerate().filter_map(|(index, inst)| {
                    let dst = inst.def()?;
                    let input = match *inst {
                        MInst::LoadImm { value, .. }
                            if crate::scalar::is_single_instruction_constant(value) =>
                        {
                            EdgeInput::Constant(value)
                        }
                        _ => EdgeInput::Value(dst),
                    };
                    Some((dst, (input, block.id, block.insts.len() - index)))
                }))
        })
        .collect::<HashMap<_, _>>();
    let mut edge_values = BTreeMap::<BlockId, BTreeMap<EdgeInput, VReg>>::new();
    for block in &mut function.blocks {
        for phi in &mut block.phis {
            for (predecessor, source) in &mut phi.sources {
                let Some(&(value, definition, distance)) = definitions.get(source) else {
                    continue;
                };
                if definition == *predecessor && distance <= 8 {
                    continue;
                }
                let values = edge_values.entry(*predecessor).or_default();
                let replacement = match values.entry(value) {
                    std::collections::btree_map::Entry::Occupied(entry) => *entry.get(),
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        let replacement = VReg(*next_value);
                        *next_value = next_value
                            .checked_add(1)
                            .ok_or(TargetRegallocError::SpillFrameOverflow)?;
                        *entry.insert(replacement)
                    }
                };
                *source = replacement;
            }
        }
    }
    if edge_values.is_empty() {
        return Ok(());
    }
    for block in &mut function.blocks {
        if let Some(values) = edge_values.remove(&block.id) {
            let terminator = block.insts.pop().expect("phi predecessor has a terminator");
            block
                .insts
                .extend(values.into_iter().map(|(value, dst)| match value {
                    EdgeInput::Constant(value) => MInst::LoadImm { dst, value },
                    EdgeInput::Value(src) => MInst::Mov { dst, src },
                }));
            block.insts.push(terminator);
        }
    }
    let used = function
        .blocks
        .iter()
        .flat_map(|block| {
            block
                .phis
                .iter()
                .flat_map(|phi| phi.sources.iter().map(|&(_, value)| value))
                .chain(block.insts.iter().flat_map(MInst::uses))
        })
        .collect::<BTreeSet<_>>();
    for block in &mut function.blocks {
        block
            .insts
            .retain(|inst| !matches!(inst, MInst::LoadImm { dst, .. } if !used.contains(dst)));
    }
    Ok(())
}
