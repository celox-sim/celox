//! AArch64 register-allocation boundary.
//!
//! Target MIR is projected into opcode-free facts consumed by shared analyses,
//! then spilled and colored against the AArch64 register file. Phi lowering and
//! edge-copy scheduling also remain target-owned; only opcode-free allocation
//! analyses and algorithms are shared.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};
use std::fmt;

use celox_backend_common::regalloc::{
    BlockAllocationFacts, CompactLiveIntervals as LiveIntervals, CompactSegments,
    FunctionAllocationFacts, InstructionAllocationFacts, PhiAllocationFacts, PhiSource,
    analyze_compact_live_intervals as analyze_live_intervals,
    color_stack_slots_with_storage as color_stack_slots,
};

use crate::allocation::{Assignment, CopyDestination, CopyOperation, CopySource, EdgeCopyPlan};
use crate::mir::{AllocatedFunction, BlockId, MFunction, MInst, VReg};
use crate::{Arm64Reg, HashMap};

mod rematerialize;

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
    Cancelled,
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
            Self::Cancelled => formatter.write_str("compilation was cancelled"),
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

#[cfg(test)]
pub(crate) fn verify_allocated(function: &AllocatedFunction) -> Result<(), TargetRegallocError> {
    let facts = build_facts(&function.function)?;
    let intervals = analyze_live_intervals(&facts)
        .map_err(|error| TargetRegallocError::InvalidFacts(error.to_string()))?;

    verify_allocated_with_intervals(function, &intervals)
}

fn verify_allocated_with_intervals(
    function: &AllocatedFunction,
    intervals: &LiveIntervals<VReg>,
) -> Result<(), TargetRegallocError> {
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
                if !(1..=15).contains(&register.number()) && !(19..=27).contains(&register.number())
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
    verify_interval_registers(function, intervals)
}

/// The values live across each extern call, keyed by block and instruction
/// index: those live after the call that it does not define.
pub(crate) fn values_live_across_calls(
    function: &MFunction,
    intervals: &LiveIntervals<VReg>,
) -> Vec<((BlockId, usize), BTreeSet<VReg>)> {
    let mut calls = Vec::new();
    for (block_index, block) in function.blocks.iter().enumerate() {
        if !block
            .insts
            .iter()
            .any(|instruction| matches!(instruction, MInst::CallExtern { .. }))
        {
            continue;
        }
        let mut live = intervals.live_out(block_index).cloned().unwrap_or_default();
        for (index, instruction) in block.insts.iter().enumerate().rev() {
            if let Some(def) = instruction.def() {
                live.remove(&def);
            }
            if matches!(instruction, MInst::CallExtern { .. }) {
                calls.push(((block.id, index), live.clone()));
            }
            live.extend(instruction.uses());
        }
    }
    calls
}

/// [`values_live_across_calls`] for allocated MIR.
pub(crate) fn allocated_values_live_across_calls(
    function: &MFunction,
) -> Result<Vec<((BlockId, usize), BTreeSet<VReg>)>, TargetRegallocError> {
    let facts = build_facts(function)?;
    let intervals = analyze_live_intervals(&facts)
        .map_err(|error| TargetRegallocError::InvalidFacts(error.to_string()))?;
    Ok(values_live_across_calls(function, &intervals))
}

/// Registers a C function preserves, which values live across an extern call
/// try first.
const CALL_PRESERVED_REGISTERS: [Arm64Reg; 9] = [
    Arm64Reg::new(19),
    Arm64Reg::new(20),
    Arm64Reg::new(21),
    Arm64Reg::new(22),
    Arm64Reg::new(23),
    Arm64Reg::new(24),
    Arm64Reg::new(25),
    Arm64Reg::new(26),
    Arm64Reg::new(27),
];

