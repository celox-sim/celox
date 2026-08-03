//! Projection from x86-owned MIR into opcode-free allocation facts.

use celox_backend_common::regalloc::{
    BlockAllocationFacts, FunctionAllocationFacts, InstructionAllocationFacts,
    InstructionConstraints, PhiAllocationFacts, PhiSource,
};

use crate::HashMap;
use crate::native::mir::{BlockId, MFunction, MInst, VReg};

use super::assignment::PhysReg;

pub(super) type ScalarAllocationFacts = FunctionAllocationFacts<VReg, PhysReg>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FactsError {
    pub block: Option<BlockId>,
    pub value: Option<VReg>,
    pub message: String,
}

/// Export scalar GPR facts from x86 MIR. Target-specific vector allocation is
/// intentionally a separate x86 pass and therefore not represented here.
pub(super) fn build(
    function: &MFunction,
    mut constraints: impl FnMut(&MInst) -> InstructionConstraints<VReg, PhysReg>,
) -> Result<ScalarAllocationFacts, FactsError> {
    let block_indices = function
        .blocks
        .iter()
        .enumerate()
        .map(|(index, block)| (block.id, index))
        .collect::<HashMap<_, _>>();
    let blocks = function
        .blocks
        .iter()
        .map(|block| {
            let successors = block
                .successors()
                .into_iter()
                .map(|successor| {
                    block_indices
                        .get(&successor)
                        .copied()
                        .ok_or_else(|| FactsError {
                            block: Some(block.id),
                            value: None,
                            message: format!("terminator targets missing block {successor}"),
                        })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let phis = block
                .phis
                .iter()
                .map(|phi| {
                    let sources = phi
                        .sources
                        .iter()
                        .map(|&(predecessor, value)| {
                            block_indices
                                .get(&predecessor)
                                .copied()
                                .map(|predecessor| PhiSource { predecessor, value })
                                .ok_or_else(|| FactsError {
                                    block: Some(block.id),
                                    value: Some(value),
                                    message: format!(
                                        "phi source names missing block {predecessor}"
                                    ),
                                })
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    Ok(PhiAllocationFacts {
                        destination: phi.dst,
                        sources,
                    })
                })
                .collect::<Result<Vec<_>, FactsError>>()?;
            let instructions = block
                .insts
                .iter()
                .map(|instruction| InstructionAllocationFacts {
                    uses: instruction.uses().into_iter().collect(),
                    defs: instruction.def().into_iter().collect(),
                    constraints: constraints(instruction),
                    is_copy: matches!(instruction, MInst::Mov { .. }),
                })
                .collect();
            Ok(BlockAllocationFacts {
                successors,
                phis,
                instructions,
            })
        })
        .collect::<Result<Vec<_>, FactsError>>()?;
    let facts = FunctionAllocationFacts { entry: 0, blocks };
    facts.verify().map_err(|error| FactsError {
        block: None,
        value: None,
        message: error.to_string(),
    })?;
    Ok(facts)
}
