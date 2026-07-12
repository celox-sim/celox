//! Pressure-aware scheduling of side-effect-free machine DAG regions.

use std::collections::{BTreeSet, HashMap};
use std::fmt;

use crate::backend::native::mir::{BaseReg, BlockId, MFunction, MInst, VReg};

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

    // Preserve exactly the memory dependences that can change a value.  Two
    // overlapping Loads commute; serializing them used to pin long runs of
    // generated bit/prefix reads in source order and prevented the scheduler
    // from moving each producer next to its consumers.  Track the last writer
    // and the readers since that writer per byte so RAW, WAR, and WAW remain
    // ordered without inventing read-after-read edges.  Dynamic/pointer/
    // release accesses and MemCopy remain region barriers.
    #[derive(Default)]
    struct MemoryHistory {
        last_writer: Option<usize>,
        readers_since_write: Vec<usize>,
    }
    let mut memory_history = HashMap::<(BaseReg, i64), MemoryHistory>::new();
    for (instruction, inst) in region.iter().enumerate() {
        let Some((base, start, bytes, is_write)) = constant_memory_access(inst) else {
            continue;
        };
        let mut memory_dependencies = BTreeSet::new();
        for byte in 0..i64::from(bytes) {
            let address = (base, start + byte);
            let history = memory_history.entry(address).or_default();
            if let Some(writer) = history.last_writer {
                memory_dependencies.insert(writer);
            }
            if is_write {
                memory_dependencies.extend(history.readers_since_write.iter().copied());
            }
        }
        for dependency in memory_dependencies {
            add_dependency(&mut dependencies, &mut users, instruction, dependency);
        }
        for byte in 0..i64::from(bytes) {
            let address = (base, start + byte);
            let history = memory_history
                .get_mut(&address)
                .expect("memory history was initialized while collecting dependencies");
            if is_write {
                history.last_writer = Some(instruction);
                history.readers_since_write.clear();
            } else if history.readers_since_write.last() != Some(&instruction) {
                history.readers_since_write.push(instruction);
            }
        }
    }

    let mut work = RegionWork::default();
    let mut ready = IndexedReadyQueue::new(region.len());
    for (index, &count) in users.iter().enumerate() {
        if count == 0 {
            enqueue_ready(&mut ready, index, region, &unique_uses, &live, &mut work);
        }
    }
    let mut reverse = Vec::with_capacity(region.len());
    while reverse.len() != region.len() {
        let Some(candidate) = ready.pop_best(&mut work) else {
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
    let instructions = dependency_verified
        .then(|| reverse.iter().map(|&index| region[index].clone()).collect())
        .unwrap_or_default();
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

fn constant_memory_access(inst: &MInst) -> Option<(BaseReg, i64, u32, bool)> {
    match inst {
        MInst::Load {
            base, offset, size, ..
        } => Some((*base, i64::from(*offset), size.bytes(), false)),
        MInst::Store {
            base, offset, size, ..
        } => Some((*base, i64::from(*offset), size.bytes(), true)),
        _ => None,
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
    buckets: [BTreeSet<usize>; PRIORITY_BUCKETS],
    priorities: Vec<i8>,
    present: Vec<bool>,
}

impl IndexedReadyQueue {
    fn new(instructions: usize) -> Self {
        Self {
            buckets: std::array::from_fn(|_| BTreeSet::new()),
            priorities: vec![0; instructions],
            present: vec![false; instructions],
        }
    }

    fn contains(&self, instruction: usize) -> bool {
        self.present[instruction]
    }

    fn insert(&mut self, instruction: usize, priority: i8, work: &mut RegionWork) {
        debug_assert!(!self.present[instruction]);
        let bucket = priority_bucket(priority);
        self.buckets[bucket].insert(instruction);
        self.priorities[instruction] = priority;
        self.present[instruction] = true;
        work.ready_insertions += 1;
    }

    fn adjust(&mut self, instruction: usize, delta: i8, work: &mut RegionWork) {
        debug_assert!(self.present[instruction]);
        let old_priority = self.priorities[instruction];
        self.buckets[priority_bucket(old_priority)].remove(&instruction);
        let new_priority = old_priority + delta;
        self.buckets[priority_bucket(new_priority)].insert(instruction);
        self.priorities[instruction] = new_priority;
        work.priority_updates += 1;
    }

    fn pop_best(&mut self, work: &mut RegionWork) -> Option<usize> {
        for bucket in &mut self.buckets {
            work.priority_bucket_probes += 1;
            if let Some(instruction) = bucket.iter().next_back().copied() {
                bucket.remove(&instruction);
                self.present[instruction] = false;
                work.ready_pops += 1;
                return Some(instruction);
            }
        }
        None
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
        && matches!(
            inst,
            MInst::Mov { .. }
                | MInst::LoadImm { .. }
                | MInst::LoadConstantTableAddr { .. }
                | MInst::Load { .. }
                | MInst::Store { .. }
                | MInst::Add { .. }
                | MInst::Sub { .. }
                | MInst::Mul { .. }
                | MInst::And { .. }
                | MInst::Or { .. }
                | MInst::Xor { .. }
                | MInst::Shr { .. }
                | MInst::Shl { .. }
                | MInst::Sar { .. }
                | MInst::AndImm { .. }
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
                | MInst::Bsr { .. }
                | MInst::BsrOr { .. }
                | MInst::Pext { .. }
                | MInst::Pdep { .. }
                | MInst::Select { .. }
                | MInst::CmpSelect { .. }
                | MInst::CmpImmSelect { .. }
                | MInst::GuardedCmpSelect { .. }
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::native::mir::{BlockId, MBlock, OpSize, SpillDesc, VRegAllocator};
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
