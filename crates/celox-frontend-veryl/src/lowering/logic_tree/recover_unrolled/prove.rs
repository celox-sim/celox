//! Proving that a recovered loop computes what the unrolled statements compute.

use super::*;

pub(super) struct ProvenGroup {
    pub(super) arena: SLTNodeArena<VarId>,
    pub(super) updates: Vec<NodeId>,
    pub(super) update_sources: HashSet<VarAtomBase<VarId>>,
    pub(super) boundaries: BoundaryMap<VarId>,
    pub(super) targets: Vec<VarAtomBase<VarId>>,
    pub(super) loop_var: VarId,
    pub(super) loop_width: usize,
    pub(super) loop_signed: bool,
    pub(super) start: BigInt,
    pub(super) step: BigInt,
    pub(super) trip_count: usize,
}

pub(super) fn recover_group(
    module: &Module,
    initial_store: &SymbolicStore<VarId>,
    statements: &[Statement],
    candidate: &LoopRecoveryCandidate,
    guard: Option<(NodeId, HashSet<VarAtomBase<VarId>>)>,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<Option<(SymbolicStore<VarId>, BoundaryMap<VarId>)>, ParserError> {
    if prefer_static_lane_writes(module, statements, candidate) {
        // These iterations build independent lanes, not a recurrence. Folding
        // them introduces a wide accumulator and dynamic shifts/masks for
        // every lane; static expansion keeps the operations narrow.
        return Ok(None);
    }
    let Some(proof) = prove_group(module, initial_store, statements, candidate, arena) else {
        return Ok(None);
    };
    if proof.targets.len() == 1
        && expanded_outputs_are_count_idioms(
            module,
            initial_store,
            statements,
            &proof.targets,
            arena,
        )
        .unwrap_or(false)
    {
        // The ordinary expansion has a stronger exact lowering (PopCount,
        // CLZ, or CTZ). Let the existing semantic matcher see that DAG rather
        // than replacing it with a counted loop.
        return Ok(None);
    }
    let (entry_guard, guard_sources) = if let Some(guard) = guard {
        guard
    } else {
        (
            arena.alloc(SLTNode::Constant(
                BigUint::from(1u8),
                BigUint::from(0u8),
                1,
                false,
            ))?,
            HashSet::default(),
        )
    };

    let mut map_cache = HashMap::default();
    let updates = proof
        .updates
        .iter()
        .map(|&update| {
            proof
                .arena
                .get(update)
                .map_addr(update, &proof.arena, arena, &mut map_cache, &|id| *id)
        })
        .collect::<Result<Vec<_>, _>>()?;

    let state_ids = proof
        .targets
        .iter()
        .map(|target| target.id)
        .collect::<HashSet<_>>();
    let mut all_sources = proof.update_sources;
    all_sources.retain(|source| source.id != proof.loop_var && !state_ids.contains(&source.id));
    all_sources.extend(guard_sources);
    let mut initials = Vec::with_capacity(proof.targets.len());
    for target in &proof.targets {
        let range_store = initial_store.get(&target.id).ok_or_else(|| {
            ParserError::illegal_context(
                "recovered loop initial state",
                "state variable is absent from the symbolic store",
                None,
            )
        })?;
        let parts = range_store
            .get_parts_ref(target.access)
            .map_err(|error| range_store_error("recovered loop initial state", error, None))?;
        if parts.iter().any(|(value, _)| value.is_none()) {
            return Ok(None);
        }
        let (initial, sources) =
            combine_parts_with_default(target.id, target.access.lsb, parts, arena)?;
        all_sources.extend(sources);
        initials.push(initial);
    }

    let states = proof
        .targets
        .iter()
        .copied()
        .zip(initials)
        .zip(updates)
        .map(|((target, initial), update)| SLTForFoldGroupState {
            target,
            initial,
            update,
        })
        .collect::<Vec<_>>();
    let group = arena.alloc(SLTNode::ForFoldGroup {
        loop_var: proof.loop_var,
        loop_width: proof.loop_width,
        loop_signed: proof.loop_signed,
        start: proof.start,
        step: proof.step,
        trip_count: proof.trip_count,
        entry_guard,
        states,
    })?;

    let total_width = get_width(group, arena);
    let mut next_msb = total_width;
    let mut result_store = initial_store.fork();
    for target in &proof.targets {
        let width = target.access.msb - target.access.lsb + 1;
        next_msb -= width;
        let projection = arena.alloc(SLTNode::Slice {
            expr: group,
            access: BitAccess::new(next_msb, next_msb + width - 1),
        })?;
        result_store
            .get_mut(&target.id)
            .ok_or_else(|| {
                ParserError::illegal_context(
                    "recovered loop result state",
                    "state variable is absent from the symbolic store",
                    None,
                )
            })?
            .update(target.access, Some((projection, all_sources.clone())))
            .map_err(|error| range_store_error("recovered loop result state", error, None))?;
    }

    Ok(Some((result_store, proof.boundaries)))
}

pub(super) fn prefer_static_lane_writes(
    module: &Module,
    statements: &[Statement],
    candidate: &LoopRecoveryCandidate,
) -> bool {
    // Bound code growth. Larger loops still use the compact counted form.
    if candidate.unrolled.iterations.len() > 64 || statements.len() > 512 {
        return false;
    }
    let mut destinations = Vec::new();
    if collect_destinations(statements, &mut destinations).is_none() {
        return false;
    }
    let mut written: HashMap<VarId, Vec<BitAccess>> = HashMap::default();
    let mut wide = false;
    for dst in destinations {
        if iteration_owner(module, &candidate.unrolled, dst.id).is_some()
            || !dst.index.0.is_empty()
            || !crate::bitaccess::is_static_access(&dst.index, &dst.select)
        {
            return false;
        }
        let Some(variable) = module.variables.get(&dst.id) else {
            return false;
        };
        let Ok(width) = resolve_total_width(module, variable) else {
            return false;
        };
        let Ok(access) = crate::bitaccess::eval_var_select(module, dst.id, &dst.index, &dst.select)
        else {
            return false;
        };
        if access.msb - access.lsb + 1 > 64 || width > 4096 {
            return false;
        }
        wide |= width > 64;
        let lanes = written.entry(dst.id).or_default();
        if lanes.iter().any(|lane| lane.overlaps(&access)) {
            return false;
        }
        lanes.push(access);
    }
    if !wide {
        return false;
    }
    let mut reads = Vec::new();
    let mut pending: Vec<_> = statements.iter().collect();
    while let Some(statement) = pending.pop() {
        match statement {
            Statement::Assign(assign) => {
                if !collect_expression_variables(&assign.expr, &mut reads) {
                    return false;
                }
            }
            Statement::If(branch) => {
                if !collect_expression_variables(&branch.cond, &mut reads) {
                    return false;
                }
                pending.extend(&branch.true_side);
                pending.extend(&branch.false_side);
            }
            _ => return false,
        }
    }
    !reads.iter().any(|id| written.contains_key(id))
}

pub(super) fn expanded_outputs_are_count_idioms(
    module: &Module,
    initial_store: &SymbolicStore<VarId>,
    statements: &[Statement],
    targets: &[VarAtomBase<VarId>],
    arena: &SLTNodeArena<VarId>,
) -> Option<bool> {
    // NodeIds stored in `initial_store` remain valid in an exact arena clone.
    // Evaluate only in this disposable copy: a non-count candidate must not
    // leave its unrolled DAG in the production arena.
    let mut scratch = arena.clone();
    let (store, _) = statements
        .iter()
        .try_fold(
            (initial_store.fork(), BoundaryMap::default()),
            |(store, boundaries), statement| {
                eval_statement(module, store, boundaries, statement, &mut scratch)
            },
        )
        .ok()?;

    targets
        .iter()
        .map(|target| {
            let parts = store.get(&target.id)?.get_parts_ref(target.access).ok()?;
            let (output, _) =
                combine_parts_with_default(target.id, target.access.lsb, parts, &mut scratch)
                    .ok()?;
            Some(celox_slt::matches_slt_count_idiom(output, &scratch))
        })
        .collect::<Option<Vec<_>>>()
        .map(|matches| matches.into_iter().any(|matched| matched))
}

pub(super) fn prove_group(
    module: &Module,
    initial_store: &SymbolicStore<VarId>,
    statements: &[Statement],
    candidate: &LoopRecoveryCandidate,
    production_arena: &SLTNodeArena<VarId>,
) -> Option<ProvenGroup> {
    if !exact_iteration_chunks(module, statements, candidate) {
        return None;
    }
    let iterations = &candidate.unrolled.iterations;
    let first_iteration = iterations.first()?;
    let second_iteration = iterations.get(1)?;
    let step = BigInt::from(second_iteration.value) - BigInt::from(first_iteration.value);
    if step == BigInt::from(0u8)
        || iterations
            .windows(2)
            .any(|pair| BigInt::from(pair[1].value) - BigInt::from(pair[0].value) != step)
    {
        return None;
    }

    let loop_variable = module.variables.get(&first_iteration.loop_var)?;
    let loop_width = resolve_total_width(module, loop_variable).ok()?;
    if loop_width == 0 {
        return None;
    }

    let chunk_len = statements.len() / iterations.len();
    let chunks = statements.chunks_exact(chunk_len).collect::<Vec<_>>();
    let targets = persistent_targets(module, &chunks, &candidate.unrolled)?;
    if targets.is_empty() {
        return None;
    }

    let target_ids = targets
        .iter()
        .map(|target| target.id)
        .collect::<HashSet<_>>();
    for target in &targets {
        let parts = initial_store
            .get(&target.id)?
            .get_parts_ref(target.access)
            .ok()?;
        if parts.iter().any(|(value, _)| value.is_none()) {
            return None;
        }
        if parts.iter().any(|(value, _)| {
            value.as_ref().is_some_and(|(_, sources)| {
                sources.iter().any(|source| target_ids.contains(&source.id))
            })
        }) {
            return None;
        }
    }

    let mut first_template = chunks[0].to_vec();
    // A fixed-trip recurrence need not read its induction variable.  The
    // per-iteration and whole-fold proofs below establish the body and final
    // result exactly; only parameterization failure is a rejection.
    parameterize_folded_loop_values(module, &mut first_template, &chunks, &candidate.unrolled)?;
    let mut scratch = SLTNodeArena::with_context_from(production_arena);
    let mut actual_outputs_by_iteration = Vec::with_capacity(iterations.len());
    let mut proof_bits = ProofBitCanonicalizer::default();
    for (iteration_index, (iteration, actual_chunk)) in
        iterations.iter().zip(chunks.iter()).enumerate()
    {
        let actual_outputs = eval_chunk_outputs(module, actual_chunk, &targets, &mut scratch)?;

        let mut concrete = first_template.clone();
        if !rewrite_statements(
            &mut concrete,
            first_iteration.loop_var,
            RewriteMode::Concrete(iteration.value),
        ) {
            return None;
        }
        let concrete_outputs = eval_chunk_outputs(module, &concrete, &targets, &mut scratch)?;
        // Analyzer folding can represent the same concrete iteration either
        // as a literal or as a constant operation.  Preserve the NodeId fast
        // path, then compare both DAGs through the same exact bit proof.
        let concrete_matches = if actual_outputs == concrete_outputs {
            true
        } else {
            let mut actual_cache = HashMap::default();
            let canonical_actual = actual_outputs
                .iter()
                .map(|&actual| {
                    specialize_slt_node(actual, None, None, &mut scratch, &mut actual_cache)
                })
                .collect::<Option<Vec<_>>>()?;
            let mut concrete_cache = HashMap::default();
            let canonical_concrete = concrete_outputs
                .iter()
                .map(|&concrete| {
                    specialize_slt_node(concrete, None, None, &mut scratch, &mut concrete_cache)
                })
                .collect::<Option<Vec<_>>>()?;
            proof_outputs_match(
                &canonical_actual,
                &canonical_concrete,
                &mut scratch,
                &mut proof_bits,
            )?
        };
        if !concrete_matches {
            return None;
        }
        actual_outputs_by_iteration.push(actual_outputs);

        let mut variables = Vec::new();
        for statement in actual_chunk.iter() {
            if !collect_statement_variables(statement, &mut variables)
                || variables.iter().any(|&variable| {
                    iteration_owner(module, &candidate.unrolled, variable)
                        .is_some_and(|owner| owner != iteration_index)
                })
            {
                return None;
            }
        }
    }

    // The recovered loop runs exactly the proven iterations, so its variable
    // stays within their values. Indices proven in range then need no check.
    let mut loop_range = super::super::LoopRangeScope::with_max(
        &mut scratch,
        first_iteration.loop_var,
        iterations.iter().map(|iteration| iteration.value).max()?,
    );
    let scoped = &mut *loop_range;
    let mut dynamic = first_template;
    if !rewrite_statements(&mut dynamic, first_iteration.loop_var, RewriteMode::Dynamic) {
        return None;
    }
    let (proof_updates, _, _) = eval_chunk_outputs_with_facts(module, &dynamic, &targets, scoped)?;

    // Canonical proof bits depend only on an immutable arena node and its bit
    // position.  The arena is append-only, so results remain valid while later
    // iterations add their specialized nodes.  Keep one cache for the whole
    // proof instead of rebuilding the same input/constant bits per iteration.
    for (iteration, actual_outputs) in iterations.iter().zip(&actual_outputs_by_iteration) {
        let mut dynamic_cache = HashMap::default();
        let specialized_updates = proof_updates
            .iter()
            .map(|&update| {
                specialize_slt_node(
                    update,
                    Some((first_iteration.loop_var, iteration.value)),
                    None,
                    scoped,
                    &mut dynamic_cache,
                )
            })
            .collect::<Option<Vec<_>>>()?;
        let mut actual_cache = HashMap::default();
        let canonical_actual = actual_outputs
            .iter()
            .map(|&actual| specialize_slt_node(actual, None, None, scoped, &mut actual_cache))
            .collect::<Option<Vec<_>>>()?;
        if !proof_outputs_match(
            &specialized_updates,
            &canonical_actual,
            scoped,
            &mut proof_bits,
        )? {
            return None;
        }
    }

    let (updates, _, boundaries) = eval_chunk_outputs_with_initial_store(
        module,
        &dynamic,
        &targets,
        first_iteration.loop_var,
        initial_store,
        production_arena,
        scoped,
    )?;
    let update_sources = collect_slt_input_sources(&updates, scoped)?;

    let mut input_variables = HashSet::default();
    let mut visited = HashSet::default();
    for &update in &updates {
        if !collect_template_inputs(update, scoped, &mut visited, &mut input_variables) {
            return None;
        }
    }
    if input_variables.iter().any(|&variable| {
        variable != first_iteration.loop_var
            && (iteration_owner(module, &candidate.unrolled, variable).is_some()
                || (!target_ids.contains(&variable)
                    && initial_store.get(&variable).is_some_and(|ranges| {
                        ranges.ranges.values().all(|(value, _, _)| value.is_some())
                    })))
    }) {
        return None;
    }

    for source in &update_sources {
        if source.id == first_iteration.loop_var || target_ids.contains(&source.id) {
            continue;
        }
        let Some(ranges) = initial_store.get(&source.id) else {
            continue;
        };
        let parts = ranges.get_parts_ref(source.access).ok()?;
        if parts.iter().any(|(value, _)| value.is_some()) {
            return None;
        }
    }

    if !whole_fold_matches_expansion(
        module,
        initial_store,
        statements,
        production_arena,
        scoped,
        &proof_updates,
        &targets,
        first_iteration.loop_var,
        iterations,
    )? {
        return None;
    }

    // Leave the scope so the proven arena no longer records the bound.
    drop(loop_range);
    Some(ProvenGroup {
        arena: scratch,
        updates,
        update_sources,
        boundaries,
        targets,
        loop_var: first_iteration.loop_var,
        loop_width,
        loop_signed: loop_variable.r#type.signed,
        start: BigInt::from(first_iteration.value),
        step,
        trip_count: iterations.len(),
    })
}

pub(super) fn persistent_targets(
    module: &Module,
    chunks: &[&[Statement]],
    candidate: &UnrolledLoopCandidate,
) -> Option<Vec<VarAtomBase<VarId>>> {
    let mut destinations_by_iteration = Vec::with_capacity(chunks.len());
    for chunk in chunks {
        let mut destinations = Vec::new();
        collect_destinations(chunk, &mut destinations)?;
        destinations_by_iteration.push(destinations);
    }
    let first = destinations_by_iteration.first()?;
    if destinations_by_iteration
        .iter()
        .any(|destinations| destinations.len() != first.len())
    {
        return None;
    }

    let mut targets = BTreeSet::new();
    for position in 0..first.len() {
        let destination = first[position];
        if iteration_owner(module, candidate, destination.id).is_some() {
            continue;
        }
        let width = resolve_total_width(module, module.variables.get(&destination.id)?).ok()?;
        if width == 0 {
            return None;
        }
        let static_accesses = destinations_by_iteration
            .iter()
            .map(|destinations| {
                let destination = destinations[position];
                if destination.id != first[position].id
                    || !crate::bitaccess::is_static_access(&destination.index, &destination.select)
                {
                    return None;
                }
                crate::bitaccess::eval_var_select(
                    module,
                    destination.id,
                    &destination.index,
                    &destination.select,
                )
                .ok()
            })
            .collect::<Option<Vec<_>>>();
        let access = static_accesses
            .filter(|accesses| accesses.windows(2).all(|pair| pair[0] == pair[1]))
            .and_then(|accesses| accesses.first().copied())
            .unwrap_or_else(|| BitAccess::new(0, width - 1));
        targets.insert((destination.id, access.lsb, access.msb));
    }
    Some(
        targets
            .into_iter()
            .map(|(id, lsb, msb)| VarAtomBase::new(id, lsb, msb))
            .collect(),
    )
}

pub(super) fn collect_destinations<'a>(
    statements: &'a [Statement],
    out: &mut Vec<&'a veryl_analyzer::ir::AssignDestination>,
) -> Option<()> {
    for statement in statements {
        match statement {
            Statement::Assign(assign) if assign.dst.len() == 1 => out.push(&assign.dst[0]),
            Statement::If(statement) => {
                collect_destinations(&statement.true_side, out)?;
                collect_destinations(&statement.false_side, out)?;
            }
            _ => return None,
        }
    }
    Some(())
}

