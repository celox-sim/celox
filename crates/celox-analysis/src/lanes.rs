//! Static assignment of a weighted dependency DAG to execution lanes.
//!
//! A lane is a sequential instruction stream executed by one worker. Work
//! items are listed in a topological order; each lane executes its items in
//! that order. Cross-lane dependencies are satisfied by explicit waits between
//! *tasks*, the maximal runs of one lane's items that need no new wait.
//!
//! [`assign_lanes`] is an earliest-start list scheduler with a fixed priority
//! (the given order). A dependency whose producer runs in another lane costs
//! an additional synchronization delay, so chains stay in one lane unless
//! another lane can start the item sufficiently earlier. An item also stays
//! in the lane of its latest producer, or else of the previous item, while
//! that lane starts it at most a locality slack later than the best lane:
//! neighbouring items usually touch neighbouring state, and runs of them in
//! one lane keep cache lines from being shared between cores. Items in one
//! ownership group always share a lane: callers use groups for work which
//! writes the same state object, so the object has exactly one writer lane.
//!
//! [`form_lane_tasks`] cuts each lane's sequence into tasks and records, for
//! every task, the minimal set of `(lane, completed task count)` waits that
//! satisfies every cross-lane dependency. A wait for a producer also implies
//! everything that producer task (transitively) waited for; those implied
//! waits are omitted. Every task only waits for tasks created before it, so
//! executing the lanes concurrently cannot deadlock.
//!
//! For `N` items, `E` dependencies, and `L` lanes, assignment costs
//! `O(N * L + E)` time and task formation `O(E + T * L)` for `T` tasks.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneError {
    /// Input slices do not describe the same number of items or groups.
    Shape,
    /// A dependency does not point to an earlier item.
    NotTopological { item: usize, predecessor: usize },
    /// A group or lane identifier is outside its declared range.
    InvalidIdentifier,
}

/// Inputs describing one lane assignment problem.
#[derive(Debug, Clone, Copy)]
pub struct LaneAssignmentInput<'a> {
    /// Estimated execution cost of each item.
    pub costs: &'a [u64],
    /// Dependencies of each item. Every predecessor precedes the item.
    pub predecessors: &'a [Vec<usize>],
    /// Ownership group of each item. Items in one group share a lane.
    pub groups: &'a [usize],
    /// Lane fixed in advance for each group, if any.
    pub group_lanes: &'a [Option<u32>],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LaneAssignmentOptions {
    /// Number of available lanes. Zero is treated as one.
    pub lanes: u32,
    /// Estimated delay for an item to observe a producer in another lane,
    /// in the same unit as item costs.
    pub synchronization_cost: u64,
    /// Largest delay accepted to keep an item in its producer's or its
    /// predecessor's lane, in the same unit as item costs.
    pub locality_slack: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaneAssignment {
    /// Lane of each item.
    pub item_lanes: Vec<u32>,
    /// Estimated completion time of the schedule.
    pub estimated_makespan: u64,
    /// Sum of all item costs, i.e. the estimated single-lane execution time.
    pub total_cost: u64,
}

