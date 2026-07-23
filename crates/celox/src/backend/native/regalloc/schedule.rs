//! Pressure-aware scheduling of side-effect-free machine DAG regions.

use std::collections::{BTreeSet, HashMap};
use std::fmt;

use celox_analysis::dependence::MemoryDependencyTracker;

use crate::backend::native::memory_effect::{self, MemoryObject, analysis_effects};
use crate::backend::native::mir::{BlockId, MFunction, MInst, VReg};

use super::analysis::AnalysisResult;
use super::cfg::NormalizedCfg;
use super::constraints::ConstraintModel;

#[derive(Debug, Default)]
pub(super) struct ScheduleStats {
    pub changed_blocks: usize,
    pub maximum_before: usize,
    pub maximum_after: usize,
    /// Instructions visited while deriving every region's live-after state.
    /// This is exactly one visit per block instruction.
    pub backward_liveness_steps: usize,
    pub regions_considered: usize,
    pub pressure_rejections: usize,
    pub ready_insertions: usize,
    pub ready_pops: usize,
    pub priority_computations: usize,
    pub priority_updates: usize,
    pub priority_bucket_probes: usize,
    pub priority_value_index_visits: usize,
}

#[derive(Debug)]
pub(super) struct ScheduleError {
    pub rule: &'static str,
    pub block: BlockId,
    pub reason: &'static str,
}

impl fmt::Display for ScheduleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "pressure scheduling failed in {}: {}",
            self.block, self.reason
        )
    }
}

impl std::error::Error for ScheduleError {}

#[derive(Debug, Default)]
struct RegionWork {
    ready_insertions: usize,
    ready_pops: usize,
    priority_computations: usize,
    priority_updates: usize,
    priority_bucket_probes: usize,
    priority_value_index_visits: usize,
}

impl ScheduleStats {
    fn add_region_work(&mut self, work: &RegionWork) {
        self.ready_insertions += work.ready_insertions;
        self.ready_pops += work.ready_pops;
        self.priority_computations += work.priority_computations;
        self.priority_updates += work.priority_updates;
        self.priority_bucket_probes += work.priority_bucket_probes;
        self.priority_value_index_visits += work.priority_value_index_visits;
    }
}

pub(super) fn schedule_for_pressure(
    func: &mut MFunction,
    cfg: &NormalizedCfg,
    constraints: &ConstraintModel,
    analysis: &AnalysisResult,
) -> Result<ScheduleStats, ScheduleError> {
    let fallback_block = func.blocks.first().map_or(BlockId(0), |block| block.id);
    if cfg.predecessors.len() != func.blocks.len()
        || constraints.instructions.len() != func.blocks.len()
        || analysis.exit_distances.len() != func.blocks.len()
    {
        return Err(ScheduleError {
            rule: "SCHEDULE.MODEL_SHAPE",
            block: fallback_block,
            reason: "CFG, constraint, or liveness tables do not cover every MIR block",
        });
    }
    if let Some((block, _)) =
        func.blocks.iter().enumerate().find(|(block, mir_block)| {
            constraints.instructions[*block].len() != mir_block.insts.len()
        })
    {
        return Err(ScheduleError {
            rule: "SCHEDULE.MODEL_SHAPE",
            block: func.blocks[block].id,
            reason: "instruction constraints do not cover every MIR instruction",
        });
    }
    let mut stats = ScheduleStats::default();
    for block_index in 0..func.blocks.len() {
        let original = func.blocks[block_index].insts.clone();
        let live_out = analysis.exit_distances[block_index]
            .keys()
            .copied()
            .collect::<BTreeSet<_>>();
        let original_pressure = max_pressure(&original, &live_out);
        stats.maximum_before = stats.maximum_before.max(original_pressure);
        let scheduled = schedule_block(
            &original,
            &constraints.instructions[block_index],
            &live_out,
            &mut stats,
        )
        .map_err(|reason| ScheduleError {
            rule: "SCHEDULE.DEPENDENCY_ORDER",
            block: func.blocks[block_index].id,
            reason,
        })?;
        let scheduled_pressure = max_pressure(&scheduled, &live_out);
        if scheduled_pressure <= original_pressure {
            if scheduled != original {
                stats.changed_blocks += 1;
            }
            func.blocks[block_index].insts = scheduled;
            stats.maximum_after = stats.maximum_after.max(scheduled_pressure);
        } else {
            stats.pressure_rejections += 1;
            stats.maximum_after = stats.maximum_after.max(original_pressure);
        }
    }
    Ok(stats)
}

fn schedule_block(
    instructions: &[MInst],
    constraints: &[super::constraints::InstructionConstraints],
    live_out: &BTreeSet<VReg>,
    stats: &mut ScheduleStats,
) -> Result<Vec<MInst>, &'static str> {
    if instructions.len() != constraints.len() {
        return Err("instruction constraint model shape mismatch");
    }

    enum ReverseChunk {
        Barrier(MInst),
        Region(Vec<MInst>),
    }

    // Discover and schedule pure regions while propagating one liveness state
    // from the block exit. The old implementation rebuilt this state by
    // rescanning the complete suffix once per region. Scheduling a DAG in
    // reverse topological order performs the region's backward transfer, so
    // no live-set clone is required at a region boundary either.
    let mut live = live_out.clone();
    let mut reverse_chunks = Vec::<ReverseChunk>::new();
    let mut cursor = instructions.len();
    while cursor != 0 {
        let last = cursor - 1;
        if !is_schedulable_at(instructions, constraints, last) {
            transfer_liveness(&instructions[last], &mut live);
            stats.backward_liveness_steps += 1;
            reverse_chunks.push(ReverseChunk::Barrier(instructions[last].clone()));
            cursor = last;
            continue;
        }

        let end = cursor;
        let mut start = last;
        while start != 0 && is_schedulable_at(instructions, constraints, start - 1) {
            start -= 1;
        }
        let scheduled = schedule_region(&instructions[start..end], live);
        stats.regions_considered += 1;
        stats.backward_liveness_steps += end - start;
        stats.add_region_work(&scheduled.work);
        if !scheduled.dependency_verified {
            return Err("candidate order violates the instruction dependency DAG");
        }
        let Some(live_before) = scheduled.live_before else {
            return Err("candidate schedule did not produce a live-in state");
        };
        live = live_before;
        reverse_chunks.push(ReverseChunk::Region(scheduled.instructions));
        cursor = start;
    }

    let mut result = Vec::with_capacity(instructions.len());
    for chunk in reverse_chunks.into_iter().rev() {
        match chunk {
            ReverseChunk::Barrier(inst) => result.push(inst),
            ReverseChunk::Region(region) => result.extend(region),
        }
    }
    Ok(result)
}