pub(super) fn proof_variable_ids(
    statements: &[Statement],
    targets: &[VarAtomBase<VarId>],
) -> Option<HashSet<VarId>> {
    let mut variables = Vec::new();
    for statement in statements {
        collect_statement_variables(statement, &mut variables).then_some(())?;
    }
    let mut variables = variables.into_iter().collect::<HashSet<_>>();
    variables.extend(targets.iter().map(|target| target.id));
    Some(variables)
}

pub(super) fn marker_store(
    module: &Module,
    variables: &HashSet<VarId>,
) -> Option<SymbolicStore<VarId>> {
    let mut store = SymbolicStore::default();
    for id in variables {
        let variable = module.variables.get(id)?;
        let width = resolve_total_width(module, variable).ok()?;
        store.insert(*id, RangeStore::new(None, width));
    }
    Some(store)
}

pub(super) fn eval_chunk_outputs(
    module: &Module,
    statements: &[Statement],
    targets: &[VarAtomBase<VarId>],
    arena: &mut SLTNodeArena<VarId>,
) -> Option<Vec<NodeId>> {
    eval_chunk_outputs_with_facts(module, statements, targets, arena).map(|x| x.0)
}

pub(super) fn eval_chunk_outputs_with_facts(
    module: &Module,
    statements: &[Statement],
    targets: &[VarAtomBase<VarId>],
    arena: &mut SLTNodeArena<VarId>,
) -> Option<(Vec<NodeId>, HashSet<VarAtomBase<VarId>>, BoundaryMap<VarId>)> {
    let variables = proof_variable_ids(statements, targets)?;
    let store = marker_store(module, &variables)?;
    let (store, boundaries) = statements
        .iter()
        .try_fold(
            (store, BoundaryMap::default()),
            |(store, boundaries), statement| {
                eval_statement(module, store, boundaries, statement, arena)
            },
        )
        .ok()?;
    let mut outputs = Vec::with_capacity(targets.len());
    let mut sources = HashSet::default();
    for target in targets {
        let parts = store.get(&target.id)?.get_parts_ref(target.access).ok()?;
        let (output, output_sources) =
            combine_parts_with_default(target.id, target.access.lsb, parts, arena).ok()?;
        outputs.push(output);
        sources.extend(output_sources);
    }
    Some((outputs, sources, boundaries))
}