/// Assign every item to a lane.
pub fn assign_lanes(
    input: LaneAssignmentInput<'_>,
    options: LaneAssignmentOptions,
) -> Result<LaneAssignment, LaneError> {
    let count = input.costs.len();
    if input.predecessors.len() != count || input.groups.len() != count {
        return Err(LaneError::Shape);
    }
    let lane_count = options.lanes.max(1) as usize;
    for &lane in input.group_lanes.iter().flatten() {
        if lane as usize >= lane_count {
            return Err(LaneError::InvalidIdentifier);
        }
    }
    let mut group_lanes = input.group_lanes.to_vec();
    let mut item_lanes = vec![0u32; count];
    let mut finish = vec![0u64; count];
    let mut lane_available = vec![0u64; lane_count];
    // Latest producer finish time per lane for the item being placed. The
    // touched list keeps resetting this proportional to the item's fan-in.
    let mut producer_finish = vec![0u64; lane_count];
    let mut touched = Vec::new();
    let mut total_cost = 0u64;
    let sync = options.synchronization_cost;

    let mut previous_lane = None::<usize>;
    for item in 0..count {
        let group = input.groups[item];
        let Some(fixed) = group_lanes.get(group).copied() else {
            return Err(LaneError::InvalidIdentifier);
        };
        let mut latest_producer = None::<(u64, usize)>;
        for &predecessor in &input.predecessors[item] {
            if predecessor >= item {
                return Err(LaneError::NotTopological { item, predecessor });
            }
            let lane = item_lanes[predecessor] as usize;
            if producer_finish[lane] == 0 {
                touched.push(lane);
            }
            producer_finish[lane] = producer_finish[lane].max(finish[predecessor].max(1));
            if latest_producer.is_none_or(|(time, _)| finish[predecessor] > time) {
                latest_producer = Some((finish[predecessor], lane));
            }
        }
        // The best and second-best producer lanes give every lane's remote
        // ready time without scanning all predecessors once per lane.
        let mut best = (0u64, usize::MAX);
        let mut second = 0u64;
        for &lane in &touched {
            let time = producer_finish[lane];
            if time > best.0 {
                second = best.0;
                best = (time, lane);
            } else if time > second {
                second = time;
            }
        }
        let ready_in = |lane: usize| {
            let remote = if best.1 == lane { second } else { best.0 };
            let remote = if remote == 0 {
                0
            } else {
                remote.saturating_add(sync)
            };
            producer_finish[lane].max(remote)
        };
        let lane = match fixed {
            Some(lane) => lane as usize,
            None => {
                let mut selected = 0usize;
                let mut selected_key = (u64::MAX, u64::MAX);
                for (lane, &available) in lane_available.iter().enumerate() {
                    let start = available.max(ready_in(lane));
                    // Prefer the earliest start, then the least loaded lane so
                    // independent work spreads instead of piling onto lane 0.
                    let key = (start, available);
                    if key < selected_key {
                        selected_key = key;
                        selected = lane;
                    }
                }
                let local = latest_producer.map(|(_, lane)| lane).or(previous_lane);
                match local {
                    Some(lane)
                        if lane_available[lane].max(ready_in(lane))
                            <= selected_key.0.saturating_add(options.locality_slack) =>
                    {
                        lane
                    }
                    _ => selected,
                }
            }
        };
        let start = lane_available[lane].max(ready_in(lane));
        let cost = input.costs[item];
        finish[item] = start.saturating_add(cost);
        lane_available[lane] = finish[item];
        item_lanes[item] = lane as u32;
        group_lanes[group] = Some(lane as u32);
        total_cost = total_cost.saturating_add(cost);
        previous_lane = Some(lane);
        for lane in touched.drain(..) {
            producer_finish[lane] = 0;
        }
    }

    Ok(LaneAssignment {
        item_lanes,
        estimated_makespan: lane_available.into_iter().max().unwrap_or(0),
        total_cost,
    })
}

/// A lane assignment together with an execution order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaneSchedulePlan {
    /// Lane of each item.
    pub item_lanes: Vec<u32>,
    /// Items in a topological order in which each lane runs its items.
    pub order: Vec<usize>,
    /// Estimated completion time of the schedule.
    pub estimated_makespan: u64,
}

