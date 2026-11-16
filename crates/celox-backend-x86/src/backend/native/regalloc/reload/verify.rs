//! Verifying materialized reloads, inserted state writes and memory phi factorings.

use super::*;

/// A reload selected after spill planning.  The expected recipe comes from a
/// verified pre-reconstruction MemorySSA snapshot; the final verifier rebuilds
/// MemorySSA and compares the emitted machine operation and reaching version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in super::super) struct ExpectedMaterializedReload {
    pub reload: VReg,
    pub expected: ResolvedRecipe,
    /// Pre-reconstruction point whose abstract reload selected this recipe.
    /// This is diagnostic/proof provenance, not an instruction coordinate in
    /// final MIR (dead-code elimination may compact that block).
    pub planned_use: Option<PointUse>,
}

#[cfg(test)]
pub(in super::super) fn verify_expected_materialized_reloads(
    func: &MFunction,
    cfg: &NormalizedCfg,
    reloads: &[ExpectedMaterializedReload],
) -> Result<(), ReloadRecipeError> {
    verify_expected_materialized_reloads_after_state_spills(func, cfg, reloads, &[], &[])
}

/// Verify recipes after SSA reconstruction has inserted allocator-owned
/// SimState stores.
///
/// Pre-reconstruction `SnapshotAccess::Write` ordinals name only the original
/// MIR writes.  Final MIR ordinals also contain the supplied allocator-owned
/// stores.  Treating those inserted stores as part of the original ordinal
/// space would renumber every later, disjoint write and reject an unchanged
/// reaching definition.  This verifier therefore compares in the stable
/// original-write space while retaining each inserted store as a distinct
/// version: a synthetic store which actually reaches a generic recipe still
/// fails the proof.
pub(in super::super) fn verify_expected_materialized_reloads_after_state_spills(
    func: &MFunction,
    cfg: &NormalizedCfg,
    reloads: &[ExpectedMaterializedReload],
    inserted_state_writes: &[(BlockId, usize)],
    memory_phi_factorings: &[MemoryPhiFactoring],
) -> Result<(), ReloadRecipeError> {
    let inserted_state_writes = validate_inserted_state_writes(func, inserted_state_writes)?;
    let memory_phi_factorings = validate_memory_phi_factorings(func, cfg, memory_phi_factorings)?;
    let mut destinations = BTreeSet::new();
    for materialization in reloads {
        if !destinations.insert(materialization.reload) {
            return Err(ReloadRecipeError::new(
                "RELOAD_RECIPE.UNIQUE_DESTINATION",
                None,
                None,
                Some(materialization.reload),
                "more than one materialized reload record names this destination",
            ));
        }
    }

    let mut locations = BTreeMap::<VReg, PointUse>::new();
    for block in &func.blocks {
        for (instruction, inst) in block.insts.iter().enumerate() {
            if let Some(definition) = inst.def()
                && destinations.contains(&definition)
                && locations
                    .insert(
                        definition,
                        PointUse {
                            block: block.id,
                            instruction,
                            value: definition,
                        },
                    )
                    .is_some()
            {
                return Err(ReloadRecipeError::new(
                    "RELOAD_RECIPE.UNIQUE_DEFINITION",
                    Some(block.id),
                    Some(instruction),
                    Some(definition),
                    "materialized reload destination has more than one MIR definition",
                ));
            }
        }
    }

    let requested_points = locations.values().copied().collect::<BTreeSet<_>>();
    let rebuilt = analyze_with_queries(func, cfg, &requested_points)?;
    let mut snapshots = SnapshotComparison::new(&inserted_state_writes, &memory_phi_factorings);
    for materialization in reloads {
        let Some(location) = locations.get(&materialization.reload).copied() else {
            return Err(ReloadRecipeError::new(
                "RELOAD_RECIPE.MATERIALIZATION_DEFINITION",
                None,
                None,
                Some(materialization.reload),
                "materialized reload destination has no MIR definition",
            ));
        };
        let Some(actual) = rebuilt.resolved_recipe(materialization.reload)? else {
            return Err(ReloadRecipeError::new(
                "RELOAD_RECIPE.MATERIALIZATION_HAS_RECIPE",
                Some(location.block),
                Some(location.instruction),
                Some(materialization.reload),
                "materialized destination has no closed recipe in final MIR",
            ));
        };
        verify_resolved_recipe_match(
            func,
            materialization.reload,
            materialization.planned_use,
            location,
            &materialization.expected,
            &actual,
            &mut snapshots,
        )?;
    }
    Ok(())
}