pub(super) fn eval_chunk_outputs_with_initial_store(
    module: &Module,
    statements: &[Statement],
    targets: &[VarAtomBase<VarId>],
    loop_var: VarId,
    initial_store: &SymbolicStore<VarId>,
    source_arena: &SLTNodeArena<VarId>,
    arena: &mut SLTNodeArena<VarId>,
) -> Option<(Vec<NodeId>, HashSet<VarAtomBase<VarId>>, BoundaryMap<VarId>)> {
    let variables = proof_variable_ids(statements, targets)?;
    let mut rebound_ids = targets
        .iter()
        .map(|target| target.id)
        .collect::<HashSet<_>>();
    rebound_ids.insert(loop_var);
    for variable in variables
        .iter()
        .filter(|variable| !rebound_ids.contains(variable))
    {
        let Some(ranges) = initial_store.get(variable) else {
            continue;
        };
        for (value, _, _) in ranges.ranges.values() {
            let Some((root, sources)) = value else {
                continue;
            };
            if sources
                .iter()
                .any(|source| rebound_ids.contains(&source.id))
                || slt_reaches_input_id(*root, source_arena, &rebound_ids)
            {
                // A value captured before the recovered loop may contain an
                // older version of a loop-carried variable. LowerEnv binds
                // those variable IDs to the current iteration state, so such
                // a DAG cannot be embedded without an explicit versioned
                // capture.
                return None;
            }
        }
    }
    let mut store =
        map_symbolic_store_roots(module, initial_store, &variables, source_arena, arena)?;
    for target in targets {
        let width = resolve_total_width(module, module.variables.get(&target.id)?).ok()?;
        store.insert(target.id, RangeStore::new(None, width));
    }
    let loop_width = resolve_total_width(module, module.variables.get(&loop_var)?).ok()?;
    store.insert(loop_var, RangeStore::new(None, loop_width));

    let (store, boundaries) = statements
        .iter()
        .try_fold(
            (store, BoundaryMap::default()),
            |(store, boundaries), statement| {
                eval_statement(module, store, boundaries, statement, arena)
            },
        )
        .ok()?;
    let mut outputs = Vec::with_capacity(targets.len());
    let mut sources = HashSet::default();
    for target in targets {
        let parts = store.get(&target.id)?.get_parts_ref(target.access).ok()?;
        let (output, output_sources) =
            combine_parts_with_default(target.id, target.access.lsb, parts, arena).ok()?;
        outputs.push(output);
        sources.extend(output_sources);
    }
    Some((outputs, sources, boundaries))
}