/// Assign items to lanes and order them by critical-path priority.
///
/// Unlike [`assign_lanes`], which places items in their given order, this
/// list scheduler places the ready item with the longest remaining path to
/// the end of the DAG first. Work that other lanes wait for therefore runs
/// early in its lane, which matters most when lanes are fixed in advance (for
/// example one lane per instance subtree). Lane choice, ownership groups,
/// synchronization and locality follow [`assign_lanes`].
pub fn list_schedule_lanes(
    input: LaneAssignmentInput<'_>,
    options: LaneAssignmentOptions,
) -> Result<LaneSchedulePlan, LaneError> {
    use std::cmp::Reverse;
    use std::collections::BinaryHeap;

    let count = input.costs.len();
    if input.predecessors.len() != count || input.groups.len() != count {
        return Err(LaneError::Shape);
    }
    let lane_count = options.lanes.max(1) as usize;
    for &lane in input.group_lanes.iter().flatten() {
        if lane as usize >= lane_count {
            return Err(LaneError::InvalidIdentifier);
        }
    }
    let mut successors = vec![Vec::new(); count];
    for (item, predecessors) in input.predecessors.iter().enumerate() {
        for &predecessor in predecessors {
            if predecessor >= item {
                return Err(LaneError::NotTopological { item, predecessor });
            }
            successors[predecessor].push(item);
        }
    }
    if input
        .groups
        .iter()
        .any(|&group| group >= input.group_lanes.len())
    {
        return Err(LaneError::InvalidIdentifier);
    }
    // Longest path from each item to the end of the DAG, itself included.
    // An edge between two lanes fixed in advance also costs a
    // synchronization, so work feeding another lane ranks higher.
    let fixed_lane = |item: usize| input.group_lanes[input.groups[item]];
    let mut bottom_level = vec![0u64; count];
    for item in (0..count).rev() {
        let tail = successors[item]
            .iter()
            .map(|&successor| {
                let crossing = matches!(
                    (fixed_lane(item), fixed_lane(successor)),
                    (Some(from), Some(to)) if from != to
                );
                bottom_level[successor].saturating_add(if crossing {
                    options.synchronization_cost
                } else {
                    0
                })
            })
            .max()
            .unwrap_or(0);
        bottom_level[item] = tail.saturating_add(input.costs[item]);
    }

    let mut group_lanes = input.group_lanes.to_vec();
    let mut item_lanes = vec![0u32; count];
    let mut start = vec![0u64; count];
    let mut finish = vec![0u64; count];
    let mut rank = vec![0usize; count];
    let mut lane_available = vec![0u64; lane_count];
    let mut producer_finish = vec![0u64; lane_count];
    let mut touched = Vec::new();
    let mut unresolved = input.predecessors.iter().map(Vec::len).collect::<Vec<_>>();
    let mut ready = (0..count)
        .filter(|&item| unresolved[item] == 0)
        .map(|item| (bottom_level[item], Reverse(item)))
        .collect::<BinaryHeap<_>>();
    let sync = options.synchronization_cost;
    let mut previous_lane = None::<usize>;
    let mut next_rank = 0;
    while let Some((_, Reverse(item))) = ready.pop() {
        let mut latest_producer = None::<(u64, usize)>;
        for &predecessor in &input.predecessors[item] {
            let lane = item_lanes[predecessor] as usize;
            if producer_finish[lane] == 0 {
                touched.push(lane);
            }
            producer_finish[lane] = producer_finish[lane].max(finish[predecessor].max(1));
            if latest_producer.is_none_or(|(time, _)| finish[predecessor] > time) {
                latest_producer = Some((finish[predecessor], lane));
            }
        }
        let mut best = (0u64, usize::MAX);
        let mut second = 0u64;
        for &lane in &touched {
            let time = producer_finish[lane];
            if time > best.0 {
                second = best.0;
                best = (time, lane);
            } else if time > second {
                second = time;
            }
        }
        let ready_in = |lane: usize| {
            let remote = if best.1 == lane { second } else { best.0 };
            let remote = if remote == 0 {
                0
            } else {
                remote.saturating_add(sync)
            };
            producer_finish[lane].max(remote)
        };
        let group = input.groups[item];
        let lane = match group_lanes[group] {
            Some(lane) => lane as usize,
            None => {
                let mut selected = 0usize;
                let mut selected_key = (u64::MAX, u64::MAX);
                for (lane, &available) in lane_available.iter().enumerate() {
                    let key = (available.max(ready_in(lane)), available);
                    if key < selected_key {
                        selected_key = key;
                        selected = lane;
                    }
                }
                match latest_producer.map(|(_, lane)| lane).or(previous_lane) {
                    Some(lane)
                        if lane_available[lane].max(ready_in(lane))
                            <= selected_key.0.saturating_add(options.locality_slack) =>
                    {
                        lane
                    }
                    _ => selected,
                }
            }
        };
        start[item] = lane_available[lane].max(ready_in(lane));
        finish[item] = start[item].saturating_add(input.costs[item]);
        lane_available[lane] = finish[item];
        item_lanes[item] = lane as u32;
        group_lanes[group] = Some(lane as u32);
        rank[item] = next_rank;
        next_rank += 1;
        previous_lane = Some(lane);
        for lane in touched.drain(..) {
            producer_finish[lane] = 0;
        }
        for &successor in &successors[item] {
            unresolved[successor] -= 1;
            if unresolved[successor] == 0 {
                ready.push((bottom_level[successor], Reverse(successor)));
            }
        }
    }
    // Start times respect every dependency, and ties keep the placement
    // order, in which producers precede their consumers.
    let mut order = (0..count).collect::<Vec<_>>();
    order.sort_unstable_by_key(|&item| (start[item], rank[item]));
    Ok(LaneSchedulePlan {
        item_lanes,
        order,
        estimated_makespan: lane_available.into_iter().max().unwrap_or(0),
    })
}