fn transfer_liveness(inst: &MInst, live: &mut BTreeSet<VReg>) {
    if let Some(definition) = inst.def() {
        live.remove(&definition);
    }
    live.extend(inst.uses());
}

struct RegionSchedule {
    instructions: Vec<MInst>,
    dependency_verified: bool,
    live_before: Option<BTreeSet<VReg>>,
    work: RegionWork,
}

fn schedule_region(region: &[MInst], mut live: BTreeSet<VReg>) -> RegionSchedule {
    if region.len() < 2 {
        if let Some(inst) = region.first() {
            transfer_liveness(inst, &mut live);
        }
        return RegionSchedule {
            instructions: region.to_vec(),
            dependency_verified: true,
            live_before: Some(live),
            work: RegionWork::default(),
        };
    }
    let definitions = region
        .iter()
        .enumerate()
        .filter_map(|(index, inst)| inst.def().map(|value| (value, index)))
        .collect::<HashMap<_, _>>();
    let unique_uses = region
        .iter()
        .map(|inst| {
            let mut result = Vec::with_capacity(inst.uses().len());
            for value in inst.uses() {
                if !result.contains(&value) {
                    result.push(value);
                }
            }
            result
        })
        .collect::<Vec<_>>();
    let mut dependencies = vec![Vec::<usize>::new(); region.len()];
    let mut users = vec![0usize; region.len()];
    let mut use_candidates = HashMap::<VReg, Vec<usize>>::new();
    for (user, uses) in unique_uses.iter().enumerate() {
        for &used in uses {
            use_candidates.entry(used).or_default().push(user);
            if let Some(&definition) = definitions.get(&used) {
                add_dependency(&mut dependencies, &mut users, user, definition);
            }
        }
    }

    // Preserve RAW, WAR, and WAW without inventing read-after-read edges.
    // The sparse interval partition scales with effect endpoints rather than
    // the physical byte length of wide state ranges.
    let mut memory = MemoryDependencyTracker::<MemoryObject, usize>::default();
    for (instruction, inst) in region.iter().enumerate() {
        let mut memory_dependencies = BTreeSet::new();
        let reads = memory_effect::reads(inst);
        let writes = memory_effect::writes(inst);
        memory.add_event(
            instruction,
            analysis_effects(&reads),
            analysis_effects(&writes),
            &mut memory_dependencies,
        );
        for dependency in memory_dependencies {
            add_dependency(&mut dependencies, &mut users, instruction, dependency);
        }
    }

    let Some(dependency_priorities) = dependency_priorities(&dependencies) else {
        return RegionSchedule {
            instructions: Vec::new(),
            dependency_verified: false,
            live_before: None,
            work: RegionWork::default(),
        };
    };
    let mut work = RegionWork::default();
    let mut ready = IndexedReadyQueue::new(dependency_priorities);
    for (index, &count) in users.iter().enumerate() {
        if count == 0 {
            enqueue_ready(&mut ready, index, region, &unique_uses, &live, &mut work);
        }
    }
    let mut reverse = Vec::with_capacity(region.len());
    while reverse.len() != region.len() {
        let Some(candidate) = ready.pop_best(live.len(), super::NUM_REGS, &mut work) else {
            return RegionSchedule {
                instructions: Vec::new(),
                dependency_verified: false,
                live_before: None,
                work,
            };
        };
        let inst = &region[candidate];
        if let Some(definition) = inst.def()
            && live.remove(&definition)
        {
            update_priorities_for_value(
                definition,
                false,
                &use_candidates,
                &definitions,
                &mut ready,
                &mut work,
            );
        }
        for &value in &unique_uses[candidate] {
            if live.insert(value) {
                update_priorities_for_value(
                    value,
                    true,
                    &use_candidates,
                    &definitions,
                    &mut ready,
                    &mut work,
                );
            }
        }
        for &dependency in &dependencies[candidate] {
            users[dependency] -= 1;
            if users[dependency] == 0 {
                enqueue_ready(
                    &mut ready,
                    dependency,
                    region,
                    &unique_uses,
                    &live,
                    &mut work,
                );
            }
        }
        reverse.push(candidate);
    }
    reverse.reverse();
    let dependency_verified = dependency_order_valid(&dependencies, &reverse);
    let instructions = if dependency_verified {
        reverse.iter().map(|&index| region[index].clone()).collect()
    } else {
        Default::default()
    };
    RegionSchedule {
        instructions,
        dependency_verified,
        live_before: dependency_verified.then_some(live),
        work,
    }
}

fn add_dependency(
    dependencies: &mut [Vec<usize>],
    users: &mut [usize],
    instruction: usize,
    dependency: usize,
) {
    if instruction != dependency && !dependencies[instruction].contains(&dependency) {
        dependencies[instruction].push(dependency);
        users[dependency] += 1;
    }
}

