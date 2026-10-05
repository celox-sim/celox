//! Lane-partitioned lowering of one combinational schedule.
//!
//! The partition reuses the sequential scheduler's MemorySSA dependency graph
//! and region order; it does not introduce another notion of dependence.
//! Scheduled work that writes the same state object shares a lane, so every
//! state home has exactly one writer lane and physical layout can keep homes
//! of different lanes apart. Observable runtime effects and stores to event
//! signals, which update shared runtime buffers, stay in lane 0.
//!
//! Each task, a run of one lane's work that needs no new cross-lane wait, is
//! lowered through its own SIR builder. The resulting units are listed in a
//! valid sequential order; their lanes only express placement.

use celox_analysis::lanes::{
    LaneAssignmentInput, LaneAssignmentOptions, LaneSchedulePlan, LaneTask, estimate_task_makespan,
    form_lane_tasks, list_schedule_lanes,
};
use celox_sir::LaneUnit;

use super::*;

/// Request for a lane-partitioned lowering.
#[derive(Debug, Clone)]
pub struct LanePartitionOptions<Addr> {
    /// Number of lanes. Fewer than two lanes disables partitioning.
    pub lanes: u32,
    /// Estimated delay, in SLT operation units, for one lane to observe a
    /// value produced by another lane.
    pub synchronization_cost: u64,
    /// Objects whose writers must run in lane 0.
    pub lane0_targets: HashSet<Addr>,
    /// Smallest accepted ratio between the single-lane cost and the
    /// estimated partitioned makespan, in percent.
    pub minimum_speedup_percent: u64,
    /// Instance hierarchy of the scheduled objects, if known.
    pub hierarchy: Option<InstanceHierarchy<Addr>>,
}

/// Instance tree of the objects in a schedule.
///
/// Logic of one instance subtree mostly communicates within the subtree, so
/// keeping subtrees whole is a partition with few cross-lane waits.
#[derive(Debug, Clone)]
pub struct InstanceHierarchy<Addr> {
    /// The instance owning an object, as an index into `parents`.
    pub instance_of: fn(&Addr) -> usize,
    /// The parent of every instance, or `usize::MAX` for a root.
    pub parents: Vec<usize>,
}

/// Estimated cost of one task boundary: the call, the progress publication,
/// and reloading state the previous task held in registers.
const TASK_OVERHEAD_DIVISOR: u64 = 4;

/// Lanes of the ownership groups when every instance subtree that fits in
/// one lane's share stays in one lane.
///
/// Subtrees are chosen top-down: a subtree whose cost fits a lane's share is
/// one cluster, otherwise the instance's own logic is a cluster and its
/// children are considered. Clusters are placed with a
/// longest-processing-time heuristic; pinned groups keep their lanes.
/// Returns `None` when fewer than two clusters have work.
fn cluster_group_lanes(
    item_instances: &[usize],
    costs: &[u64],
    groups: &[usize],
    group_lanes: &[Option<u32>],
    parents: &[usize],
    lanes: u32,
) -> Option<Vec<Option<u32>>> {
    let instance_count = parents.len();
    let root_of = |instance: usize| {
        if instance < instance_count {
            instance
        } else {
            usize::MAX
        }
    };
    let mut own_cost = vec![0u64; instance_count];
    for (item, &instance) in item_instances.iter().enumerate() {
        if let Some(cost) = own_cost.get_mut(instance) {
            *cost = cost.saturating_add(costs[item]);
        }
    }
    let mut children = vec![Vec::new(); instance_count];
    let mut roots = Vec::new();
    for (instance, &parent) in parents.iter().enumerate() {
        match children.get_mut(parent) {
            Some(list) => list.push(instance),
            None => roots.push(instance),
        }
    }
    // Subtree costs in reverse breadth-first order.
    let mut order = roots.clone();
    let mut next = 0;
    while next < order.len() {
        let instance = order[next];
        next += 1;
        order.extend(children[instance].iter().copied());
    }
    if order.len() != instance_count {
        return None;
    }
    let mut subtree = own_cost.clone();
    for &instance in order.iter().rev() {
        if let Some(&parent) = parents.get(instance)
            && parent < instance_count
        {
            subtree[parent] = subtree[parent].saturating_add(subtree[instance]);
        }
    }
    let total = costs.iter().copied().fold(0u64, u64::saturating_add);
    let share = total.div_ceil(u64::from(lanes.max(1)));

    let mut cluster = vec![usize::MAX; instance_count];
    let mut stack = roots
        .iter()
        .map(|&root| (root, None::<usize>))
        .collect::<Vec<_>>();
    while let Some((instance, inherited)) = stack.pop() {
        let assigned = match inherited {
            Some(cluster) => Some(cluster),
            None if subtree[instance] <= share => Some(instance),
            None => None,
        };
        cluster[instance] = assigned.unwrap_or(instance);
        for &child in &children[instance] {
            stack.push((child, assigned));
        }
    }

    let mut cluster_costs = HashMap::<usize, u64>::default();
    for (item, &instance) in item_instances.iter().enumerate() {
        let key = root_of(instance);
        let key = if key == usize::MAX { key } else { cluster[key] };
        *cluster_costs.entry(key).or_default() += costs[item];
    }
    if cluster_costs.values().filter(|cost| **cost > 0).count() < 2 {
        return None;
    }

    let mut loads = vec![0u64; lanes as usize];
    for (item, &group) in groups.iter().enumerate() {
        if let Some(lane) = group_lanes[group] {
            loads[lane as usize] = loads[lane as usize].saturating_add(costs[item]);
        }
    }
    let mut ordered = cluster_costs.into_iter().collect::<Vec<_>>();
    ordered.sort_unstable_by_key(|&(key, cost)| (std::cmp::Reverse(cost), key));
    let mut cluster_lanes = HashMap::<usize, u32>::default();
    for (key, cost) in ordered {
        let lane = (0..lanes as usize)
            .min_by_key(|lane| (loads[*lane], *lane))
            .expect("at least one lane");
        loads[lane] = loads[lane].saturating_add(cost);
        cluster_lanes.insert(key, lane as u32);
    }

    let mut lanes_of_groups = group_lanes.to_vec();
    for (item, &group) in groups.iter().enumerate() {
        if lanes_of_groups[group].is_none() {
            let key = root_of(item_instances[item]);
            let key = if key == usize::MAX { key } else { cluster[key] };
            lanes_of_groups[group] = cluster_lanes.get(&key).copied();
        }
    }
    Some(lanes_of_groups)
}

