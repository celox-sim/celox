//! Independent pressure proof after spill-plan materialization.

use std::collections::BTreeSet;
use std::fmt;

use crate::backend::native::mir::{BlockId, MFunction, VReg};

use super::analysis::AnalysisResult;
use super::assignment::{RegConstraint, clobbers, use_constraints};

#[derive(Debug)]
pub(super) struct PressureError {
    pub block: BlockId,
    pub instruction: usize,
    pub pressure: usize,
    pub capacity: usize,
    pub reason: &'static str,
    pub instruction_text: String,
}

impl fmt::Display for PressureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "pressure verify at {}/i{}: {} pressure {} exceeds capacity {} ({})",
            self.block,
            self.instruction,
            self.reason,
            self.pressure,
            self.capacity,
            self.instruction_text
        )
    }
}

pub(super) fn verify(
    func: &MFunction,
    analysis: &AnalysisResult,
    registers: usize,
) -> Result<(), PressureError> {
    for (block_index, block) in func.blocks.iter().enumerate() {
        let mut live_after = analysis.exit_distances[block_index]
            .keys()
            .copied()
            .collect::<BTreeSet<_>>();
        check(
            block.id,
            block.insts.len(),
            live_after.len(),
            registers,
            "edge",
            "block exit".into(),
        )?;
        for (instruction, inst) in block.insts.iter().enumerate().rev() {
            let mut live_before = live_after.clone();
            if let Some(definition) = inst.def() {
                live_before.remove(&definition);
            }
            live_before.extend(inst.uses());
            check(
                block.id,
                instruction,
                live_before.len(),
                registers,
                "general-register",
                inst.to_string(),
            )?;

            let fixed = inst
                .uses()
                .into_iter()
                .zip(use_constraints(inst))
                .filter_map(|(value, constraint)| {
                    matches!(constraint, RegConstraint::Fixed(_)).then_some(value)
                })
                .collect::<BTreeSet<VReg>>();
            if !fixed.is_empty() {
                let ordinary = live_before.difference(&fixed).count();
                check(
                    block.id,
                    instruction,
                    ordinary,
                    registers - fixed.len(),
                    "fixed-register reservation",
                    inst.to_string(),
                )?;
            }

            let clobbered = clobbers(inst).len();
            if clobbered != 0 {
                let live_through = live_before.intersection(&live_after).count();
                check(
                    block.id,
                    instruction,
                    live_through,
                    registers - clobbered,
                    "clobber reservation",
                    inst.to_string(),
                )?;
            }
            live_after = live_before;
        }
    }
    Ok(())
}

fn check(
    block: BlockId,
    instruction: usize,
    pressure: usize,
    capacity: usize,
    reason: &'static str,
    instruction_text: String,
) -> Result<(), PressureError> {
    if pressure <= capacity {
        Ok(())
    } else {
        Err(PressureError {
            block,
            instruction,
            pressure,
            capacity,
            reason,
            instruction_text,
        })
    }
}