fn dependency_order_valid(dependencies: &[Vec<usize>], order: &[usize]) -> bool {
    if dependencies.len() != order.len() {
        return false;
    }
    let mut positions = vec![usize::MAX; order.len()];
    for (position, &instruction) in order.iter().enumerate() {
        if instruction >= positions.len() || positions[instruction] != usize::MAX {
            return false;
        }
        positions[instruction] = position;
    }
    dependencies.iter().enumerate().all(|(user, definitions)| {
        definitions
            .iter()
            .all(|definition| positions[*definition] < positions[user])
    })
}

/// Compute longest dependency paths in both directions. The bottom-up list
/// scheduler uses these ranks to expose independent work while register
/// pressure is below the target's physical capacity.
fn dependency_priorities(dependencies: &[Vec<usize>]) -> Option<Vec<(usize, usize)>> {
    let mut entry_depths = vec![1usize; dependencies.len()];
    for (instruction, inputs) in dependencies.iter().enumerate() {
        let mut depth = 1usize;
        for &input in inputs {
            if input >= instruction {
                return None;
            }
            depth = depth.max(entry_depths[input].checked_add(1)?);
        }
        entry_depths[instruction] = depth;
    }
    let mut exit_depths = vec![1usize; dependencies.len()];
    for (instruction, inputs) in dependencies.iter().enumerate().rev() {
        let successor_depth = exit_depths[instruction].checked_add(1)?;
        for &input in inputs {
            exit_depths[input] = exit_depths[input].max(successor_depth);
        }
    }
    Some(exit_depths.into_iter().zip(entry_depths).collect())
}

fn enqueue_ready(
    ready: &mut IndexedReadyQueue,
    instruction: usize,
    region: &[MInst],
    unique_uses: &[Vec<VReg>],
    live: &BTreeSet<VReg>,
    work: &mut RegionWork,
) {
    let priority = priority(region[instruction].def(), &unique_uses[instruction], live);
    work.priority_computations += 1;
    ready.insert(instruction, priority, work);
}

fn priority(definition: Option<VReg>, uses: &[VReg], live: &BTreeSet<VReg>) -> i8 {
    let missing_uses = uses.iter().filter(|value| !live.contains(value)).count() as i8;
    let live_definition = i8::from(definition.is_some_and(|value| live.contains(&value)));
    missing_uses - live_definition
}

fn update_priorities_for_value(
    value: VReg,
    became_live: bool,
    use_candidates: &HashMap<VReg, Vec<usize>>,
    definitions: &HashMap<VReg, usize>,
    ready: &mut IndexedReadyQueue,
    work: &mut RegionWork,
) {
    let delta = if became_live { -1 } else { 1 };
    if let Some(candidates) = use_candidates.get(&value) {
        for &candidate in candidates {
            work.priority_value_index_visits += 1;
            if ready.contains(candidate) {
                ready.adjust(candidate, delta, work);
            }
        }
    }
    if let Some(&candidate) = definitions.get(&value) {
        work.priority_value_index_visits += 1;
        if ready.contains(candidate) {
            ready.adjust(candidate, delta, work);
        }
    }
}

const MIN_PRIORITY: i8 = -1;
const MAX_PRIORITY: i8 = 5;
const PRIORITY_BUCKETS: usize = (MAX_PRIORITY - MIN_PRIORITY + 1) as usize;

struct IndexedReadyQueue {
    buckets: [BTreeSet<(usize, usize, usize)>; PRIORITY_BUCKETS],
    priorities: Vec<i8>,
    dependency_priorities: Vec<(usize, usize)>,
    present: Vec<bool>,
}

impl IndexedReadyQueue {
    fn new(dependency_priorities: Vec<(usize, usize)>) -> Self {
        let instructions = dependency_priorities.len();
        Self {
            buckets: std::array::from_fn(|_| BTreeSet::new()),
            priorities: vec![0; instructions],
            dependency_priorities,
            present: vec![false; instructions],
        }
    }

    fn contains(&self, instruction: usize) -> bool {
        self.present[instruction]
    }

    fn insert(&mut self, instruction: usize, priority: i8, work: &mut RegionWork) {
        debug_assert!(!self.present[instruction]);
        let bucket = priority_bucket(priority);
        let (exit_depth, entry_depth) = self.dependency_priorities[instruction];
        self.buckets[bucket].insert((exit_depth, entry_depth, instruction));
        self.priorities[instruction] = priority;
        self.present[instruction] = true;
        work.ready_insertions += 1;
    }

    fn adjust(&mut self, instruction: usize, delta: i8, work: &mut RegionWork) {
        debug_assert!(self.present[instruction]);
        let old_priority = self.priorities[instruction];
        let (exit_depth, entry_depth) = self.dependency_priorities[instruction];
        let key = (exit_depth, entry_depth, instruction);
        self.buckets[priority_bucket(old_priority)].remove(&key);
        let new_priority = old_priority + delta;
        self.buckets[priority_bucket(new_priority)].insert(key);
        self.priorities[instruction] = new_priority;
        work.priority_updates += 1;
    }