/// Lane-partitioned lowering of a combinational schedule.
#[derive(Debug)]
pub struct LaneScheduleResult<Addr> {
    /// Units in a valid sequential execution order.
    pub units: Vec<LaneUnit<Addr>>,
    pub runtime_errors: HashMap<i64, RuntimeErrorInfo<Addr>>,
    /// Estimated completion time of the partitioned schedule.
    pub estimated_makespan: u64,
    /// Estimated single-lane execution time.
    pub total_cost: u64,
}

fn work_paths(work: &ScheduledWork) -> &[usize] {
    match work {
        ScheduledWork::CombPath(path) => std::slice::from_ref(path),
        ScheduledWork::CombScc(paths) | ScheduledWork::GuardedComb { paths, .. } => paths,
        ScheduledWork::Ff(_) => &[],
    }
}

/// Assumed iteration count of a runtime loop whose bounds are unknown.
const LOOP_WEIGHT: u64 = 64;

/// Smallest estimated cost of a shared SLT value that makes the work items
/// sharing it one atomic item.
const SHARED_CONE_THRESHOLD: u64 = 16;

/// Bound on the nodes visited to estimate one shared value.
const SHARED_CONE_VISIT_LIMIT: usize = 256;

/// Largest 64-bit word count charged for one value. Wider values are whole
/// arrays or memories, which lowering only ever accesses element by element
/// (a dynamic select of a memory is one indexed load, not a copy of it).
const MAX_VALUE_WORDS: u64 = 64;

/// Estimated SIR operations of one SLT node, weighted by its 64-bit word
/// count; runtime loops are weighted as several iterations.
fn node_weight<Addr: Clone + Eq + Hash>(node: NodeId, arena: &SLTNodeArena<Addr>) -> u64 {
    let words = (crate::get_width(node, arena).div_ceil(64).max(1) as u64).min(MAX_VALUE_WORDS);
    match arena.get(node) {
        SLTNode::Constant(..) => 0,
        SLTNode::ForFold { .. } | SLTNode::ForFoldGroup { .. } => words.saturating_mul(LOOP_WEIGHT),
        _ => words,
    }
}

/// Estimated SIR operations needed to evaluate a set of LogicPaths with one
/// lowering cache: every SLT node reachable from a path counts once per
/// [`PathCostWalk::begin`].
struct PathCostWalk {
    visited: Vec<u32>,
    epoch: u32,
    stack: Vec<NodeId>,
}

impl PathCostWalk {
    fn new(node_count: usize) -> Self {
        Self {
            visited: vec![0; node_count],
            epoch: 0,
            stack: Vec::new(),
        }
    }

    /// Start a new lowering cache.
    fn begin(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        if self.epoch == 0 {
            self.visited.fill(0);
            self.epoch = 1;
        }
    }

    /// Cost of one path's nodes not yet counted since [`Self::begin`].
    fn add<Addr: Clone + Eq + Hash>(
        &mut self,
        path: &LogicPath<Addr>,
        arena: &SLTNodeArena<Addr>,
    ) -> u64 {
        self.stack.push(path.expr);
        self.stack.extend(path.pre_lower_nodes.iter().copied());
        self.stack
            .extend(path.local_inputs.iter().map(|(_, node)| *node));
        let mut total = match &path.target {
            LogicPathTarget::Var(target) => ((target.access.msb - target.access.lsb + 1)
                .div_ceil(64) as u64)
                .min(MAX_VALUE_WORDS),
            LogicPathTarget::CombCaptureEvent {
                guard,
                args,
                loop_runner,
                ..
            } => {
                self.stack.extend(guard.iter().copied());
                self.stack.extend(args.iter().copied());
                self.stack.extend(loop_runner.iter().copied());
                args.len() as u64 + 1
            }
        };
        while let Some(node) = self.stack.pop() {
            let Some(seen) = self.visited.get_mut(node.0) else {
                continue;
            };
            if *seen == self.epoch {
                continue;
            }
            *seen = self.epoch;
            total = total.saturating_add(node_weight(node, arena));
            push_scheduler_node_children(node, arena, &mut self.stack);
        }
        total.max(1)
    }

