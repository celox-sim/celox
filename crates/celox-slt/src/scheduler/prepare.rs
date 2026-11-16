//! Preparing a schedule: scheduling domains, regions and materialization.

use super::*;

/// Lower a consecutive set of exact grouped-fold paths.  The packed fold
/// results are computed atomically, then each projection is created and stored
/// in topological order.  Ordinary paths bypass this buffer entirely.
pub(super) fn flush_pending_fold_paths<Addr: Clone + Eq + Ord + Hash + Debug + Copy + Display>(
    pending: &mut Vec<usize>,
    input: &[LogicPath<Addr>],
    fold_group_schedule_index: &FoldGroupScheduleIndex<Addr>,
    lowerer: &mut crate::SLTToSIRLowerer,
    builder: &mut SIRBuilder<Addr>,
    arena: &SLTNodeArena<Addr>,
    lower_cache: &mut HashMap<NodeId, RegisterId>,
    dep_memo: &mut HashMap<NodeId, HashSet<Addr>>,
    inverse_dep_memo: &mut HashMap<Addr, HashSet<NodeId>>,
    unpacked_element_widths: &HashMap<Addr, usize>,
    four_state: bool,
) {
    if pending.is_empty() {
        return;
    }

    let prepared_results = prepare_atomic_fold_group_results(
        pending,
        input,
        fold_group_schedule_index,
        lowerer,
        builder,
        arena,
        lower_cache,
        dep_memo,
        inverse_dep_memo,
        four_state,
    );
    for idx in pending.drain(..) {
        let path = &input[idx];
        collect_logic_path_input_deps(path, arena, dep_memo, inverse_dep_memo);
        let prepared_result = prepared_results.get(&idx).map(|projection| {
            lowerer.project_materialized(builder, projection.packed_result, projection.access)
        });
        emit_logic_path_store_with_result(
            lowerer,
            builder,
            path,
            arena,
            lower_cache,
            unpacked_element_widths,
            prepared_result,
        );
        invalidate_logic_path_target(path, inverse_dep_memo, lower_cache);
    }
}

pub(super) fn is_exact_fold_path<Addr: Clone + Eq + Hash + Copy>(
    path: usize,
    fold_groups: &FoldGroupScheduleIndex<Addr>,
) -> bool {
    if let Some(root) = fold_groups.direct_group_by_path[path]
        && fold_groups
            .groups
            .get(&root)
            .is_some_and(|info| info.exact_and_exclusive)
    {
        return true;
    }
    false
}

/// Assign a scheduling domain to paths whose memory effects or packed fold
/// result benefit from staying adjacent.  A domain is only a ready-queue
/// preference: it never contracts paths into one graph node and therefore
/// cannot make a not-yet-ready path execute early.
pub(super) fn logic_path_scheduling_domains<Addr: Clone + Eq + Ord + Hash + Copy>(
    input: &[LogicPath<Addr>],
    fold_groups: &FoldGroupScheduleIndex<Addr>,
) -> Vec<Option<usize>> {
    let mut next_domain = 0usize;
    let mut fold_domains = BTreeMap::<NodeId, usize>::new();
    let mut target_domains = BTreeMap::<Addr, usize>::new();
    let mut domains = Vec::with_capacity(input.len());

    for (path, logic_path) in input.iter().enumerate() {
        let exact_fold_root = fold_groups.direct_group_by_path[path].filter(|root| {
            fold_groups
                .groups
                .get(root)
                .is_some_and(|info| info.exact_and_exclusive)
        });
        let domain = if let Some(root) = exact_fold_root {
            Some(*fold_domains.entry(root).or_insert_with(|| {
                let domain = next_domain;
                next_domain += 1;
                domain
            }))
        } else {
            logic_path.target.var().map(|target| {
                *target_domains.entry(target.id).or_insert_with(|| {
                    let domain = next_domain;
                    next_domain += 1;
                    domain
                })
            })
        };
        domains.push(domain);
    }
    domains
}