const ALLOCATABLE_REGISTERS: [Arm64Reg; 24] = [
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
    is_cancelled: impl Fn() -> bool,
) -> Result<TargetAllocation, TargetRegallocError> {
    // Observed once per spill-retry round so a cancelled compile unwinds at
    // the next round instead of finishing the remaining iterations. Callers
    // without cancellation pass `|| false`, which folds away after inlining.
    if is_cancelled() {
        return Err(TargetRegallocError::Cancelled);
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
    // Spilling a shared constant must not force every phi fed by it into memory.
    // Short edge materializations remain ordinary, spillable SSA
    // values and participate in the same pressure checks.
    rematerialize::phi_constants(&mut function, &mut next_value)?;
    let initial_facts = build_facts(&function)?;
    // Spill rewriting preserves the CFG, so compute loop frequencies once,
    // rather than repeating dominance analysis for every allocation attempt.
    let frequencies = spill_frequencies(&function, &initial_facts);
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
    drop(initial_facts);
    let mut spill_frame_size = 0_u32;

    loop {
        if is_cancelled() {
            return Err(TargetRegallocError::Cancelled);
        }
        let facts = build_facts(&function)?;
        let intervals = analyze_live_intervals(&facts)
            .map_err(|error| TargetRegallocError::InvalidFacts(error.to_string()))?;
        drop(facts);
        let proactive = close_phi_spill_set(
            &function,
            select_spill_batch(&function, &intervals, &candidates, &frequencies, false),
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
        match color_intervals(&function, &intervals) {
            Ok(assignment) => {
                let allocated = finish_allocation(function, assignment, &intervals)?;
                return Ok(TargetAllocation {
                    allocated,
                    spill_frame_size,
                });
            }
            Err(TargetRegallocError::RegisterPressure { value }) => {
                let spilled = close_phi_spill_set(
                    &function,
                    select_spill_batch(&function, &intervals, &candidates, &frequencies, true),
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

fn spill_frequencies(function: &MFunction, facts: &AllocationFacts) -> BTreeMap<BlockId, u64> {
    let mut weights = vec![1_u64; function.blocks.len()];
    let successors = facts
        .blocks
        .iter()
        .map(|block| block.successors.clone())
        .collect();
    if let Ok(cfg) =
        celox_analysis::cfg::ForwardControlFlowGraph::analyze_structure(successors, facts.entry)
    {
        for region in cfg.loops {
            for block in region.blocks {
                // This estimates relative frequency, not a trip-count proof.
                // Cap nested loops so cold uses still contribute to the cost.
                weights[block] = (weights[block] * 8).min(4096);
            }
        }
    }
    function
        .blocks
        .iter()
        .zip(weights)
        .map(|(block, weight)| (block.id, weight))
        .collect()
}

/// Merge exact block-sorted intervals without copying the complete segment
/// table. The heap retains one position per value; callers reuse one block row.
struct BlockSegmentCursor<'a> {
    intervals: Vec<(VReg, &'a CompactSegments)>,
    next_segments: BinaryHeap<Reverse<(usize, usize, usize)>>,
}

impl<'a> BlockSegmentCursor<'a> {
    fn new(intervals: &'a LiveIntervals<VReg>) -> Self {
        let intervals = intervals
            .iter()
            .map(|(&value, interval)| (value, &interval.segments))
            .collect::<Vec<_>>();
        let next_segments = intervals
            .iter()
            .enumerate()
            .filter_map(|(index, (_, segments))| {
                segments
                    .first()
                    .map(|segment| Reverse((segment.block, index, 0)))
            })
            .collect();
        Self {
            intervals,
            next_segments,
        }
    }

    fn next(&mut self, segments: &mut Vec<(u64, u64, VReg)>) -> Option<usize> {
        segments.clear();
        let &Reverse((block, _, _)) = self.next_segments.peek()?;
        while let Some(&Reverse((next_block, index, position))) = self.next_segments.peek() {
            if next_block != block {
                break;
            }
            self.next_segments.pop();
            let (value, interval) = self.intervals[index];
            let segment = interval.get(position).expect("queued segment exists");
            segments.push((segment.start, segment.end, value));
            if let Some(next) = interval.get(position + 1) {
                self.next_segments
                    .push(Reverse((next.block, index, position + 1)));
            }
        }
        Some(block)
    }
}

fn select_spill_batch(
    function: &MFunction,
    intervals: &LiveIntervals<VReg>,
    candidates: &BTreeSet<VReg>,
    frequencies: &BTreeMap<BlockId, u64>,
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
    let mut use_counts = BTreeMap::<VReg, u64>::new();
    for block in &function.blocks {
        let weight = frequencies.get(&block.id).copied().unwrap_or(1);
        for phi in &block.phis {
            *use_counts.entry(phi.dst).or_default() += weight;
            for &(predecessor, source) in &phi.sources {
                *use_counts.entry(source).or_default() +=
                    frequencies.get(&predecessor).copied().unwrap_or(1);
            }
        }
        for instruction in &block.insts {
            if !matches!(instruction, MInst::KeepAlive { .. }) {
                for value in instruction.uses() {
                    *use_counts.entry(value).or_default() += weight;
                }
            }
            if let Some(value) = instruction.def() {
                *use_counts.entry(value).or_default() += weight;
            }
        }
    }
    let live_length = |value: VReg| live_lengths.get(&value).copied().unwrap_or(0);
    let compare_priority = |left: VReg, right: VReg| {
        let cost = |value| use_counts.get(&value).copied().unwrap_or(1);
        // Compare length/cost without truncating every hot value's score to
        // zero. u128 keeps the product exact even for large live intervals.
        (u128::from(live_length(left)) * u128::from(cost(right)))
            .cmp(&(u128::from(live_length(right)) * u128::from(cost(left))))
            .then_with(|| live_length(left).cmp(&live_length(right)))
            .then_with(|| right.cmp(&left))
    };
    // Reserve reload space for the common three-input operations and their
    // result. Wider pseudos are accounted for by the next allocation round,
    // instead of reducing every block's capacity for their worst case.
    let target_capacity = ALLOCATABLE_REGISTERS.len().saturating_sub(4);
    let mut selected = BTreeSet::new();
    let mut peak = Vec::new();
    // Merge the already block-sorted intervals one block at a time. Building
    // a second copy of every segment can consume gigabytes on large designs.
    let mut cursor = BlockSegmentCursor::new(intervals);
    let mut segments = Vec::new();
    while cursor.next(&mut segments).is_some() {
        segments.sort_unstable();
        let mut active = Vec::<(u64, VReg)>::new();
        for &(start, end, value) in &segments {
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
                    .max_by(|(_, left), (_, right)| compare_priority(*left, *right))
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
        .max_by(|left, right| compare_priority(*left, *right))
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
    let constants = function
        .blocks
        .iter()
        .flat_map(|block| &block.insts)
        .filter_map(|inst| match *inst {
            MInst::LoadImm { dst, value }
                if homes.contains_key(&dst)
                    && crate::scalar::is_single_instruction_constant(value) =>
            {
                Some((dst, value))
            }
            _ => None,
        })
        .collect::<BTreeMap<_, _>>();
    let all_homes = function.spill_homes.clone();
    let mut external_phis = Vec::new();
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
    let mut edge_keep_alives = BTreeMap::<BlockId, BTreeSet<VReg>>::new();
    for phi in &function.spilled_phis {
        for &(predecessor, source) in &phi.sources {
            if let crate::mir::SpilledPhiSource::Value(value) = source {
                edge_keep_alives
                    .entry(predecessor)
                    .or_default()
                    .insert(value);
            }
        }
    }
    for block in &mut function.blocks {
        let edge_uses = edge_keep_alives.remove(&block.id).unwrap_or_default();
        let original = std::mem::take(&mut block.insts)
            .into_iter()
            .filter(|inst| !matches!(inst, MInst::KeepAlive { src } if edge_uses.contains(src)))
            .collect::<Vec<_>>();
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
        let terminator_index = original.len().saturating_sub(1);
        let mut rewritten = Vec::with_capacity(original.len() + edge_uses.len());
        for (index, mut instruction) in original.into_iter().enumerate() {
            if matches!(instruction, MInst::KeepAlive { src } if homes.contains_key(&src)) {
                continue;
            }
            for spilled in spill_uses[index].iter().copied() {
                let reload = if let Some(&reload) = reload_cache.get(&spilled) {
                    reload
                } else {
                    let reload = fresh(next_value)?;
                    // Keep the same def/use positions and phi spill homes,
                    // but recover cheap constants without a memory read.
                    rewritten.push(if let Some(&value) = constants.get(&spilled) {
                        MInst::LoadImm { dst: reload, value }
                    } else {
                        MInst::Load {
                            dst: reload,
                            base: crate::mir::BaseReg::StackFrame,
                            offset: homes[&spilled],
                            size: crate::mir::OpSize::S64,
                        }
                    });
                    reload_cache.insert(spilled, reload);
                    reload
                };
                instruction.rewrite_use(spilled, reload);
            }
            if index == terminator_index {
                // Edge copies execute after the branch decision. Keep their
                // register inputs live across every terminator reload,
                // including reloads inserted by later spill rounds.
                rewritten.extend(
                    edge_uses
                        .iter()
                        .copied()
                        .map(|src| MInst::KeepAlive { src }),
                );
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
            if let Some((spilled, temporary)) = definition_cache {
                reload_cache.insert(spilled, temporary);
            }
            // An induction value can feed several addresses separated by
            // loads and masks. Keep one such reload across a short gap, in
            // addition to operands used by the very next instruction. The
            // bounds limit the extra pressure from unspillable reloads.
            let lookahead_end = spill_uses.len().min(index + 9);
            let reuse_after_gap = reload_cache
                .keys()
                .filter(|value| !next_uses.is_some_and(|uses| uses.contains(value)))
                .filter_map(|&value| {
                    spill_uses[index + 1..lookahead_end]
                        .iter()
                        .position(|uses| uses.contains(&value))
                        .map(|distance| (distance, value))
                })
                .min()
                .map(|(_, value)| value);
            reload_cache.retain(|value, _| {
                next_uses.is_some_and(|uses| uses.contains(value))
                    || reuse_after_gap == Some(*value)
            });
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
#[cfg(test)]
pub(crate) fn allocate_without_spills(
    function: MFunction,
) -> Result<AllocatedFunction, TargetRegallocError> {
    let facts = build_facts(&function)?;
    let intervals = analyze_live_intervals(&facts)
        .map_err(|error| TargetRegallocError::InvalidFacts(error.to_string()))?;
    let assignment = color_intervals(&function, &intervals)?;
    finish_allocation(function, assignment, &intervals)
}

// Coloring and verification inspect the same unchanged MIR. Reuse its liveness
// instead of retaining multiple copies of the largest allocation data structure.
fn finish_allocation(
    function: MFunction,
    assignment: Assignment<VReg>,
    intervals: &LiveIntervals<VReg>,
) -> Result<AllocatedFunction, TargetRegallocError> {
    let edge_copies = build_edge_copies(&function, &assignment)?;
    let allocated = AllocatedFunction {
        function,
        assignment,
        edge_copies,
    };
    verify_allocated_with_intervals(&allocated, intervals)?;
    Ok(allocated)
}

fn color_intervals(
    function: &MFunction,
    intervals: &LiveIntervals<VReg>,
) -> Result<Assignment<VReg>, TargetRegallocError> {
    let mut interference = intervals
        .iter()
        .map(|(&value, _)| (value, BTreeSet::new()))
        .collect::<BTreeMap<_, _>>();
    // Keep only the current block's sweep input. Duplicating all exact
    // intervals here can exceed the memory used by liveness itself.
    let mut cursor = BlockSegmentCursor::new(intervals);
    let mut segments = Vec::new();
    while cursor.next(&mut segments).is_some() {
        // Cursor rows are grouped by value before sorting. Preserve the
        // historical first-segment semantics for repeated block entries.
        segments.dedup_by_key(|segment| segment.2);
        segments.sort_unstable();
        let mut active = Vec::<(u64, VReg)>::new();
        for &(start, end, value) in &segments {
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
        for instruction in &block.insts {
            if let MInst::Mov { dst, src } | MInst::BitInsert { dst, base: src, .. } = *instruction
            {
                // BFI updates its base operand. Reusing that register avoids
                // a move when the base dies here; interference still protects
                // both operands if either value remains live afterwards.
                affinities.entry(dst).or_default().insert(src);
                affinities.entry(src).or_default().insert(dst);
            }
        }
    }
    let mut order = interference
        .iter()
        .map(|(&value, neighbors)| (Reverse(neighbors.len()), value))
        .collect::<Vec<_>>();
    order.sort_unstable();
    // The emitter saves a caller-saved register around every call it lives
    // across, so such values try the registers a C function preserves first.
    let crosses_call = values_live_across_calls(function, intervals)
        .into_iter()
        .flat_map(|(_, values)| values)
        .collect::<BTreeSet<_>>();

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
        let free = |registers: &[Arm64Reg]| {
            registers
                .iter()
                .copied()
                .find(|register| !used.contains(register))
        };
        let register = preferred
            .filter(|register| {
                !crosses_call.contains(&value) || CALL_PRESERVED_REGISTERS.contains(register)
            })
            .or_else(|| {
                crosses_call
                    .contains(&value)
                    .then(|| free(&CALL_PRESERVED_REGISTERS))
                    .flatten()
            })
            .or(preferred)
            .or_else(|| free(&ALLOCATABLE_REGISTERS))
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
    intervals: &LiveIntervals<VReg>,
) -> Result<(), TargetRegallocError> {
    let mut cursor = BlockSegmentCursor::new(intervals);
    let mut segments = Vec::new();
    let mut registered = Vec::new();
    while let Some(block) = cursor.next(&mut segments) {
        registered.clear();
        registered.extend(segments.iter().filter_map(|&(start, end, value)| {
            // Legacy edge values may reside in a stack home or immediate.
            function
                .assignment
                .get(&value)
                .map(|register| (register, start, end, value))
        }));
        registered.sort_unstable();
        let mut previous = None;
        for &(register, start, end, value) in &registered {
            if let Some((left_register, _, left_end, left)) = previous
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
            previous = Some((register, start, end, value));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod cancellation_tests {
    use super::*;
    use crate::mir::{MBlock, MFunction, MInst};

    #[test]
    fn cancelled_allocation_reports_cancellation_before_spilling() {
        let mut instructions = Vec::new();
        for value in 0..8_u32 {
            instructions.push(MInst::LoadImm {
                dst: VReg(value),
                value: u64::from(value),
            });
        }
        instructions.push(MInst::Return);
        let function = MFunction::new(
            vec![MBlock {
                id: BlockId(0),
                phis: Vec::new(),
                insts: instructions,
            }],
            Vec::new(),
        );

        let error = allocate_with_spills(function, || true)
            .err()
            .expect("a cancelled allocation must not return a result");

        assert!(matches!(error, TargetRegallocError::Cancelled));
    }
}
