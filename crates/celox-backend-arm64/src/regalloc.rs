//! AArch64 register-allocation boundary.
//!
//! Target MIR is projected into opcode-free facts consumed by shared analyses,
//! then spilled and colored against the AArch64 register file. Phi lowering and
//! edge-copy scheduling also remain target-owned; only opcode-free allocation
//! analyses and algorithms are shared.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use celox_backend_common::regalloc::{
    BlockAllocationFacts, FunctionAllocationFacts, InstructionAllocationFacts, LiveIntervals,
    PhiAllocationFacts, PhiSource, analyze_live_intervals, color_stack_slots,
};

use crate::allocation::{Assignment, CopyDestination, CopyOperation, CopySource, EdgeCopyPlan};
use crate::mir::{AllocatedFunction, BlockId, MFunction, MInst, VReg};
use crate::{Arm64Reg, HashMap};

pub(crate) type AllocationFacts = FunctionAllocationFacts<VReg, Arm64Reg>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TargetRegallocError {
    EmptyFunction,
    MissingBlock {
        block: BlockId,
        target: BlockId,
    },
    InvalidFacts(String),
    MissingAssignment {
        block: BlockId,
        instruction: usize,
        value: VReg,
    },
    ReservedAssignment {
        block: BlockId,
        instruction: usize,
        value: VReg,
        register: Arm64Reg,
    },
    RegisterConflict {
        block: BlockId,
        instruction: usize,
        left: VReg,
        right: VReg,
        register: Arm64Reg,
    },
    RegisterPressure {
        value: VReg,
    },
    ParallelCopy {
        predecessor: BlockId,
        successor: BlockId,
        message: String,
    },
    UnspillablePressure {
        value: VReg,
    },
    SpillFrameOverflow,
}

impl fmt::Display for TargetRegallocError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyFunction => formatter.write_str("AArch64 allocation has no entry block"),
            Self::MissingBlock { block, target } => {
                write!(
                    formatter,
                    "AArch64 block {block} targets missing block {target}"
                )
            }
            Self::InvalidFacts(error) => {
                write!(formatter, "invalid AArch64 allocation facts: {error}")
            }
            Self::MissingAssignment {
                block,
                instruction,
                value,
            } => write!(
                formatter,
                "AArch64 {block} instruction {instruction} value {value} has no register assignment"
            ),
            Self::ReservedAssignment {
                block,
                instruction,
                value,
                register,
            } => write!(
                formatter,
                "AArch64 {block} instruction {instruction} value {value} uses reserved x{}",
                register.number()
            ),
            Self::RegisterConflict {
                block,
                instruction,
                left,
                right,
                register,
            } => write!(
                formatter,
                "AArch64 {block} point {instruction} keeps {left} and {right} live in x{}",
                register.number()
            ),
            Self::RegisterPressure { value } => write!(
                formatter,
                "AArch64 register pressure cannot assign {value} without spilling"
            ),
            Self::ParallelCopy {
                predecessor,
                successor,
                message,
            } => write!(
                formatter,
                "AArch64 parallel copy on {predecessor} -> {successor}: {message}"
            ),
            Self::UnspillablePressure { value } => write!(
                formatter,
                "AArch64 register pressure at {value} involves only unspillable scratch values"
            ),
            Self::SpillFrameOverflow => {
                formatter.write_str("AArch64 spill frame exceeds the supported i32 offset range")
            }
        }
    }
}

impl std::error::Error for TargetRegallocError {}

pub(crate) fn build_facts(function: &MFunction) -> Result<AllocationFacts, TargetRegallocError> {
    if function.blocks.is_empty() {
        return Err(TargetRegallocError::EmptyFunction);
    }
    let indices = function
        .blocks
        .iter()
        .enumerate()
        .map(|(index, block)| (block.id, index))
        .collect::<HashMap<_, _>>();
    let blocks = function
        .blocks
        .iter()
        .map(|block| {
            let block_target = |target: BlockId| {
                indices
                    .get(&target)
                    .copied()
                    .ok_or(TargetRegallocError::MissingBlock {
                        block: block.id,
                        target,
                    })
            };
            let successors = block
                .successors()
                .into_iter()
                .map(block_target)
                .collect::<Result<_, _>>()?;
            let phis = block
                .phis
                .iter()
                .map(|phi| {
                    Ok(PhiAllocationFacts {
                        destination: phi.dst,
                        sources: phi
                            .sources
                            .iter()
                            .map(|&(predecessor, value)| {
                                Ok(PhiSource {
                                    predecessor: block_target(predecessor)?,
                                    value,
                                })
                            })
                            .collect::<Result<_, TargetRegallocError>>()?,
                    })
                })
                .collect::<Result<_, TargetRegallocError>>()?;
            let instructions = block
                .insts
                .iter()
                .map(|instruction| InstructionAllocationFacts {
                    uses: instruction.uses(),
                    defs: instruction.def().into_iter().collect(),
                    is_copy: instruction.is_copy(),
                    ..InstructionAllocationFacts::default()
                })
                .collect();
            Ok(BlockAllocationFacts {
                successors,
                phis,
                instructions,
            })
        })
        .collect::<Result<_, TargetRegallocError>>()?;
    let facts = FunctionAllocationFacts { entry: 0, blocks };
    facts
        .verify()
        .map_err(|error| TargetRegallocError::InvalidFacts(error.to_string()))?;
    Ok(facts)
}

