//! Spill-free SSA coloring.
//!
//! This is the first production slice of the replacement allocator.  It
//! colors functions whose pressure already fits the machine.  Failure is
//! side-effect free, so the caller can temporarily route high-pressure
//! functions through the legacy spilling implementation.

use std::collections::{BTreeSet, HashMap};

use crate::backend::native::mir::{BlockId, MFunction, VReg};

use super::analysis::AnalysisResult;
use super::assignment::{
    ALLOCATABLE_REGS, AssignmentMap, EdgeLocation, PhysReg, RegConstraint, clobbers,
    use_constraints,
};

#[derive(Debug)]
pub(super) struct ColorFailure {
    pub block: BlockId,
    pub value: VReg,
}

/// Color an SSA function without changing it.
pub(super) fn try_color(
    func: &MFunction,
    analysis: &AnalysisResult,
) -> Result<AssignmentMap, ColorFailure> {
    let live = compute_program_point_liveness(func, analysis);
    let mut forbidden = compute_forbidden_registers(func, &live);
    let mut result = AssignmentMap::default();

    // Fixed values are legalized to one-use SSA copies before this phase.
    // Precolor them first so ordinary values can see the constraint even when
    // their definitions dominate the fixed copy.
    for block in &func.blocks {
        for inst in &block.insts {
            for (value, constraint) in inst.uses().into_iter().zip(use_constraints(inst)) {
                if let RegConstraint::Fixed(required) = constraint {
                    if let Some(previous) = result.get(value) {
                        if previous != required {
                            return Err(ColorFailure {
                                block: block.id,
                                value,
                            });
                        }
                    } else {
                        result.set(value, required);
                    }
                }
            }
        }
    }

    for (block_index, block) in func.blocks.iter().enumerate() {
        let entry_live = live[block_index].first().cloned().unwrap_or_default();
        let mut phi_dsts = BTreeSet::new();
        for phi in &block.phis {
            phi_dsts.insert(phi.dst);
            color_value(
                block.id,
                phi.dst,
                entry_live
                    .iter()
                    .copied()
                    .chain(phi_dsts.iter().copied())
                    .filter(|value| *value != phi.dst),
                &mut forbidden,
                &mut result,
            )?;
        }

        for (index, inst) in block.insts.iter().enumerate() {
            for used in inst.uses() {
                if result.get(used).is_none() {
                    // A strict SSA use is dominated by a definition.  Missing
                    // color here means the function/order contract is broken.
                    return Err(ColorFailure {
                        block: block.id,
                        value: used,
                    });
                }
            }
            if let Some(def) = inst.def() {
                let live_after = &live[block_index][index + 1];
                color_value(
                    block.id,
                    def,
                    live_after.iter().copied().filter(|value| *value != def),
                    &mut forbidden,
                    &mut result,
                )?;
            }
        }
    }

    for successor in &func.blocks {
        for phi in &successor.phis {
            for &(pred, source) in &phi.sources {
                let Some(reg) = result.get(source) else {
                    return Err(ColorFailure {
                        block: successor.id,
                        value: source,
                    });
                };
                result.set_edge_location(pred, source, EdgeLocation::Register(reg));
            }
        }
    }
    Ok(result)
}

fn color_value(
    block: BlockId,
    value: VReg,
    interfering: impl Iterator<Item = VReg>,
    forbidden: &mut HashMap<VReg, BTreeSet<PhysReg>>,
    result: &mut AssignmentMap,
) -> Result<(), ColorFailure> {
    if result.get(value).is_some() {
        return Ok(());
    }
    let mut unavailable = forbidden.remove(&value).unwrap_or_default();
    unavailable.extend(interfering.filter_map(|other| result.get(other)));
    let Some(reg) = ALLOCATABLE_REGS
        .iter()
        .copied()
        .find(|reg| !unavailable.contains(reg))
    else {
        return Err(ColorFailure { block, value });
    };
    result.set(value, reg);
    Ok(())
}