pub(super) fn validate_inserted_state_writes(
    func: &MFunction,
    writes: &[(BlockId, usize)],
) -> Result<BTreeMap<BlockId, Vec<usize>>, ReloadRecipeError> {
    let mut grouped = BTreeMap::<BlockId, Vec<usize>>::new();
    for &(block, ordinal) in writes {
        grouped.entry(block).or_default().push(ordinal);
    }
    for (&block, ordinals) in &mut grouped {
        ordinals.sort_unstable();
        if ordinals.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(ReloadRecipeError::new(
                "RELOAD_RECIPE.INSERTED_WRITE_UNIQUE",
                Some(block),
                None,
                None,
                "more than one allocator-owned state store has the same final write identity",
            ));
        }
        let owner = func
            .blocks
            .iter()
            .find(|owner| owner.id == block)
            .ok_or_else(|| {
                ReloadRecipeError::new(
                    "RELOAD_RECIPE.INSERTED_WRITE_BLOCK",
                    Some(block),
                    None,
                    None,
                    "allocator-owned state store names a block outside final MIR",
                )
            })?;
        let write_count = owner
            .insts
            .iter()
            .filter(|inst| {
                let effect = memory_effect::writes(inst);
                matches!(
                    effect.unknown_memory(),
                    Some(memory_effect::UnknownMemory::Direct(BaseReg::SimState))
                ) || effect.ranges().any(|range| range.base == BaseReg::SimState)
            })
            .count();
        if let Some(&ordinal) = ordinals.iter().find(|&&ordinal| ordinal >= write_count) {
            return Err(ReloadRecipeError::new(
                "RELOAD_RECIPE.INSERTED_WRITE_IDENTITY",
                Some(block),
                None,
                None,
                format!("final MIR has no SimState write ordinal {ordinal}"),
            ));
        }
    }
    Ok(grouped)
}

pub(super) fn validate_memory_phi_factorings<'a>(
    func: &MFunction,
    cfg: &NormalizedCfg,
    factorings: &'a [MemoryPhiFactoring],
) -> Result<BTreeMap<BlockId, &'a MemoryPhiFactoring>, ReloadRecipeError> {
    let mut validated = BTreeMap::new();
    let sim_state = MemoryEffect::UnknownObject(MemoryObject::SimState);
    for factoring in factorings {
        if factoring.predecessors.is_empty()
            || factoring
                .predecessors
                .windows(2)
                .any(|pair| pair[0] >= pair[1])
        {
            return Err(ReloadRecipeError::new(
                "RELOAD_RECIPE.MEMORY_PHI_FACTORING_PREDECESSORS",
                Some(factoring.block),
                None,
                None,
                "factored MemoryPhi predecessors must be nonempty, unique, and sorted",
            ));
        }
        if validated.insert(factoring.block, factoring).is_some() {
            return Err(ReloadRecipeError::new(
                "RELOAD_RECIPE.MEMORY_PHI_FACTORING_UNIQUE",
                Some(factoring.block),
                None,
                None,
                "more than one CFG factoring names the same block",
            ));
        }
        let Some(&block) = cfg.block_index.get(&factoring.block) else {
            return Err(ReloadRecipeError::new(
                "RELOAD_RECIPE.MEMORY_PHI_FACTORING_BLOCK",
                Some(factoring.block),
                None,
                None,
                "factored MemoryPhi block is absent from final CFG",
            ));
        };
        let Some(&successor) = cfg.block_index.get(&factoring.successor) else {
            return Err(ReloadRecipeError::new(
                "RELOAD_RECIPE.MEMORY_PHI_FACTORING_SUCCESSOR",
                Some(factoring.block),
                None,
                None,
                "factored MemoryPhi successor is absent from final CFG",
            ));
        };
        if cfg.successors[block].as_slice() != [successor] {
            return Err(ReloadRecipeError::new(
                "RELOAD_RECIPE.MEMORY_PHI_FACTORING_SUCCESSOR",
                Some(factoring.block),
                None,
                None,
                "factored MemoryPhi block no longer has its recorded unique successor",
            ));
        }
        let mut actual_predecessors = cfg.predecessors[block]
            .iter()
            .map(|&predecessor| func.blocks[predecessor].id)
            .collect::<Vec<_>>();
        actual_predecessors.sort_unstable();
        if actual_predecessors.as_slice() != factoring.predecessors.as_ref() {
            return Err(ReloadRecipeError::new(
                "RELOAD_RECIPE.MEMORY_PHI_FACTORING_PREDECESSORS",
                Some(factoring.block),
                None,
                None,
                "factored MemoryPhi incoming edges changed after reconstruction",
            ));
        }
        if func.blocks[block].insts.iter().any(|inst| {
            analysis_effects(&memory_effect::writes(inst))
                .any(|effect| effects_may_alias(effect, sim_state))
        }) {
            return Err(ReloadRecipeError::new(
                "RELOAD_RECIPE.MEMORY_PHI_FACTORING_WRITE",
                Some(factoring.block),
                None,
                None,
                "factored MemoryPhi block contains a write which may alias SimState",
            ));
        }
    }
    Ok(validated)
}