/// Produce a deterministic topological order of the SCC condensation graph.
/// A ready effect domain is drained while possible, but each component remains
/// an independent queue item.  This preserves target/fold locality without
/// treating a layer or a complete ready frontier as a lowering batch.  Actual
/// register-pressure scheduling is performed later on MIR instructions and
/// their real VRegs.
pub(super) fn stable_topological_sccs(
    sccs: Vec<Vec<usize>>,
    adj: &[Vec<usize>],
    path_domains: &[Option<usize>],
) -> Option<(Vec<Vec<usize>>, Vec<usize>)> {
    if path_domains.len() != adj.len() {
        return None;
    }
    let component_count = sccs.len();
    let mut component_by_path = vec![usize::MAX; adj.len()];
    let mut keys = Vec::with_capacity(component_count);
    let mut component_domains = Vec::with_capacity(component_count);
    for (component, scc) in sccs.iter().enumerate() {
        keys.push(*scc.iter().min()?);
        for &path in scc {
            if path >= adj.len() || component_by_path[path] != usize::MAX {
                return None;
            }
            component_by_path[path] = component;
        }
        let singleton = (scc.len() == 1).then_some(scc[0]);
        let acyclic = singleton.is_some_and(|path| !adj[path].contains(&path));
        component_domains.push(
            acyclic
                .then(|| path_domains[singleton.expect("acyclic SCC is a singleton")])
                .flatten(),
        );
    }
    if component_by_path.contains(&usize::MAX) {
        return None;
    }

    let mut outgoing = vec![Vec::<usize>::new(); component_count];
    for (definition, users) in adj.iter().enumerate() {
        let source = component_by_path[definition];
        for &user in users {
            let target = *component_by_path.get(user)?;
            if source != target {
                outgoing[source].push(target);
            }
        }
    }
    let mut indegree = vec![0usize; component_count];
    for edges in &mut outgoing {
        edges.sort_unstable();
        edges.dedup();
        for &target in edges.iter() {
            indegree[target] = indegree[target].checked_add(1)?;
        }
    }

    let mut ready = BTreeSet::new();
    let mut ready_by_domain = HashMap::<usize, BTreeSet<(usize, usize)>>::default();
    for (component, degree) in indegree.iter().enumerate() {
        if *degree == 0 {
            let entry = (keys[component], component);
            ready.insert(entry);
            if let Some(domain) = component_domains[component] {
                ready_by_domain.entry(domain).or_default().insert(entry);
            }
        }
    }
    let mut components = sccs.into_iter().map(Some).collect::<Vec<_>>();
    let mut ordered = Vec::with_capacity(component_count);
    let mut active_domain = None;
    while !ready.is_empty() {
        let selected = active_domain
            .and_then(|domain| ready_by_domain.get(&domain)?.iter().next().copied())
            .or_else(|| ready.iter().next().copied())?;
        let (_, component) = selected;
        ready.remove(&selected);
        if let Some(domain) = component_domains[component] {
            ready_by_domain.get_mut(&domain)?.remove(&selected);
        }
        active_domain = component_domains[component];
        ordered.push(components[component].take()?);
        for &target in &outgoing[component] {
            indegree[target] = indegree[target].checked_sub(1)?;
            if indegree[target] == 0 {
                let entry = (keys[target], target);
                ready.insert(entry);
                if let Some(domain) = component_domains[target] {
                    ready_by_domain.entry(domain).or_default().insert(entry);
                }
            }
        }
    }
    (ordered.len() == component_count).then_some((ordered, component_by_path))
}

pub(super) fn logic_path_is_scheduling_barrier<Addr: Clone + Eq + Hash>(
    path: &LogicPath<Addr>,
) -> bool {
    matches!(
        path.target,
        LogicPathTarget::CombCaptureEvent { .. } | LogicPathTarget::Process { .. }
    ) || !path.comb_capture_enable_sites.is_empty()
}

pub(super) fn cached_logic_path_roots<Addr: Clone + Eq + Hash>(
    path: &LogicPath<Addr>,
) -> Vec<NodeId> {
    let mut roots = path
        .local_inputs
        .iter()
        .map(|(_, node)| *node)
        .collect::<Vec<_>>();
    if path.local_inputs.is_empty() && matches!(path.target, LogicPathTarget::Var(_)) {
        roots.extend(path.pre_lower_nodes.iter().copied());
        roots.push(path.expr);
    }
    roots
}