/// `points[block][i]` is the live set before instruction `i`; the final entry
/// is the set after the block terminator.
fn compute_program_point_liveness(
    func: &MFunction,
    analysis: &AnalysisResult,
) -> Vec<Vec<BTreeSet<VReg>>> {
    func.blocks
        .iter()
        .enumerate()
        .map(|(block_index, block)| {
            let mut points = vec![BTreeSet::new(); block.insts.len() + 1];
            let mut live = analysis.exit_distances[block_index]
                .keys()
                .copied()
                .collect::<BTreeSet<_>>();
            points[block.insts.len()] = live.clone();
            for (index, inst) in block.insts.iter().enumerate().rev() {
                if let Some(def) = inst.def() {
                    live.remove(&def);
                }
                live.extend(inst.uses());
                points[index] = live.clone();
            }
            points
        })
        .collect()
}

fn compute_forbidden_registers(
    func: &MFunction,
    live: &[Vec<BTreeSet<VReg>>],
) -> HashMap<VReg, BTreeSet<PhysReg>> {
    let mut forbidden = HashMap::<VReg, BTreeSet<PhysReg>>::new();
    for (block_index, block) in func.blocks.iter().enumerate() {
        for (index, inst) in block.insts.iter().enumerate() {
            let before = &live[block_index][index];
            let after = &live[block_index][index + 1];
            for (fixed, constraint) in inst.uses().into_iter().zip(use_constraints(inst)) {
                let RegConstraint::Fixed(required) = constraint else {
                    continue;
                };
                for &value in before.union(after) {
                    if value != fixed {
                        forbidden.entry(value).or_default().insert(required);
                    }
                }
            }
            for &clobbered in clobbers(inst) {
                for &value in before.intersection(after) {
                    forbidden.entry(value).or_default().insert(clobbered);
                }
            }
        }
    }
    forbidden
}

#[cfg(test)]
mod tests {
    use crate::backend::native::mir::{MBlock, MFunction, MInst, SpillDesc, VRegAllocator};

    use super::*;

    #[test]
    fn colors_overlapping_ssa_values_differently() {
        let mut vregs = VRegAllocator::new();
        let a = vregs.alloc();
        let b = vregs.alloc();
        let sum = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 3]);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::LoadImm { dst: a, value: 1 });
        block.push(MInst::LoadImm { dst: b, value: 2 });
        block.push(MInst::Add {
            dst: sum,
            lhs: a,
            rhs: b,
        });
        block.push(MInst::Return);
        func.push_block(block);
        let analysis = super::super::analysis::analyze(&func);
        let assignment = try_color(&func, &analysis).unwrap();
        assert_ne!(assignment.get(a), assignment.get(b));
        assert!(assignment.get(sum).is_some());
    }

    #[test]
    fn reserves_fixed_register_for_short_lived_copy() {
        let mut vregs = VRegAllocator::new();
        let lhs = vregs.alloc();
        let amount = vregs.alloc();
        let shifted = vregs.alloc();
        let later = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 4]);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::LoadImm { dst: lhs, value: 8 });
        block.push(MInst::LoadImm {
            dst: amount,
            value: 1,
        });
        block.push(MInst::Shl {
            dst: shifted,
            lhs,
            rhs: amount,
        });
        block.push(MInst::Add {
            dst: later,
            lhs,
            rhs: shifted,
        });
        block.push(MInst::Return);
        func.push_block(block);
        super::super::legalize::isolate_fixed_uses(&mut func);
        let fixed = match &func.blocks[0].insts[2] {
            MInst::Mov { dst, .. } => *dst,
            inst => panic!("expected fixed-use copy, got {inst}"),
        };
        let analysis = super::super::analysis::analyze(&func);
        let assignment = try_color(&func, &analysis).unwrap();
        assert_eq!(assignment.get(fixed), Some(PhysReg::RCX));
        assert_ne!(assignment.get(lhs), Some(PhysReg::RCX));
    }
}
