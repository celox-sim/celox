//! SSA spill placement, live-range splitting, and coloring.
//!
//! Spill-free probing is side-effect free.  The forced SSA path additionally
//! splits pressure-selected live ranges with fresh reload definitions and
//! represents spilled phi operands as edge homes.

use std::collections::{BTreeSet, HashMap};

use crate::HashSet;
use crate::backend::native::mir::{BlockId, MFunction, MInst, SpillDesc, SpillKind, VReg};

use super::analysis::AnalysisResult;
use super::assignment::{
    ALLOCATABLE_REGS, AssignmentMap, EdgeLocation, PhysReg, RegConstraint, clobbers,
    use_constraints,
};
use super::spilling::{SpillSlotAllocator, make_reload, make_spill};

#[derive(Debug)]
pub(super) struct ColorFailure {
    pub block: BlockId,
    pub value: VReg,
    pub interfering: Vec<VReg>,
}

pub(super) struct Allocation {
    pub assignment: AssignmentMap,
    pub spill_frame_size: u32,
}

/// Run SSA splitting and coloring until coloring succeeds.
///
/// Every iteration removes all ordinary uses of one original live range and
/// replaces each use point with a fresh, dominating reload definition.  There
/// is no iteration limit: the transformation is accepted only when it strictly
/// shortens the selected live range.
pub(super) fn allocate(func: &mut MFunction) -> Result<Allocation, ColorFailure> {
    let mut slots = SpillSlotAllocator::new();
    let mut edge_homes = HashMap::<VReg, EdgeLocation>::new();
    let mut phi_dest_homes = HashMap::<VReg, i32>::new();
    let mut split_count = 0usize;
    let timing = std::env::var_os("CELOX_REGALLOC_TIMING").is_some();
    loop {
        let analysis_start = timing.then(crate::timing::now);
        let ignored = edge_homes.keys().copied().collect::<HashSet<_>>();
        let analysis = super::analysis::analyze_ignoring_phi_sources(func, &ignored);
        if let Some(start) = analysis_start {
            eprintln!(
                "[ssa-spill-timing] phase=analysis split_count={} elapsed={:?}",
                split_count,
                start.elapsed()
            );
        }
        let plan_start = timing.then(crate::timing::now);
        let pressure_spills = plan_pressure_spills(func, &analysis, &edge_homes, &phi_dest_homes);
        if let Some(start) = plan_start {
            eprintln!(
                "[ssa-spill-timing] phase=plan candidates={} elapsed={:?}",
                pressure_spills.len(),
                start.elapsed()
            );
        }
        if !pressure_spills.is_empty() {
            split_count += pressure_spills.len();
            let split_start = timing.then(crate::timing::now);
            split_live_ranges(
                func,
                &pressure_spills,
                &mut slots,
                &mut edge_homes,
                &mut phi_dest_homes,
            );
            if let Some(start) = split_start {
                eprintln!(
                    "[ssa-spill-timing] phase=split split_count={} insts={} elapsed={:?}",
                    split_count,
                    func.blocks
                        .iter()
                        .map(|block| block.insts.len())
                        .sum::<usize>(),
                    start.elapsed()
                );
            }
            if cfg!(debug_assertions) || std::env::var_os("CELOX_REGALLOC_VERIFY").is_some() {
                func.verify();
            }
            continue;
        }
        match try_color_with_edge_homes(func, &analysis, &edge_homes, &phi_dest_homes) {
            Ok(assignment) => {
                return Ok(Allocation {
                    assignment,
                    spill_frame_size: slots.total_size() as u32,
                });
            }
            Err(failure) => {
                let Some(victim) =
                    choose_spill_candidate(func, &failure, &edge_homes, &phi_dest_homes)
                else {
                    return Err(failure);
                };
                if std::env::var_os("CELOX_REGALLOC_TIMING").is_some() {
                    eprintln!(
                        "[ssa-spill] split={} block={} failed={} victim={} interference={}",
                        split_count,
                        failure.block,
                        failure.value,
                        victim,
                        failure.interfering.len()
                    );
                }
                split_live_ranges(
                    func,
                    &BTreeSet::from([victim]),
                    &mut slots,
                    &mut edge_homes,
                    &mut phi_dest_homes,
                );
                split_count += 1;
                if cfg!(debug_assertions) || std::env::var_os("CELOX_REGALLOC_VERIFY").is_some() {
                    func.verify();
                }
            }
        }
    }
}