/// Build the sparse incidence relation between LogicPaths and SLT values
/// which can actually survive in `lower_cache` across path boundaries.
///
/// A tree-only node cannot be independently reused: a cached ancestor hides
/// it from later lowering.  Only DAG joins (including roots named by multiple
/// paths) therefore become materialization tokens.  Constants are deliberately
/// omitted because target lowering can rematerialize them without retaining a
/// source register.
pub(super) fn logic_path_materialization_tokens<Addr: Clone + Eq + Hash>(
    input: &[LogicPath<Addr>],
    arena: &SLTNodeArena<Addr>,
) -> (Vec<Vec<usize>>, Vec<usize>) {
    let mut references = vec![0usize; arena.len()];
    let mut children = Vec::new();
    for raw in 0..arena.len() {
        children.clear();
        push_scheduler_node_children(NodeId(raw), arena, &mut children);
        children.sort_unstable();
        children.dedup();
        for &child in &children {
            references[child.0] = references[child.0].saturating_add(1);
        }
    }
    for path in input {
        let mut roots = cached_logic_path_roots(path);
        roots.sort_unstable();
        roots.dedup();
        for root in roots {
            references[root.0] = references[root.0].saturating_add(1);
        }
    }

    let candidates = references
        .iter()
        .enumerate()
        .map(|(raw, references)| {
            *references > 1 && !matches!(arena.get(NodeId(raw)), SLTNode::Constant(..))
        })
        .collect::<Vec<_>>();
    let mut raw_tokens = vec![Vec::<usize>::new(); input.len()];
    let mut token_users = vec![0usize; arena.len()];
    let mut visited = vec![0usize; arena.len()];
    let mut epoch = 0usize;
    let mut work = Vec::new();
    for (path_index, path) in input.iter().enumerate() {
        epoch = epoch.wrapping_add(1);
        if epoch == 0 {
            visited.fill(0);
            epoch = 1;
        }
        work.extend(cached_logic_path_roots(path));
        while let Some(node) = work.pop() {
            if visited[node.0] == epoch {
                continue;
            }
            visited[node.0] = epoch;
            if candidates[node.0] {
                raw_tokens[path_index].push(node.0);
            }
            push_scheduler_node_children(node, arena, &mut work);
        }
        raw_tokens[path_index].sort_unstable();
        raw_tokens[path_index].dedup();
        for &token in &raw_tokens[path_index] {
            token_users[token] = token_users[token].saturating_add(1);
        }
    }

    let mut dense_by_raw = vec![usize::MAX; arena.len()];
    let mut weights = Vec::new();
    for (raw, users) in token_users.into_iter().enumerate() {
        if users < 2 {
            continue;
        }
        dense_by_raw[raw] = weights.len();
        let width = crate::get_width(NodeId(raw), arena);
        weights.push(width.div_ceil(64).max(1));
    }
    for row in &mut raw_tokens {
        row.retain(|raw| dense_by_raw[*raw] != usize::MAX);
        for token in row.iter_mut() {
            *token = dense_by_raw[*token];
        }
    }
    (raw_tokens, weights)
}

pub(super) fn exact_native_storage_store(width: usize) -> bool {
    width != 0 && width <= 64 && matches!(width.div_ceil(8), 1 | 2 | 4 | 8)
}

pub(super) fn static_store_avoids_native_rmw<Addr: Copy + Eq + Hash>(
    write: &VarAtomBase<Addr>,
    var_widths: &HashMap<Addr, usize>,
    unpacked_element_widths: &HashMap<Addr, usize>,
) -> bool {
    let Some(width) = write
        .access
        .msb
        .checked_sub(write.access.lsb)
        .and_then(|width| width.checked_add(1))
    else {
        return false;
    };

    if let Some(&element_width) = unpacked_element_widths.get(&write.id) {
        // Unpacked layout selection happens after scheduling. Only a complete
        // byte-aligned native-width element avoids RMW in both packed and
        // independently strided layouts.
        return element_width != 0
            && width == element_width
            && write.access.lsb.is_multiple_of(element_width)
            && matches!(width, 8 | 16 | 32 | 64);
    }

    let whole_object = write.access.lsb == 0
        && var_widths.get(&write.id).copied() == Some(width)
        && exact_native_storage_store(width);
    let aligned_native_range =
        write.access.lsb.is_multiple_of(8) && matches!(width, 8 | 16 | 32 | 64);
    whole_object || aligned_native_range
}