    /// Cost of scheduled work lowered after the work already counted since
    /// [`Self::begin`]. Loop SCCs execute their members repeatedly.
    fn add_work<Addr: Clone + Eq + Hash>(
        &mut self,
        work: &ScheduledWork,
        input: &[LogicPath<Addr>],
        arena: &SLTNodeArena<Addr>,
    ) -> u64 {
        let paths = work_paths(work);
        let cost = paths
            .iter()
            .map(|&path| self.add(&input[path], arena))
            .fold(0u64, u64::saturating_add);
        match work {
            ScheduledWork::CombScc(_) => cost.saturating_mul(paths.len() as u64 + 1),
            _ => cost,
        }
    }
}

/// Capped estimate of the work needed to compute one SLT value.
struct ConeCost {
    memo: Vec<u64>,
    visited: Vec<u32>,
    epoch: u32,
    stack: Vec<NodeId>,
}

impl ConeCost {
    fn new(node_count: usize) -> Self {
        Self {
            memo: vec![u64::MAX; node_count],
            visited: vec![0; node_count],
            epoch: 0,
            stack: Vec::new(),
        }
    }

    /// Estimated cost of `node` and everything below it, saturating at
    /// [`SHARED_CONE_THRESHOLD`].
    fn cost<Addr: Clone + Eq + Hash>(&mut self, node: NodeId, arena: &SLTNodeArena<Addr>) -> u64 {
        let Some(&known) = self.memo.get(node.0) else {
            return 0;
        };
        if known != u64::MAX {
            return known;
        }
        self.epoch = self.epoch.wrapping_add(1);
        if self.epoch == 0 {
            self.visited.fill(0);
            self.epoch = 1;
        }
        let mut total = 0u64;
        let mut visits = 0usize;
        self.stack.push(node);
        while let Some(current) = self.stack.pop() {
            let Some(seen) = self.visited.get_mut(current.0) else {
                continue;
            };
            if *seen == self.epoch {
                continue;
            }
            *seen = self.epoch;
            total = total.saturating_add(node_weight(current, arena));
            visits += 1;
            if total >= SHARED_CONE_THRESHOLD || visits >= SHARED_CONE_VISIT_LIMIT {
                break;
            }
            push_scheduler_node_children(current, arena, &mut self.stack);
        }
        self.stack.clear();
        let total = total.min(SHARED_CONE_THRESHOLD);
        self.memo[node.0] = total;
        total
    }
}

/// Join work items which share expensive SLT values.
///
/// Every builder lowers the SLT nodes its work reaches, so work lowered by
/// different builders recomputes each value it shares. Recomputing a load
/// or a slice is cheaper than the parallelism lost by joining, but a shared
/// deep expression would be repeated in every lane. Each node is expanded
/// by the first item that reaches it; a later item stops at the node and
/// records the sharing, so the walk is linear in the DAG.
fn join_expensive_sharing<Addr: Clone + Eq + Hash>(
    work: &[ScheduledWork],
    input: &[LogicPath<Addr>],
    arena: &SLTNodeArena<Addr>,
    parents: &mut [usize],
) {
    let mut owner = vec![usize::MAX; arena.len()];
    let mut seen = vec![usize::MAX; arena.len()];
    let mut cones = ConeCost::new(arena.len());
    let mut stack = Vec::new();
    let mut shared = Vec::new();
    for (index, item) in work.iter().enumerate() {
        if let ScheduledWork::GuardedComb { condition, .. } = item {
            stack.push(*condition);
        }
        for &path in work_paths(item) {
            stack.extend(cached_logic_path_roots(&input[path]));
        }
        while let Some(node) = stack.pop() {
            let Some(last) = seen.get_mut(node.0) else {
                continue;
            };
            if *last == index {
                continue;
            }
            *last = index;
            match owner[node.0] {
                usize::MAX => {
                    owner[node.0] = index;
                    push_scheduler_node_children(node, arena, &mut stack);
                }
                first => shared.push((first, node)),
            }
        }
        for (first, node) in shared.drain(..) {
            if find_root(parents, first) != find_root(parents, index)
                && cones.cost(node, arena) >= SHARED_CONE_THRESHOLD
            {
                union(parents, first, index);
            }
        }
    }
}

