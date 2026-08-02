//! Transitional adapter from the established x86 allocator result.
//!
//! No AArch64 emitter code should interpret x86 register colors directly.
//! This module is deleted once the production AArch64 MIR uses its own target
//! driver with the shared opcode-free allocation facts.

use celox_backend_x86::native::mir::{BlockId, MFunction, VReg};
use celox_backend_x86::native::regalloc::AssignmentMap as LegacyAssignment;
use celox_backend_x86::native::regalloc::assignment::PhysReg;
use celox_backend_x86::native::ssa_destroy::{
    ParallelCopyDestination, ParallelCopyOperation, ParallelCopySource, SsaDestructionError,
    SsaDestructionPlan,
};

use crate::Arm64Reg;
use crate::allocation::{Assignment, CopyDestination, CopyOperation, CopySource, EdgeCopyPlan};

pub(crate) fn adapt(
    function: &MFunction,
    assignment: &LegacyAssignment,
    spill_frame_size: u32,
) -> Result<(Assignment<VReg>, EdgeCopyPlan<BlockId>), SsaDestructionError> {
    let legacy_plan = SsaDestructionPlan::build(function, assignment)?;
    legacy_plan.verify(function, assignment, spill_frame_size)?;

    let mut target_assignment = Assignment::default();
    for (value, register) in assignment.sorted_entries() {
        target_assignment.set(value, adapt_register(register));
    }

    let mut target_plan = EdgeCopyPlan::default();
    for block in &function.blocks {
        for successor in block.successors() {
            let Some(edge) = legacy_plan.edge(block.id, successor) else {
                continue;
            };
            target_plan.insert(
                block.id,
                successor,
                edge.operations.iter().copied().map(adapt_copy).collect(),
            );
        }
    }
    Ok((target_assignment, target_plan))
}

fn adapt_register(register: PhysReg) -> Arm64Reg {
    match register {
        PhysReg::RAX => Arm64Reg::new(1),
        PhysReg::RCX => Arm64Reg::new(2),
        PhysReg::RDX => Arm64Reg::new(3),
        PhysReg::RBX => Arm64Reg::new(4),
        PhysReg::RBP => Arm64Reg::new(5),
        PhysReg::RSI => Arm64Reg::new(6),
        PhysReg::RDI => Arm64Reg::new(7),
        PhysReg::R8 => Arm64Reg::new(8),
        PhysReg::R9 => Arm64Reg::new(9),
        PhysReg::R10 => Arm64Reg::new(10),
        PhysReg::R11 => Arm64Reg::new(11),
        PhysReg::R12 => Arm64Reg::new(12),
        PhysReg::R13 => Arm64Reg::new(13),
        PhysReg::R14 => Arm64Reg::new(14),
        PhysReg::R15 => Arm64Reg::new(15),
    }
}

fn adapt_copy(operation: ParallelCopyOperation) -> CopyOperation {
    match operation {
        ParallelCopyOperation::Move {
            destination,
            source,
        } => CopyOperation::Move {
            destination: adapt_destination(destination),
            source: adapt_source(source),
        },
        ParallelCopyOperation::SwapRegisters { left, right } => CopyOperation::SwapRegisters {
            left: adapt_register(left),
            right: adapt_register(right),
        },
        ParallelCopyOperation::SaveTemporary(destination) => {
            CopyOperation::SaveTemporary(adapt_destination(destination))
        }
        ParallelCopyOperation::RestoreTemporary(destination) => {
            CopyOperation::RestoreTemporary(adapt_destination(destination))
        }
    }
}

fn adapt_destination(destination: ParallelCopyDestination) -> CopyDestination {
    match destination {
        ParallelCopyDestination::Register(register) => {
            CopyDestination::Register(adapt_register(register))
        }
        ParallelCopyDestination::Stack(offset) => CopyDestination::Stack(offset),
    }
}

fn adapt_source(source: ParallelCopySource) -> CopySource {
    match source {
        ParallelCopySource::Register(register) => CopySource::Register(adapt_register(register)),
        ParallelCopySource::Stack(offset) => CopySource::Stack(offset),
        ParallelCopySource::Immediate(value) => CopySource::Immediate(value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_colors_map_to_distinct_non_reserved_aarch64_registers() {
        let colors = [
            PhysReg::RAX,
            PhysReg::RCX,
            PhysReg::RDX,
            PhysReg::RBX,
            PhysReg::RBP,
            PhysReg::RSI,
            PhysReg::RDI,
            PhysReg::R8,
            PhysReg::R9,
            PhysReg::R10,
            PhysReg::R11,
            PhysReg::R12,
            PhysReg::R13,
            PhysReg::R14,
            PhysReg::R15,
        ];
        let mut registers = colors.map(adapt_register).map(Arm64Reg::number);
        registers.sort_unstable();

        assert_eq!(
            registers,
            [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]
        );
        assert!(
            !registers.contains(&0),
            "x0 is the simulator-state register"
        );
        assert!(
            !registers.contains(&16),
            "x16 is an emitter scratch register"
        );
        assert!(
            !registers.contains(&17),
            "x17 is an emitter scratch register"
        );
    }
}
