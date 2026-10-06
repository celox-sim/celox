//! Lane-partitioned alternatives of the scheduled simulation phases.
//!
//! The combinational settle is partitioned by the SLT scheduler, which owns
//! the dependency graph. Sequential events are partitioned here from their
//! per-instance evaluate and apply units: units which write the same state
//! object share a lane, and every object keeps one lane across all events so
//! physical layout can give each lane its own storage. FF work follows the
//! lane that accesses its state in the combinational settle, so each lane
//! keeps working on data already in its core's cache.

use celox_design::{InstanceId, RegionedAbsoluteAddrBase};
use celox_sir::{ExecutionUnit, LaneUnit, ParallelFfKernel, SIRInstruction};

use crate::symbolic::artifact::FfPart;
use crate::{HashMap, HashSet, SourceAddr, SourceVarId};

type AbsoluteAddr = SourceAddr;
type RegionedAbsoluteAddr = RegionedAbsoluteAddrBase<SourceVarId>;

/// Lane partitioning requested for the hot simulation phases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParallelScheduleOptions {
    /// Number of simulation lanes. One disables partitioning.
    pub lanes: u32,
    /// Estimated cost of a cross-lane dependency, in SLT operation units.
    pub synchronization_cost: u64,
    /// Smallest accepted estimated speedup of a partitioned phase, in
    /// percent of its single-lane cost.
    pub minimum_speedup_percent: u64,
}

impl ParallelScheduleOptions {
    /// Sequential scheduling only.
    pub const SEQUENTIAL: Self = Self {
        lanes: 1,
        synchronization_cost: 400,
        minimum_speedup_percent: 125,
    };

    /// Request `lanes` simulation lanes with the default cost model.
    pub fn with_lanes(lanes: u32) -> Self {
        Self {
            lanes: lanes.max(1),
            ..Self::SEQUENTIAL
        }
    }

    pub fn enabled(&self) -> bool {
        self.lanes > 1
    }
}

impl Default for ParallelScheduleOptions {
    fn default() -> Self {
        Self::SEQUENTIAL
    }
}

/// Measured cost of one sparse summary word in a commit, including the dirty
/// words it leads to, in operations.
const SPARSE_SUMMARY_WORD_COST: u64 = 16;

/// Largest 64-bit word count charged for one store; wider stores target
/// memories, whose lowering touches single elements.
const MAX_STORE_WORDS: u64 = 64;

/// Declared sizes of the state objects, used to estimate how many bytes a
/// commit touches before the physical layout exists.
#[derive(Clone, Copy)]
pub(crate) struct ObjectSizes<'a> {
    /// Width in bits of every object.
    pub(crate) widths: &'a HashMap<AbsoluteAddr, usize>,
    /// Element width in bits of every unpacked array.
    pub(crate) element_widths: &'a HashMap<AbsoluteAddr, usize>,
}

impl ObjectSizes<'_> {
    /// Estimated 64-bit words of one plane of `object`. An unpacked array
    /// may give every element a power-of-two number of bytes (the element-
    /// strided layout of native backends), so that padding is included;
    /// packed layouts are slightly overcharged.
    fn plane_words(&self, object: &AbsoluteAddr) -> Option<u64> {
        let width = *self.widths.get(object)? as u64;
        let padded = self
            .element_widths
            .get(object)
            .map(|&element_width| element_width as u64)
            .filter(|&element_width| {
                element_width > 0 && width > element_width && width.is_multiple_of(element_width)
            })
            .map(|element_width| {
                let stride_bytes = element_width.div_ceil(8).next_power_of_two();
                stride_bytes * 8 * (width / element_width)
            })
            .unwrap_or(width);
        Some(padded.div_ceil(64).max(1))
    }
}

