//! Scheduling combinational logic paths around flip-flop reads through memory SSA.

use super::*;

/// Memory definitions and uses induced by one set of LogicPaths.
///
/// A variable target is one bit-range MemoryDef. A current-value source is a
/// MemoryUse of every overlapping definition; a previous-value source is a
/// live-on-entry use plus an anti-dependence which keeps that use before an
/// overlapping definition. `dependencies` contains all semantic edges while
/// `values` contains only Def-to-Use edges which can become a forwarded value
/// during later lowering. Both relations retain their forward and reverse
/// adjacency so regional scheduling can borrow either direction.
pub(super) struct LogicPathMemorySsa {
    pub(super) dependencies: LogicPathEdges,
    pub(super) values: LogicPathEdges,
}

pub(super) struct LogicPathEdges {
    pub(super) users: Vec<Vec<usize>>,
    pub(super) predecessors: Vec<Vec<usize>>,
}

impl LogicPathEdges {
    pub(super) fn new(node_count: usize) -> Self {
        Self {
            users: vec![Vec::new(); node_count],
            predecessors: vec![Vec::new(); node_count],
        }
    }

    pub(super) fn resize(&mut self, node_count: usize) {
        self.users.resize_with(node_count, Vec::new);
        self.predecessors.resize_with(node_count, Vec::new);
    }

    pub(super) fn push(&mut self, predecessor: usize, user: usize) {
        self.users[predecessor].push(user);
        self.predecessors[user].push(predecessor);
    }

