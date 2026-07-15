//! Independent verification of a completed physical assignment.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::backend::native::mir::{BlockId, MFunction, VReg};

use super::analysis::AnalysisResult;
use super::assignment::{
    AssignmentMap, EdgeLocation, PhysReg, RegConstraint, clobbers, use_constraints,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllocationError {
    pub block: BlockId,
    pub instruction: Option<usize>,
    pub message: String,
}

impl fmt::Display for AllocationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "regalloc verify at {}", self.block)?;
        if let Some(index) = self.instruction {
            write!(f, "/i{index}")?;
        }
        write!(f, ": {}", self.message)
    }
}

impl std::error::Error for AllocationError {}

pub(super) fn verify(
    func: &MFunction,
    analysis: &AnalysisResult,
    assignment: &AssignmentMap,
) -> Result<(), AllocationError> {
    for (block_index, block) in func.blocks.iter().enumerate() {
        // The test-only legacy allocator can represent a value coupled through
        // memory by omitting all local references to its original VReg; a fresh
        // reload is referenced instead. Such a value remains dataflow-live but
        // its stale global register is not resident here.
        let mut locally_resident = block
            .insts
            .iter()
            .flat_map(|inst| inst.uses())
            .collect::<BTreeSet<_>>();
        locally_resident.extend(block.insts.iter().filter_map(|inst| inst.def()));
        locally_resident.extend(block.phis.iter().map(|phi| phi.dst));
        for phi in &block.phis {
            require_location(block.id, None, phi.dst, assignment)?;
            for &(pred, source) in &phi.sources {
                if assignment.edge_location(pred, source).is_none()
                    && assignment.get(source).is_none()
                    && assignment.edge_spill_slot(source).is_none()
                {
                    return Err(error(
                        block.id,
                        None,
                        format!("phi source {source} from {pred} has no edge location"),
                    ));
                }
            }
        }

        let mut live_after = analysis.exit_distances[block_index]
            .keys()
            .copied()
            .collect::<BTreeSet<_>>();
        check_unique(
            block.id,
            block.insts.len(),
            &live_after,
            &locally_resident,
            assignment,
        )?;

        for (index, inst) in block.insts.iter().enumerate().rev() {
            if let Some(def) = inst.def() {
                require_location(block.id, Some(index), def, assignment)?;
            }
            for used in inst.uses() {
                require_location(block.id, Some(index), used, assignment)?;
            }

            let mut live_before = live_after.clone();
            if let Some(def) = inst.def() {
                live_before.remove(&def);
            }
            live_before.extend(inst.uses());
            check_unique(block.id, index, &live_before, &locally_resident, assignment)?;

            let constraints = use_constraints(inst, func.target_features.variable_shift_encoding());
            for (used, constraint) in inst.uses().into_iter().zip(constraints) {
                if let RegConstraint::Fixed(required) = constraint {
                    let actual = assignment.get(used);
                    if actual != Some(required) {
                        return Err(error(
                            block.id,
                            Some(index),
                            format!("fixed use {used} occupies {actual:?}, requires {required}"),
                        ));
                    }
                }
            }

            if !clobbers(inst).is_empty() {
                for value in live_before.intersection(&live_after) {
                    if let Some(reg) = assignment.get(*value)
                        && clobbers(inst).contains(&reg)
                    {
                        return Err(error(
                            block.id,
                            Some(index),
                            format!("{value} is live across {inst} in clobbered {reg}"),
                        ));
                    }
                }
            }
            live_after = live_before;
        }

        // Phi definitions replace their incoming edge values simultaneously.
        // They are live at the first instruction but not before block entry.
        check_unique(block.id, 0, &live_after, &locally_resident, assignment)?;
    }

    verify_edge_locations(func, assignment)
}

fn require_location(
    block: BlockId,
    instruction: Option<usize>,
    value: VReg,
    assignment: &AssignmentMap,
) -> Result<(), AllocationError> {
    if assignment.get(value).is_some() || assignment.edge_spill_slot(value).is_some() {
        Ok(())
    } else {
        Err(error(
            block,
            instruction,
            format!("{value} has no physical assignment or spill home"),
        ))
    }
}