/// Condense joined work into items listed in a topological order.
///
/// `atomic` joins work items that one builder must lower. Joined work that
/// depends on itself through other work absorbs that work too, so the
/// condensed graph is acyclic. Members keep the scheduled order, which
/// already respects every dependency among them, and the items prefer the
/// scheduled order of their first members.
///
/// Returns the members and the predecessors of every item.
fn condense_work(
    atomic: &mut [usize],
    edges: &[(usize, usize)],
) -> (Vec<Vec<usize>>, Vec<Vec<usize>>) {
    let work_count = atomic.len();
    let mut class_of_root = vec![usize::MAX; work_count];
    let mut class_of_work = vec![usize::MAX; work_count];
    let mut class_count = 0;
    for (work, class) in class_of_work.iter_mut().enumerate() {
        let root = find_root(atomic, work);
        if class_of_root[root] == usize::MAX {
            class_of_root[root] = class_count;
            class_count += 1;
        }
        *class = class_of_root[root];
    }
    let mut successors = vec![Vec::new(); class_count];
    let mut predecessors = vec![Vec::new(); class_count];
    for &(producer, consumer) in edges {
        let (from, to) = (class_of_work[producer], class_of_work[consumer]);
        if from != to {
            successors[from].push(to);
            predecessors[to].push(from);
        }
    }
    for list in successors.iter_mut().chain(&mut predecessors) {
        list.sort_unstable();
        list.dedup();
    }

    // Kosaraju: finishing order on the graph, then components on its
    // transpose in reverse finishing order.
    let mut visited = vec![false; class_count];
    let mut finished = Vec::with_capacity(class_count);
    for seed in 0..class_count {
        if visited[seed] {
            continue;
        }
        visited[seed] = true;
        let mut stack = vec![(seed, 0usize)];
        while let Some((class, next)) = stack.last_mut() {
            if let Some(&successor) = successors[*class].get(*next) {
                *next += 1;
                if !visited[successor] {
                    visited[successor] = true;
                    stack.push((successor, 0));
                }
            } else {
                finished.push(*class);
                stack.pop();
            }
        }
    }
    let mut component_of_class = vec![usize::MAX; class_count];
    let mut component_count = 0;
    for &seed in finished.iter().rev() {
        if component_of_class[seed] != usize::MAX {
            continue;
        }
        component_of_class[seed] = component_count;
        let mut stack = vec![seed];
        while let Some(class) = stack.pop() {
            for &predecessor in &predecessors[class] {
                if component_of_class[predecessor] == usize::MAX {
                    component_of_class[predecessor] = component_count;
                    stack.push(predecessor);
                }
            }
        }
        component_count += 1;
    }

    let mut members = vec![Vec::new(); component_count];
    for work in 0..work_count {
        members[component_of_class[class_of_work[work]]].push(work);
    }
    let mut component_successors = vec![Vec::new(); component_count];
    for (class, targets) in successors.iter().enumerate() {
        let from = component_of_class[class];
        for &target in targets {
            let to = component_of_class[target];
            if from != to {
                component_successors[from].push(to);
            }
        }
    }
    let mut indegree = vec![0usize; component_count];
    for targets in &mut component_successors {
        targets.sort_unstable();
        targets.dedup();
        for &target in targets.iter() {
            indegree[target] += 1;
        }
    }
    let mut ready = (0..component_count)
        .filter(|component| indegree[*component] == 0)
        .map(|component| (members[component][0], component))
        .collect::<BTreeSet<_>>();
    let mut position = vec![usize::MAX; component_count];
    let mut order = Vec::with_capacity(component_count);
    while let Some((_, component)) = ready.pop_first() {
        position[component] = order.len();
        order.push(component);
        for &target in &component_successors[component] {
            indegree[target] -= 1;
            if indegree[target] == 0 {
                ready.insert((members[target][0], target));
            }
        }
    }
    debug_assert_eq!(order.len(), component_count, "condensation is acyclic");
    let mut item_predecessors = vec![Vec::new(); component_count];
    for (component, targets) in component_successors.iter().enumerate() {
        for &target in targets {
            item_predecessors[position[target]].push(position[component]);
        }
    }
    for list in &mut item_predecessors {
        list.sort_unstable();
    }
    let items = order
        .into_iter()
        .map(|component| std::mem::take(&mut members[component]))
        .collect();
    (items, item_predecessors)
}

/// Union every pair of items whose written byte ranges of one object
/// overlap. Writers of disjoint bytes (such as separate array elements) may
/// run in different lanes; physical read-modify-write spans are ordered later
/// by the effect analysis of the final code.
fn union_overlapping_writes<Addr: Clone + Eq + Hash + Ord>(
    parents: &mut [usize],
    mut writes: Vec<(Addr, usize, usize, usize)>,
) {
    writes.sort_unstable_by(|left, right| {
        (&left.0, left.1, left.2, left.3).cmp(&(&right.0, right.1, right.2, right.3))
    });
    let mut run: Option<(Addr, usize, usize)> = None;
    for (object, begin, end, item) in writes {
        match &mut run {
            Some((run_object, run_end, representative))
                if *run_object == object && begin < *run_end =>
            {
                *run_end = (*run_end).max(end);
                union(parents, *representative, item);
            }
            _ => run = Some((object, end, item)),
        }
    }
}

fn find_root(parents: &mut [usize], mut item: usize) -> usize {
    while parents[item] != item {
        parents[item] = parents[parents[item]];
        item = parents[item];
    }
    item
}

fn union(parents: &mut [usize], left: usize, right: usize) {
    let left = find_root(parents, left);
    let right = find_root(parents, right);
    if left != right {
        // Keep the earliest item as representative for determinism.
        let (root, child) = if left < right {
            (left, right)
        } else {
            (right, left)
        };
        parents[child] = root;
    }
}

