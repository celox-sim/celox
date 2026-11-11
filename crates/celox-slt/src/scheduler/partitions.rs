//! Planning the partitions of a settle.

use super::*;

/// One partition of a settle plan.
pub(super) struct PlannedPartition<Addr> {
    pub(super) reads: Vec<Addr>,
    pub(super) writes: Vec<Addr>,
    /// The partitions whose flags it sets whenever it runs: the readers of
    /// what it writes whose width is unknown.
    pub(super) marks: Vec<usize>,
    /// What it writes that other partitions read, itself included when it
    /// reads the previous value of its own writes. It sets the readers'
    /// flags only when it changes the value.
    pub(super) watched: Vec<WatchedChunk<Addr>>,
    pub(super) pinned: bool,
    /// Approximate expression nodes of its paths.
    pub(super) cost: usize,
    /// The bits it reads ([`SettlePartitionPlan::read_bits`]).
    pub(super) read_bits: Vec<(Addr, ReadBits)>,
}

/// A chunk of an object a partition writes, compared before and after the
/// partition runs.
pub(super) struct WatchedChunk<Addr> {
    pub(super) object: Addr,
    pub(super) offset: SIROffset,
    /// First bit of the chunk in the object.
    pub(super) lsb: usize,
    pub(super) width: usize,
    pub(super) readers: Vec<usize>,
}

/// The bit ranges of an object a partition reads, `None` for all of it.
pub(super) type ReadBits = Option<Vec<(usize, usize)>>;

/// Note that a partition reads bits `range` (`lsb`, `msb`) of `address`,
/// `None` for all of them.
pub(super) fn note_read_bits<Addr: Eq + Hash>(
    bits: &mut HashMap<Addr, ReadBits>,
    address: Addr,
    range: Option<(usize, usize)>,
) {
    match (
        bits.entry(address).or_insert_with(|| Some(Vec::new())),
        range,
    ) {
        (Some(ranges), Some(range)) => ranges.push(range),
        (entry, _) => *entry = None,
    }
}

/// The widest chunk of a written object compared at once.
pub(super) const MAX_WATCHED_WIDTH: usize = 4096;

/// The partitions of a settle, each a contiguous run of scheduled work.
pub(super) struct PartitionPlan<Addr> {
    /// The first scheduled work of each partition.
    pub(super) first_work: Vec<usize>,
    pub(super) partitions: Vec<PlannedPartition<Addr>>,
    /// The whole-variable copies, target then source.
    pub(super) copies: Vec<(Addr, Addr)>,
}

/// The paths of one piece of scheduled combinational work.
pub(super) fn work_paths(work: &ScheduledWork) -> &[usize] {
    match work {
        ScheduledWork::CombPath(path) => std::slice::from_ref(path),
        ScheduledWork::CombScc(paths) | ScheduledWork::GuardedComb { paths, .. } => paths,
        ScheduledWork::Ff(_) => &[],
    }
}

/// The approximate lowering cost of a path: its distinct expression nodes,
/// or a process unit's instructions.
pub fn path_cost<Addr: Clone + Eq + Hash>(
    arena: &SLTNodeArena<Addr>,
    path: &LogicPath<Addr>,
) -> usize {
    if let LogicPathTarget::Process { unit, .. } = &path.target {
        return unit
            .blocks
            .values()
            .map(|block| block.instructions.len() + 1)
            .sum();
    }
    let mut seen = HashSet::default();
    let mut stack = vec![path.expr];
    let mut cost = 0;
    while let Some(node) = stack.pop() {
        if !seen.insert(node) {
            continue;
        }
        cost += 1;
        match arena.get(node) {
            SLTNode::Binary(lhs, _, rhs) => stack.extend([*lhs, *rhs]),
            SLTNode::Unary(_, operand)
            | SLTNode::Slice { expr: operand, .. }
            | SLTNode::Capture { expr: operand, .. } => stack.push(*operand),
            SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } => stack.extend([*cond, *then_expr, *else_expr]),
            SLTNode::Concat(parts) => stack.extend(parts.iter().map(|(part, _)| *part)),
            // A loop's cost scales with its trip count; count it as large.
            SLTNode::ForFold { .. } | SLTNode::ForFoldGroup { .. } => cost += 64,
            SLTNode::Input { .. } | SLTNode::Constant(..) => {}
        }
    }
    cost
}

