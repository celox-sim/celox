//! Lowering an `always_comb` declaration to logic paths.

use super::*;

#[cfg(test)]
pub(super) fn parse_comb(
    module: &Module,
    decl: &CombDeclaration,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<
    (
        Vec<LogicPath<VarId>>,
        SymbolicStore<VarId>,
        BoundaryMap<VarId>,
        Vec<CombObserver<VarId>>,
        Vec<RuntimeEventSite>,
    ),
    ParserError,
> {
    parse_comb_with_loop_recovery(module, decl, arena, &[], 0)
}

type ParsedComb = (
    Vec<LogicPath<VarId>>,
    SymbolicStore<VarId>,
    BoundaryMap<VarId>,
    Vec<CombObserver<VarId>>,
    Vec<RuntimeEventSite>,
);

pub fn parse_comb_with_loop_recovery(
    module: &Module,
    decl: &CombDeclaration,
    arena: &mut SLTNodeArena<VarId>,
    loop_candidates: &[LoopRecoveryCandidate],
    capture_namespace: u32,
) -> Result<ParsedComb, ParserError> {
    // A failed attempt, which may be retried, loads nothing.
    let loads = arena.memory_loads.len();
    let parsed = parse_comb_attempt(module, decl, arena, loop_candidates, capture_namespace);
    if parsed.is_err() {
        arena.memory_loads.truncate(loads);
    }
    parsed
}

fn parse_comb_attempt(
    module: &Module,
    decl: &CombDeclaration,
    arena: &mut SLTNodeArena<VarId>,
    loop_candidates: &[LoopRecoveryCandidate],
    capture_namespace: u32,
) -> Result<ParsedComb, ParserError> {
    let loads = arena.memory_loads.len();
    let mut written_accesses = HashMap::default();
    collect_written_accesses(module, &decl.statements, &mut written_accesses)?;

    // 1. Initialization: only definitions this process can write need mutable
    // symbolic ranges. Reads of every other variable lower directly to Input,
    // keeping setup proportional to the block instead of the whole module.
    let mut current_store = SymbolicStore::default();
    for id in written_accesses.keys() {
        let var = &module.variables[id];
        let width = resolve_total_width(module, var)?;
        current_store.insert(*id, RangeStore::new(None, width));
    }

    let written_atoms: Vec<_> = written_accesses
        .iter()
        .flat_map(|(&id, accesses)| {
            accesses
                .iter()
                .map(move |access| VarAtomBase::new(id, access.lsb, access.msb))
        })
        .collect();

    // 2. Symbolic Execution: Evaluate statements sequentially to update the symbolic state.
    let effect_initial_store =
        statements_contain_runtime_effect(module, &decl.statements).then(|| current_store.fork());
    let (final_store, boundaries) = recover_unrolled::eval_statements(
        module,
        current_store,
        BoundaryMap::default(),
        &decl.statements,
        arena,
        loop_candidates,
        None,
    )?;
    let mut effects = CombEffectCollector::with_capture_namespace(capture_namespace);
    if let Some(effect_initial_store) = effect_initial_store {
        collect_comb_effects_statements(
            module,
            effect_initial_store,
            &decl.statements,
            arena,
            &mut effects,
        )?;
    }

    // 3. Path Extraction: Convert written definitions into LogicPaths. The
    // statement scan already found every possible write, so avoid revisiting
    // unrelated module variables in the final symbolic store.
    let mut paths = Vec::new();
    for id in written_accesses.keys() {
        let Some(range_store) = final_store.get(id) else {
            continue;
        };
        if module.variables[id].affiliation == veryl_analyzer::symbol::Affiliation::AlwaysComb {
            continue;
        }
        for (&lsb, (val_opt, width, origin)) in &range_store.ranges {
            if let Some((expr, sources)) = val_opt {
                let msb = lsb + width - 1;

                // Calculate relative bit positions by adjusting for the range's origin.
                let rel_lsb = lsb - origin;
                let rel_msb = msb - origin;
                let original_width = get_width(*expr, arena);

                // If not using the entire stored node, apply Slice
                let final_expr = if rel_lsb == 0 && *width == original_width {
                    *expr
                } else {
                    arena.alloc(SLTNode::Slice {
                        expr: *expr,
                        access: BitAccess::new(rel_lsb, rel_msb),
                    })?
                };

                paths.push(LogicPath::<VarId> {
                    transparent_copy: false,
                    target: LogicPathTarget::Var(VarAtomBase::new(*id, lsb, msb)),
                    sources: sources.clone(),
                    previous_sources: sources
                        .iter()
                        .copied()
                        .filter(|source| {
                            source.id != *id || !source.access.overlaps(&BitAccess::new(lsb, msb))
                        })
                        .filter(|source| {
                            written_atoms.iter().any(|written| {
                                written.id == source.id && written.access.overlaps(&source.access)
                            })
                        })
                        .collect(),
                    address_sources: HashSet::default(),
                    local_inputs: Vec::new(),
                    order_before: HashSet::default(),
                    comb_capture_enable_sites: Vec::new(),
                    comb_capture_enable_always: false,
                    pre_lower_nodes: Vec::new(),
                    expr: final_expr,
                });
            }
        }
    }
    let mut process_sensitivity = effects.sensitivity;
    for path in &paths {
        process_sensitivity.extend(path.sources.iter().copied());
    }
    let process_sensitivity = subtract_written_sensitivity(process_sensitivity, &written_atoms);
    let process_sensitivity: Vec<_> = process_sensitivity.into_iter().collect();
    // A block that loads a memory file runs once, at time zero, only when it
    // reads no signal (see `apply_readmem_writes`).
    if arena.memory_loads.len() > loads && !process_sensitivity.is_empty() {
        return Err(ParserError::illegal_context(
            "$readmemh in always_comb",
            "a block that loads a memory file must not read signals; load it in an initial or always_ff block",
            arena.memory_load_call.as_ref(),
        ));
    }
    for observer in &mut effects.observers {
        observer.sensitivity = process_sensitivity.clone();
        observer.written_input_atoms = observer
            .observed_inputs
            .iter()
            .chain(observer.position_inputs.iter())
            .copied()
            .filter(|atom| {
                written_atoms
                    .iter()
                    .any(|written| written.id == atom.id && written.access.overlaps(&atom.access))
            })
            .collect();
        let mut written_inputs = HashSet::default();
        for atom in &observer.written_input_atoms {
            written_inputs.insert(atom.id);
        }
        observer.written_inputs = written_inputs.into_iter().collect();
    }
    dump_comb_path_stats_if_requested(module, &paths, arena);
    Ok((
        paths,
        final_store,
        boundaries,
        effects.observers,
        effects.sites,
    ))
}

#[derive(Default)]
pub(super) struct CombPathStats {
    pub(super) nodes: usize,
    pub(super) for_folds: usize,
    pub(super) muxes: usize,
    pub(super) inputs: usize,
}

pub(super) fn dump_comb_path_stats_if_requested(
    module: &Module,
    paths: &[LogicPath<VarId>],
    arena: &SLTNodeArena<VarId>,
) {
    if !tracing::enabled!(tracing::Level::DEBUG) {
        return;
    }

    let module_name = resource_table::get_str_value(module.name).unwrap_or_default();

    let mut entries = Vec::new();
    let mut total_nodes = 0usize;
    let mut total_for_folds = 0usize;
    let mut total_muxes = 0usize;
    let mut total_inputs = 0usize;
    for path in paths {
        let mut visited = HashSet::default();
        let mut stats = CombPathStats::default();
        collect_comb_path_stats(path.expr, arena, &mut visited, &mut stats);
        total_nodes += stats.nodes;
        total_for_folds += stats.for_folds;
        total_muxes += stats.muxes;
        total_inputs += stats.inputs;
        let target = match &path.target {
            LogicPathTarget::Var(var) | LogicPathTarget::Process { target: var, .. } => {
                module.variables.get(&var.id).map_or_else(
                    || var.to_string(),
                    |info| format!("{}[{}:{}]", info.path, var.access.msb, var.access.lsb),
                )
            }
            LogicPathTarget::CombCaptureEvent { site_id, .. } => {
                format!("capture_event({site_id})")
            }
        };
        entries.push((
            stats.nodes,
            stats.for_folds,
            stats.muxes,
            stats.inputs,
            target,
        ));
    }
    entries.sort_by(|a, b| b.cmp(a));

    tracing::debug!(
        "[comb-path-summary] module={} paths={} total_nodes={} total_for_folds={} total_muxes={} total_inputs={}",
        module_name,
        paths.len(),
        total_nodes,
        total_for_folds,
        total_muxes,
        total_inputs,
    );

    let limit = 20;
    for (rank, (nodes, for_folds, muxes, inputs, target)) in
        entries.into_iter().take(limit).enumerate()
    {
        tracing::debug!(
            "[comb-path-stats] module={} rank={} target={} nodes={} for_folds={} muxes={} inputs={}",
            module_name,
            rank + 1,
            target,
            nodes,
            for_folds,
            muxes,
            inputs,
        );
    }
}

pub(super) fn collect_comb_path_stats(
    node: NodeId,
    arena: &SLTNodeArena<VarId>,
    visited: &mut HashSet<NodeId>,
    stats: &mut CombPathStats,
) {
    if !visited.insert(node) {
        return;
    }
    stats.nodes += 1;
    match arena.get(node) {
        SLTNode::Input { .. } => stats.inputs += 1,
        SLTNode::Constant(_, _, _, _) => {}
        SLTNode::Binary(lhs, _, rhs) => {
            collect_comb_path_stats(*lhs, arena, visited, stats);
            collect_comb_path_stats(*rhs, arena, visited, stats);
        }
        SLTNode::Unary(_, inner) | SLTNode::Capture { expr: inner, .. } => {
            collect_comb_path_stats(*inner, arena, visited, stats)
        }
        SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        } => {
            stats.muxes += 1;
            collect_comb_path_stats(*cond, arena, visited, stats);
            collect_comb_path_stats(*then_expr, arena, visited, stats);
            collect_comb_path_stats(*else_expr, arena, visited, stats);
        }
        SLTNode::Concat(parts) => {
            for (part, _) in parts {
                collect_comb_path_stats(*part, arena, visited, stats);
            }
        }
        SLTNode::Slice { expr, .. } => collect_comb_path_stats(*expr, arena, visited, stats),
        SLTNode::ForFold {
            start,
            end,
            result,
            initials,
            updates,
            effects,
            continue_cond,
            ..
        } => {
            stats.for_folds += 1;
            if let SLTLoopBound::Expr(node) | SLTLoopBound::TypedExpr { node, .. } = start {
                collect_comb_path_stats(*node, arena, visited, stats);
            }
            if let SLTLoopBound::Expr(node) | SLTLoopBound::TypedExpr { node, .. } = end {
                collect_comb_path_stats(*node, arena, visited, stats);
            }
            if let SLTForFoldResult::Transient { initial, update } = result {
                collect_comb_path_stats(*initial, arena, visited, stats);
                collect_comb_path_stats(*update, arena, visited, stats);
            }
            for init in initials {
                collect_comb_path_stats(init.expr, arena, visited, stats);
            }
            for update in updates {
                collect_comb_path_stats(update.expr, arena, visited, stats);
            }
            for effect in effects {
                match effect {
                    SLTForEffect::Event { guard, args, .. } => {
                        if let Some(guard) = guard {
                            collect_comb_path_stats(*guard, arena, visited, stats);
                        }
                        for arg in args {
                            collect_comb_path_stats(*arg, arena, visited, stats);
                        }
                    }
                    SLTForEffect::Runner(runner) => {
                        collect_comb_path_stats(*runner, arena, visited, stats);
                    }
                }
            }
            collect_comb_path_stats(*continue_cond, arena, visited, stats);
        }
        SLTNode::ForFoldGroup {
            entry_guard,
            states,
            ..
        } => {
            stats.for_folds += 1;
            collect_comb_path_stats(*entry_guard, arena, visited, stats);
            for state in states {
                collect_comb_path_stats(state.initial, arena, visited, stats);
                collect_comb_path_stats(state.update, arena, visited, stats);
            }
        }
    }
}