/// One wait in a task prologue: the lane must have completed at least
/// `completed_tasks` tasks of the current execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LaneWait {
    pub lane: u32,
    pub completed_tasks: u32,
}

/// A maximal run of one lane's items that starts after its waits complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaneTask {
    pub lane: u32,
    /// Position of this task in its lane's task sequence.
    pub sequence: u32,
    /// Items in execution order.
    pub items: Vec<usize>,
    /// Non-redundant waits, sorted by lane.
    pub waits: Vec<LaneWait>,
}

/// Cut lane sequences into tasks with minimal cross-lane waits.
///
/// The returned tasks are listed in creation order, which is also a valid
/// sequential execution order of all items.
pub fn form_lane_tasks(
    lane_count: u32,
    item_lanes: &[u32],
    predecessors: &[Vec<usize>],
) -> Result<Vec<LaneTask>, LaneError> {
    form_lane_tasks_separated(lane_count, item_lanes, predecessors, &[])
}

/// [`form_lane_tasks`] with items that must not share a task.
///
/// `classes` holds a two-bit class per item (or is empty). An item of class
/// 1 and an item of class 2 never share a task; class 0 is compatible with
/// both, and an item of class 3 shares a task with nothing. Callers use this
/// when a task becomes one generated function whose code generation depends
/// on the combination of its items.
pub fn form_lane_tasks_separated(
    lane_count: u32,
    item_lanes: &[u32],
    predecessors: &[Vec<usize>],
    classes: &[u8],
) -> Result<Vec<LaneTask>, LaneError> {
    let count = item_lanes.len();
    if predecessors.len() != count || (!classes.is_empty() && classes.len() != count) {
        return Err(LaneError::Shape);
    }
    let class_of = |item: usize| classes.get(item).copied().unwrap_or(0) & 3;
    let mut task_classes: Vec<u8> = Vec::new();
    let lane_count = lane_count.max(1) as usize;
    let mut tasks: Vec<LaneTask> = Vec::new();
    // Vector clock known at the start of each task: for every lane, how many
    // of its tasks have completed.
    let mut task_clocks: Vec<Vec<u32>> = Vec::new();
    let mut lane_tasks: Vec<Vec<usize>> = vec![Vec::new(); lane_count];
    let mut open = vec![None::<usize>; lane_count];
    let mut lane_clock = vec![vec![0u32; lane_count]; lane_count];
    let mut task_of_item = vec![usize::MAX; count];
    let mut required = vec![0u32; lane_count];
    let mut touched = Vec::new();

    for item in 0..count {
        let lane = item_lanes[item] as usize;
        if lane >= lane_count {
            return Err(LaneError::InvalidIdentifier);
        }
        for &predecessor in &predecessors[item] {
            if predecessor >= item {
                return Err(LaneError::NotTopological { item, predecessor });
            }
            let producer_lane = item_lanes[predecessor] as usize;
            if producer_lane == lane {
                continue;
            }
            let producer_task = task_of_item[predecessor];
            let completed = tasks[producer_task].sequence + 1;
            if completed > lane_clock[lane][producer_lane] {
                if required[producer_lane] == 0 {
                    touched.push(producer_lane);
                }
                required[producer_lane] = required[producer_lane].max(completed);
            }
        }

        if touched.is_empty()
            && let Some(task) = open[lane]
            && (task_classes[task] | class_of(item)) != 3
        {
            tasks[task].items.push(item);
            task_classes[task] |= class_of(item);
            task_of_item[item] = task;
            continue;
        }

        touched.sort_unstable();
        let mut clock = lane_clock[lane].clone();
        let mut waits = Vec::with_capacity(touched.len());
        for &producer_lane in &touched {
            let completed = required[producer_lane];
            let producer_task = lane_tasks[producer_lane][completed as usize - 1];
            // Later items of the producer lane must not join a task that a
            // consumer already waits for.
            if open[producer_lane] == Some(producer_task) {
                open[producer_lane] = None;
            }
            for (known, &implied) in clock.iter_mut().zip(&task_clocks[producer_task]) {
                *known = (*known).max(implied);
            }
            waits.push(LaneWait {
                lane: producer_lane as u32,
                completed_tasks: completed,
            });
        }
        for &producer_lane in &touched {
            clock[producer_lane] = clock[producer_lane].max(required[producer_lane]);
        }
        // Drop waits implied by another wait's transitive knowledge.
        let direct = waits
            .iter()
            .copied()
            .filter(|wait| {
                !waits.iter().any(|other| {
                    other.lane != wait.lane && {
                        let other_task =
                            lane_tasks[other.lane as usize][other.completed_tasks as usize - 1];
                        task_clocks[other_task][wait.lane as usize] >= wait.completed_tasks
                    }
                })
            })
            .collect::<Vec<_>>();
        for producer_lane in touched.drain(..) {
            required[producer_lane] = 0;
        }

        let task = tasks.len();
        let sequence = lane_tasks[lane].len() as u32;
        tasks.push(LaneTask {
            lane: lane as u32,
            sequence,
            items: vec![item],
            waits: direct,
        });
        task_classes.push(class_of(item));
        lane_clock[lane] = clock.clone();
        task_clocks.push(clock);
        lane_tasks[lane].push(task);
        open[lane] = Some(task);
        task_of_item[item] = task;
    }
    Ok(tasks)
}