/// Whether a path's expression captures a value at an evaluation position.
pub(super) fn path_has_capture<Addr: Clone + Eq + Hash>(
    arena: &SLTNodeArena<Addr>,
    path: &LogicPath<Addr>,
) -> bool {
    if matches!(path.target, LogicPathTarget::CombCaptureEvent { .. }) {
        return true;
    }
    let mut seen = HashSet::default();
    let mut stack = vec![path.expr];
    while let Some(node) = stack.pop() {
        if !seen.insert(node) {
            continue;
        }
        match arena.get(node) {
            SLTNode::Capture { .. } => return true,
            SLTNode::Binary(lhs, _, rhs) => stack.extend([*lhs, *rhs]),
            SLTNode::Unary(_, operand) | SLTNode::Slice { expr: operand, .. } => {
                stack.push(*operand)
            }
            SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } => stack.extend([*cond, *then_expr, *else_expr]),
            SLTNode::Concat(parts) => stack.extend(parts.iter().map(|(part, _)| *part)),
            _ => {}
        }
    }
    false
}

/// Whether a path does more than store its value: comb capture events,
/// activation of capture sites, and process units with runtime events or
/// extern calls. A partition holding one runs on every settle.
pub(super) fn path_has_effects<Addr: Clone + Eq + Hash>(path: &LogicPath<Addr>) -> bool {
    if !path.comb_capture_enable_sites.is_empty() || path.comb_capture_enable_always {
        return true;
    }
    match &path.target {
        LogicPathTarget::CombCaptureEvent { .. } => true,
        LogicPathTarget::Process { unit, .. } => unit.blocks.values().any(|block| {
            block
                .instructions
                .iter()
                .any(|instruction| match instruction {
                    SIRInstruction::RuntimeEvent { .. }
                    | SIRInstruction::CombCaptureEvent { .. }
                    | SIRInstruction::CombCaptureEnableIfChanged { .. }
                    | SIRInstruction::ExternCall { .. } => true,
                    SIRInstruction::Store(.., capture_sites) => !capture_sites.is_empty(),
                    _ => false,
                })
        }),
        LogicPathTarget::Var(_) => false,
    }
}