    fn pop_best(
        &mut self,
        current_pressure: usize,
        register_capacity: usize,
        work: &mut RegionWork,
    ) -> Option<usize> {
        let mut pressure_best = None::<(usize, usize, usize, usize)>;
        let mut dependency_best = None::<(usize, usize, usize, usize)>;
        for (bucket_index, bucket) in self.buckets.iter().enumerate() {
            work.priority_bucket_probes += 1;
            if let Some(&(exit_depth, entry_depth, instruction)) = bucket.iter().next_back() {
                pressure_best.get_or_insert((exit_depth, entry_depth, bucket_index, instruction));
                if dependency_best.is_none_or(|(best_exit, best_entry, _, _)| {
                    (exit_depth, entry_depth) > (best_exit, best_entry)
                }) {
                    dependency_best = Some((exit_depth, entry_depth, bucket_index, instruction));
                }
            }
        }
        let dependency_best = dependency_best?;
        let dependency_delta = self.priorities[dependency_best.3];
        let dependency_pressure = projected_pressure(current_pressure, dependency_delta);
        let selected =
            if current_pressure <= register_capacity && dependency_pressure <= register_capacity {
                dependency_best
            } else {
                pressure_best.expect("a dependency-ready instruction is also pressure-ready")
            };
        let (exit_depth, entry_depth, bucket, instruction) = selected;
        self.buckets[bucket].remove(&(exit_depth, entry_depth, instruction));
        self.present[instruction] = false;
        work.ready_pops += 1;
        Some(instruction)
    }
}

fn projected_pressure(current: usize, delta: i8) -> usize {
    if delta >= 0 {
        current.saturating_add(delta as usize)
    } else {
        current.saturating_sub(delta.unsigned_abs() as usize)
    }
}

fn priority_bucket(priority: i8) -> usize {
    debug_assert!((MIN_PRIORITY..=MAX_PRIORITY).contains(&priority));
    (priority - MIN_PRIORITY) as usize
}

fn max_pressure(instructions: &[MInst], live_out: &BTreeSet<VReg>) -> usize {
    let mut live = live_out.clone();
    let mut maximum = live.len();
    for inst in instructions.iter().rev() {
        if let Some(definition) = inst.def() {
            live.remove(&definition);
        }
        live.extend(inst.uses());
        maximum = maximum.max(live.len());
    }
    maximum
}

fn is_schedulable_at(
    instructions: &[MInst],
    constraints: &[super::constraints::InstructionConstraints],
    index: usize,
) -> bool {
    let inst = &instructions[index];
    let facts = &constraints[index];
    let is_fixed_copy = inst.def().is_some_and(|definition| {
        constraints.get(index + 1).is_some_and(|next| {
            next.fixed_uses
                .iter()
                .any(|(value, _)| *value == definition)
        })
    });
    !is_fixed_copy
        && facts.fixed_uses.is_empty()
        && facts.clobbers.is_empty()
        && is_pressure_schedulable_kind(inst)
}