/// Estimated completion time of `tasks` when every task costs the sum of its
/// items plus `task_overhead`, and a wait completes `synchronization_cost`
/// after the producer task finishes.
///
/// Unlike the estimate of [`assign_lanes`], this charges the task boundaries
/// that the waits actually create.
pub fn estimate_task_makespan(
    lane_count: u32,
    tasks: &[LaneTask],
    item_costs: &[u64],
    synchronization_cost: u64,
    task_overhead: u64,
) -> u64 {
    let lane_count = lane_count.max(1) as usize;
    let mut lane_time = vec![0u64; lane_count];
    let mut finished = vec![Vec::<u64>::new(); lane_count];
    for task in tasks {
        let lane = (task.lane as usize).min(lane_count - 1);
        let mut start = lane_time[lane];
        for wait in &task.waits {
            let producer = finished
                .get(wait.lane as usize)
                .and_then(|times| times.get((wait.completed_tasks as usize).checked_sub(1)?))
                .copied()
                .unwrap_or(0);
            start = start.max(producer.saturating_add(synchronization_cost));
        }
        let cost = task
            .items
            .iter()
            .filter_map(|&item| item_costs.get(item))
            .fold(task_overhead, |total, cost| total.saturating_add(*cost));
        let finish = start.saturating_add(cost);
        lane_time[lane] = finish;
        finished[lane].push(finish);
    }
    lane_time.into_iter().max().unwrap_or(0)
}