/// Estimated work of one FF instruction. Most instructions cost one
/// operation. A commit copies its range, or the whole padded object when it
/// commits all of it; a store costs its words up to [`MAX_STORE_WORDS`]. A
/// sparse commit scans one summary word for every 4096 of its object's
/// 64-bit words, whatever it changed; on a large memory that scan dominates
/// the event.
fn instruction_cost(
    instruction: &SIRInstruction<RegionedAbsoluteAddr>,
    sizes: ObjectSizes<'_>,
) -> u64 {
    match instruction {
        SIRInstruction::Commit(source, _, _, width, _)
            if source.region == celox_design::SPARSE_WORKING_REGION =>
        {
            sizes
                .plane_words(&source.absolute_addr())
                .unwrap_or_else(|| (*width as u64).div_ceil(64))
                .div_ceil(4096)
                .saturating_mul(SPARSE_SUMMARY_WORD_COST)
                .max(1)
        }
        SIRInstruction::Commit(source, _, offset, width, _) => {
            let object = source.absolute_addr();
            let whole =
                offset.constant_bit_offset() == Some(0) && sizes.widths.get(&object) == Some(width);
            whole
                .then(|| sizes.plane_words(&object))
                .flatten()
                .unwrap_or_else(|| (*width as u64).div_ceil(64))
                .max(1)
        }
        SIRInstruction::Store(_, _, width, ..) => {
            (*width as u64).div_ceil(64).clamp(1, MAX_STORE_WORDS)
        }
        _ => 1,
    }
}

fn unit_cost(unit: &ExecutionUnit<RegionedAbsoluteAddr>, sizes: ObjectSizes<'_>) -> u64 {
    unit.blocks
        .values()
        .map(|block| {
            block
                .instructions
                .iter()
                .map(|instruction| instruction_cost(instruction, sizes))
                .fold(1u64, u64::saturating_add)
        })
        .fold(0u64, u64::saturating_add)
        .max(1)
}

/// Estimated FF work of every instance in its most expensive event, indexed
/// by instance id. A partitioned combinational settle runs fused with the FF
/// update, and FF work follows the lane of the instance's logic, so balancing
/// lanes needs both.
pub(crate) fn instance_ff_costs(
    parts: &HashMap<AbsoluteAddr, Vec<(InstanceId, FfPart<RegionedAbsoluteAddr>)>>,
    instance_count: usize,
    sizes: ObjectSizes<'_>,
) -> Vec<u64> {
    let mut costs = vec![0u64; instance_count];
    for event_parts in parts.values() {
        let mut event_costs = HashMap::<usize, u64>::default();
        for (instance, part) in event_parts {
            *event_costs.entry(instance.0).or_default() +=
                unit_cost(&part.evaluate, sizes).saturating_add(unit_cost(&part.apply, sizes));
        }
        for (instance, cost) in event_costs {
            if let Some(slot) = costs.get_mut(instance) {
                *slot = (*slot).max(cost);
            }
        }
    }
    costs
}