pub(super) fn direct_ff_write_ranges<Addr: Copy + Eq + Hash>(
    input: &[LogicPath<Addr>],
    summaries: &[FfAccessSummary<Addr>],
    plan: &FfCombSchedulePlan,
    retained: &HashSet<(usize, usize)>,
    ff_node_base: usize,
    var_widths: &HashMap<Addr, usize>,
    unpacked_element_widths: &HashMap<Addr, usize>,
) -> Vec<Vec<VarAtomBase<Addr>>> {
    // Index reads by object so the overlap probes below only scan atoms that
    // share the object under test. Only the comb readers the plan actually
    // consults need an index; every other path would pay for an unused map.
    let mut consulted_comb_readers = vec![false; input.len()];
    for readers in &plan.comb_before_direct_write {
        for &reader in readers {
            consulted_comb_readers[reader] = true;
        }
    }
    let comb_reads = input
        .iter()
        .zip(&consulted_comb_readers)
        .map(|(path, &consulted)| {
            consulted.then(|| index_accesses(path.sources.iter().chain(&path.previous_sources)))
        })
        .collect::<Vec<_>>();
    let ff_reads = summaries
        .iter()
        .map(|summary| index_accesses(&summary.reads))
        .collect::<Vec<_>>();

    summaries
        .iter()
        .enumerate()
        .map(|(writer, summary)| {
            let mut direct = summary
                .writes
                .iter()
                .map(|write| {
                    // Direct publication is only a throughput optimization.
                    // Dynamic destinations and static bitfields requiring a
                    // physical read-modify-write stay staged.
                    if summary.dynamic_writes.contains(&write.id)
                        || !static_store_avoids_native_rmw(
                            write,
                            var_widths,
                            unpacked_element_widths,
                        )
                    {
                        return false;
                    }

                    // A read in the same always_ff action cannot be ordered
                    // before only one of its own Stores. Keep just the
                    // overlapping range staged so lowering reads pre-edge
                    // state without penalizing disjoint writes.
                    if indexed_overlaps(&ff_reads[writer], write) {
                        return false;
                    }

                    let writer_node = ff_node_base + writer;
                    let comb_readers_proven = plan.comb_before_direct_write[writer]
                        .iter()
                        .filter(|&&reader| {
                            comb_reads[reader]
                                .as_ref()
                                .is_some_and(|reads| indexed_overlaps(reads, write))
                        })
                        .all(|&reader| retained.contains(&(reader, writer_node)));
                    let ff_readers_proven = plan.ff_before_direct_write[writer]
                        .iter()
                        .filter(|&&reader| indexed_overlaps(&ff_reads[reader], write))
                        .all(|&reader| retained.contains(&(ff_node_base + reader, writer_node)));
                    comb_readers_proven && ff_readers_proven
                })
                .collect::<Vec<_>>();

            // Direct Stores publish immediately, while staged Stores publish
            // together in finish(). Mixing those destinations inside one
            // overlapping write component can reverse procedural last-write-
            // wins order. Propagate staging through the component while
            // leaving disjoint ranges eligible for direct publication.
            let mut staged = direct
                .iter()
                .enumerate()
                .filter_map(|(index, &direct)| (!direct).then_some(index))
                .collect::<Vec<_>>();
            while let Some(staged_index) = staged.pop() {
                let staged_write = summary.writes[staged_index];
                for (candidate, candidate_write) in summary.writes.iter().enumerate() {
                    if direct[candidate]
                        && staged_write.id == candidate_write.id
                        && staged_write.access.overlaps(&candidate_write.access)
                    {
                        direct[candidate] = false;
                        staged.push(candidate);
                    }
                }
            }

            summary
                .writes
                .iter()
                .zip(direct)
                .filter_map(|(write, direct)| direct.then_some(write))
                .copied()
                .collect()
        })
        .collect()
}

pub(super) fn schedule_acyclic_path_region(
    paths: &[usize],
    dependencies: &LogicPathEdges,
    values: &LogicPathEdges,
    materialization_tokens: &[Vec<usize>],
    token_weights: &[usize],
    local_by_path: &mut [usize],
) -> Option<Vec<usize>> {
    for (local, &path) in paths.iter().enumerate() {
        if path >= local_by_path.len() || local_by_path[path] != usize::MAX {
            for &mapped in &paths[..local] {
                local_by_path[mapped] = usize::MAX;
            }
            return None;
        }
        local_by_path[path] = local;
    }

    let result = (|| {
        let dependency_predecessors =
            MappedGraphRows::new(&dependencies.predecessors, paths, local_by_path).ok()?;
        let value_predecessors =
            MappedGraphRows::new(&values.predecessors, paths, local_by_path).ok()?;
        let tokens = MappedNodeRows::new(materialization_tokens, paths).ok()?;
        let successors = MappedGraphRows::new(&dependencies.users, paths, local_by_path).ok()?;
        let value_users = MappedGraphRows::new(&values.users, paths, local_by_path).ok()?;
        let order = schedule_min_live_values_and_tokens_with_mapped_rows(
            dependency_predecessors,
            value_predecessors,
            tokens,
            token_weights,
            successors,
            value_users,
        )
        .ok()?;
        Some(order.into_iter().map(|local| paths[local]).collect())
    })();

    for &path in paths {
        local_by_path[path] = usize::MAX;
    }
    result
}

