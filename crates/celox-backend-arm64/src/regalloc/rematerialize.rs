//! Give constant phi inputs short ranges at their predecessor edges.

use super::*;

pub(super) fn phi_constants(
    function: &mut MFunction,
    next_value: &mut u32,
) -> Result<(), TargetRegallocError> {
    let constants = function
        .blocks
        .iter()
        .flat_map(|block| {
            block.insts.iter().enumerate().filter_map(|(index, inst)| {
                if let MInst::LoadImm { dst, value } = *inst
                    && crate::scalar::is_single_instruction_constant(value)
                {
                    Some((dst, (value, block.id, block.insts.len() - index)))
                } else {
                    None
                }
            })
        })
        .collect::<HashMap<_, _>>();
    let mut edge_values = BTreeMap::<BlockId, BTreeMap<u64, VReg>>::new();
    for block in &mut function.blocks {
        for phi in &mut block.phis {
            for (predecessor, source) in &mut phi.sources {
                let Some(&(value, definition, distance)) = constants.get(source) else {
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
            block.insts.extend(
                values
                    .into_iter()
                    .map(|(value, dst)| MInst::LoadImm { dst, value }),
            );
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