pub(super) fn slt_reaches_input_id(
    root: NodeId,
    arena: &SLTNodeArena<VarId>,
    variables: &HashSet<VarId>,
) -> bool {
    let mut visited = HashSet::default();
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        if !visited.insert(node) {
            continue;
        }
        match arena.get(node) {
            SLTNode::Input {
                variable, index, ..
            } => {
                if variables.contains(variable) {
                    return true;
                }
                pending.extend(index.iter().map(|entry| entry.node));
            }
            SLTNode::Binary(lhs, _, rhs) => {
                pending.push(*lhs);
                pending.push(*rhs);
            }
            SLTNode::Unary(_, inner) | SLTNode::Capture { expr: inner, .. } => pending.push(*inner),
            SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } => {
                pending.push(*cond);
                pending.push(*then_expr);
                pending.push(*else_expr);
            }
            SLTNode::Concat(parts) => pending.extend(parts.iter().map(|(part, _)| *part)),
            SLTNode::Slice { expr, .. } => pending.push(*expr),
            SLTNode::Constant(..) => {}
            SLTNode::ForFold { .. } | SLTNode::ForFoldGroup { .. } => return true,
        }
    }
    false
}

pub(super) fn collect_slt_input_sources(
    roots: &[NodeId],
    arena: &SLTNodeArena<VarId>,
) -> Option<HashSet<VarAtomBase<VarId>>> {
    let mut sources = HashSet::default();
    let mut visited = HashSet::default();
    let mut pending = roots.to_vec();
    while let Some(node) = pending.pop() {
        if !visited.insert(node) {
            continue;
        }
        match arena.get(node) {
            SLTNode::Input {
                variable,
                index,
                access,
                ..
            } => {
                sources.insert(VarAtomBase::new(*variable, access.lsb, access.msb));
                pending.extend(index.iter().map(|entry| entry.node));
            }
            SLTNode::Binary(lhs, _, rhs) => {
                pending.push(*lhs);
                pending.push(*rhs);
            }
            SLTNode::Unary(_, inner) | SLTNode::Capture { expr: inner, .. } => pending.push(*inner),
            SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } => {
                pending.push(*cond);
                pending.push(*then_expr);
                pending.push(*else_expr);
            }
            SLTNode::Concat(parts) => pending.extend(parts.iter().map(|(part, _)| *part)),
            SLTNode::Slice { expr, .. } => pending.push(*expr),
            SLTNode::Constant(..) => {}
            SLTNode::ForFold { .. } | SLTNode::ForFoldGroup { .. } => return None,
        }
    }
    Some(sources)
}