/// Partition a combinational schedule into lanes and lower each task.
///
/// Returns `None` when the estimated partitioned makespan does not improve on
/// the single-lane cost by `minimum_speedup_percent`, or when the request has
/// fewer than two lanes.
#[allow(clippy::too_many_arguments)]
pub fn sort_lanes<Addr: Clone + Eq + Ord + Hash + Debug + Copy + Display>(
    input: Vec<LogicPath<Addr>>,
    arena: &SLTNodeArena<Addr>,
    ignored_loops: &HashSet<(Addr, Addr)>,
    true_loops: &HashMap<(Addr, Addr), usize>,
    four_state: bool,
    var_widths: &HashMap<Addr, usize>,
    unpacked_element_widths: &HashMap<Addr, usize>,
    first_runtime_error_code: i64,
    options: &LanePartitionOptions<Addr>,
) -> Result<Option<LaneScheduleResult<Addr>>, SchedulerError<Addr>> {
    if options.lanes < 2 || input.is_empty() {
        return Ok(None);
    }
    let prepared = prepare_schedule(
        input,
        arena,
        four_state,
        var_widths,
        unpacked_element_widths,
        None,
    )?;
    let work = &prepared.scheduled_work;
    let path_count = prepared.input.len();

    let mut work_of_path = vec![usize::MAX; path_count];
    for (index, item) in work.iter().enumerate() {
        for &path in work_paths(item) {
            *work_of_path
                .get_mut(path)
                .ok_or(SchedulerError::InvalidDependencyGraph)? = index;
        }
    }
    if work_of_path.contains(&usize::MAX) {
        return Err(SchedulerError::InvalidDependencyGraph);
    }

    // Atomic items, each lowered through one builder. A write which must
    // precede a comb observer materializes the observer's arguments before
    // storing, and the observer reuses those registers. Work sharing an
    // expensive value computes it once.
    let mut atomic = (0..work.len()).collect::<Vec<_>>();
    for (path, logic_path) in prepared.input.iter().enumerate() {
        if logic_path.pre_lower_nodes.is_empty() {
            continue;
        }
        for observer in &logic_path.order_before {
            let is_observer = prepared.input.get(observer.0).is_some_and(|observer| {
                matches!(observer.target, LogicPathTarget::CombCaptureEvent { .. })
            });
            if is_observer {
                union(&mut atomic, work_of_path[path], work_of_path[observer.0]);
            }
        }
    }
    join_expensive_sharing(work, &prepared.input, arena, &mut atomic);
    let mut edges = Vec::new();
    for (definition, users) in prepared.adj.iter().enumerate().take(path_count) {
        let producer = work_of_path[definition];
        for &user in users {
            let Some(&consumer) = work_of_path.get(user) else {
                return Err(SchedulerError::InvalidDependencyGraph);
            };
            if consumer != producer {
                edges.push((producer, consumer));
            }
        }
    }
    let (items, mut predecessors) = condense_work(&mut atomic, &edges);
    let item_paths = |item: usize| {
        items[item]
            .iter()
            .flat_map(|&work_item| work_paths(&work[work_item]).iter().copied())
    };
    // Runtime events are observable in order. Chain the items emitting them
    // so that reordering for lanes never swaps two of them.
    let mut previous_event = None;
    for (item, row) in predecessors.iter_mut().enumerate() {
        if item_paths(item).any(|path| prepared.input[path].target.var().is_none()) {
            if let Some(previous) = previous_event
                && !row.contains(&previous)
            {
                row.push(previous);
                row.sort_unstable();
            }
            previous_event = Some(item);
        }
    }

    // An item's cost counts every value it reaches once, including values
    // it shares with other items, because another builder recomputes them.
    // The single-lane cost counts every value once.
    let mut cost_walk = PathCostWalk::new(arena.len());
    let costs = items
        .iter()
        .map(|members| {
            cost_walk.begin();
            members
                .iter()
                .map(|&work_item| cost_walk.add_work(&work[work_item], &prepared.input, arena))
                .fold(0u64, u64::saturating_add)
        })
        .collect::<Vec<_>>();
    cost_walk.begin();
    let sequential_cost = work
        .iter()
        .map(|item| cost_walk.add_work(item, &prepared.input, arena))
        .fold(0u64, u64::saturating_add);

    // Ownership groups: writers of overlapping bytes of one object, and every
    // member of one exact fold group, share a lane.
    let mut parents = (0..items.len()).collect::<Vec<_>>();
    let mut writes = Vec::new();
    let mut fold_writer = HashMap::<NodeId, usize>::default();
    let mut lane0 = Vec::with_capacity(items.len());
    for index in 0..items.len() {
        let mut pinned = false;
        for path in item_paths(index) {
            let logic_path = &prepared.input[path];
            match logic_path.target.var() {
                Some(target) => {
                    writes.push((
                        target.id,
                        target.access.lsb / 8,
                        target.access.msb / 8 + 1,
                        index,
                    ));
                    if options.lane0_targets.contains(&target.id) {
                        pinned = true;
                    }
                }
                // Runtime events append to the single-producer event ring.
                None => pinned = true,
            }
            if !logic_path.comb_capture_enable_sites.is_empty() {
                pinned = true;
            }
            if let Some(root) = prepared.fold_group_schedule_index.direct_group_by_path[path] {
                match fold_writer.get(&root) {
                    Some(&writer) => union(&mut parents, writer, index),
                    None => {
                        fold_writer.insert(root, index);
                    }
                }
            }
        }
        lane0.push(pinned);
    }
    union_overlapping_writes(&mut parents, writes);
    let mut group_ids = vec![usize::MAX; items.len()];
    let mut groups = Vec::with_capacity(items.len());
    let mut group_lanes = Vec::new();
    for (index, &pinned) in lane0.iter().enumerate() {
        let root = find_root(&mut parents, index);
        if group_ids[root] == usize::MAX {
            group_ids[root] = group_lanes.len();
            group_lanes.push(None);
        }
        let group = group_ids[root];
        if pinned {
            group_lanes[group] = Some(0);
        }
        groups.push(group);
    }

    let assignment_options = LaneAssignmentOptions {
        lanes: options.lanes,
        synchronization_cost: options.synchronization_cost,
        // Locality may delay an item by about one synchronization, but by no
        // more than a small fraction of a lane's share.
        locality_slack: options.synchronization_cost.min(
            costs.iter().copied().fold(0u64, u64::saturating_add) / (u64::from(options.lanes) * 16),
        ),
    };
    let schedule = |group_lanes: &[Option<u32>]| {
        list_schedule_lanes(
            LaneAssignmentInput {
                costs: &costs,
                predecessors: &predecessors,
                groups: &groups,
                group_lanes,
            },
            assignment_options,
        )
    };
    // Candidate partitions: items placed one by one, and instance subtrees
    // kept whole. Each is charged for the tasks and waits it creates.
    let mut candidates: Vec<LaneSchedulePlan> = Vec::new();
    match schedule(&group_lanes) {
        Ok(plan) => candidates.push(plan),
        Err(error) => tracing::warn!("lane partitioning skipped: {error:?}"),
    }
    if let Some(hierarchy) = &options.hierarchy {
        let item_instances = (0..items.len())
            .map(|item| {
                item_paths(item)
                    .find_map(|path| prepared.input[path].target.var())
                    .map_or(usize::MAX, |target| (hierarchy.instance_of)(&target.id))
            })
            .collect::<Vec<_>>();
        if let Some(clustered) = cluster_group_lanes(
            &item_instances,
            &costs,
            &groups,
            &group_lanes,
            &hierarchy.parents,
            options.lanes,
        ) && let Ok(plan) = schedule(&clustered)
        {
            candidates.push(plan);
        }
    }
    let task_overhead = options.synchronization_cost / TASK_OVERHEAD_DIVISOR;
    let mut best: Option<(u64, LaneSchedulePlan, Vec<LaneTask>)> = None;
    for plan in candidates {
        // Tasks over the plan's order; their items are positions in it.
        let mut position = vec![0usize; items.len()];
        for (index, &item) in plan.order.iter().enumerate() {
            position[item] = index;
        }
        let ordered_lanes = plan
            .order
            .iter()
            .map(|&item| plan.item_lanes[item])
            .collect::<Vec<_>>();
        let ordered_predecessors = plan
            .order
            .iter()
            .map(|&item| {
                let mut row = predecessors[item]
                    .iter()
                    .map(|&predecessor| position[predecessor])
                    .collect::<Vec<_>>();
                row.sort_unstable();
                row
            })
            .collect::<Vec<_>>();
        let ordered_costs = plan
            .order
            .iter()
            .map(|&item| costs[item])
            .collect::<Vec<_>>();
        let tasks = match form_lane_tasks(options.lanes, &ordered_lanes, &ordered_predecessors) {
            Ok(tasks) => tasks,
            Err(error) => {
                tracing::warn!("lane task formation skipped: {error:?}");
                continue;
            }
        };
        let makespan = estimate_task_makespan(
            options.lanes,
            &tasks,
            &ordered_costs,
            options.synchronization_cost,
            task_overhead,
        );
        tracing::debug!(
            makespan,
            tasks = tasks.len(),
            waits = tasks.iter().map(|task| task.waits.len()).sum::<usize>(),
            "[parallel] comb partition candidate"
        );
        if best
            .as_ref()
            .is_none_or(|(best_makespan, ..)| makespan < *best_makespan)
        {
            best = Some((makespan, plan, tasks));
        }
    }
    let Some((estimated_makespan, plan, tasks)) = best else {
        return Ok(None);
    };
    // Starting and joining the lanes costs about one synchronization.
    let parallel_cost = estimated_makespan.saturating_add(options.synchronization_cost);
    if estimated_makespan == 0
        || sequential_cost.saturating_mul(100)
            < parallel_cost.saturating_mul(options.minimum_speedup_percent)
    {
        tracing::debug!(
            sequential_cost,
            estimated_makespan,
            items = items.len(),
            "[parallel] comb partition declined"
        );
        return Ok(None);
    }
    let lowerer = crate::SLTToSIRLowerer::new(four_state)
        .with_unpacked_input_types(arena, unpacked_element_widths);
    let context = WorkLoweringContext {
        input: &prepared.input,
        arena,
        adj: &prepared.adj,
        component_by_path: &prepared.component_by_path,
        ignored_loops,
        true_loops,
        fold_group_schedule_index: &prepared.fold_group_schedule_index,
        unpacked_element_widths,
        direct_ff_writes_by_action: &prepared.direct_ff_writes_by_action,
        lowerer: &lowerer,
        four_state,
        split_large_units: true,
    };
    let mut runtime_errors = HashMap::default();
    let mut next_runtime_error_code = first_runtime_error_code;
    let mut units = Vec::new();
    for task in &tasks {
        let mut lowering = WorkLowering::new(&context);
        for &position in &task.items {
            for &work_item in &items[plan.order[position]] {
                lowering
                    .lower::<std::convert::Infallible>(
                        &work[work_item],
                        None,
                        &mut runtime_errors,
                        &mut next_runtime_error_code,
                    )
                    .map_err(|error| match error {
                        ClockSortError::Scheduler(error) => error,
                        ClockSortError::Lowering(never) => match never {},
                    })?;
            }
        }
        lowering.flush_pending_folds();
        units.extend(
            lowering
                .finish()
                .into_iter()
                .map(|unit| LaneUnit::new(task.lane, unit)),
        );
    }
    Ok(Some(LaneScheduleResult {
        units,
        runtime_errors,
        estimated_makespan,
        total_cost: sequential_cost,
    }))
}

