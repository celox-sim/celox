//! Verified phase ownership for a composed clock-event SIR CFG.

use std::collections::HashSet;

use crate::ir::{BlockId, ExecutionUnit, SIRTerminator, SirMergeProvenance};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EventPhase {
    Comb,
    Ff,
}

/// Explicit phase cut retained by the fused clock-event path.
///
/// The complete per-block map is intentionally kept even though the current
/// backend consumes only `ff_entry`: Reactive SSA projection will use the same
/// verified ownership instead of reconstructing it from block numbering.
#[derive(Debug)]
pub(crate) struct FusedPhaseCut {
    ff_entry: BlockId,
    phase_by_block: crate::HashMap<BlockId, EventPhase>,
}

impl FusedPhaseCut {
    pub(crate) const fn ff_entry(&self) -> BlockId {
        self.ff_entry
    }

    pub(crate) fn ff_block_count(&self) -> usize {
        self.phase_by_block
            .values()
            .filter(|&&phase| phase == EventPhase::Ff)
            .count()
    }

    pub(crate) fn is_ff_block(&self, block: BlockId) -> bool {
        self.phase_by_block.get(&block) == Some(&EventPhase::Ff)
    }
}

pub(crate) fn verify<A>(
    eu: &ExecutionUnit<A>,
    provenance: &SirMergeProvenance,
    first_ff_unit: usize,
) -> Result<FusedPhaseCut, String> {
    if first_ff_unit >= provenance.unit_entries.len() {
        return Err(format!(
            "first FF unit {first_ff_unit} is outside {} merged units",
            provenance.unit_entries.len()
        ));
    }
    if provenance.block_units.len() != eu.blocks.len()
        || eu
            .blocks
            .keys()
            .any(|block| !provenance.block_units.contains_key(block))
    {
        return Err("merged block provenance does not cover the SIR CFG exactly".into());
    }

    let ff_entry = provenance.unit_entries[first_ff_unit];
    if !eu.blocks.contains_key(&ff_entry) {
        return Err(format!("FF entry b{} does not exist", ff_entry.0));
    }

    let phase_by_block = provenance
        .block_units
        .iter()
        .map(|(&block, &unit)| {
            (
                block,
                if unit < first_ff_unit {
                    EventPhase::Comb
                } else {
                    EventPhase::Ff
                },
            )
        })
        .collect::<crate::HashMap<_, _>>();

    for (&source, block) in &eu.blocks {
        let source_phase = phase_by_block[&source];
        for target in successors(&block.terminator) {
            let Some(&target_phase) = phase_by_block.get(&target) else {
                return Err(format!(
                    "phase edge b{} -> b{} targets a block without provenance",
                    source.0, target.0
                ));
            };
            if source_phase == EventPhase::Ff && target_phase == EventPhase::Comb {
                return Err(format!(
                    "FF phase edge b{} -> b{} returns to combinational code",
                    source.0, target.0
                ));
            }
            if source_phase == EventPhase::Comb
                && target_phase == EventPhase::Ff
                && target != ff_entry
            {
                return Err(format!(
                    "comb phase edge b{} enters FF phase through b{} instead of b{}",
                    source.0, target.0, ff_entry.0
                ));
            }
        }
    }

    let mut reachable = HashSet::new();
    let mut work = vec![ff_entry];
    while let Some(block) = work.pop() {
        if !reachable.insert(block) {
            continue;
        }
        for successor in successors(&eu.blocks[&block].terminator) {
            if phase_by_block[&successor] == EventPhase::Ff {
                work.push(successor);
            }
        }
    }
    if let Some(block) = phase_by_block.iter().find_map(|(&block, &phase)| {
        (phase == EventPhase::Ff && !reachable.contains(&block)).then_some(block)
    }) {
        return Err(format!(
            "FF-owned block b{} is unreachable from FF entry b{}",
            block.0, ff_entry.0
        ));
    }

    Ok(FusedPhaseCut {
        ff_entry,
        phase_by_block,
    })
}

fn successors(terminator: &SIRTerminator) -> Vec<BlockId> {
    match terminator {
        SIRTerminator::Jump(target, _) => vec![*target],
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{BasicBlock, RegisterId, RegisterType};

    fn unit(entry: usize, blocks: &[(usize, SIRTerminator)]) -> ExecutionUnit<()> {
        ExecutionUnit {
            entry_block_id: BlockId(entry),
            blocks: blocks
                .iter()
                .map(|(id, terminator)| {
                    (
                        BlockId(*id),
                        BasicBlock {
                            id: BlockId(*id),
                            params: Vec::new(),
                            instructions: Vec::new(),
                            terminator: terminator.clone(),
                        },
                    )
                })
                .collect(),
            register_map: crate::HashMap::<RegisterId, RegisterType>::default(),
        }
    }

    #[test]
    fn phase_cut_uses_source_provenance_not_block_number_or_entry_zero() {
        let comb = unit(7, &[(7, SIRTerminator::Return)]);
        let ff = unit(
            9,
            &[
                (3, SIRTerminator::Return),
                (9, SIRTerminator::Jump(BlockId(3), Vec::new())),
            ],
        );
        let (merged, provenance) = crate::ir::merge_sir_eu_refs_with_provenance(&[&comb, &ff]);

        let cut = verify(&merged, &provenance, 1).unwrap();

        assert_eq!(cut.ff_entry(), provenance.unit_entries[1]);
        assert_eq!(cut.ff_block_count(), 2);
    }

    #[test]
    fn phase_cut_rejects_an_ff_edge_back_to_comb() {
        let comb = unit(0, &[(0, SIRTerminator::Return)]);
        let ff = unit(0, &[(0, SIRTerminator::Return)]);
        let (mut merged, provenance) = crate::ir::merge_sir_eu_refs_with_provenance(&[&comb, &ff]);
        let ff_entry = provenance.unit_entries[1];
        merged.blocks.get_mut(&ff_entry).unwrap().terminator =
            SIRTerminator::Jump(provenance.unit_entries[0], Vec::new());

        let error = verify(&merged, &provenance, 1).unwrap_err();

        assert!(error.contains("returns to combinational code"));
    }
}