fn check_unique(
    block: BlockId,
    program_point: usize,
    live: &BTreeSet<VReg>,
    locally_resident: &BTreeSet<VReg>,
    assignment: &AssignmentMap,
) -> Result<(), AllocationError> {
    let mut occupied = BTreeMap::<PhysReg, VReg>::new();
    for &value in live {
        if !locally_resident.contains(&value)
            && assignment
                .edge_location_at(block, value, program_point)
                .is_none()
        {
            continue;
        }
        let location = assignment
            .edge_location_at(block, value, program_point)
            .or_else(|| assignment.get(value).map(EdgeLocation::Register));
        let Some(EdgeLocation::Register(reg)) = location else {
            continue;
        };
        if let Some(other) = occupied.insert(reg, value)
            && other != value
        {
            return Err(error(
                block,
                Some(program_point),
                format!("simultaneously live {other} and {value} both occupy {reg}"),
            ));
        }
    }
    Ok(())
}

fn verify_edge_locations(
    func: &MFunction,
    assignment: &AssignmentMap,
) -> Result<(), AllocationError> {
    for successor in &func.blocks {
        let mut by_pred = BTreeMap::<BlockId, Vec<VReg>>::new();
        for phi in &successor.phis {
            for &(pred, source) in &phi.sources {
                by_pred.entry(pred).or_default().push(source);
            }
        }
        for (pred, sources) in by_pred {
            let mut registers = BTreeMap::<PhysReg, VReg>::new();
            for source in sources {
                let location = assignment
                    .edge_location(pred, source)
                    .or_else(|| assignment.get(source).map(EdgeLocation::Register))
                    .or_else(|| assignment.edge_spill_slot(source).map(EdgeLocation::Stack));
                if let Some(EdgeLocation::Register(reg)) = location
                    && let Some(other) = registers.insert(reg, source)
                    && other != source
                {
                    return Err(error(
                        successor.id,
                        None,
                        format!(
                            "phi edge from {pred} needs distinct {other} and {source} in {reg}"
                        ),
                    ));
                }
            }
        }
    }
    Ok(())
}

fn error(block: BlockId, instruction: Option<usize>, message: String) -> AllocationError {
    AllocationError {
        block,
        instruction,
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::native::features::X86Features;
    use crate::backend::native::mir::{BaseReg, MBlock, MInst, OpSize, SpillDesc, VRegAllocator};

    fn shift_function(bmi2: bool) -> MFunction {
        let mut vregs = VRegAllocator::new();
        let lhs = vregs.alloc();
        let count = vregs.alloc();
        let result = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 3]);
        func.target_features = X86Features::for_test(bmi2);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::LoadImm { dst: lhs, value: 8 });
        block.push(MInst::LoadImm {
            dst: count,
            value: 1,
        });
        block.push(MInst::Shl {
            dst: result,
            lhs,
            rhs: count,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 0,
            src: result,
            size: OpSize::S64,
        });
        block.push(MInst::Return);
        func.push_block(block);
        func
    }

    fn shift_assignment(count: PhysReg) -> AssignmentMap {
        let mut assignment = AssignmentMap::default();
        assignment.set(VReg(0), PhysReg::RAX);
        assignment.set(VReg(1), count);
        // The lhs dies at the shift, so the result may reuse its register.
        assignment.set(VReg(2), PhysReg::RAX);
        assignment
    }

    #[test]
    fn bmi2_shift_accepts_a_count_outside_rcx() {
        let func = shift_function(true);
        let analysis = super::super::analysis::analyze(&func);

        verify(&func, &analysis, &shift_assignment(PhysReg::RDX)).unwrap();
    }

    #[test]
    fn legacy_shift_requires_rcx_for_the_count() {
        let func = shift_function(false);
        let analysis = super::super::analysis::analyze(&func);

        let error = verify(&func, &analysis, &shift_assignment(PhysReg::RDX)).unwrap_err();
        assert!(error.message.contains("requires rcx"), "{error}");
        verify(&func, &analysis, &shift_assignment(PhysReg::RCX)).unwrap();
    }
}