#[cfg(test)]
mod tests {
    use num_bigint::BigUint;

    use super::*;

    fn path(
        arena: &mut SLTNodeArena<u32>,
        target: u32,
        access: (usize, usize),
        source: Option<u32>,
    ) -> LogicPath<u32> {
        let width = access.1 - access.0 + 1;
        let expr = match source {
            Some(source) => arena
                .alloc(SLTNode::Input {
                    variable: source,
                    signed: false,
                    index: Vec::new(),
                    access: BitAccess::new(0, width - 1),
                })
                .unwrap(),
            None => arena
                .alloc(SLTNode::Constant(
                    BigUint::from(target),
                    BigUint::from(0u8),
                    width,
                    false,
                ))
                .unwrap(),
        };
        LogicPath {
            target: LogicPathTarget::Var(VarAtomBase::new(target, access.0, access.1)),
            sources: source
                .map(|source| {
                    [VarAtomBase::new(source, 0, width - 1)]
                        .into_iter()
                        .collect()
                })
                .unwrap_or_default(),
            previous_sources: HashSet::default(),
            address_sources: HashSet::default(),
            local_inputs: Vec::new(),
            order_before: HashSet::default(),
            comb_capture_enable_sites: Vec::new(),
            comb_capture_enable_always: false,
            pre_lower_nodes: Vec::new(),
            expr,
        }
    }