/// Logical byte ranges written by a unit, per object and independent of the
/// storage region (a staged write and its publication cover the same bytes).
/// Dynamic writes cover the whole object.
fn written_ranges(unit: &ExecutionUnit<RegionedAbsoluteAddr>) -> Vec<(AbsoluteAddr, usize, usize)> {
    let mut ranges = unit
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match instruction {
            SIRInstruction::Store(address, offset, width, ..)
            | SIRInstruction::Commit(_, address, offset, width, _) => {
                let (begin, end) = match offset.constant_bit_offset() {
                    Some(bit) if !offset.is_dynamic() => (bit / 8, (bit + width).div_ceil(8)),
                    _ => (0, usize::MAX),
                };
                Some((address.absolute_addr(), begin, end.max(begin + 1)))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    ranges.sort_unstable();
    ranges.dedup();
    ranges
}

/// Whether a unit must stay in lane 0 because it appends runtime events.
fn emits_runtime_events(unit: &ExecutionUnit<RegionedAbsoluteAddr>) -> bool {
    unit.blocks
        .values()
        .flat_map(|block| &block.instructions)
        .any(|instruction| {
            matches!(
                instruction,
                SIRInstruction::RuntimeEvent { .. }
                    | SIRInstruction::CombCaptureEvent { .. }
                    | SIRInstruction::CombCaptureEnableIfChanged { .. }
            )
        })
}

/// The lane performing most accesses of every state object in the
/// partitioned combinational settle.
fn comb_object_lanes(comb_units: &[LaneUnit<RegionedAbsoluteAddr>]) -> HashMap<AbsoluteAddr, u32> {
    let mut counts = HashMap::<(AbsoluteAddr, u32), u64>::default();
    for unit in comb_units {
        for object in accessed_objects(&unit.unit) {
            *counts.entry((object, unit.lane)).or_default() += 1;
        }
    }
    let mut best = HashMap::<AbsoluteAddr, (u64, u32)>::default();
    for ((object, lane), count) in counts {
        let entry = best.entry(object).or_insert((0, u32::MAX));
        // Most accesses first, then the lowest lane for determinism.
        if (count, std::cmp::Reverse(lane)) > (entry.0, std::cmp::Reverse(entry.1)) {
            *entry = (count, lane);
        }
    }
    best.into_iter()
        .map(|(object, (_, lane))| (object, lane))
        .collect()
}

/// State objects a unit reads or writes, with repetitions.
fn accessed_objects(
    unit: &ExecutionUnit<RegionedAbsoluteAddr>,
) -> impl Iterator<Item = AbsoluteAddr> + '_ {
    unit.blocks
        .values()
        .flat_map(|block| &block.instructions)
        .flat_map(|instruction| {
            let objects = match instruction {
                SIRInstruction::Load(_, address, ..) | SIRInstruction::Store(address, ..) => {
                    [Some(address), None]
                }
                SIRInstruction::Commit(source, destination, ..) => {
                    [Some(source), Some(destination)]
                }
                _ => [None, None],
            };
            objects
                .into_iter()
                .flatten()
                .map(RegionedAbsoluteAddr::absolute_addr)
        })
}

fn find_root(parents: &mut [usize], mut item: usize) -> usize {
    while parents[item] != item {
        parents[item] = parents[parents[item]];
        item = parents[item];
    }
    item
}

/// Partition the FF parts of every event with more than one part.
///
/// `events` lists the events to partition in a deterministic order, and
/// `parts` holds every event's independently evaluated FF parts with their
/// instances. Objects in `lane0_objects` are event signals; their stores
/// update shared trigger bytes, so their writers run in lane 0. `comb_units`
/// is the partitioned combinational settle, whose lanes the FF parts prefer.
pub(crate) fn plan_parallel_ff_kernels(
    events: &[AbsoluteAddr],
    mut parts: HashMap<AbsoluteAddr, Vec<(InstanceId, FfPart<RegionedAbsoluteAddr>)>>,
    lane0_objects: &HashSet<AbsoluteAddr>,
    comb_units: &[LaneUnit<RegionedAbsoluteAddr>],
    sizes: ObjectSizes<'_>,
    options: &ParallelScheduleOptions,
) -> HashMap<AbsoluteAddr, ParallelFfKernel<RegionedAbsoluteAddr>> {
    let lanes = options.lanes.max(1);
    let mut kernels = HashMap::default();
    if lanes < 2 {
        return kernels;
    }
    let comb_lanes = comb_object_lanes(comb_units);
    // Keep a written range in one lane across events for locality.
    let mut range_lanes = HashMap::<(AbsoluteAddr, usize), u32>::default();
    for event in events {
        let Some(mut event_parts) = parts.remove(event) else {
            continue;
        };
        if event_parts.len() < 2 {
            continue;
        }
        // Runtime events (such as `$display`) of different parts must keep
        // the sequential kernel's order, which its own schedule decides.
        // One emitting part cannot be reordered against another; with more,
        // the event keeps its sequential kernel.
        if event_parts
            .iter()
            .filter(|(_, part)| {
                emits_runtime_events(&part.evaluate) || emits_runtime_events(&part.apply)
            })
            .count()
            > 1
        {
            continue;
        }
        // Instance order keeps the unit order independent of map iteration;
        // the sort is stable, so parts of one instance keep their order.
        event_parts.sort_by_key(|(instance, _)| *instance);
        let (evaluations, applications): (Vec<_>, Vec<_>) = event_parts
            .into_iter()
            .map(|(_, part)| (part.evaluate, part.apply))
            .unzip();
        let units = evaluations.iter().chain(&applications).collect::<Vec<_>>();
        let writes = units
            .iter()
            .map(|unit| written_ranges(unit))
            .collect::<Vec<_>>();

        // Units writing overlapping bytes of one object form one placement
        // group; disjoint parts of an object (such as separate array
        // elements) may be placed in different lanes.
        let mut parents = (0..units.len()).collect::<Vec<_>>();
        let mut sweep = writes
            .iter()
            .enumerate()
            .flat_map(|(index, ranges)| {
                ranges
                    .iter()
                    .map(move |&(object, begin, end)| (object, begin, end, index))
            })
            .collect::<Vec<_>>();
        sweep.sort_unstable();
        let mut run: Option<(AbsoluteAddr, usize, usize)> = None;
        for (object, begin, end, index) in sweep {
            match &mut run {
                Some((run_object, run_end, representative))
                    if *run_object == object && begin < *run_end =>
                {
                    *run_end = (*run_end).max(end);
                    let left = find_root(&mut parents, *representative);
                    let right = find_root(&mut parents, index);
                    if left != right {
                        parents[left.max(right)] = left.min(right);
                    }
                }
                _ => run = Some((object, end, index)),
            }
        }
        let mut group_of_unit = vec![usize::MAX; units.len()];
        let mut group_costs = Vec::<u64>::new();
        let mut group_lanes = Vec::<Option<u32>>::new();
        let mut group_votes = Vec::<Vec<u64>>::new();
        let mut group_roots = HashMap::<usize, usize>::default();
        for index in 0..units.len() {
            let root = find_root(&mut parents, index);
            let group = *group_roots.entry(root).or_insert_with(|| {
                group_costs.push(0);
                group_lanes.push(None);
                group_votes.push(vec![0; lanes as usize]);
                group_costs.len() - 1
            });
            group_of_unit[index] = group;
            group_costs[group] = group_costs[group].saturating_add(unit_cost(units[index], sizes));
            for object in accessed_objects(units[index]) {
                if let Some(&lane) = comb_lanes.get(&object) {
                    group_votes[group][lane as usize] += 1;
                }
            }
            let pinned = emits_runtime_events(units[index])
                || writes[index]
                    .iter()
                    .any(|(object, ..)| lane0_objects.contains(object));
            let previous = writes[index]
                .iter()
                .find_map(|(object, begin, _)| range_lanes.get(&(*object, *begin)).copied());
            if pinned {
                group_lanes[group] = Some(0);
            } else if group_lanes[group].is_none() {
                group_lanes[group] = previous;
            }
        }

        // Longest-processing-time placement of the free groups. A group
        // takes the lane accessing its state in the combinational settle
        // while that lane stays within a balanced share.
        let mut loads = vec![0u64; lanes as usize];
        for (group, lane) in group_lanes.iter().enumerate() {
            if let Some(lane) = lane {
                loads[*lane as usize] = loads[*lane as usize].saturating_add(group_costs[group]);
            }
        }
        let total = group_costs.iter().copied().fold(0u64, u64::saturating_add);
        let share = total
            .div_ceil(u64::from(lanes))
            .saturating_add(total / (u64::from(lanes) * 10));
        let mut free = (0..group_costs.len())
            .filter(|group| group_lanes[*group].is_none())
            .collect::<Vec<_>>();
        free.sort_by_key(|group| (std::cmp::Reverse(group_costs[*group]), *group));
        for group in free {
            let least_loaded = (0..lanes as usize)
                .min_by_key(|lane| (loads[*lane], *lane))
                .expect("at least one lane");
            let preferred = (0..lanes as usize)
                .filter(|lane| group_votes[group][*lane] > 0)
                .max_by_key(|lane| (group_votes[group][*lane], std::cmp::Reverse(*lane)));
            let lane = match preferred {
                Some(lane)
                    if lane == least_loaded
                        || loads[lane].saturating_add(group_costs[group]) <= share =>
                {
                    lane
                }
                _ => least_loaded,
            };
            loads[lane] = loads[lane].saturating_add(group_costs[group]);
            group_lanes[group] = Some(lane as u32);
        }

        let makespan = loads.iter().copied().max().unwrap_or(0);
        if makespan == 0
            || total.saturating_mul(100) < makespan.saturating_mul(options.minimum_speedup_percent)
        {
            continue;
        }
        for (index, ranges) in writes.iter().enumerate() {
            let lane = group_lanes[group_of_unit[index]].expect("every group has a lane");
            for (object, begin, _) in ranges {
                range_lanes.entry((*object, *begin)).or_insert(lane);
            }
        }
        let lane_of = |index: usize| group_lanes[group_of_unit[index]].expect("assigned lane");
        let evaluation_count = evaluations.len();
        kernels.insert(
            *event,
            ParallelFfKernel {
                evaluations: evaluations
                    .into_iter()
                    .enumerate()
                    .map(|(index, unit)| LaneUnit::new(lane_of(index), unit))
                    .collect(),
                applications: applications
                    .into_iter()
                    .enumerate()
                    .map(|(index, unit)| LaneUnit::new(lane_of(evaluation_count + index), unit))
                    .collect(),
            },
        );
    }
    kernels
}

#[cfg(test)]
mod tests {
    use celox_design::{AbsoluteAddrBase, SPARSE_WORKING_REGION, STABLE_REGION};
    use celox_sir::SIROffset;

    use super::*;

    fn object(var: u32) -> AbsoluteAddr {
        AbsoluteAddrBase {
            instance_id: InstanceId(1),
            var_id: SourceVarId(var),
        }
    }

    fn address(region: u32, var: u32) -> RegionedAbsoluteAddr {
        RegionedAbsoluteAddr::from_absolute_addr(region, object(var))
    }

    fn commit(source_region: u32, var: u32, width: usize) -> SIRInstruction<RegionedAbsoluteAddr> {
        SIRInstruction::Commit(
            address(source_region, var),
            address(STABLE_REGION, var),
            SIROffset::Static(0),
            width,
            Vec::new(),
        )
    }

    #[test]
    fn ff_costs_follow_the_data_an_instruction_moves() {
        // Objects: 0 is 64 bits, 1 is 2^20 bits, 2 is a 32 MiB memory, and
        // 3 is an array of 2^20 one-bit elements padded to a byte each.
        let widths = [(0, 64), (1, 1 << 20), (2, 1 << 28), (3, 1 << 20)]
            .into_iter()
            .map(|(var, width)| (object(var), width))
            .collect::<HashMap<_, _>>();
        let element_widths = [(object(3), 1)].into_iter().collect::<HashMap<_, _>>();
        let sizes = ObjectSizes {
            widths: &widths,
            element_widths: &element_widths,
        };
        let cost = |instruction| instruction_cost(&instruction, sizes);
        assert_eq!(cost(commit(1, 0, 32)), 1);
        assert_eq!(cost(commit(1, 0, 64)), 1);
        // A commit copies its whole range, padding included.
        assert_eq!(cost(commit(1, 1, 1 << 20)), 1 << 14);
        assert_eq!(cost(commit(1, 3, 1 << 20)), 1 << 17);
        // A store into a memory is charged as element accesses.
        let store = SIRInstruction::Store(
            address(1, 1),
            SIROffset::Static(0),
            1 << 20,
            celox_sir::RegisterId(0),
            Vec::new(),
            Vec::new(),
        );
        assert_eq!(cost(store), MAX_STORE_WORDS);
        // A sparse commit scans the whole object, however few bits it names:
        // 1024 summary words for 32 MiB, 32 for the padded bit array.
        assert_eq!(
            cost(commit(SPARSE_WORKING_REGION, 2, 1 << 10)),
            1024 * SPARSE_SUMMARY_WORD_COST
        );
        assert_eq!(
            cost(commit(SPARSE_WORKING_REGION, 3, 1 << 20)),
            32 * SPARSE_SUMMARY_WORD_COST
        );
    }
}