fn plan_pressure_spills(
    func: &MFunction,
    analysis: &AnalysisResult,
    edge_homes: &HashMap<VReg, EdgeLocation>,
    phi_dest_homes: &HashMap<VReg, i32>,
) -> BTreeSet<VReg> {
    let fixed = fixed_values(func);
    let defined = defined_values(func);
    let use_counts = use_counts(func);
    let mut planned = BTreeSet::new();

    for (block_index, block) in func.blocks.iter().enumerate() {
        let mut live = analysis.exit_distances[block_index]
            .keys()
            .copied()
            .collect::<BTreeSet<_>>();
        plan_at_point(
            func,
            &live,
            &fixed,
            &defined,
            &use_counts,
            edge_homes,
            phi_dest_homes,
            &mut planned,
        );
        for inst in block.insts.iter().rev() {
            if let Some(def) = inst.def() {
                live.remove(&def);
            }
            live.extend(inst.uses());
            plan_at_point(
                func,
                &live,
                &fixed,
                &defined,
                &use_counts,
                edge_homes,
                phi_dest_homes,
                &mut planned,
            );
        }
    }
    planned
}

#[allow(clippy::too_many_arguments)]
fn plan_at_point(
    func: &MFunction,
    live: &BTreeSet<VReg>,
    fixed: &BTreeSet<VReg>,
    defined: &BTreeSet<VReg>,
    use_counts: &HashMap<VReg, usize>,
    edge_homes: &HashMap<VReg, EdgeLocation>,
    phi_dest_homes: &HashMap<VReg, i32>,
    planned: &mut BTreeSet<VReg>,
) {
    let active_count = live
        .iter()
        .filter(|value| {
            !planned.contains(*value)
                && !edge_homes.contains_key(*value)
                && !phi_dest_homes.contains_key(*value)
        })
        .count();
    let excess = active_count.saturating_sub(super::NUM_REGS);
    if excess == 0 {
        return;
    }
    let mut candidates = live
        .iter()
        .copied()
        .filter(|value| {
            !planned.contains(value)
                && !fixed.contains(value)
                && defined.contains(value)
                && !edge_homes.contains_key(value)
                && !phi_dest_homes.contains_key(value)
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|value| spill_score(func, use_counts, *value));
    for victim in candidates.into_iter().take(excess) {
        planned.insert(victim);
    }
}

/// Color an SSA function without changing it.
pub(super) fn try_color(
    func: &MFunction,
    analysis: &AnalysisResult,
) -> Result<AssignmentMap, ColorFailure> {
    try_color_with_edge_homes(func, analysis, &HashMap::new(), &HashMap::new())
}

fn try_color_with_edge_homes(
    func: &MFunction,
    analysis: &AnalysisResult,
    edge_homes: &HashMap<VReg, EdgeLocation>,
    phi_dest_homes: &HashMap<VReg, i32>,
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
                                interfering: Vec::new(),
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
            if let Some(&slot) = phi_dest_homes.get(&phi.dst) {
                result.set_edge_spill_slot(phi.dst, slot);
                continue;
            }
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
                        interfering: Vec::new(),
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
                if let Some(&home) = edge_homes.get(&source) {
                    result.set_edge_location(pred, source, home);
                    continue;
                }
                let Some(reg) = result.get(source) else {
                    return Err(ColorFailure {
                        block: successor.id,
                        value: source,
                        interfering: Vec::new(),
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
    let interfering = interfering.collect::<Vec<_>>();
    let mut unavailable = forbidden.remove(&value).unwrap_or_default();
    unavailable.extend(interfering.iter().filter_map(|other| result.get(*other)));
    let Some(reg) = ALLOCATABLE_REGS
        .iter()
        .copied()
        .find(|reg| !unavailable.contains(reg))
    else {
        return Err(ColorFailure {
            block,
            value,
            interfering,
        });
    };
    result.set(value, reg);
    Ok(())
}

fn choose_spill_candidate(
    func: &MFunction,
    failure: &ColorFailure,
    edge_homes: &HashMap<VReg, EdgeLocation>,
    phi_dest_homes: &HashMap<VReg, i32>,
) -> Option<VReg> {
    let fixed = fixed_values(func);
    let defined = defined_values(func);
    let use_counts = use_counts(func);

    std::iter::once(failure.value)
        .chain(failure.interfering.iter().copied())
        .filter(|value| {
            !fixed.contains(value)
                && !edge_homes.contains_key(value)
                && !phi_dest_homes.contains_key(value)
                && defined.contains(value)
        })
        .min_by_key(|value| spill_score(func, &use_counts, *value))
}

fn fixed_values(func: &MFunction) -> BTreeSet<VReg> {
    func.blocks
        .iter()
        .flat_map(|block| &block.insts)
        .flat_map(|inst| inst.uses().into_iter().zip(use_constraints(inst)))
        .filter_map(|(value, constraint)| {
            matches!(constraint, RegConstraint::Fixed(_)).then_some(value)
        })
        .collect()
}

fn defined_values(func: &MFunction) -> BTreeSet<VReg> {
    func.blocks
        .iter()
        .flat_map(|block| {
            block
                .phis
                .iter()
                .map(|phi| phi.dst)
                .chain(block.insts.iter().filter_map(|inst| inst.def()))
        })
        .collect()
}

fn use_counts(func: &MFunction) -> HashMap<VReg, usize> {
    let mut counts = HashMap::new();
    for block in &func.blocks {
        for inst in &block.insts {
            for used in inst.uses() {
                *counts.entry(used).or_default() += 1;
            }
        }
    }
    counts
}

fn spill_score(
    func: &MFunction,
    use_counts: &HashMap<VReg, usize>,
    value: VReg,
) -> (usize, usize, u32) {
    let uses = use_counts.get(&value).copied().unwrap_or(1);
    let cost = func.spill_desc(value).map_or(4, |desc| {
        usize::from(desc.spill_cost) + usize::from(desc.reload_cost) * uses
    });
    (cost, uses, value.0)
}

fn split_live_ranges(
    func: &mut MFunction,
    values: &BTreeSet<VReg>,
    slots: &mut SpillSlotAllocator,
    edge_homes: &mut HashMap<VReg, EdgeLocation>,
    phi_dest_homes: &mut HashMap<VReg, i32>,
) {
    let phi_sources = func
        .blocks
        .iter()
        .flat_map(|block| &block.phis)
        .flat_map(|phi| phi.sources.iter().map(|(_, source)| *source))
        .collect::<BTreeSet<_>>();
    let phi_destinations = func
        .blocks
        .iter()
        .flat_map(|block| block.phis.iter().map(|phi| phi.dst))
        .collect::<BTreeSet<_>>();
    let mut spills = HashMap::<VReg, Option<MInst>>::new();
    let mut destination_slots = HashMap::<VReg, i32>::new();

    for &value in values {
        if phi_destinations.contains(&value) {
            let slot = slots.slot_for(value);
            phi_dest_homes.insert(value, slot);
            destination_slots.insert(value, slot);
            spills.insert(value, None);
        } else if phi_sources.contains(&value) {
            let spill = match func.spill_desc(value).map(|desc| &desc.kind) {
                Some(SpillKind::Remat { value: immediate }) => {
                    edge_homes.insert(value, EdgeLocation::Immediate(*immediate));
                    None
                }
                _ => {
                    let slot = slots.slot_for(value);
                    edge_homes.insert(value, EdgeLocation::Stack(slot));
                    Some(MInst::Store {
                        base: crate::backend::native::mir::BaseReg::StackFrame,
                        offset: slot,
                        src: value,
                        size: crate::backend::native::mir::OpSize::S64,
                    })
                }
            };
            spills.insert(value, spill);
        } else {
            spills.insert(value, make_spill(value, func, slots));
        }
    }

    for block_index in 0..func.blocks.len() {
        let phi_defs = func.blocks[block_index]
            .phis
            .iter()
            .map(|phi| phi.dst)
            .filter(|value| values.contains(value))
            .collect::<Vec<_>>();
        let old_insts = std::mem::take(&mut func.blocks[block_index].insts);
        let mut rewritten = Vec::with_capacity(old_insts.len() + phi_defs.len());
        for value in phi_defs {
            if let Some(store) = spills[&value].clone() {
                rewritten.push(store);
            }
        }

        for mut inst in old_insts {
            let reload_values = inst
                .uses()
                .into_iter()
                .filter(|value| values.contains(value))
                .collect::<BTreeSet<_>>();
            for value in reload_values {
                let fresh = alloc_reload_vreg(func, value);
                let mut reload = if let Some(&offset) = destination_slots.get(&value) {
                    MInst::Load {
                        dst: fresh,
                        base: crate::backend::native::mir::BaseReg::StackFrame,
                        offset,
                        size: crate::backend::native::mir::OpSize::S64,
                    }
                } else {
                    make_reload(value, func, slots)
                };
                set_reload_destination(&mut reload, fresh);
                rewritten.push(reload);
                inst.rewrite_use(value, fresh);
            }
            let definition = inst.def().filter(|value| values.contains(value));
            rewritten.push(inst);
            if let Some(value) = definition
                && let Some(store) = spills[&value].clone()
            {
                rewritten.push(store);
            }
        }
        func.blocks[block_index].insts = rewritten;
    }
}

fn alloc_reload_vreg(func: &mut MFunction, source: VReg) -> VReg {
    let desc = match func.spill_desc(source).map(|desc| &desc.kind) {
        Some(SpillKind::Remat { .. }) => func.spill_desc(source).unwrap().clone(),
        _ => SpillDesc::transient(),
    };
    let width = func.value_widths.get(source.0 as usize).copied().flatten();
    let fresh = func.vregs.alloc();
    assert_eq!(fresh.0 as usize, func.spill_descs.len());
    func.spill_descs.push(desc);
    if !func.value_widths.is_empty() {
        assert_eq!(fresh.0 as usize, func.value_widths.len());
        func.value_widths.push(width);
    }
    fresh
}

fn set_reload_destination(inst: &mut MInst, destination: VReg) {
    match inst {
        MInst::LoadImm { dst, .. } | MInst::Load { dst, .. } => *dst = destination,
        _ => unreachable!("spill reload must define one VReg"),
    }
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