pub(super) fn whole_fold_matches_expansion(
    module: &Module,
    initial_store: &SymbolicStore<VarId>,
    statements: &[Statement],
    production_arena: &SLTNodeArena<VarId>,
    dynamic_arena: &SLTNodeArena<VarId>,
    dynamic_updates: &[NodeId],
    targets: &[VarAtomBase<VarId>],
    loop_var: VarId,
    iterations: &[crate::loop_provenance::UnrolledIteration],
) -> Option<bool> {
    let variables = proof_variable_ids(statements, targets)?;
    let mut proof_arena = SLTNodeArena::with_context_from(dynamic_arena);
    let mapped_initial = map_symbolic_store_roots(
        module,
        initial_store,
        &variables,
        production_arena,
        &mut proof_arena,
    )?;

    let (expanded_store, _) = statements
        .iter()
        .try_fold(
            (mapped_initial.fork(), BoundaryMap::default()),
            |(store, boundaries), statement| {
                eval_statement(module, store, boundaries, statement, &mut proof_arena)
            },
        )
        .ok()?;
    let expanded_outputs = read_target_outputs(&expanded_store, targets, &mut proof_arena)?;

    let mut update_map_cache = HashMap::default();
    let mapped_updates = dynamic_updates
        .iter()
        .map(|&update| {
            dynamic_arena
                .get(update)
                .map_addr(
                    update,
                    dynamic_arena,
                    &mut proof_arena,
                    &mut update_map_cache,
                    &|id| *id,
                )
                .ok()
        })
        .collect::<Option<Vec<_>>>()?;
    let target_ids = targets
        .iter()
        .map(|target| target.id)
        .collect::<HashSet<_>>();
    let mut state_values = HashMap::default();
    for variable in &variables {
        if *variable == loop_var || target_ids.contains(variable) {
            continue;
        }
        let width = resolve_total_width(module, module.variables.get(variable)?).ok()?;
        if width == 0 {
            return None;
        }
        let access = BitAccess::new(0, width - 1);
        let parts = mapped_initial.get(variable)?.get_parts_ref(access).ok()?;
        if parts.iter().any(|(value, _)| value.is_none()) {
            continue;
        }
        let (value, _) = combine_parts_with_default(*variable, 0, parts, &mut proof_arena).ok()?;
        state_values.insert(*variable, (access, value));
    }
    let initial_values = read_target_outputs(&mapped_initial, targets, &mut proof_arena)?;
    state_values.extend(
        targets
            .iter()
            .copied()
            .zip(initial_values)
            .map(|(target, value)| (target.id, (target.access, value))),
    );

    for iteration in iterations {
        let mut cache = HashMap::default();
        let next_values = mapped_updates
            .iter()
            .map(|&update| {
                specialize_slt_node(
                    update,
                    Some((loop_var, iteration.value)),
                    Some(&state_values),
                    &mut proof_arena,
                    &mut cache,
                )
            })
            .collect::<Option<Vec<_>>>()?;
        for (target, value) in targets.iter().copied().zip(next_values) {
            state_values.insert(target.id, (target.access, value));
        }
    }

    let composed_outputs = targets
        .iter()
        .map(|target| state_values.get(&target.id).map(|(_, value)| *value))
        .collect::<Option<Vec<_>>>()?;
    let mut expanded_cache = HashMap::default();
    let canonical_expanded = expanded_outputs
        .iter()
        .map(|&output| {
            specialize_slt_node(output, None, None, &mut proof_arena, &mut expanded_cache)
        })
        .collect::<Option<Vec<_>>>()?;
    let mut composed_cache = HashMap::default();
    let canonical_composed = composed_outputs
        .iter()
        .map(|&output| {
            specialize_slt_node(output, None, None, &mut proof_arena, &mut composed_cache)
        })
        .collect::<Option<Vec<_>>>()?;
    let mut proof_bits = ProofBitCanonicalizer::default();
    proof_outputs_match(
        &canonical_expanded,
        &canonical_composed,
        &mut proof_arena,
        &mut proof_bits,
    )
}