/// Check that `tasks` honour every dependency of `predecessors`.
///
/// Returns the first violated `(predecessor, item)` edge. This is an
/// independent oracle for [`form_lane_tasks`] and for callers that rewrite a
/// task list after formation.
pub fn verify_lane_tasks(
    lane_count: u32,
    tasks: &[LaneTask],
    predecessors: &[Vec<usize>],
) -> Result<(), (usize, usize)> {
    let lane_count = lane_count.max(1) as usize;
    let count = predecessors.len();
    let mut position = vec![(usize::MAX, usize::MAX); count];
    let mut lane_sequences: Vec<Vec<usize>> = vec![Vec::new(); lane_count];
    for (index, task) in tasks.iter().enumerate() {
        let lane = task.lane as usize;
        if lane >= lane_count || task.sequence as usize != lane_sequences[lane].len() {
            return Err((usize::MAX, usize::MAX));
        }
        lane_sequences[lane].push(index);
        for (offset, &item) in task.items.iter().enumerate() {
            if item >= count || position[item].0 != usize::MAX {
                return Err((usize::MAX, item.min(count)));
            }
            position[item] = (index, offset);
        }
    }
    if position.iter().any(|(task, _)| *task == usize::MAX) {
        return Err((usize::MAX, usize::MAX));
    }
    // Transitive knowledge at each task start, computed independently of
    // the formation algorithm.
    let mut clocks: Vec<Vec<u32>> = Vec::with_capacity(tasks.len());
    let mut lane_previous: Vec<Option<usize>> = vec![None; lane_count];
    for (index, task) in tasks.iter().enumerate() {
        let lane = task.lane as usize;
        let mut clock = lane_previous[lane]
            .map(|previous| {
                let mut clock: Vec<u32> = clocks[previous].clone();
                clock[lane] = clock[lane].max(tasks[previous].sequence + 1);
                clock
            })
            .unwrap_or_else(|| vec![0; lane_count]);
        for wait in &task.waits {
            let waited_lane = wait.lane as usize;
            if waited_lane >= lane_count || waited_lane == lane || wait.completed_tasks == 0 {
                return Err((usize::MAX, usize::MAX));
            }
            let Some(&waited) = lane_sequences[waited_lane].get(wait.completed_tasks as usize - 1)
            else {
                return Err((usize::MAX, usize::MAX));
            };
            if waited >= index {
                return Err((usize::MAX, usize::MAX));
            }
            for (known, &implied) in clock.iter_mut().zip(&clocks[waited]) {
                *known = (*known).max(implied);
            }
            clock[waited_lane] = clock[waited_lane].max(wait.completed_tasks);
        }
        lane_previous[lane] = Some(index);
        clocks.push(clock);
    }
    for (item, item_predecessors) in predecessors.iter().enumerate() {
        let (task, offset) = position[item];
        for &predecessor in item_predecessors {
            let (producer_task, producer_offset) = position[predecessor];
            let ordered = if tasks[producer_task].lane == tasks[task].lane {
                (producer_task, producer_offset) < (task, offset)
            } else {
                clocks[task][tasks[producer_task].lane as usize] > tasks[producer_task].sequence
            };
            if !ordered {
                return Err((predecessor, item));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assign(costs: &[u64], predecessors: &[Vec<usize>], lanes: u32, sync: u64) -> LaneAssignment {
        let groups = (0..costs.len()).collect::<Vec<_>>();
        let group_lanes = vec![None; costs.len()];
        assign_lanes(
            LaneAssignmentInput {
                costs,
                predecessors,
                groups: &groups,
                group_lanes: &group_lanes,
            },
            LaneAssignmentOptions {
                lanes,
                synchronization_cost: sync,
                locality_slack: 0,
            },
        )
        .unwrap()
    }

    #[test]
    fn independent_items_spread_across_lanes() {
        let costs = vec![10; 8];
        let predecessors = vec![Vec::new(); 8];
        let result = assign(&costs, &predecessors, 4, 100);
        let mut per_lane = [0; 4];
        for lane in &result.item_lanes {
            per_lane[*lane as usize] += 1;
        }
        assert_eq!(per_lane, [2, 2, 2, 2]);
        assert_eq!(result.estimated_makespan, 20);
        assert_eq!(result.total_cost, 80);
    }

    #[test]
    fn expensive_synchronization_keeps_a_chain_in_one_lane() {
        let costs = vec![10; 4];
        let predecessors = vec![vec![], vec![0], vec![1], vec![2]];
        let result = assign(&costs, &predecessors, 4, 1000);
        assert!(
            result
                .item_lanes
                .iter()
                .all(|lane| *lane == result.item_lanes[0])
        );
        assert_eq!(result.estimated_makespan, 40);
    }

    #[test]
    fn locality_slack_keeps_runs_of_independent_items_together() {
        let costs = vec![10; 8];
        let predecessors = vec![Vec::new(); 8];
        let groups = (0..8).collect::<Vec<_>>();
        let group_lanes = vec![None; 8];
        let result = assign_lanes(
            LaneAssignmentInput {
                costs: &costs,
                predecessors: &predecessors,
                groups: &groups,
                group_lanes: &group_lanes,
            },
            LaneAssignmentOptions {
                lanes: 2,
                synchronization_cost: 100,
                locality_slack: 30,
            },
        )
        .unwrap();
        // A lane keeps taking items until it is more than the slack ahead.
        assert_eq!(result.item_lanes, vec![0, 0, 0, 0, 1, 1, 1, 1]);
        assert_eq!(result.estimated_makespan, 40);
    }

    #[test]
    fn ownership_groups_share_a_lane_and_pins_are_respected() {
        let costs = vec![10, 10, 10, 10];
        let predecessors = vec![Vec::new(); 4];
        let groups = vec![0, 1, 0, 2];
        let group_lanes = vec![None, Some(3), None];
        let result = assign_lanes(
            LaneAssignmentInput {
                costs: &costs,
                predecessors: &predecessors,
                groups: &groups,
                group_lanes: &group_lanes,
            },
            LaneAssignmentOptions {
                lanes: 4,
                synchronization_cost: 1,
                locality_slack: 0,
            },
        )
        .unwrap();
        assert_eq!(result.item_lanes[0], result.item_lanes[2]);
        assert_eq!(result.item_lanes[1], 3);
    }

    #[test]
    fn non_topological_input_is_rejected() {
        let costs = vec![1, 1];
        let predecessors = vec![vec![1], vec![]];
        let groups = vec![0, 1];
        let group_lanes = vec![None, None];
        assert_eq!(
            assign_lanes(
                LaneAssignmentInput {
                    costs: &costs,
                    predecessors: &predecessors,
                    groups: &groups,
                    group_lanes: &group_lanes,
                },
                LaneAssignmentOptions {
                    lanes: 2,
                    synchronization_cost: 0,
                    locality_slack: 0,
                },
            ),
            Err(LaneError::NotTopological {
                item: 0,
                predecessor: 1
            })
        );
    }

    #[test]
    fn tasks_split_only_at_new_cross_lane_dependencies() {
        // lane 0: 0 -> 2 -> 4 -> 6, lane 1: 1 -> 3 (depends on 0) -> 5
        // (depends on 2, already covered) -> 7 (depends on 6).
        let lanes = [0, 1, 0, 1, 0, 1, 0, 1];
        let predecessors = vec![
            vec![],
            vec![],
            vec![0],
            vec![1, 0],
            vec![2],
            vec![3, 2],
            vec![4],
            vec![5, 6],
        ];
        let tasks = form_lane_tasks(2, &lanes, &predecessors).unwrap();
        verify_lane_tasks(2, &tasks, &predecessors).unwrap();
        let lane0 = tasks
            .iter()
            .filter(|task| task.lane == 0)
            .collect::<Vec<_>>();
        // The consumer of item 0 closes the task holding items 0 and 2.
        assert_eq!(lane0[0].items, vec![0, 2]);
        assert_eq!(lane0[1].items, vec![4, 6]);
        let lane1 = tasks
            .iter()
            .filter(|task| task.lane == 1)
            .collect::<Vec<_>>();
        assert_eq!(lane1.len(), 3);
        assert_eq!(lane1[0].items, vec![1]);
        assert!(lane1[0].waits.is_empty());
        assert_eq!(lane1[1].items, vec![3, 5]);
        assert_eq!(
            lane1[1].waits,
            vec![LaneWait {
                lane: 0,
                completed_tasks: 1
            }]
        );
        assert_eq!(lane1[2].items, vec![7]);
        assert_eq!(
            lane1[2].waits,
            vec![LaneWait {
                lane: 0,
                completed_tasks: 2
            }]
        );
    }

    #[test]
    fn list_scheduling_runs_critical_work_first() {
        // Item 0 is independent; item 1 feeds a chain on another lane whose
        // synchronization makes it the longer path. With fixed lanes, lane 0
        // should run 1 before 0.
        let costs = vec![6, 1, 1, 1];
        let predecessors = vec![vec![], vec![], vec![1], vec![2]];
        let groups = vec![0, 0, 1, 1];
        let group_lanes = vec![Some(0), Some(1)];
        let plan = list_schedule_lanes(
            LaneAssignmentInput {
                costs: &costs,
                predecessors: &predecessors,
                groups: &groups,
                group_lanes: &group_lanes,
            },
            LaneAssignmentOptions {
                lanes: 2,
                synchronization_cost: 5,
                locality_slack: 0,
            },
        )
        .unwrap();
        assert_eq!(plan.item_lanes, vec![0, 0, 1, 1]);
        let position = |item| plan.order.iter().position(|&x| x == item).unwrap();
        assert!(position(1) < position(0));
        // Lane 0 runs 1 then 0 (finishing at 7); lane 1 starts the chain
        // after the synchronization and finishes at 1 + 5 + 2.
        assert_eq!(plan.estimated_makespan, 8);
        for (item, predecessors) in predecessors.iter().enumerate() {
            for &predecessor in predecessors {
                assert!(position(predecessor) < position(item));
            }
        }
    }

    #[test]
    fn task_makespan_charges_task_boundaries_and_waits() {
        let tasks = vec![
            LaneTask {
                lane: 0,
                sequence: 0,
                items: vec![0],
                waits: Vec::new(),
            },
            LaneTask {
                lane: 1,
                sequence: 0,
                items: vec![1, 2],
                waits: vec![LaneWait {
                    lane: 0,
                    completed_tasks: 1,
                }],
            },
        ];
        // Lane 1 starts at 10 + 5 (wait) and runs 2 + 3 + 1 (overhead).
        assert_eq!(estimate_task_makespan(2, &tasks, &[9, 2, 3], 5, 1), 21);
    }

    #[test]
    fn transitive_waits_are_omitted() {
        // 0 (lane 0) -> 1 (lane 1) -> 2 (lane 2); 2 also depends on 0.
        let lanes = [0, 1, 2];
        let predecessors = vec![vec![], vec![0], vec![1, 0]];
        let tasks = form_lane_tasks(3, &lanes, &predecessors).unwrap();
        verify_lane_tasks(3, &tasks, &predecessors).unwrap();
        let last = tasks.iter().find(|task| task.lane == 2).unwrap();
        assert_eq!(
            last.waits,
            vec![LaneWait {
                lane: 1,
                completed_tasks: 1
            }]
        );
    }

    #[test]
    fn separated_classes_never_share_a_task() {
        let lanes = [0, 0, 0, 0];
        let predecessors = vec![vec![], vec![], vec![], vec![]];
        let tasks = form_lane_tasks_separated(1, &lanes, &predecessors, &[1, 0, 2, 2]).unwrap();
        verify_lane_tasks(1, &tasks, &predecessors).unwrap();
        assert_eq!(
            tasks
                .iter()
                .map(|task| task.items.clone())
                .collect::<Vec<_>>(),
            vec![vec![0, 1], vec![2, 3]]
        );
    }

    #[test]
    fn verification_rejects_a_missing_wait() {
        let lanes = [0, 1];
        let predecessors = vec![vec![], vec![0]];
        let mut tasks = form_lane_tasks(2, &lanes, &predecessors).unwrap();
        for task in &mut tasks {
            task.waits.clear();
        }
        assert_eq!(verify_lane_tasks(2, &tasks, &predecessors), Err((0, 1)));
    }

    #[test]
    fn randomized_graphs_form_valid_tasks() {
        // Deterministic pseudo-random DAGs; the verifier is the oracle.
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..200 {
            let count = (next() % 40) as usize + 1;
            let lane_count = (next() % 5) as u32 + 1;
            let mut predecessors = vec![Vec::new(); count];
            for (item, row) in predecessors.iter_mut().enumerate() {
                for predecessor in 0..item {
                    if next() % 5 == 0 {
                        row.push(predecessor);
                    }
                }
            }
            let costs = (0..count).map(|_| next() % 50 + 1).collect::<Vec<_>>();
            let assignment = assign(&costs, &predecessors, lane_count, next() % 40);
            let tasks = form_lane_tasks(lane_count, &assignment.item_lanes, &predecessors).unwrap();
            verify_lane_tasks(lane_count, &tasks, &predecessors).unwrap();
            let random_lanes = (0..count)
                .map(|_| (next() % u64::from(lane_count)) as u32)
                .collect::<Vec<_>>();
            let tasks = form_lane_tasks(lane_count, &random_lanes, &predecessors).unwrap();
            verify_lane_tasks(lane_count, &tasks, &predecessors).unwrap();

            // Priority list scheduling: a topological order that keeps
            // ownership groups and fixed lanes.
            let groups = (0..count)
                .map(|_| (next() % count as u64) as usize)
                .collect::<Vec<_>>();
            let group_lanes = (0..count)
                .map(|_| (next() % 3 == 0).then(|| (next() % u64::from(lane_count)) as u32))
                .collect::<Vec<_>>();
            let plan = list_schedule_lanes(
                LaneAssignmentInput {
                    costs: &costs,
                    predecessors: &predecessors,
                    groups: &groups,
                    group_lanes: &group_lanes,
                },
                LaneAssignmentOptions {
                    lanes: lane_count,
                    synchronization_cost: next() % 40,
                    locality_slack: next() % 20,
                },
            )
            .unwrap();
            let mut position = vec![usize::MAX; count];
            for (index, &item) in plan.order.iter().enumerate() {
                assert_eq!(position[item], usize::MAX, "item listed twice");
                position[item] = index;
            }
            for (item, row) in predecessors.iter().enumerate() {
                assert!(
                    row.iter()
                        .all(|&predecessor| position[predecessor] < position[item])
                );
                if let Some(lane) = group_lanes[groups[item]] {
                    assert_eq!(plan.item_lanes[item], lane);
                }
                let first = groups
                    .iter()
                    .position(|&group| group == groups[item])
                    .unwrap();
                assert_eq!(plan.item_lanes[item], plan.item_lanes[first]);
            }
        }
    }
}