/// Classify every MIR opcode explicitly so adding a new width or side effect
/// cannot silently turn it into a scheduling barrier (or make it movable).
fn is_pressure_schedulable_kind(inst: &MInst) -> bool {
    match inst {
        MInst::Mov { .. }
        | MInst::Mov32 { .. }
        | MInst::LoadImm { .. }
        | MInst::Scratch { .. }
        | MInst::LoadConstantTableAddr { .. }
        | MInst::Load { .. }
        | MInst::LoadIndexed { .. }
        | MInst::PackedLaneCompare { .. }
        | MInst::Store { .. }
        | MInst::StoreIndexed { .. }
        | MInst::OrStoreIndexed { .. }
        | MInst::MemFill { .. }
        | MInst::SparseMarkActive { .. }
        | MInst::Add { .. }
        | MInst::Add32 { .. }
        | MInst::Sub { .. }
        | MInst::Sub32 { .. }
        | MInst::Mul { .. }
        | MInst::Mul32 { .. }
        | MInst::And { .. }
        | MInst::And32 { .. }
        | MInst::Or { .. }
        | MInst::Or32 { .. }
        | MInst::Xor { .. }
        | MInst::Xor32 { .. }
        | MInst::Shr { .. }
        | MInst::Shl { .. }
        | MInst::Sar { .. }
        | MInst::AndImm { .. }
        | MInst::AndImm32 { .. }
        | MInst::OrImm { .. }
        | MInst::ShrImm { .. }
        | MInst::ShlImm { .. }
        | MInst::SarImm { .. }
        | MInst::AddImm { .. }
        | MInst::SubImm { .. }
        | MInst::Cmp { .. }
        | MInst::CmpImm { .. }
        | MInst::BitNot { .. }
        | MInst::Neg { .. }
        | MInst::Popcnt { .. }
        | MInst::Bsf { .. }
        | MInst::Bsr { .. }
        | MInst::BsrOr { .. }
        | MInst::Pext { .. }
        | MInst::Pdep { .. }
        | MInst::Select { .. }
        | MInst::CmpSelect { .. }
        | MInst::CmpImmSelect { .. }
        | MInst::GuardedCmpSelect { .. } => true,
        MInst::LoadPtr { .. }
        | MInst::StorePtr { .. }
        | MInst::ReleaseStorePtr { .. }
        | MInst::LoadPtrIndexed { .. }
        | MInst::StorePtrIndexed { .. }
        | MInst::ReleaseStorePtrIndexed { .. }
        | MInst::MemCopy { .. }
        | MInst::SparseCommit { .. }
        | MInst::SparseCommitWorklist { .. }
        | MInst::UMulHi { .. }
        | MInst::UDiv { .. }
        | MInst::URem { .. }
        | MInst::SDiv { .. }
        | MInst::SRem { .. }
        | MInst::Branch { .. }
        | MInst::Jump { .. }
        | MInst::Return
        | MInst::ReturnError { .. } => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::native::mir::{
        BaseReg, BlockId, MBlock, MemoryAliasRange, OpSize, SpillDesc, VRegAllocator,
    };
    #[test]
    fn indexed_buckets_do_not_scan_a_long_ready_set() {
        const INSTRUCTIONS: usize = 4096;
        let region = (0..INSTRUCTIONS)
            .map(|index| MInst::LoadImm {
                dst: VReg(index as u32),
                value: index as u64,
            })
            .collect::<Vec<_>>();

        let scheduled = schedule_region(&region, BTreeSet::new());

        assert!(scheduled.dependency_verified);
        assert_eq!(scheduled.instructions, region);
        assert_eq!(scheduled.work.ready_insertions, INSTRUCTIONS);
        assert_eq!(scheduled.work.ready_pops, INSTRUCTIONS);
        assert_eq!(scheduled.work.priority_computations, INSTRUCTIONS);
        assert_eq!(scheduled.work.priority_updates, 0);
        assert_eq!(scheduled.work.priority_value_index_visits, 0);
        // Every pop probes only the fixed number of score buckets. The former
        // min-by-key implementation inspected a shrinking ready set and did
        // INSTRUCTIONS * (INSTRUCTIONS + 1) / 2 candidate evaluations here.
        assert!(
            scheduled.work.priority_bucket_probes <= INSTRUCTIONS * PRIORITY_BUCKETS,
            "bucket probes must be O(region length)"
        );
    }

    #[test]
    fn memory_dependent_block_uses_one_backward_liveness_pass() {
        const REGIONS: usize = 512;
        let mut vregs = VRegAllocator::new();
        let mut instructions = Vec::with_capacity(REGIONS * 2 + 1);
        for value in 0..REGIONS {
            let register = vregs.alloc();
            instructions.push(MInst::LoadImm {
                dst: register,
                value: value as u64,
            });
            instructions.push(MInst::Store {
                base: BaseReg::StackFrame,
                offset: 0,
                src: register,
                size: OpSize::S64,
            });
        }
        instructions.push(MInst::Return);
        let instruction_count = instructions.len();
        let mut block = MBlock::new(BlockId(0));
        block.insts = instructions;
        let mut func = MFunction::new(
            vregs,
            (0..REGIONS).map(|_| SpillDesc::transient()).collect(),
        );
        func.blocks.push(block);
        func.verify();
        let cfg = super::super::cfg::normalize(&mut func).unwrap();
        let constraints = ConstraintModel::build(&func, &cfg).unwrap();
        let analysis = super::super::analysis::analyze(&func);

        let stats = schedule_for_pressure(&mut func, &cfg, &constraints, &analysis).unwrap();

        assert_eq!(stats.regions_considered, 1);
        assert_eq!(stats.backward_liveness_steps, instruction_count);
        assert!(stats.maximum_after <= stats.maximum_before);
        func.verify();
    }

    #[test]
    fn sparse_pseudo_effects_feed_shared_dependence_analysis() {
        const MARKS: usize = 256;
        const ACTIVE_CAPACITY: usize = 1_000_000;
        let mut tracker = MemoryDependencyTracker::<MemoryObject, usize>::default();
        for index in 0..MARKS {
            let inst = MInst::SparseMarkActive {
                active_index: index as u32,
                active_bits_offset: 1024,
                active_capacity: ACTIVE_CAPACITY,
            };
            let mut dependencies = BTreeSet::new();
            let reads = memory_effect::reads(&inst);
            let writes = memory_effect::writes(&inst);
            tracker.add_event(
                index,
                analysis_effects(&reads),
                analysis_effects(&writes),
                &mut dependencies,
            );
            if index % 64 != 0 {
                assert!(dependencies.contains(&(index - 1)));
            } else if index != 0 {
                assert!(!dependencies.contains(&(index - 1)));
            }
        }

        let mark = MInst::SparseMarkActive {
            active_index: MARKS as u32,
            active_bits_offset: 1024,
            active_capacity: ACTIVE_CAPACITY,
        };
        assert!(
            is_pressure_schedulable_kind(&mark),
            "exact sparse metadata effects must be represented by DAG edges, not a region barrier"
        );
    }

    #[test]
    fn disjoint_memory_chains_are_scheduled_near_their_uses() {
        let first = VReg(0);
        let second = VReg(1);
        let first_result = VReg(2);
        let second_result = VReg(3);
        let region = vec![
            MInst::Load {
                dst: first,
                base: BaseReg::SimState,
                offset: 0,
                size: OpSize::S64,
            },
            MInst::Load {
                dst: second,
                base: BaseReg::SimState,
                offset: 8,
                size: OpSize::S64,
            },
            MInst::AddImm {
                dst: first_result,
                src: first,
                imm: 1,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 16,
                src: first_result,
                size: OpSize::S64,
            },
            MInst::AddImm {
                dst: second_result,
                src: second,
                imm: 1,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 24,
                src: second_result,
                size: OpSize::S64,
            },
        ];

        let scheduled = schedule_region(&region, BTreeSet::new());

        assert!(scheduled.dependency_verified);
        assert!(
            max_pressure(&scheduled.instructions, &BTreeSet::new())
                < max_pressure(&region, &BTreeSet::new())
        );
        for chain in [
            [region[0].clone(), region[2].clone(), region[3].clone()],
            [region[1].clone(), region[4].clone(), region[5].clone()],
        ] {
            let positions = chain.map(|instruction| {
                scheduled
                    .instructions
                    .iter()
                    .position(|candidate| candidate == &instruction)
                    .unwrap()
            });
            assert!(positions[0] < positions[1] && positions[1] < positions[2]);
        }
    }

    #[test]
    fn w32_operations_do_not_split_a_pressure_scheduling_region() {
        let mut vregs = VRegAllocator::new();
        let first = vregs.alloc();
        let second = vregs.alloc();
        let first_masked = vregs.alloc();
        let second_masked = vregs.alloc();
        let first_result = vregs.alloc();
        let second_result = vregs.alloc();
        let mut block = MBlock::new(BlockId(0));
        block.insts = vec![
            MInst::Load {
                dst: first,
                base: BaseReg::SimState,
                offset: 0,
                size: OpSize::S32,
            },
            MInst::Load {
                dst: second,
                base: BaseReg::SimState,
                offset: 4,
                size: OpSize::S32,
            },
            MInst::AndImm32 {
                dst: first_masked,
                src: first,
                imm: 0xff,
            },
            MInst::AndImm32 {
                dst: second_masked,
                src: second,
                imm: 0xff,
            },
            MInst::Mov32 {
                dst: first_result,
                src: first_masked,
            },
            MInst::Mov32 {
                dst: second_result,
                src: second_masked,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 8,
                src: first_result,
                size: OpSize::S32,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 12,
                src: second_result,
                size: OpSize::S32,
            },
            MInst::Return,
        ];
        let mut func = MFunction::new(vregs, (0..6).map(|_| SpillDesc::transient()).collect());
        func.blocks.push(block);
        func.verify();
        let cfg = super::super::cfg::normalize(&mut func).unwrap();
        let constraints = ConstraintModel::build(&func, &cfg).unwrap();
        let analysis = super::super::analysis::analyze(&func);

        let stats = schedule_for_pressure(&mut func, &cfg, &constraints, &analysis).unwrap();

        assert_eq!(stats.regions_considered, 1);
        assert!(stats.maximum_after < stats.maximum_before);
        func.verify();
    }

    #[test]
    fn dependency_spine_stays_behind_its_independent_w32_input() {
        let mut vregs = VRegAllocator::new();
        let first = vregs.alloc();
        let second = vregs.alloc();
        let side_input = vregs.alloc();
        let first_reduction = vregs.alloc();
        let result = vregs.alloc();
        let mut block = MBlock::new(BlockId(0));
        block.insts = vec![
            MInst::Load {
                dst: first,
                base: BaseReg::SimState,
                offset: 0,
                size: OpSize::S32,
            },
            MInst::Load {
                dst: second,
                base: BaseReg::SimState,
                offset: 4,
                size: OpSize::S32,
            },
            MInst::Load {
                dst: side_input,
                base: BaseReg::SimState,
                offset: 8,
                size: OpSize::S32,
            },
            MInst::And32 {
                dst: first_reduction,
                lhs: first,
                rhs: second,
            },
            MInst::And32 {
                dst: result,
                lhs: first_reduction,
                rhs: side_input,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 12,
                src: result,
                size: OpSize::S32,
            },
            MInst::Return,
        ];
        let mut func = MFunction::new(vregs, (0..5).map(|_| SpillDesc::transient()).collect());
        func.blocks.push(block);
        func.verify();
        let cfg = super::super::cfg::normalize(&mut func).unwrap();
        let constraints = ConstraintModel::build(&func, &cfg).unwrap();
        let analysis = super::super::analysis::analyze(&func);

        schedule_for_pressure(&mut func, &cfg, &constraints, &analysis).unwrap();

        let instructions = &func.blocks[0].insts;
        let side_input_position = instructions
            .iter()
            .position(|inst| inst.def() == Some(side_input))
            .unwrap();
        let first_reduction_position = instructions
            .iter()
            .position(|inst| inst.def() == Some(first_reduction))
            .unwrap();
        let result_position = instructions
            .iter()
            .position(|inst| inst.def() == Some(result))
            .unwrap();
        assert!(side_input_position < first_reduction_position);
        assert!(first_reduction_position < result_position);
        func.verify();
    }

    #[test]
    fn ready_queue_switches_to_pressure_at_the_register_capacity() {
        let priorities = vec![(8, 8), (1, 1)];
        let mut below_capacity = IndexedReadyQueue::new(priorities.clone());
        let mut work = RegionWork::default();
        below_capacity.insert(0, 2, &mut work);
        below_capacity.insert(1, -1, &mut work);
        assert_eq!(below_capacity.pop_best(10, 14, &mut work), Some(0));

        let mut at_capacity = IndexedReadyQueue::new(priorities);
        at_capacity.insert(0, 2, &mut work);
        at_capacity.insert(1, -1, &mut work);
        assert_eq!(at_capacity.pop_best(14, 14, &mut work), Some(1));
    }

    #[test]
    fn overlapping_memory_access_order_is_preserved() {
        let before = VReg(0);
        let stored = VReg(1);
        let after = VReg(2);
        let region = vec![
            MInst::Load {
                dst: before,
                base: BaseReg::SimState,
                offset: 0,
                size: OpSize::S64,
            },
            MInst::LoadImm {
                dst: stored,
                value: 7,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 4,
                src: stored,
                size: OpSize::S32,
            },
            MInst::Load {
                dst: after,
                base: BaseReg::SimState,
                offset: 7,
                size: OpSize::S8,
            },
        ];

        let scheduled = schedule_region(&region, BTreeSet::from([before, after]));
        let positions = [0, 2, 3].map(|original| {
            scheduled
                .instructions
                .iter()
                .position(|candidate| candidate == &region[original])
                .unwrap()
        });

        assert!(scheduled.dependency_verified);
        assert!(positions[0] < positions[1] && positions[1] < positions[2]);
    }

    #[test]
    fn memfill_is_scheduled_by_its_exact_memory_dependencies() {
        let before = VReg(0);
        let after = VReg(1);
        let region = vec![
            MInst::Load {
                dst: before,
                base: BaseReg::SimState,
                offset: 32,
                size: OpSize::S64,
            },
            MInst::MemFill {
                dst_offset: 36,
                byte_len: 8,
                value: 0,
            },
            MInst::Load {
                dst: after,
                base: BaseReg::SimState,
                offset: 43,
                size: OpSize::S8,
            },
        ];

        assert!(is_pressure_schedulable_kind(&region[1]));
        let scheduled = schedule_region(&region, BTreeSet::from([before, after]));
        let positions = [0, 1, 2].map(|original| {
            scheduled
                .instructions
                .iter()
                .position(|candidate| candidate == &region[original])
                .unwrap()
        });

        assert!(scheduled.dependency_verified);
        assert!(positions[0] < positions[1] && positions[1] < positions[2]);
    }

    #[test]
    fn overlapping_loads_can_move_with_their_independent_consumers() {
        let first = VReg(0);
        let second = VReg(1);
        let first_result = VReg(2);
        let second_result = VReg(3);
        let region = vec![
            MInst::Load {
                dst: first,
                base: BaseReg::SimState,
                offset: 0,
                size: OpSize::S64,
            },
            MInst::Load {
                dst: second,
                base: BaseReg::SimState,
                offset: 0,
                size: OpSize::S64,
            },
            MInst::AddImm {
                dst: second_result,
                src: second,
                imm: 1,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 16,
                src: second_result,
                size: OpSize::S64,
            },
            MInst::AddImm {
                dst: first_result,
                src: first,
                imm: 1,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 24,
                src: first_result,
                size: OpSize::S64,
            },
        ];

        let scheduled = schedule_region(&region, BTreeSet::new());
        let first_load = scheduled
            .instructions
            .iter()
            .position(|inst| inst == &region[0])
            .unwrap();
        let second_load = scheduled
            .instructions
            .iter()
            .position(|inst| inst == &region[1])
            .unwrap();

        assert!(scheduled.dependency_verified);
        assert!(
            second_load < first_load,
            "read-after-read must not pin independent producer chains in source order"
        );
        assert!(
            max_pressure(&scheduled.instructions, &BTreeSet::new())
                < max_pressure(&region, &BTreeSet::new())
        );
    }

    #[test]
    fn overlapping_store_stays_after_every_prior_reader() {
        let first = VReg(0);
        let second = VReg(1);
        let stored = VReg(2);
        let region = vec![
            MInst::Load {
                dst: first,
                base: BaseReg::SimState,
                offset: 0,
                size: OpSize::S64,
            },
            MInst::Load {
                dst: second,
                base: BaseReg::SimState,
                offset: 4,
                size: OpSize::S32,
            },
            MInst::LoadImm {
                dst: stored,
                value: 9,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 0,
                src: stored,
                size: OpSize::S64,
            },
        ];

        let scheduled = schedule_region(&region, BTreeSet::from([first, second]));
        let positions = [0, 1, 3].map(|original| {
            scheduled
                .instructions
                .iter()
                .position(|candidate| candidate == &region[original])
                .unwrap()
        });

        assert!(scheduled.dependency_verified);
        assert!(positions[0] < positions[2]);
        assert!(positions[1] < positions[2]);
    }

    #[test]
    fn indexed_or_store_preserves_read_and_write_dependencies() {
        let before = VReg(0);
        let index = VReg(1);
        let mask = VReg(2);
        let after = VReg(3);
        let region = vec![
            MInst::Load {
                dst: before,
                base: BaseReg::SimState,
                offset: 32,
                size: OpSize::S64,
            },
            MInst::LoadImm {
                dst: index,
                value: 0,
            },
            MInst::LoadImm {
                dst: mask,
                value: 4,
            },
            MInst::OrStoreIndexed {
                base: BaseReg::SimState,
                offset: 32,
                index,
                src: mask,
                size: OpSize::S64,
                alias_range: MemoryAliasRange::new(32, 8),
            },
            MInst::Load {
                dst: after,
                base: BaseReg::SimState,
                offset: 32,
                size: OpSize::S64,
            },
        ];

        assert!(
            is_pressure_schedulable_kind(&region[3]),
            "indexed memory RMW must use its conservative alias range inside the scheduling DAG"
        );
        let scheduled = schedule_region(&region, BTreeSet::from([before, after]));
        let positions = [0, 3, 4].map(|original| {
            scheduled
                .instructions
                .iter()
                .position(|candidate| candidate == &region[original])
                .unwrap()
        });

        assert!(scheduled.dependency_verified);
        assert!(positions[0] < positions[1]);
        assert!(positions[1] < positions[2]);
    }

    #[test]
    fn indexed_loads_are_scheduled_with_their_consumers() {
        let index = VReg(0);
        let loaded = [VReg(1), VReg(2), VReg(3), VReg(4)];
        let masked = [VReg(5), VReg(6), VReg(7), VReg(8)];
        let combined = [VReg(9), VReg(10), VReg(11)];
        let mut region = vec![MInst::LoadImm {
            dst: index,
            value: 0,
        }];
        for (lane, &dst) in loaded.iter().enumerate() {
            region.push(MInst::LoadIndexed {
                dst,
                base: BaseReg::SimState,
                offset: (lane * 8) as i32,
                index,
                size: OpSize::S64,
                alias_range: None,
            });
        }
        for (&dst, &src) in masked.iter().zip(&loaded) {
            region.push(MInst::AndImm { dst, src, imm: 1 });
        }
        region.extend([
            MInst::Or {
                dst: combined[0],
                lhs: masked[0],
                rhs: masked[1],
            },
            MInst::Or {
                dst: combined[1],
                lhs: masked[2],
                rhs: masked[3],
            },
            MInst::Or {
                dst: combined[2],
                lhs: combined[0],
                rhs: combined[1],
            },
        ]);

        let scheduled = schedule_region(&region, BTreeSet::from([combined[2]]));

        assert!(
            region.iter().all(is_pressure_schedulable_kind),
            "indexed reads and their pure consumers must form one production scheduling region"
        );
        assert!(scheduled.dependency_verified);
        assert!(
            max_pressure(&scheduled.instructions, &BTreeSet::from([combined[2]]))
                < max_pressure(&region, &BTreeSet::from([combined[2]])),
            "commuting indexed reads must not keep every loaded lane live together"
        );
    }

    #[test]
    fn indexed_load_keeps_conservative_store_order() {
        let stored_before = VReg(0);
        let index = VReg(1);
        let loaded = VReg(2);
        let stored_after = VReg(3);
        let region = vec![
            MInst::LoadImm {
                dst: stored_before,
                value: 7,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 0,
                src: stored_before,
                size: OpSize::S64,
            },
            MInst::LoadImm {
                dst: index,
                value: 0,
            },
            MInst::LoadIndexed {
                dst: loaded,
                base: BaseReg::SimState,
                offset: 64,
                index,
                size: OpSize::S64,
                alias_range: None,
            },
            MInst::AddImm {
                dst: stored_after,
                src: loaded,
                imm: 1,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 128,
                src: stored_after,
                size: OpSize::S64,
            },
        ];

        let scheduled = schedule_region(&region, BTreeSet::new());
        let positions = [1, 3, 5].map(|original| {
            scheduled
                .instructions
                .iter()
                .position(|candidate| candidate == &region[original])
                .unwrap()
        });

        assert!(scheduled.dependency_verified);
        assert!(positions[0] < positions[1] && positions[1] < positions[2]);
    }

    #[test]
    fn indexed_stores_do_not_split_independent_value_chains() {
        let index = VReg(0);
        let first = VReg(1);
        let second = VReg(2);
        let first_result = VReg(3);
        let second_result = VReg(4);
        let region = vec![
            MInst::Load {
                dst: first,
                base: BaseReg::SimState,
                offset: 0,
                size: OpSize::S64,
            },
            MInst::Load {
                dst: second,
                base: BaseReg::SimState,
                offset: 8,
                size: OpSize::S64,
            },
            MInst::AddImm {
                dst: first_result,
                src: first,
                imm: 1,
            },
            MInst::StoreIndexed {
                base: BaseReg::SimState,
                offset: 32,
                index,
                src: first_result,
                size: OpSize::S64,
                alias_range: MemoryAliasRange::new(32, 8),
            },
            MInst::AddImm {
                dst: second_result,
                src: second,
                imm: 1,
            },
            MInst::StoreIndexed {
                base: BaseReg::SimState,
                offset: 48,
                index,
                src: second_result,
                size: OpSize::S64,
                alias_range: MemoryAliasRange::new(48, 8),
            },
        ];

        assert!(region.iter().all(is_pressure_schedulable_kind));
        let scheduled = schedule_region(&region, BTreeSet::new());

        assert!(scheduled.dependency_verified);
        assert!(
            max_pressure(&scheduled.instructions, &BTreeSet::new())
                < max_pressure(&region, &BTreeSet::new()),
            "disjoint indexed stores must allow each producer chain to close before the next one"
        );
    }

    #[test]
    fn stale_constraint_shape_is_a_structured_error() {
        let mut func = MFunction::new(VRegAllocator::new(), Vec::new());
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::Return);
        func.blocks.push(block);
        let cfg = super::super::cfg::normalize(&mut func).unwrap();
        let mut constraints = ConstraintModel::build(&func, &cfg).unwrap();
        constraints.instructions[0].pop();
        let analysis = super::super::analysis::analyze(&func);

        let error = schedule_for_pressure(&mut func, &cfg, &constraints, &analysis).unwrap_err();

        assert_eq!(error.rule, "SCHEDULE.MODEL_SHAPE");
        assert_eq!(error.block, BlockId(0));
    }

    #[test]
    fn dependency_verifier_rejects_non_topological_orders() {
        let dependencies = vec![vec![], vec![0], vec![1]];

        assert!(dependency_order_valid(&dependencies, &[0, 1, 2]));
        assert!(!dependency_order_valid(&dependencies, &[1, 0, 2]));
        assert!(!dependency_order_valid(&dependencies, &[0, 1, 1]));
    }

    #[test]
    fn cyclic_dependency_is_an_error_not_an_unchanged_fallback() {
        let region = vec![
            MInst::Neg {
                dst: VReg(0),
                src: VReg(1),
            },
            MInst::Neg {
                dst: VReg(1),
                src: VReg(0),
            },
        ];
        let constraints = vec![Default::default(), Default::default()];
        let mut stats = ScheduleStats::default();

        let error = schedule_block(&region, &constraints, &BTreeSet::new(), &mut stats)
            .expect_err("cyclic producer input must not silently keep the original order");

        assert!(error.contains("dependency DAG"));
    }

    #[test]
    fn value_changes_rekey_only_indexed_ready_instructions() {
        let source = VReg(0);
        let first = VReg(1);
        let second = VReg(2);
        let region = vec![
            MInst::LoadImm {
                dst: source,
                value: 7,
            },
            MInst::Neg {
                dst: first,
                src: source,
            },
            MInst::BitNot {
                dst: second,
                src: source,
            },
        ];
        let live_out = BTreeSet::from([first, second]);

        let scheduled = schedule_region(&region, live_out);

        assert!(scheduled.dependency_verified);
        assert_eq!(scheduled.work.ready_pops, region.len());
        assert!(scheduled.work.priority_updates > 0);
        // Each live/dead transition visits only instructions named in the
        // value's use/definition index, never the whole ready population.
        assert!(scheduled.work.priority_value_index_visits <= 2 * (region.len() + 2));
    }
}