/// Schedule maximal acyclic runs by MemoryDef/MemoryUse liveness. Loop SCCs
/// and observable events are synchronization boundaries, so no path is moved
/// across an iteration or externally visible effect.
pub(super) fn schedule_logic_path_regions<Addr: Clone + Eq + Hash>(
    topological_sccs: Vec<Vec<usize>>,
    dependencies: &LogicPathEdges,
    values: &LogicPathEdges,
    materialization_tokens: &[Vec<usize>],
    token_weights: &[usize],
    input: &[LogicPath<Addr>],
) -> Option<Vec<ScheduledWork>> {
    if dependencies.users.len() != values.users.len()
        || dependencies.users.len() != materialization_tokens.len()
        || dependencies.users.len() < input.len()
    {
        return None;
    }
    let mut local_by_path = vec![usize::MAX; dependencies.users.len()];
    let mut result = Vec::with_capacity(topological_sccs.len());
    let mut pending = Vec::new();
    let flush = |pending: &mut Vec<usize>,
                 result: &mut Vec<ScheduledWork>,
                 local_by_path: &mut [usize]|
     -> Option<()> {
        if pending.is_empty() {
            return Some(());
        }
        let ordered = schedule_acyclic_path_region(
            pending,
            dependencies,
            values,
            materialization_tokens,
            token_weights,
            local_by_path,
        )?;
        result.extend(ordered.into_iter().map(|path| {
            if path < input.len() {
                ScheduledWork::CombPath(path)
            } else {
                ScheduledWork::Ff(path - input.len())
            }
        }));
        pending.clear();
        Some(())
    };

    for scc in topological_sccs {
        if let [path] = scc.as_slice()
            && !dependencies.users[*path].contains(path)
            && (*path >= input.len() || !logic_path_is_scheduling_barrier(&input[*path]))
        {
            pending.push(*path);
        } else {
            flush(&mut pending, &mut result, &mut local_by_path)?;
            if let [path] = scc.as_slice() {
                if *path < input.len() {
                    result.push(ScheduledWork::CombPath(*path));
                } else {
                    result.push(ScheduledWork::Ff(*path - input.len()));
                }
            } else if scc.iter().all(|path| *path < input.len()) {
                result.push(ScheduledWork::CombScc(scc));
            } else {
                return None;
            }
        }
    }
    flush(&mut pending, &mut result, &mut local_by_path)?;
    Some(result)
}

/// Dependency analysis and region scheduling shared by every lowering of one
/// LogicPath set.
pub(super) struct PreparedSchedule<Addr: Clone + Eq + Hash> {
    pub(super) input: Vec<LogicPath<Addr>>,
    /// Semantic dependency users, indexed by path and then FF action.
    pub(super) adj: Vec<Vec<usize>>,
    pub(super) component_by_path: Vec<usize>,
    pub(super) scheduled_work: Vec<ScheduledWork>,
    pub(super) fold_group_schedule_index: FoldGroupScheduleIndex<Addr>,
    pub(super) direct_ff_writes_by_action: Vec<Vec<VarAtomBase<Addr>>>,
}