/// Cut a combinational schedule into partitions of about
/// `partitioning.target_cost`, or `None` when it would be one.
pub(super) fn plan_settle_partitions<Addr: Clone + Eq + Ord + Hash + Debug + Copy + Display>(
    prepared: &PreparedSchedule<Addr>,
    arena: &SLTNodeArena<Addr>,
    var_widths: &HashMap<Addr, usize>,
    unpacked_element_widths: &HashMap<Addr, usize>,
    partitioning: SettlePartitioning<Addr>,
) -> Option<(PartitionPlan<Addr>, Addr)> {
    let input = &prepared.input;
    // A captured value or a pre-lowered node is materialized at one place
    // and reused at a later one through the lowering cache, which a
    // partition boundary between them would clear.
    if input
        .iter()
        .any(|path| !path.pre_lower_nodes.is_empty() || path_has_capture(arena, path))
    {
        return None;
    }
    // Whole-variable copies, by target.
    let mut copy_source: HashMap<Addr, Addr> = HashMap::default();
    for work in &prepared.scheduled_work {
        if let ScheduledWork::CombPath(path) = work
            && let Some((target, source)) = whole_variable_copy(arena, &input[*path], var_widths)
        {
            copy_source.insert(target, source);
        }
    }
    // WorkLowering executes an acyclic process only at its first
    // projection: that execution writes every output. Later projections
    // emit no code and must not acquire duplicate reads, writes or watches.
    // Cyclic components and guarded regions retain their existing plan.
    let mut run_processes = HashSet::default();
    let mut skipped_work = HashSet::default();
    for (index, work) in prepared.scheduled_work.iter().enumerate() {
        if let ScheduledWork::CombPath(path) = work
            && !prepared.adj[*path].contains(path)
            && let LogicPathTarget::Process { process, .. } = &input[*path].target
            && !run_processes.insert(*process)
        {
            skipped_work.insert(index);
        }
    }
    // Whether a piece of work reads or writes a clock net.
    let clocked = |work: &ScheduledWork| {
        work_paths(work).iter().any(|&path| {
            let path = &input[path];
            path.target
                .var()
                .is_some_and(|target| partitioning.clock_nets.contains(&target.id))
                || path
                    .sources
                    .iter()
                    .chain(&path.address_sources)
                    .any(|atom| partitioning.clock_nets.contains(&atom.id))
        })
    };
    let mut first_work = Vec::new();
    let mut cost = 0;
    let mut previous_clocked = false;
    for (index, work) in prepared.scheduled_work.iter().enumerate() {
        if skipped_work.contains(&index) {
            continue;
        }
        let clocked = clocked(work);
        if first_work.is_empty() || cost >= partitioning.target_cost || clocked != previous_clocked
        {
            first_work.push(index);
            cost = 0;
        }
        previous_clocked = clocked;
        cost += work_paths(work)
            .iter()
            .map(|&path| path_cost(arena, &input[path]))
            .sum::<usize>();
    }
    if first_work.len() < 2 {
        return None;
    }
    let mut partition_of_work = vec![0; prepared.scheduled_work.len()];
    let mut costs = vec![0; first_work.len()];
    for (partition, &first) in first_work.iter().enumerate() {
        let end = first_work
            .get(partition + 1)
            .copied()
            .unwrap_or(prepared.scheduled_work.len());
        partition_of_work[first..end].fill(partition);
    }
    let count = first_work.len();
    let mut reads = vec![BTreeSet::new(); count];
    let mut writes = vec![BTreeSet::new(); count];
    // The bits each partition writes of each object, `None` once a write
    // has no static range: only those bits are compared after it ran.
    let mut written: Vec<HashMap<Addr, Option<Vec<(usize, usize)>>>> =
        vec![HashMap::default(); count];
    let mut previous = vec![BTreeSet::new(); count];
    let mut pinned = vec![false; count];
    // The bits of each object each partition reads, `None` for all of
    // them: a write marks only the readers of the bits it changed.
    let mut read_bits: Vec<HashMap<Addr, ReadBits>> = vec![HashMap::default(); count];
    for (index, work) in prepared.scheduled_work.iter().enumerate() {
        if skipped_work.contains(&index) {
            continue;
        }
        let partition = partition_of_work[index];
        for &path_index in work_paths(work) {
            let path = &input[path_index];
            costs[partition] += path_cost(arena, path);
            pinned[partition] |= path_has_effects(path);
            let atoms = path
                .sources
                .iter()
                .chain(&path.address_sources)
                .chain(&path.previous_sources);
            for atom in atoms {
                reads[partition].insert(atom.id);
                note_read_bits(
                    &mut read_bits[partition],
                    atom.id,
                    Some((atom.access.lsb, atom.access.msb)),
                );
            }
            // A process's activation guard reads its occurrence counters and
            // startup slots as well.
            if let LogicPathTarget::Process {
                sensitivity_counters,
                startup,
                unit,
                ..
            } = &path.target
            {
                // The unit may read more than its listed sources, such as
                // force controls. It runs at its process's first projection
                // and writes every output there.
                let mut unit_writes = Vec::new();
                let mut unknown = false;
                for block in unit.blocks.values() {
                    for instruction in &block.instructions {
                        match instruction {
                            SIRInstruction::Load(_, address, ..) => {
                                reads[partition].insert(*address);
                                note_read_bits(&mut read_bits[partition], *address, None);
                            }
                            SIRInstruction::Store(address, offset, width, ..) => {
                                writes[partition].insert(*address);
                                unit_writes.push((*address, static_written_range(offset, *width)));
                            }
                            SIRInstruction::Commit(source, target, offset, width, _) => {
                                reads[partition].insert(*source);
                                note_read_bits(&mut read_bits[partition], *source, None);
                                writes[partition].insert(*target);
                                unit_writes.push((*target, static_written_range(offset, *width)));
                            }
                            _ => {}
                        }
                        // A callee's writes have no range here.
                        if let Some(effects) = instruction.call_effects() {
                            unit_writes
                                .extend(effects.writes.iter().map(|address| (*address, None)));
                        }
                        unknown |= instruction.accesses_unknown_state();
                    }
                }
                for (address, range) in unit_writes {
                    note_written_range(
                        &mut written[partition],
                        address,
                        range.filter(|_| !unknown),
                    );
                }
                let mut whole = Vec::new();
                for (atom, counter) in sensitivity_counters {
                    whole.extend([atom.id, *counter]);
                }
                if let Some(startup) = startup {
                    whole.push(startup.initialized);
                    for (atom, slot) in &startup.inputs {
                        whole.extend([atom.id, *slot]);
                    }
                    for (counter, slot) in &startup.counters {
                        whole.extend([*counter, *slot]);
                    }
                }
                for address in whole {
                    reads[partition].insert(address);
                    note_read_bits(&mut read_bits[partition], address, None);
                }
            }
            previous[partition].extend(path.previous_sources.iter().map(|atom| atom.id));
            if let Some(target) = path.target.var() {
                writes[partition].insert(target.id);
                note_written_range(
                    &mut written[partition],
                    target.id,
                    Some((target.access.lsb, target.access.msb)),
                );
            }
        }
    }
    // Reading a copy's target reads its source: the copy may become an
    // alias of its source, whose changes then reach the target without the
    // copy running.
    for (read, bits) in reads.iter_mut().zip(&mut read_bits) {
        let mut sources = Vec::new();
        for &addr in read.iter() {
            let mut current = addr;
            let mut steps = 0;
            while let Some(&source) = copy_source.get(&current) {
                sources.push((source, addr));
                current = source;
                steps += 1;
                if steps > copy_source.len() {
                    break;
                }
            }
        }
        // A whole-variable copy keeps every bit where it is.
        for (source, target) in sources {
            read.insert(source);
            match bits.get(&target).cloned() {
                Some(Some(ranges)) => {
                    for range in ranges {
                        note_read_bits(bits, source, Some(range));
                    }
                }
                _ => note_read_bits(bits, source, None),
            }
        }
    }
    let mut readers: HashMap<Addr, Vec<usize>> = HashMap::default();
    for (partition, read) in reads.iter().enumerate() {
        for &addr in read {
            readers.entry(addr).or_default().push(partition);
        }
    }
    // An external reader of a copy's target reads its source too.
    for (addr, flags) in &partitioning.external_readers {
        let mut current = *addr;
        let mut steps = 0;
        loop {
            let readers = readers.entry(current).or_default();
            readers.extend(
                flags
                    .iter()
                    .map(|flag| celox_design::settle_wait_flag(count, *flag)),
            );
            readers.sort_unstable();
            readers.dedup();
            match copy_source.get(&current) {
                Some(&source) if steps <= copy_source.len() => {
                    current = source;
                    steps += 1;
                }
                _ => break,
            }
        }
    }
    // A whole-variable copy's target changes only when its source does,
    // whose writer marks the target's readers as readers of the source
    // (above): the copy need not compare what it wrote, when nothing else
    // writes its target.
    let mut writers: HashMap<Addr, usize> = HashMap::default();
    for path in input {
        if let Some(target) = path.target.var() {
            *writers.entry(target.id).or_default() += 1;
        }
        if let LogicPathTarget::Process { unit, .. } = &path.target {
            for block in unit.blocks.values() {
                for instruction in &block.instructions {
                    match instruction {
                        SIRInstruction::Store(address, ..) => {
                            *writers.entry(*address).or_default() += 1;
                        }
                        SIRInstruction::Commit(_, target, ..) => {
                            *writers.entry(*target).or_default() += 1;
                        }
                        _ => {}
                    }
                    if let Some(effects) = instruction.call_effects() {
                        for address in &effects.writes {
                            *writers.entry(*address).or_default() += 1;
                        }
                    }
                }
            }
        }
    }
    let partitions = (0..count)
        .map(|partition| {
            let mut marks = BTreeSet::new();
            let mut watched = Vec::new();
            for addr in &writes[partition] {
                let copy_only = copy_source.contains_key(addr) && writers.get(addr) == Some(&1);
                let targets: Vec<usize> = readers
                    .get(addr)
                    .into_iter()
                    .flatten()
                    .copied()
                    .filter(|&reader| {
                        (!copy_only || reader >= count)
                            && (reader != partition || previous[partition].contains(addr))
                    })
                    .collect();
                if targets.is_empty() {
                    continue;
                }
                let Some(&width) = var_widths.get(addr) else {
                    marks.extend(targets);
                    continue;
                };
                // Compare an unpacked array in whole elements.
                let element = unpacked_element_widths
                    .get(addr)
                    .copied()
                    .filter(|&element| element != 0 && width.is_multiple_of(element))
                    .unwrap_or(1);
                let step = (MAX_WATCHED_WIDTH / element).max(1) * element;
                // Only the bits the partition writes can change while it
                // runs: compare those, in whole elements.
                let ranges = match written[partition].get(addr) {
                    Some(Some(ranges)) => written_spans(ranges, width, element),
                    _ => vec![(0, width)],
                };
                for (start, end) in ranges {
                    for lsb in (start..end).step_by(step) {
                        let chunk = (end - lsb).min(step);
                        // The readers of these bits; another reader of the
                        // object does not depend on them.
                        let targets: Vec<usize> = targets
                            .iter()
                            .copied()
                            .filter(|&reader| {
                                read_bits
                                    .get(reader)
                                    .is_none_or(|bits| match bits.get(addr) {
                                        Some(Some(ranges)) => ranges
                                            .iter()
                                            .any(|&(low, high)| low < lsb + chunk && lsb <= high),
                                        _ => true,
                                    })
                            })
                            .collect();
                        if targets.is_empty() {
                            continue;
                        }
                        watched.push(WatchedChunk {
                            object: *addr,
                            offset: static_access_offset(
                                addr,
                                BitAccess::new(lsb, lsb + chunk - 1),
                                unpacked_element_widths,
                            ),
                            lsb,
                            width: chunk,
                            readers: targets.clone(),
                        });
                    }
                }
            }
            let mut bits: Vec<(Addr, ReadBits)> = read_bits[partition]
                .iter()
                .map(|(address, ranges)| (*address, ranges.clone()))
                .collect();
            bits.sort_unstable_by_key(|(address, _)| *address);
            PlannedPartition {
                reads: reads[partition].iter().copied().collect(),
                writes: writes[partition].iter().copied().collect(),
                marks: marks.into_iter().collect(),
                watched,
                pinned: pinned[partition],
                cost: costs[partition],
                read_bits: bits,
            }
        })
        .collect();
    let mut copies: Vec<(Addr, Addr)> = copy_source.into_iter().collect();
    copies.sort_unstable();
    Some((
        PartitionPlan {
            first_work,
            partitions,
            copies,
        },
        partitioning.flags,
    ))
}