    pub(super) fn canonicalize(&mut self) {
        for row in self.users.iter_mut().chain(&mut self.predecessors) {
            row.sort_unstable();
            row.dedup();
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FfCombSchedulePlan {
    pub required_comb: Vec<bool>,
    pub comb_value_predecessors: Vec<Vec<usize>>,
    pub comb_before_direct_write: Vec<Vec<usize>>,
    pub ff_before_direct_write: Vec<Vec<usize>>,
}

pub(super) fn bit_interval(access: BitAccess) -> Option<(usize, usize)> {
    Some((
        access.lsb,
        access.msb.checked_sub(access.lsb)?.checked_add(1)?,
    ))
}

pub(super) fn build_logic_path_memory_ssa<Addr>(
    input: &[LogicPath<Addr>],
) -> Result<LogicPathMemorySsa, SchedulerError<Addr>>
where
    Addr: Copy + Ord + Hash + Eq + Display + Debug,
{
    let mut definition_intervals = Vec::new();
    for (path, logic_path) in input.iter().enumerate() {
        let Some(target) = logic_path.target.var() else {
            continue;
        };
        let Some((start, length)) = bit_interval(target.access) else {
            return Err(SchedulerError::InvalidDependencyGraph);
        };
        definition_intervals.push(ExactInterval {
            object: target.id,
            start,
            length,
            value: path,
        });
    }
    let definitions = match DisjointIntervalMap::try_new(definition_intervals) {
        Ok(definitions) => definitions,
        Err(DisjointIntervalError::Overlap { first, second }) => {
            return Err(SchedulerError::MultipleDriver {
                blocks: vec![input[first].clone(), input[second].clone()],
            });
        }
        Err(DisjointIntervalError::Empty { .. } | DisjointIntervalError::Overflow { .. }) => {
            return Err(SchedulerError::InvalidDependencyGraph);
        }
    };

    let mut dependencies = LogicPathEdges::new(input.len());
    let mut values = LogicPathEdges::new(input.len());
    for (user, path) in input.iter().enumerate() {
        for source in &path.sources {
            let Some((start, length)) = bit_interval(source.access) else {
                return Err(SchedulerError::InvalidDependencyGraph);
            };
            let reaching = definitions
                .overlapping(&source.id, start, length)
                .map_err(|_| SchedulerError::InvalidDependencyGraph)?;
            for definition in reaching {
                dependencies.push(definition, user);
                values.push(definition, user);
            }
        }

        // A previous-value source reads the live-on-entry version. Preserve
        // that read before every overlapping definition. A self read/write is
        // already one atomic LogicPath and needs no self anti-dependence.
        for source in &path.previous_sources {
            let Some((start, length)) = bit_interval(source.access) else {
                return Err(SchedulerError::InvalidDependencyGraph);
            };
            let reaching = definitions
                .overlapping(&source.id, start, length)
                .map_err(|_| SchedulerError::InvalidDependencyGraph)?;
            for definition in reaching.filter(|definition| *definition != user) {
                dependencies.push(user, definition);
            }
        }
        for target in &path.order_before {
            if target.0 < input.len() && target.0 != user {
                dependencies.push(user, target.0);
            }
        }
    }
    // A process publishes multiple outputs but owns one activation guard and
    // RNG state. Keep its projections ordered, including in cyclic schedules.
    let mut previous_process_path = HashMap::default();
    for (index, path) in input.iter().enumerate() {
        if let LogicPathTarget::Process { process, .. } = &path.target {
            if let Some(previous) = previous_process_path.insert(*process, index) {
                dependencies.push(previous, index);
            }
        }
    }
    dependencies.canonicalize();
    values.canonicalize();
    Ok(LogicPathMemorySsa {
        dependencies,
        values,
    })
}

/// Paths participating in a semantic dependency cycle, including self loops.
/// Frontends can use this before scheduling to refine atomic pure expressions
/// without perturbing the representation of already acyclic logic.
pub fn cyclic_logic_paths<Addr>(
    input: &[LogicPath<Addr>],
) -> Result<Vec<usize>, SchedulerError<Addr>>
where
    Addr: Copy + Ord + Hash + Eq + Display + Debug,
{
    let memory = build_logic_path_memory_ssa(input)?;
    let edges = &memory.dependencies.users;
    let components = component_map(edges);
    Ok(edges
        .iter()
        .enumerate()
        .filter_map(|(source, targets)| {
            targets
                .iter()
                .any(|&target| components[source] == components[target])
                .then_some(source)
        })
        .collect())
}

/// Group bit ranges by destination object for overlap probes.
///
/// Overlap tests pair a read with the writes of the same object, or a write
/// with the reads of the same object. Indexing one side by object id lets the
/// probe scan only atoms that share the object instead of the whole access
/// list, which keeps the FF/comb scheduling passes from nesting linearly.
pub(super) fn index_accesses<'a, Addr: Copy + Eq + Hash + 'a>(
    accesses: impl IntoIterator<Item = &'a VarAtomBase<Addr>>,
) -> HashMap<Addr, Vec<BitAccess>> {
    let mut index = HashMap::<Addr, Vec<BitAccess>>::default();
    for access in accesses {
        index.entry(access.id).or_default().push(access.access);
    }
    index
}

/// Returns true when any indexed range overlaps `access`.
pub(super) fn indexed_overlaps<Addr: Eq + Hash>(
    index: &HashMap<Addr, Vec<BitAccess>>,
    access: &VarAtomBase<Addr>,
) -> bool {
    index
        .get(&access.id)
        .is_some_and(|ranges| ranges.iter().any(|range| range.overlaps(&access.access)))
}

pub(crate) fn plan_ff_comb_schedule<Addr>(
    input: &[LogicPath<Addr>],
    ff: &[FfAccessSummary<Addr>],
) -> Result<FfCombSchedulePlan, SchedulerError<Addr>>
where
    Addr: Copy + Ord + Hash + Eq + Display + Debug,
{
    let memory = build_logic_path_memory_ssa(input)?;
    let mut definition_intervals = Vec::new();
    for (path, logic_path) in input.iter().enumerate() {
        let Some(target) = logic_path.target.var() else {
            continue;
        };
        let Some((start, length)) = bit_interval(target.access) else {
            return Err(SchedulerError::InvalidDependencyGraph);
        };
        definition_intervals.push(ExactInterval {
            object: target.id,
            start,
            length,
            value: path,
        });
    }
    let definitions = DisjointIntervalMap::try_new(definition_intervals)
        .map_err(|_| SchedulerError::InvalidDependencyGraph)?;

    let mut comb_value_predecessors = vec![Vec::new(); ff.len()];
    let mut required_comb = vec![false; input.len()];
    let mut work = Vec::new();
    for (ff_index, summary) in ff.iter().enumerate() {
        for read in &summary.reads {
            let Some((start, length)) = bit_interval(read.access) else {
                return Err(SchedulerError::InvalidDependencyGraph);
            };
            for definition in definitions
                .overlapping(&read.id, start, length)
                .map_err(|_| SchedulerError::InvalidDependencyGraph)?
            {
                comb_value_predecessors[ff_index].push(definition);
                if !std::mem::replace(&mut required_comb[definition], true) {
                    work.push(definition);
                }
            }
        }
    }
    for (path, logic_path) in input.iter().enumerate() {
        if logic_path_is_scheduling_barrier(logic_path)
            && !std::mem::replace(&mut required_comb[path], true)
        {
            work.push(path);
        }
    }
    for row in &mut comb_value_predecessors {
        row.sort_unstable();
        row.dedup();
    }

    while let Some(path) = work.pop() {
        for &predecessor in &memory.dependencies.predecessors[path] {
            if !std::mem::replace(&mut required_comb[predecessor], true) {
                work.push(predecessor);
            }
        }
    }

    // Index each summary's writes by object once; both loops below probe the
    // same write set repeatedly.
    let ff_writes = ff
        .iter()
        .map(|summary| index_accesses(&summary.writes))
        .collect::<Vec<_>>();

    let mut comb_before_direct_write = vec![Vec::new(); ff.len()];
    for (path_index, path) in input.iter().enumerate() {
        if !required_comb[path_index] {
            continue;
        }
        for (ff_index, writes) in ff_writes.iter().enumerate() {
            if path
                .sources
                .iter()
                .chain(&path.previous_sources)
                .any(|read| indexed_overlaps(writes, read))
            {
                comb_before_direct_write[ff_index].push(path_index);
            }
        }
    }

    let mut ff_before_direct_write = vec![Vec::new(); ff.len()];
    for (writer, writes) in ff_writes.iter().enumerate() {
        for (reader, read_summary) in ff.iter().enumerate() {
            if writer != reader
                && read_summary
                    .reads
                    .iter()
                    .any(|read| indexed_overlaps(writes, read))
            {
                ff_before_direct_write[writer].push(reader);
            }
        }
    }
    for row in comb_before_direct_write
        .iter_mut()
        .chain(&mut ff_before_direct_write)
    {
        row.sort_unstable();
        row.dedup();
    }

    Ok(FfCombSchedulePlan {
        required_comb,
        comb_value_predecessors,
        comb_before_direct_write,
        ff_before_direct_write,
    })
}