pub(super) fn map_symbolic_store_roots(
    module: &Module,
    store: &SymbolicStore<VarId>,
    variables: &HashSet<VarId>,
    source_arena: &SLTNodeArena<VarId>,
    target_arena: &mut SLTNodeArena<VarId>,
) -> Option<SymbolicStore<VarId>> {
    let mut mapped = SymbolicStore::default();
    let mut cache = HashMap::default();
    for id in variables {
        let mut range_store = if let Some(range_store) = store.get(id) {
            range_store.clone()
        } else {
            let variable = module.variables.get(id)?;
            let width = resolve_total_width(module, variable).ok()?;
            RangeStore::new(None, width)
        };
        for (value, _, _) in range_store.ranges.values_mut() {
            let Some((node, _)) = value else {
                continue;
            };
            *node = source_arena
                .get(*node)
                .map_addr(*node, source_arena, target_arena, &mut cache, &|id| *id)
                .ok()?;
        }
        mapped.insert(*id, range_store);
    }
    Some(mapped)
}

pub(super) fn read_target_outputs(
    store: &SymbolicStore<VarId>,
    targets: &[VarAtomBase<VarId>],
    arena: &mut SLTNodeArena<VarId>,
) -> Option<Vec<NodeId>> {
    targets
        .iter()
        .map(|target| {
            let parts = store.get(&target.id)?.get_parts_ref(target.access).ok()?;
            combine_parts_with_default(target.id, target.access.lsb, parts, arena)
                .ok()
                .map(|(output, _)| output)
        })
        .collect()
}