/// Build the MemorySSA dependency graph, condense its SCCs, and order the
/// resulting work for lowering.
pub(super) fn prepare_schedule<Addr: Clone + Eq + Ord + Hash + Debug + Copy + Display>(
    input: Vec<LogicPath<Addr>>,
    arena: &SLTNodeArena<Addr>,
    four_state: bool,
    var_widths: &HashMap<Addr, usize>,
    unpacked_element_widths: &HashMap<Addr, usize>,
    ff_summaries: Option<&[FfAccessSummary<Addr>]>,
) -> Result<PreparedSchedule<Addr>, SchedulerError<Addr>> {
    let (input, ff_plan) = if let Some(summaries) = ff_summaries {
        let mut plan = plan_ff_comb_schedule(&input, summaries)?;
        let mut old_to_new = vec![usize::MAX; input.len()];
        let mut filtered = Vec::with_capacity(
            plan.required_comb
                .iter()
                .filter(|required| **required)
                .count(),
        );
        for (old, path) in input.into_iter().enumerate() {
            if plan.required_comb[old] {
                old_to_new[old] = filtered.len();
                filtered.push(path);
            }
        }
        let remap_paths = |paths: &mut Vec<usize>| {
            for path in paths.iter_mut() {
                *path = old_to_new[*path];
            }
            paths.retain(|path| *path != usize::MAX);
            paths.sort_unstable();
            paths.dedup();
        };
        for paths in plan
            .comb_value_predecessors
            .iter_mut()
            .chain(&mut plan.comb_before_direct_write)
        {
            remap_paths(paths);
        }
        plan.required_comb = vec![true; filtered.len()];
        (filtered, Some(plan))
    } else {
        (input, None)
    };

    // 1. Build bit-range MemoryDefs/MemoryUses and their semantic graph.
    // This is the source dataflow adapter; interval indexing and DAG
    // scheduling remain IR-independent in celox-analysis.
    let n = input.len();
    let (mut materialization_tokens, token_weights) =
        logic_path_materialization_tokens(&input, arena);
    let LogicPathMemorySsa {
        mut dependencies,
        mut values,
    } = build_logic_path_memory_ssa(&input)?;

    // FF actions are ordinary sinks in the existing comb dependency graph.
    // The added nodes go through the same SCC, topological, and ready-queue
    // scheduling as LogicPaths; there is no second clock-specific scheduler.
    let ff_count = ff_summaries.map_or(0, <[FfAccessSummary<Addr>]>::len);
    let mut direct_ff_writes_by_action = vec![Vec::new(); ff_count];
    if let Some(plan) = &ff_plan {
        dependencies.resize(n + ff_count);
        values.resize(n + ff_count);
        materialization_tokens.resize_with(n + ff_count, Vec::new);
        for (ff_index, predecessors) in plan.comb_value_predecessors.iter().enumerate() {
            let ff_node = n + ff_index;
            for &definition in predecessors {
                dependencies.push(definition, ff_node);
                values.push(definition, ff_node);
            }
        }
        let write_order_edges = plan
            .comb_before_direct_write
            .iter()
            .zip(&plan.ff_before_direct_write)
            .enumerate()
            .map(|(writer, (comb_readers, ff_readers))| {
                comb_readers
                    .iter()
                    .copied()
                    .map(|reader| (reader, n + writer))
                    .chain(
                        ff_readers
                            .iter()
                            .copied()
                            .map(|reader| (n + reader, n + writer)),
                    )
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let retained = add_acyclic_ff_write_order_edges(
            &mut dependencies.users,
            write_order_edges.iter().flatten().copied(),
        );
        for &(predecessor, user) in &retained {
            dependencies.predecessors[user].push(predecessor);
        }
        direct_ff_writes_by_action = direct_ff_write_ranges(
            &input,
            ff_summaries.expect("an FF schedule plan requires FF access summaries"),
            plan,
            &retained,
            n,
            var_widths,
            unpacked_element_widths,
        );
        dependencies.canonicalize();
        values.canonicalize();
    }

    // 2. SCC extraction identifies the synchronization boundaries at which
    // the dataflow equations must be iterated rather than freely reordered.
    let mut ctx = TarjanContext {
        index: 0,
        stack: Vec::new(),
        on_stack: HashSet::default(),
        indices: vec![None; n + ff_count],
        lowlink: vec![None; n + ff_count],
        sccs: Vec::new(),
    };
    for i in 0..(n + ff_count) {
        if ctx.indices[i].is_none() {
            strong_connect(i, &dependencies.users, &mut ctx);
        }
    }
    let fold_group_schedule_index = build_fold_group_schedule_index(&input, arena);
    let mut path_domains = logic_path_scheduling_domains(&input, &fold_group_schedule_index);
    path_domains.resize(n + ff_count, None);
    let (topological_sccs, component_by_path) =
        stable_topological_sccs(ctx.sccs, &dependencies.users, &path_domains)
            .ok_or(SchedulerError::InvalidDependencyGraph)?;
    let scheduled_work = schedule_logic_path_regions(
        topological_sccs,
        &dependencies,
        &values,
        &materialization_tokens,
        &token_weights,
        &input,
    )
    .ok_or(SchedulerError::InvalidDependencyGraph)?;
    let adj = dependencies.users;
    drop(dependencies.predecessors);
    drop(values);
    let scheduled_work = form_scheduled_guard_regions(scheduled_work, &input, arena, four_state);

    Ok(PreparedSchedule {
        input,
        adj,
        component_by_path,
        scheduled_work,
        fold_group_schedule_index,
        direct_ff_writes_by_action,
    })
}