    fn options(lanes: u32, lane0_targets: &[u32]) -> LanePartitionOptions<u32> {
        LanePartitionOptions {
            lanes,
            synchronization_cost: 0,
            lane0_targets: lane0_targets.iter().copied().collect(),
            minimum_speedup_percent: 100,
            hierarchy: None,
        }
    }

    fn partition(
        paths: Vec<LogicPath<u32>>,
        arena: &SLTNodeArena<u32>,
        options: &LanePartitionOptions<u32>,
    ) -> Option<LaneScheduleResult<u32>> {
        sort_lanes(
            paths,
            arena,
            &HashSet::default(),
            &HashMap::default(),
            false,
            &HashMap::default(),
            &HashMap::default(),
            1,
            options,
        )
        .unwrap()
    }

    fn stores(unit: &ExecutionUnit<u32>) -> Vec<u32> {
        let mut blocks = unit.blocks.keys().copied().collect::<Vec<_>>();
        blocks.sort_unstable();
        blocks
            .into_iter()
            .flat_map(|block| &unit.blocks[&block].instructions)
            .filter_map(|instruction| match instruction {
                SIRInstruction::Store(address, ..) => Some(*address),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn independent_chains_use_several_lanes_in_a_valid_order() {
        let mut arena = SLTNodeArena::new();
        let mut paths = Vec::new();
        for chain in 0..4u32 {
            let base = 10 * (chain + 1);
            paths.push(path(&mut arena, base, (0, 7), None));
            paths.push(path(&mut arena, base + 1, (0, 7), Some(base)));
            paths.push(path(&mut arena, base + 2, (0, 7), Some(base + 1)));
        }
        let result = partition(paths, &arena, &options(4, &[])).unwrap();
        let lanes = result
            .units
            .iter()
            .map(|unit| unit.lane)
            .collect::<std::collections::BTreeSet<_>>();
        assert!(
            lanes.len() > 1,
            "independent chains should spread: {lanes:?}"
        );

        // Every object has one writer lane, and the listed order keeps each
        // chain's producer before its consumer.
        let mut writer_lane = HashMap::default();
        let mut order = Vec::new();
        for unit in &result.units {
            for target in stores(&unit.unit) {
                assert_eq!(*writer_lane.entry(target).or_insert(unit.lane), unit.lane);
                order.push(target);
            }
        }
        for chain in 0..4u32 {
            let base = 10 * (chain + 1);
            let position = |target| order.iter().position(|stored| *stored == target).unwrap();
            assert!(position(base) < position(base + 1));
            assert!(position(base + 1) < position(base + 2));
        }
    }

    #[test]
    fn writers_of_one_object_share_a_lane_and_pins_apply() {
        let mut arena = SLTNodeArena::new();
        let paths = vec![
            path(&mut arena, 1, (0, 7), None),
            path(&mut arena, 2, (0, 7), None),
            path(&mut arena, 3, (0, 7), None),
            path(&mut arena, 4, (0, 7), None),
            // Two bit ranges of one byte of object 9, from different producers.
            path(&mut arena, 9, (0, 3), Some(1)),
            path(&mut arena, 9, (4, 7), Some(2)),
            path(&mut arena, 5, (0, 7), Some(3)),
            path(&mut arena, 6, (0, 7), Some(4)),
        ];
        let result = partition(paths, &arena, &options(4, &[6])).unwrap();
        let mut writer_lanes = HashMap::<u32, std::collections::BTreeSet<u32>>::default();
        for unit in &result.units {
            for target in stores(&unit.unit) {
                writer_lanes.entry(target).or_default().insert(unit.lane);
            }
        }
        assert_eq!(writer_lanes[&9].len(), 1);
        assert_eq!(writer_lanes[&6], [0].into_iter().collect());
    }

    #[test]
    fn writers_of_disjoint_bytes_of_one_object_may_use_different_lanes() {
        let mut arena = SLTNodeArena::new();
        let mut paths = Vec::new();
        for element in 0..8usize {
            paths.push(path(
                &mut arena,
                9,
                (element * 8, element * 8 + 7),
                Some(element as u32 + 1),
            ));
        }
        let result = partition(paths, &arena, &options(4, &[])).unwrap();
        let lanes = result
            .units
            .iter()
            .map(|unit| unit.lane)
            .collect::<std::collections::BTreeSet<_>>();
        assert!(lanes.len() > 1, "disjoint element writers stayed together");
    }

    fn input(arena: &mut SLTNodeArena<u32>, variable: u32) -> NodeId {
        arena
            .alloc(SLTNode::Input {
                variable,
                signed: false,
                index: Vec::new(),
                access: BitAccess::new(0, 31),
            })
            .unwrap()
    }

    /// `depth` multiplications applied to `node`; `salt` keeps chains apart.
    fn chain(arena: &mut SLTNodeArena<u32>, mut node: NodeId, depth: u32, salt: u32) -> NodeId {
        for step in 0..depth {
            let constant = arena
                .alloc(SLTNode::Constant(
                    BigUint::from(salt * 1000 + step + 3),
                    BigUint::from(0u8),
                    32,
                    false,
                ))
                .unwrap();
            node = arena
                .alloc(SLTNode::Binary(node, BinaryOp::Mul, constant))
                .unwrap();
        }
        node
    }

    fn word_path(target: u32, sources: &[u32], expr: NodeId) -> LogicPath<u32> {
        LogicPath {
            target: LogicPathTarget::Var(VarAtomBase::new(target, 0, 31)),
            sources: sources
                .iter()
                .map(|source| VarAtomBase::new(*source, 0, 31))
                .collect(),
            previous_sources: HashSet::default(),
            address_sources: HashSet::default(),
            local_inputs: Vec::new(),
            order_before: HashSet::default(),
            comb_capture_enable_sites: Vec::new(),
            comb_capture_enable_always: false,
            pre_lower_nodes: Vec::new(),
            expr,
        }
    }

    /// The unit index and position of every stored target.
    fn store_positions(result: &LaneScheduleResult<u32>) -> HashMap<u32, (usize, usize)> {
        let mut positions = HashMap::default();
        let mut position = 0;
        for (index, unit) in result.units.iter().enumerate() {
            for target in stores(&unit.unit) {
                positions.insert(target, (index, position));
                position += 1;
            }
        }
        positions
    }

    #[test]
    fn work_sharing_an_expensive_value_is_lowered_together() {
        let mut arena = SLTNodeArena::new();
        let mut paths = Vec::new();
        for pair in 0..2u32 {
            let source = input(&mut arena, pair + 1);
            let shared = chain(&mut arena, source, 32, pair);
            let tail = chain(&mut arena, shared, 1, pair + 10);
            paths.push(word_path(10 * (pair + 1), &[pair + 1], shared));
            paths.push(word_path(10 * (pair + 1) + 1, &[pair + 1], tail));
        }
        let result = partition(paths, &arena, &options(2, &[])).unwrap();
        let positions = store_positions(&result);
        // Each pair is one atomic item, so one unit computes its chain once.
        assert_eq!(positions[&10].0, positions[&11].0);
        assert_eq!(positions[&20].0, positions[&21].0);
        assert_ne!(
            result.units[positions[&10].0].lane,
            result.units[positions[&20].0].lane
        );
    }

    #[test]
    fn work_sharing_only_cheap_values_stays_independent() {
        let mut arena = SLTNodeArena::new();
        let source = input(&mut arena, 1);
        let paths = (0..4u32)
            .map(|index| word_path(10 + index, &[1], chain(&mut arena, source, 32, index)))
            .collect::<Vec<_>>();
        let result = partition(paths, &arena, &options(4, &[])).unwrap();
        let lanes = result
            .units
            .iter()
            .map(|unit| unit.lane)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            lanes.len(),
            4,
            "independent chains reading one input stayed together"
        );
    }

    #[test]
    fn joined_work_absorbs_work_on_a_dependency_cycle() {
        let mut arena = SLTNodeArena::new();
        let source = input(&mut arena, 1);
        let shared = chain(&mut arena, source, 32, 0);
        // 10 = shared; 11 = f(10); 12 = shared + 11. Joining 10 and 12
        // requires 11 between them in the same item.
        let first = input(&mut arena, 10);
        let middle = chain(&mut arena, first, 2, 5);
        let second = input(&mut arena, 11);
        let last = arena
            .alloc(SLTNode::Binary(shared, BinaryOp::Add, second))
            .unwrap();
        // Independent work keeps the partition profitable.
        let other_source = input(&mut arena, 2);
        let other = chain(&mut arena, other_source, 64, 9);
        let paths = vec![
            word_path(10, &[1], shared),
            word_path(11, &[10], middle),
            word_path(12, &[1, 11], last),
            word_path(20, &[2], other),
        ];
        let result = partition(paths, &arena, &options(2, &[])).unwrap();
        let positions = store_positions(&result);
        assert_eq!(positions[&10].0, positions[&11].0);
        assert_eq!(positions[&11].0, positions[&12].0);
        assert!(positions[&10].1 < positions[&11].1);
        assert!(positions[&11].1 < positions[&12].1);
    }

    #[test]
    fn unprofitable_partitions_are_declined() {
        let mut arena = SLTNodeArena::new();
        let paths = vec![
            path(&mut arena, 1, (0, 7), None),
            path(&mut arena, 2, (0, 7), Some(1)),
        ];
        let mut request = options(4, &[]);
        request.synchronization_cost = 1_000;
        request.minimum_speedup_percent = 120;
        assert!(partition(paths, &arena, &request).is_none());
    }
}