pub(super) fn collect_template_inputs(
    root: NodeId,
    arena: &SLTNodeArena<VarId>,
    visited: &mut HashSet<NodeId>,
    inputs: &mut HashSet<VarId>,
) -> bool {
    if !visited.insert(root) {
        return true;
    }
    match arena.get(root) {
        SLTNode::Input {
            variable, index, ..
        } => {
            inputs.insert(*variable);
            index
                .iter()
                .all(|index| collect_template_inputs(index.node, arena, visited, inputs))
        }
        SLTNode::Constant(_, _, _, _) => true,
        SLTNode::Binary(lhs, _, rhs) => {
            collect_template_inputs(*lhs, arena, visited, inputs)
                && collect_template_inputs(*rhs, arena, visited, inputs)
        }
        SLTNode::Unary(_, inner) | SLTNode::Capture { expr: inner, .. } => {
            collect_template_inputs(*inner, arena, visited, inputs)
        }
        SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        } => {
            collect_template_inputs(*cond, arena, visited, inputs)
                && collect_template_inputs(*then_expr, arena, visited, inputs)
                && collect_template_inputs(*else_expr, arena, visited, inputs)
        }
        SLTNode::Concat(parts) => parts
            .iter()
            .all(|(part, _)| collect_template_inputs(*part, arena, visited, inputs)),
        SLTNode::Slice { expr, .. } => collect_template_inputs(*expr, arena, visited, inputs),
        SLTNode::ForFold { .. } | SLTNode::ForFoldGroup { .. } => false,
    }
}