pub(crate) fn verify_allocated(function: &AllocatedFunction) -> Result<(), TargetRegallocError> {
    let facts = build_facts(&function.function)?;
    let intervals = analyze_live_intervals(&facts)
        .map_err(|error| TargetRegallocError::InvalidFacts(error.to_string()))?;

    for block in &function.function.blocks {
        for (instruction_index, instruction) in block.insts.iter().enumerate() {
            for value in instruction.uses().into_iter().chain(instruction.def()) {
                let Some(register) = function.assignment.get(&value) else {
                    return Err(TargetRegallocError::MissingAssignment {
                        block: block.id,
                        instruction: instruction_index,
                        value,
                    });
                };
                if !(1..=15).contains(&register.number()) && !(19..=28).contains(&register.number())
                {
                    return Err(TargetRegallocError::ReservedAssignment {
                        block: block.id,
                        instruction: instruction_index,
                        value,
                        register,
                    });
                }
            }
        }
    }
    verify_interval_registers(function, &intervals)
}

const ALLOCATABLE_REGISTERS: [Arm64Reg; 25] = [
    Arm64Reg::new(1),
    Arm64Reg::new(2),
    Arm64Reg::new(3),
    Arm64Reg::new(4),
    Arm64Reg::new(5),
    Arm64Reg::new(6),
    Arm64Reg::new(7),
    Arm64Reg::new(8),
    Arm64Reg::new(9),
    Arm64Reg::new(10),
    Arm64Reg::new(11),
    Arm64Reg::new(12),
    Arm64Reg::new(13),
    Arm64Reg::new(14),
    Arm64Reg::new(15),
    Arm64Reg::new(19),
    Arm64Reg::new(20),
    Arm64Reg::new(21),
    Arm64Reg::new(22),
    Arm64Reg::new(23),
    Arm64Reg::new(24),
    Arm64Reg::new(25),
    Arm64Reg::new(26),
    Arm64Reg::new(27),
    Arm64Reg::new(28),
];

pub(crate) struct TargetAllocation {
    pub(crate) allocated: AllocatedFunction,
    pub(crate) spill_frame_size: u32,
}

/// Allocate target MIR, inserting simple AArch64-owned stack spills when the
/// interference graph does not fit the register file.
///
/// SSA values are split into one stack home, short definition temporaries, and
/// reload ranges shared only across adjacent instructions. Spilled phis become
/// target edge copies whose sources and destinations may be stack slots;
/// register sources receive allocation-only edge uses to keep them live until
/// the copy. Noninterfering homes share colored frame slots. This keeps spill
/// policy and rewriting on the target side.
pub(crate) fn allocate_with_spills(
    mut function: MFunction,
) -> Result<TargetAllocation, TargetRegallocError> {
    let initial_facts = build_facts(&function)?;
    let mut candidates = initial_facts
        .blocks
        .iter()
        .flat_map(|block| {
            block.phis.iter().map(|phi| phi.destination).chain(
                block
                    .instructions
                    .iter()
                    .flat_map(|instruction| instruction.defs.iter().copied()),
            )
        })
        .collect::<BTreeSet<_>>();
    for block in &function.blocks {
        for instruction in &block.insts {
            if let MInst::Scratch { dst } = instruction {
                candidates.remove(dst);
            }
        }
    }

    let mut next_value = function
        .blocks
        .iter()
        .flat_map(|block| {
            block
                .phis
                .iter()
                .flat_map(|phi| std::iter::once(phi.dst).chain(phi.sources.iter().map(|row| row.1)))
                .chain(block.insts.iter().flat_map(|instruction| {
                    instruction.uses().into_iter().chain(instruction.def())
                }))
        })
        .map(|value| value.0)
        .max()
        .map_or(0, |value| value.saturating_add(1));
    let mut spill_frame_size = 0_u32;

    loop {
        let facts = build_facts(&function)?;
        let intervals = analyze_live_intervals(&facts)
            .map_err(|error| TargetRegallocError::InvalidFacts(error.to_string()))?;
        let proactive = close_phi_spill_set(
            &function,
            select_spill_batch(&function, &intervals, &candidates, false),
        );
        if !proactive.is_empty() {
            insert_spill_batch(
                &mut function,
                &mut candidates,
                proactive,
                &intervals,
                &mut spill_frame_size,
                &mut next_value,
            )?;
            continue;
        }
        match allocate_without_spills(function.clone()) {
            Ok(allocated) => {
                return Ok(TargetAllocation {
                    allocated,
                    spill_frame_size,
                });
            }
            Err(TargetRegallocError::RegisterPressure { value }) => {
                let spilled = close_phi_spill_set(
                    &function,
                    select_spill_batch(&function, &intervals, &candidates, true),
                );
                if spilled.is_empty() {
                    return Err(TargetRegallocError::UnspillablePressure { value });
                }
                insert_spill_batch(
                    &mut function,
                    &mut candidates,
                    spilled,
                    &intervals,
                    &mut spill_frame_size,
                    &mut next_value,
                )?;
            }
            Err(error) => return Err(error),
        }
    }
}