/// The inclusive bit range a store of `width` bits at `offset` writes, if
/// the offset is static.
pub(super) fn static_written_range(offset: &SIROffset, width: usize) -> Option<(usize, usize)> {
    let lsb = match offset {
        SIROffset::Static(bit)
        | SIROffset::PackedElements {
            bit_offset: bit, ..
        } => *bit,
        SIROffset::Dynamic(_) | SIROffset::Element { .. } => return None,
    };
    Some((lsb, lsb + width.max(1) - 1))
}

/// Record a write of `range` (`None` when its bits are unknown) to `addr`.
pub(super) fn note_written_range<Addr: Eq + Hash>(
    written: &mut HashMap<Addr, Option<Vec<(usize, usize)>>>,
    addr: Addr,
    range: Option<(usize, usize)>,
) {
    let ranges = written.entry(addr).or_insert_with(|| Some(Vec::new()));
    match (ranges, range) {
        (Some(ranges), Some(range)) => ranges.push(range),
        (ranges, None) => *ranges = None,
        (None, _) => {}
    }
}

/// The disjoint half-open spans covering the inclusive bit `ranges` of an
/// object `width` bits wide, each widened to whole elements of `element`
/// bits.
pub(super) fn written_spans(
    ranges: &[(usize, usize)],
    width: usize,
    element: usize,
) -> Vec<(usize, usize)> {
    let mut spans: Vec<(usize, usize)> = ranges
        .iter()
        .filter(|&&(lsb, _)| lsb < width)
        .map(|&(lsb, msb)| {
            let start = lsb / element * element;
            let end = (msb + 1).div_ceil(element) * element;
            (start, end.min(width))
        })
        .collect();
    spans.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (start, end) in spans {
        match merged.last_mut() {
            Some((_, last)) if start <= *last => *last = (*last).max(end),
            _ => merged.push((start, end)),
        }
    }
    merged
}

/// The `(target, source)` of a path that copies one whole variable into
/// another of the same width.
pub(super) fn whole_variable_copy<Addr: Clone + Eq + Hash + Copy>(
    arena: &SLTNodeArena<Addr>,
    path: &LogicPath<Addr>,
    var_widths: &HashMap<Addr, usize>,
) -> Option<(Addr, Addr)> {
    if !path.transparent_copy || !path.previous_sources.is_empty() {
        return None;
    }
    let target = match &path.target {
        LogicPathTarget::Var(target) => target,
        _ => return None,
    };
    let width = *var_widths.get(&target.id)?;
    if target.access.lsb != 0 || target.access.msb + 1 != width {
        return None;
    }
    match arena.get(path.expr) {
        SLTNode::Input {
            variable,
            index,
            access,
            ..
        } if index.is_empty()
            && access.lsb == 0
            && access.msb + 1 == width
            && var_widths.get(variable) == Some(&width)
            && *variable != target.id =>
        {
            Some((target.id, *variable))
        }
        _ => None,
    }
}
