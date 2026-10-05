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

fn unit_cost(unit: &ExecutionUnit<RegionedAbsoluteAddr>) -> u64 {
    unit.blocks
        .values()
        .map(|block| block.instructions.len() as u64 + 1)
        .sum::<u64>()
        .max(1)
}

/// Estimated FF work of every instance in its most expensive event, indexed
/// by instance id. A partitioned combinational settle runs fused with the FF
/// update, and FF work follows the lane of the instance's logic, so balancing
/// lanes needs both.
pub(crate) fn instance_ff_costs(
    parts: &HashMap<AbsoluteAddr, Vec<(InstanceId, FfPart<RegionedAbsoluteAddr>)>>,
    instance_count: usize,
) -> Vec<u64> {
    let mut costs = vec![0u64; instance_count];
    for event_parts in parts.values() {
        let mut event_costs = HashMap::<usize, u64>::default();
        for (instance, part) in event_parts {
            *event_costs.entry(instance.0).or_default() +=
                unit_cost(&part.evaluate).saturating_add(unit_cost(&part.apply));
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
            group_costs[group] = group_costs[group].saturating_add(unit_cost(units[index]));
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