fn close_phi_spill_set(function: &MFunction, spilled: Vec<VReg>) -> Vec<VReg> {
    let mut all_spilled = function
        .spill_homes
        .keys()
        .copied()
        .collect::<BTreeSet<_>>();
    let mut newly_spilled = spilled.into_iter().collect::<BTreeSet<_>>();
    all_spilled.extend(newly_spilled.iter().copied());
    loop {
        let mut changed = false;
        for block in &function.blocks {
            for phi in &block.phis {
                let touches_spill = all_spilled.contains(&phi.dst)
                    || phi
                        .sources
                        .iter()
                        .any(|&(_, source)| all_spilled.contains(&source));
                if touches_spill && all_spilled.insert(phi.dst) {
                    newly_spilled.insert(phi.dst);
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    newly_spilled.into_iter().collect()
}

fn select_spill_batch(
    function: &MFunction,
    intervals: &celox_backend_common::regalloc::LiveIntervals<VReg>,
    candidates: &BTreeSet<VReg>,
    force: bool,
) -> Vec<VReg> {
    let live_lengths = intervals
        .iter()
        .map(|(&value, interval)| {
            let length = interval
                .segments
                .iter()
                .map(|segment| segment.end.saturating_sub(segment.start))
                .sum::<u64>();
            (value, length)
        })
        .collect::<BTreeMap<_, _>>();
    let live_length = |value: VReg| live_lengths.get(&value).copied().unwrap_or(0);
    // The widest target instruction has five uses and one definition. Keep
    // that many registers free so a spilled row's local reload/definition
    // temporaries do not immediately create a second pressure wave.
    let target_capacity = ALLOCATABLE_REGISTERS.len().saturating_sub(6);
    let mut selected = BTreeSet::new();
    let mut peak = Vec::new();
    for block in 0..function.blocks.len() {
        let mut segments = intervals
            .iter()
            .filter_map(|(&value, interval)| {
                interval
                    .segment_in_block(block)
                    .map(|segment| (segment.start, segment.end, value))
            })
            .collect::<Vec<_>>();
        segments.sort_unstable();
        let mut active = Vec::<(u64, VReg)>::new();
        for (start, end, value) in segments {
            active.retain(|(active_end, active_value)| {
                *active_end > start && !selected.contains(active_value)
            });
            if !selected.contains(&value) {
                active.push((end, value));
            }
            if active.len() > peak.len() {
                peak = active.iter().map(|&(_, value)| value).collect();
            }
            while active.len() > target_capacity {
                let Some(spilled) = active
                    .iter()
                    .filter(|(_, value)| candidates.contains(value))
                    .max_by_key(|(_, value)| (live_length(*value), Reverse(*value)))
                    .map(|&(_, value)| value)
                else {
                    break;
                };
                selected.insert(spilled);
                active.retain(|&(_, value)| value != spilled);
            }
        }
    }

    if !selected.is_empty() {
        return selected.into_iter().collect();
    }
    if !force {
        return Vec::new();
    }
    peak.into_iter()
        .filter(|value| candidates.contains(value))
        .max_by_key(|value| (live_length(*value), Reverse(*value)))
        .into_iter()
        .collect()
}

fn insert_spill_batch(
    function: &mut MFunction,
    candidates: &mut BTreeSet<VReg>,
    spilled: Vec<VReg>,
    intervals: &LiveIntervals<VReg>,
    spill_frame_size: &mut u32,
    next_value: &mut u32,
) -> Result<(), TargetRegallocError> {
    // Instruction insertion changes the numeric interval coordinate system
    // between spill iterations. Color values selected from this one immutable
    // analysis together, and keep separate batches in disjoint frame regions.
    let batch_base = *spill_frame_size;
    let spill_intervals = spilled
        .iter()
        .map(|value| {
            intervals.get(value).ok_or_else(|| {
                TargetRegallocError::InvalidFacts(format!(
                    "spill candidate {value} has no exact live interval"
                ))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let stack_coloring = color_stack_slots(spill_intervals)
        .map_err(|error| TargetRegallocError::InvalidFacts(error.to_string()))?;
    let mut homes = BTreeMap::new();
    for spilled in spilled {
        candidates.remove(&spilled);
        let slot = stack_coloring
            .get(&spilled)
            .expect("each selected spill candidate has a colored stack slot");
        let local_offset = slot
            .checked_mul(8)
            .ok_or(TargetRegallocError::SpillFrameOverflow)?;
        let offset = batch_base
            .checked_add(local_offset)
            .and_then(|offset| i32::try_from(offset).ok())
            .ok_or(TargetRegallocError::SpillFrameOverflow)?;
        homes.insert(spilled, offset);
        *spill_frame_size = (*spill_frame_size).max(
            batch_base
                .checked_add(local_offset)
                .and_then(|offset| offset.checked_add(8))
                .ok_or(TargetRegallocError::SpillFrameOverflow)?,
        );
    }
    function
        .spill_homes
        .extend(homes.iter().map(|(&value, &offset)| (value, offset)));
    spill_values(function, &homes, next_value)
}

fn spill_values(
    function: &mut MFunction,
    homes: &BTreeMap<VReg, i32>,
    next_value: &mut u32,
) -> Result<(), TargetRegallocError> {
    let fresh = |next_value: &mut u32| {
        let value = VReg(*next_value);
        *next_value = next_value
            .checked_add(1)
            .ok_or(TargetRegallocError::SpillFrameOverflow)?;
        Ok(value)
    };
    let mut rewritten_definitions = BTreeSet::new();
    let all_homes = function.spill_homes.clone();
    let mut external_phis = Vec::new();
    let mut edge_keep_alives = BTreeMap::<BlockId, BTreeSet<VReg>>::new();
    for block in &mut function.blocks {
        let mut retained = Vec::with_capacity(block.phis.len());
        for phi in std::mem::take(&mut block.phis) {
            if let Some(&destination) = all_homes.get(&phi.dst) {
                let spilled = phi.dst;
                external_phis.push(crate::mir::SpilledPhiNode {
                    successor: block.id,
                    destination,
                    sources: phi
                        .sources
                        .into_iter()
                        .map(|(predecessor, source)| {
                            let source = if let Some(&offset) = all_homes.get(&source) {
                                crate::mir::SpilledPhiSource::Stack(offset)
                            } else {
                                edge_keep_alives
                                    .entry(predecessor)
                                    .or_default()
                                    .insert(source);
                                crate::mir::SpilledPhiSource::Value(source)
                            };
                            (predecessor, source)
                        })
                        .collect(),
                });
                rewritten_definitions.insert(spilled);
            } else {
                debug_assert!(
                    phi.sources
                        .iter()
                        .all(|&(_, source)| !all_homes.contains_key(&source)),
                    "phi spill closure must externalize a destination with a spilled source"
                );
                retained.push(phi);
            }
        }
        block.phis = retained;
    }
    for phi in &mut function.spilled_phis {
        for (_, source) in &mut phi.sources {
            if let crate::mir::SpilledPhiSource::Value(value) = *source
                && let Some(&offset) = all_homes.get(&value)
            {
                *source = crate::mir::SpilledPhiSource::Stack(offset);
            }
        }
    }
    function.spilled_phis.extend(external_phis);
    for block in &mut function.blocks {
        let original = std::mem::take(&mut block.insts);
        let spill_uses = original
            .iter()
            .map(|instruction| {
                if matches!(instruction, MInst::KeepAlive { src } if homes.contains_key(src)) {
                    BTreeSet::new()
                } else {
                    instruction
                        .uses()
                        .into_iter()
                        .filter(|value| homes.contains_key(value))
                        .collect()
                }
            })
            .collect::<Vec<BTreeSet<_>>>();
        let mut reload_cache = BTreeMap::<VReg, VReg>::new();
        let existing_keep_alives = original
            .iter()
            .filter_map(|instruction| match instruction {
                MInst::KeepAlive { src } if !homes.contains_key(src) => Some(*src),
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        let new_keep_alives = edge_keep_alives
            .remove(&block.id)
            .unwrap_or_default()
            .difference(&existing_keep_alives)
            .copied()
            .collect::<Vec<_>>();
        let terminator_index = original.len().saturating_sub(1);
        let mut rewritten = Vec::with_capacity(original.len() + new_keep_alives.len());
        for (index, mut instruction) in original.into_iter().enumerate() {
            if matches!(instruction, MInst::KeepAlive { src } if homes.contains_key(&src)) {
                continue;
            }
            if index == terminator_index {
                rewritten.extend(
                    new_keep_alives
                        .iter()
                        .copied()
                        .map(|src| MInst::KeepAlive { src }),
                );
            }
            for spilled in spill_uses[index].iter().copied() {
                let reload = if let Some(&reload) = reload_cache.get(&spilled) {
                    reload
                } else {
                    let reload = fresh(next_value)?;
                    rewritten.push(MInst::Load {
                        dst: reload,
                        base: crate::mir::BaseReg::StackFrame,
                        offset: homes[&spilled],
                        size: crate::mir::OpSize::S64,
                    });
                    reload_cache.insert(spilled, reload);
                    reload
                };
                instruction.rewrite_use(spilled, reload);
            }
            let mut definition_cache = None;
            if let Some(spilled) = instruction.def().filter(|value| homes.contains_key(value)) {
                let temporary = fresh(next_value)?;
                *instruction
                    .def_mut()
                    .expect("the matching spill definition has a mutable destination") = temporary;
                rewritten.push(instruction);
                rewritten.push(MInst::Store {
                    base: crate::mir::BaseReg::StackFrame,
                    offset: homes[&spilled],
                    src: temporary,
                    size: crate::mir::OpSize::S64,
                });
                rewritten_definitions.insert(spilled);
                definition_cache = Some((spilled, temporary));
            } else {
                rewritten.push(instruction);
            }
            let next_uses = spill_uses.get(index + 1);
            reload_cache
                .retain(|value, _| next_uses.is_some_and(|next_uses| next_uses.contains(value)));
            if let Some((spilled, temporary)) = definition_cache
                && next_uses.is_some_and(|next_uses| next_uses.contains(&spilled))
            {
                reload_cache.insert(spilled, temporary);
            }
        }
        block.insts = rewritten;
    }
    if let Some(spilled) = homes
        .keys()
        .find(|value| !rewritten_definitions.contains(value))
    {
        return Err(TargetRegallocError::InvalidFacts(format!(
            "spill candidate {spilled} has no instruction definition"
        )));
    }
    Ok(())
}

/// Allocate AArch64-owned target MIR without importing x86 register colors.
///
/// This first target-native path intentionally reports pressure instead of
/// hiding a spill policy. Spill/reload insertion is the next AArch64-owned
/// layer and can preserve this coloring and edge-copy boundary.
pub(crate) fn allocate_without_spills(
    function: MFunction,
) -> Result<AllocatedFunction, TargetRegallocError> {
    let facts = build_facts(&function)?;
    let intervals = analyze_live_intervals(&facts)
        .map_err(|error| TargetRegallocError::InvalidFacts(error.to_string()))?;
    let assignment = color_intervals(&function, &intervals)?;
    let edge_copies = build_edge_copies(&function, &assignment)?;
    let allocated = AllocatedFunction {
        function,
        assignment,
        edge_copies,
    };
    verify_allocated(&allocated)?;
    Ok(allocated)
}

fn color_intervals(
    function: &MFunction,
    intervals: &celox_backend_common::regalloc::LiveIntervals<VReg>,
) -> Result<Assignment<VReg>, TargetRegallocError> {
    let mut interference = intervals
        .iter()
        .map(|(&value, _)| (value, BTreeSet::new()))
        .collect::<BTreeMap<_, _>>();
    for block in 0..function.blocks.len() {
        let mut segments = intervals
            .iter()
            .filter_map(|(&value, interval)| {
                interval
                    .segment_in_block(block)
                    .map(|segment| (segment.start, segment.end, value))
            })
            .collect::<Vec<_>>();
        segments.sort_unstable();
        let mut active = Vec::<(u64, VReg)>::new();
        for (start, end, value) in segments {
            active.retain(|(active_end, _)| *active_end > start);
            for &(_, other) in &active {
                interference.entry(value).or_default().insert(other);
                interference.entry(other).or_default().insert(value);
            }
            active.push((end, value));
        }
    }

    let mut affinities = BTreeMap::<VReg, BTreeSet<VReg>>::new();
    for block in &function.blocks {
        for phi in &block.phis {
            for &(_, source) in &phi.sources {
                affinities.entry(phi.dst).or_default().insert(source);
                affinities.entry(source).or_default().insert(phi.dst);
            }
        }
    }
    let mut order = interference
        .iter()
        .map(|(&value, neighbors)| (Reverse(neighbors.len()), value))
        .collect::<Vec<_>>();
    order.sort_unstable();

    let mut assignment = Assignment::default();
    for (_, value) in order {
        let used = interference[&value]
            .iter()
            .filter_map(|neighbor| assignment.get(neighbor))
            .collect::<BTreeSet<_>>();
        let preferred = affinities
            .get(&value)
            .into_iter()
            .flatten()
            .filter_map(|neighbor| assignment.get(neighbor))
            .find(|register| !used.contains(register));
        let register = preferred
            .or_else(|| {
                ALLOCATABLE_REGISTERS
                    .iter()
                    .copied()
                    .find(|register| !used.contains(register))
            })
            .ok_or(TargetRegallocError::RegisterPressure { value })?;
        assignment.set(value, register);
    }
    Ok(assignment)
}

fn build_edge_copies(
    function: &MFunction,
    assignment: &Assignment<VReg>,
) -> Result<EdgeCopyPlan<BlockId>, TargetRegallocError> {
    use celox_backend_common::regalloc as common;

    let mut plan = EdgeCopyPlan::default();
    for successor in &function.blocks {
        let mut rows_by_predecessor =
            BTreeMap::<BlockId, Vec<common::ParallelCopy<Arm64Reg>>>::new();
        for phi in &successor.phis {
            let destination =
                assignment
                    .get(&phi.dst)
                    .ok_or(TargetRegallocError::MissingAssignment {
                        block: successor.id,
                        instruction: 0,
                        value: phi.dst,
                    })?;
            for &(predecessor, source_value) in &phi.sources {
                let source = assignment.get(&source_value).ok_or(
                    TargetRegallocError::MissingAssignment {
                        block: predecessor,
                        instruction: 0,
                        value: source_value,
                    },
                )?;
                rows_by_predecessor
                    .entry(predecessor)
                    .or_default()
                    .push(common::ParallelCopy {
                        destination: common::CopyDestination::Register(destination),
                        source: common::CopySource::Register(source),
                    });
            }
        }
        for phi in function
            .spilled_phis
            .iter()
            .filter(|phi| phi.successor == successor.id)
        {
            for &(predecessor, source) in &phi.sources {
                let source = match source {
                    crate::mir::SpilledPhiSource::Value(value) => {
                        common::CopySource::Register(assignment.get(&value).ok_or(
                            TargetRegallocError::MissingAssignment {
                                block: predecessor,
                                instruction: 0,
                                value,
                            },
                        )?)
                    }
                    crate::mir::SpilledPhiSource::Stack(offset) => {
                        common::CopySource::Stack(offset)
                    }
                };
                rows_by_predecessor
                    .entry(predecessor)
                    .or_default()
                    .push(common::ParallelCopy {
                        destination: common::CopyDestination::Stack(phi.destination),
                        source,
                    });
            }
        }
        for (predecessor, rows) in rows_by_predecessor {
            let resolution = common::resolve_parallel_copies(&rows).map_err(|error| {
                TargetRegallocError::ParallelCopy {
                    predecessor,
                    successor: successor.id,
                    message: error.to_string(),
                }
            })?;
            let operations = resolution
                .operations
                .into_iter()
                .map(|operation| match operation {
                    common::CopyOperation::Move {
                        destination,
                        source,
                    } => CopyOperation::Move {
                        destination: adapt_copy_destination(destination),
                        source: adapt_copy_source(source),
                    },
                    common::CopyOperation::SwapRegisters { left, right } => {
                        CopyOperation::SwapRegisters { left, right }
                    }
                    common::CopyOperation::SaveTemporary(destination) => {
                        CopyOperation::SaveTemporary(adapt_copy_destination(destination))
                    }
                    common::CopyOperation::RestoreTemporary(destination) => {
                        CopyOperation::RestoreTemporary(adapt_copy_destination(destination))
                    }
                })
                .collect();
            plan.insert(predecessor, successor.id, operations);
        }
    }
    Ok(plan)
}

fn adapt_copy_destination(
    destination: celox_backend_common::regalloc::CopyDestination<Arm64Reg>,
) -> CopyDestination {
    match destination {
        celox_backend_common::regalloc::CopyDestination::Register(register) => {
            CopyDestination::Register(register)
        }
        celox_backend_common::regalloc::CopyDestination::Stack(offset) => {
            CopyDestination::Stack(offset)
        }
    }
}

fn adapt_copy_source(source: celox_backend_common::regalloc::CopySource<Arm64Reg>) -> CopySource {
    match source {
        celox_backend_common::regalloc::CopySource::Register(register) => {
            CopySource::Register(register)
        }
        celox_backend_common::regalloc::CopySource::Stack(offset) => CopySource::Stack(offset),
        celox_backend_common::regalloc::CopySource::Immediate(value) => {
            CopySource::Immediate(value)
        }
    }
}

fn verify_interval_registers(
    function: &AllocatedFunction,
    intervals: &celox_backend_common::regalloc::LiveIntervals<VReg>,
) -> Result<(), TargetRegallocError> {
    let mut segments = Vec::new();
    for (&value, interval) in intervals.iter() {
        let Some(register) = function.assignment.get(&value) else {
            // Legacy edge values may reside in a stack home or immediate.
            continue;
        };
        segments.extend(
            interval
                .segments
                .iter()
                .map(|segment| (segment.block, register, segment.start, segment.end, value)),
        );
    }
    segments.sort_unstable_by_key(|&(block, register, start, end, value)| {
        (block, register, start, end, value)
    });
    let mut previous = None;
    for (block, register, start, end, value) in segments {
        if let Some((left_block, left_register, _, left_end, left)) = previous
            && left_block == block
            && left_register == register
            && start < left_end
            && left != value
        {
            return Err(TargetRegallocError::RegisterConflict {
                block: function.function.blocks[block].id,
                instruction: usize::try_from(start / 3).unwrap_or(usize::MAX),
                left,
                right: value,
                register,
            });
        }
        previous = Some((block, register, start, end, value));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::allocation::{Assignment, EdgeCopyPlan};
    use crate::mir::{BaseReg, MBlock, MInst, OpSize, PhiNode};

    fn diamond_function() -> MFunction {
        let entry = MBlock {
            id: BlockId(10),
            phis: Vec::new(),
            insts: vec![
                MInst::LoadImm {
                    dst: VReg(0),
                    value: 1,
                },
                MInst::Branch {
                    cond: VReg(0),
                    true_bb: BlockId(20),
                    false_bb: BlockId(30),
                },
            ],
        };
        let left = MBlock {
            id: BlockId(20),
            phis: Vec::new(),
            insts: vec![
                MInst::LoadImm {
                    dst: VReg(1),
                    value: 2,
                },
                MInst::Jump {
                    target: BlockId(40),
                },
            ],
        };
        let right = MBlock {
            id: BlockId(30),
            phis: Vec::new(),
            insts: vec![
                MInst::LoadImm {
                    dst: VReg(2),
                    value: 3,
                },
                MInst::Jump {
                    target: BlockId(40),
                },
            ],
        };
        let join = MBlock {
            id: BlockId(40),
            phis: vec![PhiNode {
                dst: VReg(3),
                sources: vec![(BlockId(20), VReg(1)), (BlockId(30), VReg(2))],
            }],
            insts: vec![MInst::Return],
        };
        MFunction::new(vec![entry, left, right, join], Vec::new())
    }

    #[test]
    fn exports_normalized_cfg_phi_and_instruction_facts() {
        let facts = build_facts(&diamond_function()).unwrap();

        assert_eq!(facts.blocks[0].successors, vec![1, 2]);
        assert_eq!(facts.blocks[0].instructions[0].defs, vec![VReg(0)]);
        assert_eq!(facts.blocks[0].instructions[1].uses, vec![VReg(0)]);
        assert_eq!(facts.blocks[3].phis[0].destination, VReg(3));
        assert_eq!(facts.blocks[3].phis[0].sources[0].predecessor, 1);
        assert_eq!(facts.blocks[3].phis[0].sources[1].predecessor, 2);
        analyze_live_intervals(&facts).unwrap();
    }

    #[test]
    fn rejects_missing_and_reserved_instruction_assignments() {
        let function = diamond_function();
        let mut allocated = AllocatedFunction {
            function,
            assignment: Assignment::default(),
            edge_copies: EdgeCopyPlan::default(),
        };
        assert!(matches!(
            verify_allocated(&allocated),
            Err(TargetRegallocError::MissingAssignment { value: VReg(0), .. })
        ));

        for (value, register) in [
            (VReg(0), Arm64Reg::new(1)),
            (VReg(1), Arm64Reg::new(2)),
            (VReg(2), Arm64Reg::new(3)),
        ] {
            allocated.assignment.set(value, register);
        }
        allocated.assignment.set(VReg(0), Arm64Reg::new(16));
        assert!(matches!(
            verify_allocated(&allocated),
            Err(TargetRegallocError::ReservedAssignment { value: VReg(0), .. })
        ));
    }

    #[test]
    fn rejects_interfering_values_assigned_to_one_register() {
        let function = MFunction::new(
            vec![MBlock {
                id: BlockId(0),
                phis: Vec::new(),
                insts: vec![
                    MInst::LoadImm {
                        dst: VReg(0),
                        value: 1,
                    },
                    MInst::LoadImm {
                        dst: VReg(1),
                        value: 2,
                    },
                    MInst::Add {
                        dst: VReg(2),
                        lhs: VReg(0),
                        rhs: VReg(1),
                    },
                    MInst::Return,
                ],
            }],
            Vec::new(),
        );
        let mut assignment = Assignment::default();
        assignment.set(VReg(0), Arm64Reg::new(1));
        assignment.set(VReg(1), Arm64Reg::new(1));
        assignment.set(VReg(2), Arm64Reg::new(2));
        let allocated = AllocatedFunction {
            function,
            assignment,
            edge_copies: EdgeCopyPlan::default(),
        };

        assert!(matches!(
            verify_allocated(&allocated),
            Err(TargetRegallocError::RegisterConflict {
                register,
                ..
            }) if register == Arm64Reg::new(1)
        ));
    }

    #[test]
    fn allocates_target_owned_mir_without_legacy_colors() {
        let allocated = allocate_without_spills(diamond_function()).unwrap();

        for value in [VReg(0), VReg(1), VReg(2), VReg(3)] {
            let register = allocated.assignment.get(&value).unwrap();
            assert!(
                (1..=15).contains(&register.number()) || (19..=28).contains(&register.number())
            );
        }
        verify_allocated(&allocated).unwrap();
    }

    #[test]
    fn lowers_target_phi_rows_with_the_common_copy_resolver() {
        let predecessor = MBlock {
            id: BlockId(0),
            phis: Vec::new(),
            insts: vec![
                MInst::LoadImm {
                    dst: VReg(0),
                    value: 7,
                },
                MInst::Jump { target: BlockId(1) },
            ],
        };
        let successor = MBlock {
            id: BlockId(1),
            phis: vec![PhiNode {
                dst: VReg(1),
                sources: vec![(BlockId(0), VReg(0))],
            }],
            insts: vec![MInst::Return],
        };
        let function = MFunction::new(vec![predecessor, successor], Vec::new());
        let mut assignment = Assignment::default();
        assignment.set(VReg(0), Arm64Reg::new(1));
        assignment.set(VReg(1), Arm64Reg::new(2));

        let copies = build_edge_copies(&function, &assignment).unwrap();

        assert_eq!(
            copies.edge(BlockId(0), BlockId(1)),
            Some(
                [CopyOperation::Move {
                    destination: CopyDestination::Register(Arm64Reg::new(2)),
                    source: CopySource::Register(Arm64Reg::new(1)),
                }]
                .as_slice()
            )
        );
    }

    #[test]
    fn reports_pressure_before_spill_support_is_enabled() {
        let mut instructions = (0..26)
            .map(|value| MInst::LoadImm {
                dst: VReg(value),
                value: u64::from(value),
            })
            .collect::<Vec<_>>();
        instructions.extend((0..26).map(|value| MInst::Store {
            base: BaseReg::SimState,
            offset: value * 8,
            src: VReg(value as u32),
            size: OpSize::S64,
        }));
        instructions.push(MInst::Return);
        let function = MFunction::new(
            vec![MBlock {
                id: BlockId(0),
                phis: Vec::new(),
                insts: instructions,
            }],
            Vec::new(),
        );

        assert!(matches!(
            allocate_without_spills(function),
            Err(TargetRegallocError::RegisterPressure { .. })
        ));
    }

    #[test]
    fn inserts_target_owned_spill_and_reload_instructions() {
        let mut instructions = (0..26)
            .map(|value| MInst::LoadImm {
                dst: VReg(value),
                value: u64::from(value),
            })
            .collect::<Vec<_>>();
        instructions.extend((0..26).map(|value| MInst::Store {
            base: BaseReg::SimState,
            offset: value * 8,
            src: VReg(value as u32),
            size: OpSize::S64,
        }));
        instructions.push(MInst::Return);
        let allocation = allocate_with_spills(MFunction::new(
            vec![MBlock {
                id: BlockId(0),
                phis: Vec::new(),
                insts: instructions,
            }],
            Vec::new(),
        ))
        .unwrap();

        assert!(allocation.spill_frame_size >= 8);
        assert_eq!(allocation.spill_frame_size % 8, 0);
        let reload_count = allocation.allocated.function.blocks[0]
            .insts
            .iter()
            .filter(|instruction| {
                matches!(
                    instruction,
                    MInst::Load {
                        base: BaseReg::StackFrame,
                        size: OpSize::S64,
                        ..
                    }
                )
            })
            .count();
        assert!(reload_count > 0);
        assert!(
            allocation.allocated.function.blocks[0]
                .insts
                .iter()
                .any(|instruction| matches!(
                    instruction,
                    MInst::Store {
                        base: BaseReg::StackFrame,
                        offset: 0,
                        size: OpSize::S64,
                        ..
                    }
                ))
        );
        verify_allocated(&allocation.allocated).unwrap();
    }

    #[test]
    fn reuses_one_reload_across_adjacent_uses() {
        let mut function = MFunction::new(
            vec![MBlock {
                id: BlockId(0),
                phis: Vec::new(),
                insts: vec![
                    MInst::LoadImm {
                        dst: VReg(0),
                        value: 42,
                    },
                    MInst::Store {
                        base: BaseReg::SimState,
                        offset: 0,
                        src: VReg(0),
                        size: OpSize::S64,
                    },
                    MInst::Store {
                        base: BaseReg::SimState,
                        offset: 8,
                        src: VReg(0),
                        size: OpSize::S64,
                    },
                    MInst::Return,
                ],
            }],
            Vec::new(),
        );
        let homes = [(VReg(0), 0)].into_iter().collect();
        function.spill_homes.insert(VReg(0), 0);
        let mut next_value = 1;

        spill_values(&mut function, &homes, &mut next_value).unwrap();

        assert_eq!(
            function.blocks[0]
                .insts
                .iter()
                .filter(|instruction| matches!(
                    instruction,
                    MInst::Load {
                        base: BaseReg::StackFrame,
                        ..
                    }
                ))
                .count(),
            0
        );
        allocate_without_spills(function).unwrap();
    }

    #[test]
    fn colors_disjoint_pressure_waves_into_shared_stack_slots() {
        let mut instructions = Vec::new();
        for wave in 0..2_u32 {
            let first = wave * 26;
            instructions.extend((0..26).map(|value| MInst::LoadImm {
                dst: VReg(first + value),
                value: u64::from(first + value),
            }));
            instructions.extend((0..26).map(|value| MInst::Store {
                base: BaseReg::SimState,
                offset: i32::try_from((first + value) * 8).unwrap(),
                src: VReg(first + value),
                size: OpSize::S64,
            }));
        }
        instructions.push(MInst::Return);
        let allocation = allocate_with_spills(MFunction::new(
            vec![MBlock {
                id: BlockId(0),
                phis: Vec::new(),
                insts: instructions,
            }],
            Vec::new(),
        ))
        .unwrap();

        let spill_count = allocation.allocated.function.spill_homes.len();
        let slot_count = allocation.spill_frame_size / 8;
        assert!(spill_count > usize::try_from(slot_count).unwrap());
        verify_allocated(&allocation.allocated).unwrap();
    }

    #[test]
    fn keeps_external_phi_register_sources_live_to_the_edge() {
        let mut function = MFunction::new(
            vec![
                MBlock {
                    id: BlockId(0),
                    phis: Vec::new(),
                    insts: vec![
                        MInst::LoadImm {
                            dst: VReg(0),
                            value: 42,
                        },
                        MInst::Jump { target: BlockId(1) },
                    ],
                },
                MBlock {
                    id: BlockId(1),
                    phis: vec![PhiNode {
                        dst: VReg(1),
                        sources: vec![(BlockId(0), VReg(0))],
                    }],
                    insts: vec![
                        MInst::Store {
                            base: BaseReg::SimState,
                            offset: 0,
                            src: VReg(1),
                            size: OpSize::S64,
                        },
                        MInst::Return,
                    ],
                },
            ],
            Vec::new(),
        );
        let homes = [(VReg(1), 8)].into_iter().collect();
        let mut next_value = 2;
        function.spill_homes.insert(VReg(1), 8);

        spill_values(&mut function, &homes, &mut next_value).unwrap();

        let facts = build_facts(&function).unwrap();
        let intervals = analyze_live_intervals(&facts).unwrap();
        assert!(intervals.get(&VReg(0)).is_some());
        assert!(intervals.get(&VReg(1)).is_none());
        assert!(matches!(
            function.blocks[0].insts.as_slice(),
            [
                MInst::LoadImm { .. },
                MInst::KeepAlive { src: VReg(0) },
                MInst::Jump { .. }
            ]
        ));
        assert!(matches!(
            function.blocks[1].insts.first(),
            Some(MInst::Load {
                base: BaseReg::StackFrame,
                offset: 8,
                ..
            })
        ));
        assert_eq!(function.spilled_phis.len(), 1);
        let allocated = allocate_without_spills(function).unwrap();
        let source = allocated.assignment.get(&VReg(0)).unwrap();
        assert_eq!(
            allocated.edge_copies.edge(BlockId(0), BlockId(1)),
            Some(
                [CopyOperation::Move {
                    destination: CopyDestination::Stack(8),
                    source: CopySource::Register(source),
                }]
                .as_slice()
            )
        );
    }

    #[test]
    fn uses_preserved_registers_above_caller_saved_pressure() {
        let mut instructions = (0..16)
            .map(|value| MInst::LoadImm {
                dst: VReg(value),
                value: u64::from(value),
            })
            .collect::<Vec<_>>();
        instructions.extend((0..16).map(|value| MInst::Store {
            base: BaseReg::SimState,
            offset: value * 8,
            src: VReg(value as u32),
            size: OpSize::S64,
        }));
        instructions.push(MInst::Return);
        let allocated = allocate_without_spills(MFunction::new(
            vec![MBlock {
                id: BlockId(0),
                phis: Vec::new(),
                insts: instructions,
            }],
            Vec::new(),
        ))
        .unwrap();

        assert!(
            allocated
                .assignment
                .iter()
                .any(|(_, register)| (19..=28).contains(&register.number()))
        );
    }
}