pub(super) fn verify_resolved_recipe_match(
    func: &MFunction,
    reload: VReg,
    planned_use: Option<PointUse>,
    final_location: PointUse,
    original: &ResolvedRecipe,
    materialized: &ResolvedRecipe,
    snapshots: &mut SnapshotComparison<'_>,
) -> Result<(), ReloadRecipeError> {
    match (&original.base, &materialized.base) {
        (ResolvedBase::Constant(left), ResolvedBase::Constant(right)) if left == right => {}
        (ResolvedBase::State(left), ResolvedBase::State(right)) => {
            // `observed_bits` proves that the selected store home survives
            // intervening semantic RMWs. Once reconstruction emits the
            // selected physical load, the rebuilt base observes the full
            // machine word and the identical `steps` extract the value. The
            // original point recipe was independently rebuilt before this
            // check; materialization must match its load, version, and
            // operations, not that pre-load proof aid.
            if left.load != right.load {
                return Err(ReloadRecipeError::new(
                    "RELOAD_RECIPE.PHYSICAL_SHAPE_MATCHES",
                    None,
                    None,
                    Some(reload),
                    format!(
                        "materialized load {:?} differs from selected load {:?}",
                        right.load, left.load
                    ),
                ));
            }
            if let Some((expected, actual)) =
                snapshots.first_difference(&left.snapshot, &right.snapshot)
            {
                let first_difference = format!(
                    "; expected_write={} actual_write={}",
                    describe_expected_snapshot_write(
                        func,
                        expected,
                        snapshots.inserted_state_writes
                    ),
                    describe_snapshot_write(func, actual)
                );
                return Err(ReloadRecipeError::new(
                    "RELOAD_RECIPE.STATE_SNAPSHOT_CURRENT",
                    None,
                    None,
                    Some(reload),
                    format!(
                        "an overlapping or unknown state write changed the selected snapshot {:?} to {:?}{}; planned_use={planned_use:?} final_load={}/i{}",
                        left.snapshot,
                        right.snapshot,
                        first_difference,
                        final_location.block,
                        final_location.instruction
                    ),
                ));
            }
        }
        _ => {
            return Err(ReloadRecipeError::new(
                "RELOAD_RECIPE.PURE_BASE_MATCHES",
                None,
                None,
                Some(reload),
                format!(
                    "materialized pure base {:?} differs from original {:?}",
                    materialized.base, original.base
                ),
            ));
        }
    }
    if original.steps != materialized.steps {
        return Err(ReloadRecipeError::new(
            "RELOAD_RECIPE.PURE_STEPS_MATCH",
            None,
            None,
            Some(reload),
            format!(
                "materialized pure steps {:?} differ from original {:?}",
                materialized.steps, original.steps
            ),
        ));
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn stable_memory_snapshot_matches(
    expected: &MemorySnapshot,
    actual: &MemorySnapshot,
    inserted_state_writes: &BTreeMap<BlockId, Vec<usize>>,
    memory_phi_factorings: &BTreeMap<BlockId, &MemoryPhiFactoring>,
) -> bool {
    first_snapshot_difference(
        expected,
        actual,
        inserted_state_writes,
        memory_phi_factorings,
    )
    .is_none()
}

pub(super) type SnapshotPair = (SnapshotAccess, SnapshotAccess, i64, i64);

pub(super) struct EqualSnapshotPairs {
    // Keep the immutable tables alive: allocation addresses must never be
    // reused for a different graph while its proof results are cached.
    pub(super) expected: Rc<PhiTable>,
    pub(super) actual: Rc<PhiTable>,
    pub(super) equal: HashSet<SnapshotPair>,
}

/// Shared proofs within one final-MIR verification. The write renumbering and
/// validated factoring environment remain borrowed and immutable throughout.
/// A result from another graph, byte range or verification cannot be reused.
pub(super) struct SnapshotComparison<'a> {
    pub(super) inserted_state_writes: &'a BTreeMap<BlockId, Vec<usize>>,
    pub(super) memory_phi_factorings: &'a BTreeMap<BlockId, &'a MemoryPhiFactoring>,
    pub(super) tables: Vec<EqualSnapshotPairs>,
}

impl<'a> SnapshotComparison<'a> {
    pub(super) fn new(
        inserted_state_writes: &'a BTreeMap<BlockId, Vec<usize>>,
        memory_phi_factorings: &'a BTreeMap<BlockId, &'a MemoryPhiFactoring>,
    ) -> Self {
        Self {
            inserted_state_writes,
            memory_phi_factorings,
            tables: Vec::new(),
        }
    }

    pub(super) fn first_difference(
        &mut self,
        expected: &MemorySnapshot,
        actual: &MemorySnapshot,
    ) -> Option<(SnapshotAccess, SnapshotAccess)> {
        let index = self
            .tables
            .iter()
            .position(|table| {
                Rc::ptr_eq(&table.expected, &expected.phis)
                    && Rc::ptr_eq(&table.actual, &actual.phis)
            })
            .unwrap_or_else(|| {
                self.tables.push(EqualSnapshotPairs {
                    expected: Rc::clone(&expected.phis),
                    actual: Rc::clone(&actual.phis),
                    equal: HashSet::default(),
                });
                self.tables.len() - 1
            });
        first_snapshot_difference_with_cache(
            expected,
            actual,
            self.inserted_state_writes,
            self.memory_phi_factorings,
            &mut self.tables[index].equal,
        )
    }
}

/// First pair of differing accesses over the covered range.
///
/// Both snapshots describe, for every byte, a tree of versions rooted at the
/// byte's reaching version.  The walk compares those trees run by run,
/// splitting a run wherever either side's segments or phi pieces do, mapping
/// final write ordinals back to the stable ordinal space and inlining
/// MemoryPhis of validated reconstruction factorings.
pub(super) fn first_snapshot_difference(
    expected: &MemorySnapshot,
    actual: &MemorySnapshot,
    inserted_state_writes: &BTreeMap<BlockId, Vec<usize>>,
    memory_phi_factorings: &BTreeMap<BlockId, &MemoryPhiFactoring>,
) -> Option<(SnapshotAccess, SnapshotAccess)> {
    first_snapshot_difference_with_cache(
        expected,
        actual,
        inserted_state_writes,
        memory_phi_factorings,
        &mut HashSet::default(),
    )
}

pub(super) fn first_snapshot_difference_with_cache(
    expected: &MemorySnapshot,
    actual: &MemorySnapshot,
    inserted_state_writes: &BTreeMap<BlockId, Vec<usize>>,
    memory_phi_factorings: &BTreeMap<BlockId, &MemoryPhiFactoring>,
    equal: &mut HashSet<SnapshotPair>,
) -> Option<(SnapshotAccess, SnapshotAccess)> {
    let expected_span = expected.span();
    if expected_span != actual.span() {
        return Some((
            expected.root_at(expected_span?.0)?,
            actual
                .root_at(actual.span()?.0)
                .unwrap_or(SnapshotAccess::LiveOnEntry),
        ));
    }
    let mut pending = Vec::new();
    for expected_segment in expected.root.iter() {
        for actual_segment in actual.root.iter() {
            let start = expected_segment.start.max(actual_segment.start);
            let end = expected_segment.end.min(actual_segment.end);
            if start < end {
                pending.push((expected_segment.access, actual_segment.access, start, end));
            }
        }
    }
    let mut visited = HashSet::default();
    while let Some((expected_access, actual_access, start, end)) = pending.pop() {
        let pair = (expected_access, actual_access, start, end);
        if equal.contains(&pair) || !visited.insert(pair) {
            continue;
        }
        match (expected_access, actual_access) {
            (SnapshotAccess::LiveOnEntry, SnapshotAccess::LiveOnEntry) => {}
            (
                SnapshotAccess::Write {
                    block: expected_block,
                    ordinal: expected_ordinal,
                },
                SnapshotAccess::Write {
                    block: actual_block,
                    ordinal: actual_ordinal,
                },
            ) if expected_block == actual_block => {
                let inserted = inserted_state_writes
                    .get(&actual_block)
                    .map(Vec::as_slice)
                    .unwrap_or_default();
                let matches = match inserted.binary_search(&actual_ordinal) {
                    Ok(_) => false,
                    Err(insertions_before) => actual_ordinal
                        .checked_sub(insertions_before)
                        .is_some_and(|stable_ordinal| stable_ordinal == expected_ordinal),
                };
                if !matches {
                    return Some((expected_access, actual_access));
                }
            }
            (SnapshotAccess::Phi(expected_block), SnapshotAccess::Phi(actual_block))
                if expected_block == actual_block
                    || memory_phi_factorings
                        .get(&actual_block)
                        .is_some_and(|factoring| factoring.successor == expected_block) =>
            {
                let mut covered = start;
                for (run_start, run_end, expected_inputs) in
                    expected.phi_runs(expected_block, start, end)
                {
                    if run_start != covered {
                        return Some((expected_access, actual_access));
                    }
                    covered = run_end;
                    let mut actual_covered = run_start;
                    for (piece_start, piece_end, actual_inputs) in
                        actual.phi_runs(actual_block, run_start, run_end)
                    {
                        if piece_start != actual_covered {
                            return Some((expected_access, actual_access));
                        }
                        actual_covered = piece_end;
                        // Factoring all non-self inputs of a loop phi can
                        // make that phi trivial: its remaining inputs carry
                        // itself, and the rebuilt snapshot names the new phi.
                        // Recreate only those proven unchanged self edges;
                        // every factored incoming version is still compared.
                        let restored_inputs;
                        let actual_inputs = if expected_block != actual_block {
                            let factoring = memory_phi_factorings[&actual_block];
                            let mut inputs = vec![(actual_block, actual_access)];
                            for &(predecessor, input) in expected_inputs {
                                if factoring.predecessors.contains(&predecessor) {
                                    continue;
                                }
                                if input != expected_access {
                                    return Some((expected_access, actual_access));
                                }
                                inputs.push((predecessor, actual_access));
                            }
                            inputs.sort_unstable_by_key(|&(predecessor, _)| predecessor);
                            restored_inputs = inputs;
                            restored_inputs.as_slice()
                        } else {
                            actual_inputs
                        };
                        let mut active = BTreeSet::new();
                        let Some(expansions) = expand_factored_snapshot_inputs(
                            actual,
                            expected_block,
                            actual_inputs,
                            piece_start,
                            piece_end,
                            memory_phi_factorings,
                            &mut active,
                        ) else {
                            return Some((expected_access, actual_access));
                        };
                        for (expansion_start, expansion_end, actual_inputs) in expansions {
                            if expected_inputs.len() != actual_inputs.len() {
                                return Some((expected_access, actual_access));
                            }
                            for (
                                &(expected_predecessor, expected_input),
                                &(actual_predecessor, actual_input),
                            ) in expected_inputs.iter().zip(actual_inputs.iter())
                            {
                                if expected_predecessor != actual_predecessor {
                                    return Some((expected_access, actual_access));
                                }
                                pending.push((
                                    expected_input,
                                    actual_input,
                                    expansion_start,
                                    expansion_end,
                                ));
                            }
                        }
                    }
                    if actual_covered != run_end {
                        return Some((expected_access, actual_access));
                    }
                }
                if covered != end {
                    return Some((expected_access, actual_access));
                }
            }
            _ => return Some((expected_access, actual_access)),
        }
    }
    // A cycle is provisionally accepted only within this walk. Publish no
    // new proof until all reachable inputs have passed, including exits from
    // cycles; a failed walk must not leave behind assumed equal subgraphs.
    equal.extend(visited);
    None
}

/// Input tuples of the MemoryPhi of `parent` over `start..end` with every
/// MemoryPhi backed by a validated reconstruction factoring inlined, as
/// `(start, end, inputs)` runs sorted by predecessor.  Ordinary MemoryPhis
/// remain structural proof nodes, so changing an original incoming write or
/// swapping two original paths is still rejected.  `active` holds the
/// factored blocks on the current inlining path and rejects cycles.
pub(super) fn expand_factored_snapshot_inputs(
    actual: &MemorySnapshot,
    parent: BlockId,
    inputs: &[(BlockId, SnapshotAccess)],
    start: i64,
    end: i64,
    factorings: &BTreeMap<BlockId, &MemoryPhiFactoring>,
    active: &mut BTreeSet<BlockId>,
) -> Option<Vec<(i64, i64, Vec<(BlockId, SnapshotAccess)>)>> {
    let mut partials = vec![(start, end, Vec::new())];
    for &(predecessor, access) in inputs {
        let Some(factoring) = factorings.get(&predecessor) else {
            for (_, _, partial) in &mut partials {
                partial.push((predecessor, access));
            }
            continue;
        };
        if factoring.successor != parent || !active.insert(predecessor) {
            return None;
        }
        let mut next = Vec::new();
        for (partial_start, partial_end, partial) in partials {
            let expansions = match access {
                SnapshotAccess::Phi(phi_block) if phi_block == predecessor => {
                    let mut expansions = Vec::new();
                    let mut covered = partial_start;
                    for (run_start, run_end, phi_inputs) in
                        actual.phi_runs(phi_block, partial_start, partial_end)
                    {
                        if run_start != covered
                            || phi_inputs
                                .iter()
                                .map(|&(input_predecessor, _)| input_predecessor)
                                .ne(factoring.predecessors.iter().copied())
                        {
                            return None;
                        }
                        covered = run_end;
                        expansions.extend(expand_factored_snapshot_inputs(
                            actual,
                            predecessor,
                            phi_inputs,
                            run_start,
                            run_end,
                            factorings,
                            active,
                        )?);
                    }
                    if covered != partial_end {
                        return None;
                    }
                    expansions
                }
                // A byte whose incoming versions all agree has no phi at the
                // factored block.  The validated write-free factoring still
                // proves that the shared edge block represents every recorded
                // predecessor.  The carried access may itself be an existing
                // MemoryPhi from before the factoring; replicate that version
                // over the original edges instead of mistaking it for a
                // folded synthetic phi.
                access => {
                    let replicated = factoring
                        .predecessors
                        .iter()
                        .map(|&predecessor| (predecessor, access))
                        .collect::<Vec<_>>();
                    expand_factored_snapshot_inputs(
                        actual,
                        predecessor,
                        &replicated,
                        partial_start,
                        partial_end,
                        factorings,
                        active,
                    )?
                }
            };
            for (expansion_start, expansion_end, expansion) in expansions {
                let mut combined = partial.clone();
                combined.extend(expansion);
                next.push((expansion_start, expansion_end, combined));
            }
        }
        active.remove(&predecessor);
        partials = next;
    }
    for (_, _, partial) in &mut partials {
        partial.sort_unstable_by_key(|&(predecessor, access)| (predecessor, access));
    }
    Some(partials)
}

pub(super) fn describe_expected_snapshot_write(
    func: &MFunction,
    access: SnapshotAccess,
    inserted_state_writes: &BTreeMap<BlockId, Vec<usize>>,
) -> String {
    let SnapshotAccess::Write { block, ordinal } = access else {
        return format!("{access:?}");
    };
    let mut final_ordinal = ordinal;
    for &inserted in inserted_state_writes
        .get(&block)
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        if inserted > final_ordinal {
            break;
        }
        let Some(next) = final_ordinal.checked_add(1) else {
            return format!("{access:?} (final ordinal overflow)");
        };
        final_ordinal = next;
    }
    describe_snapshot_write(
        func,
        SnapshotAccess::Write {
            block,
            ordinal: final_ordinal,
        },
    )
}

pub(super) fn describe_snapshot_write(func: &MFunction, access: SnapshotAccess) -> String {
    let SnapshotAccess::Write { block, ordinal } = access else {
        return format!("{access:?}");
    };
    let Some(owner) = func.blocks.iter().find(|owner| owner.id == block) else {
        return format!("{access:?} (block absent from final MIR)");
    };
    let located = owner
        .insts
        .iter()
        .enumerate()
        .filter(|(_, inst)| {
            let effect = memory_effect::writes(inst);
            matches!(
                effect.unknown_memory(),
                Some(memory_effect::UnknownMemory::Direct(BaseReg::SimState))
            ) || effect.ranges().any(|range| range.base == BaseReg::SimState)
        })
        .nth(ordinal);
    located.map_or_else(
        || format!("{access:?} (ordinal absent from final MIR)"),
        |(instruction, inst)| format!("{block}/i{instruction} {inst:?}"),
    )
}
