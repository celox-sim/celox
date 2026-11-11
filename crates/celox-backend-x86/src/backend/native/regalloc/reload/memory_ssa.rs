//! Memory SSA of the state a function reads and writes.

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RecipeBase {
    Constant,
    State(VReg),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct MemoryDefinition {
    pub(super) block: BlockId,
    pub(super) ordinal: usize,
}

/// Reload-specific adapter around the per-byte memory versions.  MIR effects
/// and stable write identities stay here; neither is part of
/// `celox-analysis`.
#[derive(Debug)]
pub(super) struct ReloadMemorySsa {
    pub(super) versions: MemoryVersions<MemoryDefinition>,
    pub(super) block_ids: Vec<BlockId>,
    pub(super) phis: Rc<PhiTable>,
    /// Interned certificates keyed by their root segments.
    pub(super) snapshots: HashMap<Box<[SnapshotSegment]>, MemorySnapshot>,
    pub(super) segments: Vec<VersionSegment<MemoryDefinition>>,
}

impl ReloadMemorySsa {
    pub(super) fn snapshot_at_block_entry(
        &mut self,
        block: usize,
        load: StateLoad,
    ) -> Result<MemorySnapshot, ReloadRecipeError> {
        let version = self.versions.block_entry(block).ok_or_else(|| {
            ReloadRecipeError::new(
                "RELOAD_RECIPE.MEMORY_BLOCK_ENTRY",
                self.block_ids.get(block).copied(),
                None,
                None,
                "MemorySSA has no block-entry coordinate",
            )
        })?;
        self.snapshot_at(version, load)
    }

    /// Certificate of the bytes of `load` in memory state `at`.
    pub(super) fn snapshot_at(
        &mut self,
        at: VersionId,
        load: StateLoad,
    ) -> Result<MemorySnapshot, ReloadRecipeError> {
        let bytes = load.bytes().ok_or_else(|| {
            ReloadRecipeError::new(
                "RELOAD_RECIPE.STATE_RANGE",
                None,
                None,
                None,
                "state load byte range overflows i64",
            )
        })?;
        self.segments.clear();
        self.versions
            .segments(at, bytes.start, bytes.end, &mut self.segments)
            .ok_or_else(|| {
                ReloadRecipeError::new(
                    "RELOAD_RECIPE.CLOBBER_ACCESS",
                    None,
                    None,
                    None,
                    "snapshot query starts outside the MemorySSA graph",
                )
            })?;
        let mut root = Vec::with_capacity(self.segments.len());
        for segment in &self.segments {
            root.push(SnapshotSegment {
                start: segment.start,
                end: segment.end,
                access: Self::stable_access(&self.block_ids, segment.version)?,
            });
        }
        if let Some(snapshot) = self.snapshots.get(root.as_slice()) {
            return Ok(snapshot.clone());
        }
        let snapshot = MemorySnapshot {
            root: root.clone().into(),
            phis: Rc::clone(&self.phis),
        };
        self.snapshots
            .insert(root.into_boxed_slice(), snapshot.clone());
        Ok(snapshot)
    }

    /// Every per-byte phi equation of the function, as pieces with one input
    /// tuple each.
    pub(super) fn build_phi_table(
        versions: &MemoryVersions<MemoryDefinition>,
        block_ids: &[BlockId],
    ) -> Result<PhiTable, ReloadRecipeError> {
        let mut pieces = Vec::new();
        let mut per_predecessor =
            Vec::<(BlockId, usize, Vec<VersionSegment<MemoryDefinition>>)>::new();
        for (block, &block_id) in block_ids.iter().enumerate() {
            let ranges = versions.phi_ranges(block);
            if ranges.is_empty() {
                continue;
            }
            per_predecessor.clear();
            for &predecessor in versions.predecessors(block) {
                let predecessor_id = block_ids.get(predecessor).copied().ok_or_else(|| {
                    ReloadRecipeError::new(
                        "RELOAD_RECIPE.SNAPSHOT_PREDECESSOR",
                        Some(block_id),
                        None,
                        None,
                        "MemoryPhi predecessor is outside the MIR block table",
                    )
                })?;
                per_predecessor.push((predecessor_id, predecessor, Vec::new()));
            }
            per_predecessor.sort_by_key(|(predecessor, _, _)| *predecessor);
            for (start, end) in ranges {
                for (predecessor_id, predecessor, segments) in &mut per_predecessor {
                    let exit = versions.block_exit(*predecessor).ok_or_else(|| {
                        ReloadRecipeError::new(
                            "RELOAD_RECIPE.MEMORY_BLOCK_EXIT",
                            Some(*predecessor_id),
                            None,
                            None,
                            "MemorySSA has no block-exit coordinate",
                        )
                    })?;
                    segments.clear();
                    versions
                        .segments(exit, start, end, segments)
                        .ok_or_else(|| {
                            ReloadRecipeError::new(
                                "RELOAD_RECIPE.SNAPSHOT_INPUT",
                                Some(block_id),
                                None,
                                None,
                                "MemoryPhi input state is outside the MemorySSA graph",
                            )
                        })?;
                }
                // Sweep the common refinement of every predecessor's runs and
                // merge adjacent pieces with equal input tuples.
                let mut cursors = vec![0usize; per_predecessor.len()];
                let mut position = start;
                while position < end {
                    let mut next = end;
                    let mut tuple = Vec::with_capacity(per_predecessor.len());
                    for (index, (predecessor, _, segments)) in per_predecessor.iter().enumerate() {
                        while segments
                            .get(cursors[index])
                            .is_some_and(|segment| segment.end <= position)
                        {
                            cursors[index] += 1;
                        }
                        let segment = segments.get(cursors[index]).ok_or_else(|| {
                            ReloadRecipeError::new(
                                "RELOAD_RECIPE.SNAPSHOT_INPUT",
                                Some(block_id),
                                None,
                                None,
                                "MemoryPhi input does not cover the phi's bytes",
                            )
                        })?;
                        next = next.min(segment.end);
                        tuple.push((
                            *predecessor,
                            Self::stable_access(block_ids, segment.version)?,
                        ));
                    }
                    match pieces.last_mut() {
                        Some(SnapshotPhi {
                            block: last_block,
                            end: last_end,
                            inputs: last_inputs,
                            ..
                        }) if *last_block == block_id
                            && *last_end == position
                            && **last_inputs == *tuple =>
                        {
                            *last_end = next;
                        }
                        _ => pieces.push(SnapshotPhi {
                            block: block_id,
                            start: position,
                            end: next,
                            inputs: tuple.into_boxed_slice(),
                        }),
                    }
                    position = next;
                }
            }
        }
        Ok(PhiTable::from_pieces(pieces))
    }

    pub(super) fn stable_access(
        block_ids: &[BlockId],
        version: MemoryVersion<MemoryDefinition>,
    ) -> Result<SnapshotAccess, ReloadRecipeError> {
        match version {
            MemoryVersion::LiveOnEntry => Ok(SnapshotAccess::LiveOnEntry),
            MemoryVersion::Definition(definition) => Ok(SnapshotAccess::Write {
                block: definition.block,
                ordinal: definition.ordinal,
            }),
            MemoryVersion::Phi { block } => block_ids
                .get(block)
                .copied()
                .map(SnapshotAccess::Phi)
                .ok_or_else(|| {
                    ReloadRecipeError::new(
                        "RELOAD_RECIPE.SNAPSHOT_BLOCK",
                        None,
                        None,
                        None,
                        "MemoryPhi block is outside the MIR block table",
                    )
                }),
        }
    }
}

/// Insert `start..end` into a set of disjoint sorted byte ranges, appending
/// the parts that were not yet covered to `added`.
pub(super) fn insert_byte_range(
    set: &mut BTreeMap<i64, i64>,
    start: i64,
    end: i64,
    added: &mut Vec<(i64, i64)>,
) {
    let mut overlapping = set
        .range(..=end)
        .rev()
        .take_while(|&(_, &existing_end)| existing_end >= start)
        .map(|(&existing_start, &existing_end)| (existing_start, existing_end))
        .collect::<Vec<_>>();
    overlapping.reverse();
    let mut cursor = start;
    let mut new_start = start;
    let mut new_end = end;
    for (existing_start, existing_end) in overlapping {
        if existing_start > cursor {
            added.push((cursor, existing_start));
        }
        cursor = cursor.max(existing_end);
        new_start = new_start.min(existing_start);
        new_end = new_end.max(existing_end);
        set.remove(&existing_start);
    }
    if cursor < end {
        added.push((cursor, end));
    }
    set.insert(new_start, new_end);
}

pub(super) fn build_reload_memory_ssa(
    func: &MFunction,
    cfg: &NormalizedCfg,
) -> Result<ReloadMemorySsa, ReloadRecipeError> {
    let mut events = vec![Vec::<MemoryVersionEvent<MemoryDefinition>>::new(); func.blocks.len()];
    let mut writes = Vec::<RangeWrite>::new();
    let sim_state = MemoryEffect::UnknownObject(MemoryObject::SimState);

    for (block, mir_block) in func.blocks.iter().enumerate() {
        let mut write_ordinal = 0usize;
        for (instruction, inst) in mir_block.insts.iter().enumerate() {
            let write_start = writes.len();
            for effect in analysis_effects(&memory_effect::writes(inst))
                .filter(|effect| effects_may_alias(*effect, sim_state))
            {
                let write = match effect {
                    MemoryEffect::Exact(location) => {
                        let end = location
                            .end()
                            .filter(|_| location.byte_len != 0)
                            .ok_or_else(|| {
                                ReloadRecipeError::new(
                                    "RELOAD_RECIPE.MEMORY_RANGE",
                                    Some(mir_block.id),
                                    Some(instruction),
                                    None,
                                    "MIR SimState write has an empty or overflowing range",
                                )
                            })?;
                        RangeWrite::Exact {
                            start: location.offset,
                            end,
                        }
                    }
                    MemoryEffect::UnknownObject(_) | MemoryEffect::UnknownAll => RangeWrite::All,
                };
                writes.push(write);
            }
            let definition = if writes.len() == write_start {
                None
            } else {
                let ordinal = write_ordinal;
                write_ordinal = write_ordinal.checked_add(1).ok_or_else(|| {
                    ReloadRecipeError::new(
                        "RELOAD_RECIPE.WRITE_ORDINAL_RANGE",
                        Some(mir_block.id),
                        Some(instruction),
                        None,
                        "per-block MemorySSA write ordinal exceeds addressable MIR size",
                    )
                })?;
                Some(MemoryDefinition {
                    block: mir_block.id,
                    ordinal,
                })
            };
            events[block].push(MemoryVersionEvent {
                definition,
                write_start,
                write_end: writes.len(),
            });
        }
    }

    let versions = memory_versions::build(cfg, &events, &writes).map_err(|error| {
        ReloadRecipeError::new(
            error.rule,
            error
                .block
                .and_then(|block| func.blocks.get(block).map(|block| block.id)),
            None,
            None,
            error.message,
        )
    })?;
    let block_ids = func.blocks.iter().map(|block| block.id).collect::<Vec<_>>();
    let phis = ReloadMemorySsa::build_phi_table(&versions, &block_ids)?;
    Ok(ReloadMemorySsa {
        versions,
        block_ids,
        phis: Rc::new(phis),
        snapshots: HashMap::default(),
        segments: Vec::new(),
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn rename_memory_ssa(
    func: &MFunction,
    cfg: &NormalizedCfg,
    state_loads: &[Option<StateLoad>],
    pure_recipes: &[PureRecipe],
    recipes: &mut [ReloadRecipe],
    memory_ssa: &mut ReloadMemorySsa,
    store_homes: &HashMap<(usize, usize), Vec<StoreHomeSpec>>,
    fragment_homes: &HashMap<(usize, usize), ValidatedStateFragment>,
    preserving_writes: &HashMap<(usize, usize), ValidatedStateFragment>,
    canonical_bits: &[u8],
    collect_fragment_homes: bool,
    relevant_values: &BTreeSet<VReg>,
    requested_points: &BTreeSet<PointUse>,
    collect_all_uses: bool,
    point_recipes: &mut BTreeMap<PointUse, ResolvedRecipe>,
    edge_recipes: &mut BTreeMap<EdgeUse, ResolvedRecipe>,
    point_fragment_recipes: &mut BTreeMap<PointUse, CompositeStateRecipe>,
    edge_fragment_recipes: &mut BTreeMap<EdgeUse, CompositeStateRecipe>,
    valid_point_uses: &mut BTreeSet<PointUse>,
    valid_edge_uses: &mut BTreeSet<EdgeUse>,
) -> Result<(), ReloadRecipeError> {
    let mut children = vec![Vec::<usize>::new(); func.blocks.len()];
    for block in 1..func.blocks.len() {
        let Some(parent) = cfg.idom[block] else {
            return Err(ReloadRecipeError::new(
                "RELOAD_RECIPE.DOMINATOR_TREE",
                Some(func.blocks[block].id),
                None,
                None,
                "reachable non-entry block has no immediate dominator",
            ));
        };
        children[parent].push(block);
    }

    enum Action {
        Enter(usize),
        Exit {
            home_pushes: Vec<VReg>,
            fragment_pushes: Vec<VReg>,
        },
    }

    let mut current_homes = BTreeMap::<VReg, Vec<StoreHome>>::new();
    let mut current_home_index = StoreHomeIndex::new();
    let mut current_fragments = BTreeMap::<VReg, Vec<StateFragmentHome>>::new();
    let mut current_fragment_index = FragmentHomeIndex::new();
    let requested_by_location = requested_points.iter().fold(
        BTreeMap::<(BlockId, usize), Vec<VReg>>::new(),
        |mut locations, point| {
            locations
                .entry((point.block, point.instruction))
                .or_default()
                .push(point.value);
            locations
        },
    );
    let mut actions = vec![Action::Enter(0)];
    while let Some(action) = actions.pop() {
        let block = match action {
            Action::Exit {
                home_pushes,
                fragment_pushes,
            } => {
                for value in fragment_pushes.into_iter().rev() {
                    let Some(fragments) = current_fragments.get_mut(&value) else {
                        return Err(ReloadRecipeError::new(
                            "RELOAD_RECIPE.FRAGMENT_HOME_SCOPE",
                            None,
                            None,
                            Some(value),
                            "state fragment disappeared before dominator exit",
                        ));
                    };
                    let fragment = fragments
                        .pop()
                        .expect("dominator-scoped fragment push has a matching pop");
                    unindex_fragment_home(&mut current_fragment_index, value, &fragment);
                    if fragments.is_empty() {
                        current_fragments.remove(&value);
                    }
                }
                for value in home_pushes.into_iter().rev() {
                    let Some(homes) = current_homes.get_mut(&value) else {
                        return Err(ReloadRecipeError::new(
                            "RELOAD_RECIPE.STORE_HOME_SCOPE",
                            None,
                            None,
                            Some(value),
                            "store-backed recipe disappeared before dominator exit",
                        ));
                    };
                    let home = homes
                        .pop()
                        .expect("dominator-scoped store-home push has a matching pop");
                    unindex_store_home(&mut current_home_index, value, &home);
                    if homes.is_empty() {
                        current_homes.remove(&value);
                    }
                }
                continue;
            }
            Action::Enter(block) => block,
        };
        let mut home_pushes = Vec::new();
        let mut fragment_pushes = Vec::new();

        let block_id = func.blocks[block].id;
        // A register phi that merges values already committed to the same
        // exact SimState expression has the MemorySSA phi for that slot as a
        // home. Identity copies introduced during reconstruction do not
        // change the shape.
        // Derive this only after every forward predecessor edge has supplied
        // an independently validated recipe.  Missing backedge facts keep a
        // loop phi on its stack fallback; they are never guessed.
        for phi in &func.blocks[block].phis {
            if !relevant_values.contains(&phi.dst) {
                continue;
            }
            let mut common_load = None::<StateLoad>;
            let mut common_observed_bits = None::<StateBitRange>;
            let mut common_steps = None::<Vec<PureStep>>;
            let mut complete = !phi.sources.is_empty();
            for &(predecessor, source) in &phi.sources {
                let edge = EdgeUse {
                    predecessor,
                    successor: block_id,
                    value: source,
                };
                let Some(incoming) = edge_recipes.get(&edge) else {
                    complete = false;
                    break;
                };
                let ResolvedRecipe {
                    base: ResolvedBase::State(state),
                    steps,
                } = incoming
                else {
                    complete = false;
                    break;
                };
                let steps = steps
                    .iter()
                    .copied()
                    .filter(|step| !matches!(step, PureStep::Copy64))
                    .collect::<Vec<_>>();
                match common_load {
                    Some(load) if load != state.load => {
                        complete = false;
                        break;
                    }
                    Some(_) => {}
                    None => common_load = Some(state.load),
                }
                match common_observed_bits {
                    Some(bits) if bits != state.observed_bits => {
                        complete = false;
                        break;
                    }
                    Some(_) => {}
                    None => common_observed_bits = Some(state.observed_bits),
                }
                match &common_steps {
                    Some(common) if *common != steps => {
                        complete = false;
                        break;
                    }
                    Some(_) => {}
                    None => common_steps = Some(steps),
                }
            }
            if complete {
                let Some(load) = common_load else {
                    continue;
                };
                let Some(observed_bits) = common_observed_bits else {
                    continue;
                };
                let home = StoreHome {
                    state: StateRecipe {
                        load,
                        snapshot: memory_ssa.snapshot_at_block_entry(block, load)?,
                        observed_bits,
                    },
                    steps: common_steps.unwrap_or_default(),
                };
                index_store_home(&mut current_home_index, phi.dst, &home);
                current_homes.entry(phi.dst).or_default().push(home);
                home_pushes.push(phi.dst);
            }
        }

        for (instruction, inst) in func.blocks[block].insts.iter().enumerate() {
            let (point_before, point_after) = memory_ssa
                .versions
                .event(block, instruction)
                .ok_or_else(|| {
                    ReloadRecipeError::new(
                        "RELOAD_RECIPE.MEMORY_POINT",
                        Some(block_id),
                        Some(instruction),
                        None,
                        "MemorySSA does not cover this MIR instruction",
                    )
                })?;
            if collect_all_uses {
                for value in inst.uses().into_iter().collect::<BTreeSet<_>>() {
                    let point = PointUse {
                        block: block_id,
                        instruction,
                        value,
                    };
                    if let Some(recipe) = available_recipe(
                        value,
                        recipes,
                        pure_recipes,
                        &current_homes,
                        point_before,
                        memory_ssa,
                    )? {
                        point_recipes.insert(point, recipe);
                        valid_point_uses.insert(point);
                    }
                    if collect_fragment_homes
                        && let Some(recipe) = available_fragment_recipe(
                            value,
                            canonical_bits,
                            &current_fragments,
                            point_before,
                            memory_ssa,
                        )?
                    {
                        point_fragment_recipes.insert(point, recipe);
                    }
                }
            }
            if let Some(values) = requested_by_location.get(&(block_id, instruction)) {
                for &value in values {
                    let point = PointUse {
                        block: block_id,
                        instruction,
                        value,
                    };
                    if let Some(recipe) = available_recipe(
                        value,
                        recipes,
                        pure_recipes,
                        &current_homes,
                        point_before,
                        memory_ssa,
                    )? {
                        point_recipes.insert(point, recipe);
                    }
                    if collect_fragment_homes
                        && let Some(recipe) = available_fragment_recipe(
                            value,
                            canonical_bits,
                            &current_fragments,
                            point_before,
                            memory_ssa,
                        )?
                    {
                        point_fragment_recipes.insert(point, recipe);
                    }
                }
            }

            if let Some(definition) = inst.def()
                && relevant_values.contains(&definition)
                && let Some(load) = state_loads.get(definition.0 as usize).copied().flatten()
            {
                recipes[definition.0 as usize] = ReloadRecipe::StateVersion(StateRecipe {
                    load,
                    snapshot: memory_ssa.snapshot_at(point_before, load)?,
                    observed_bits: StateBitRange::from_load(load).ok_or_else(|| {
                        ReloadRecipeError::new(
                            "RELOAD_RECIPE.STATE_RANGE",
                            Some(block_id),
                            Some(instruction),
                            Some(definition),
                            "state load bit range overflows i64",
                        )
                    })?,
                });
            }

            let mut preserved_homes = Vec::<(VReg, StoreHome)>::new();
            let mut preserved_fragments = Vec::<(VReg, StateFragmentHome)>::new();
            if let Some(insert) = preserving_writes.get(&(block, instruction)).copied() {
                let written_bits = insert.observed_bits;
                let mut candidates = BTreeSet::<VReg>::new();
                for byte in insert
                    .load
                    .bytes()
                    .expect("validated preserving write has a finite byte range")
                {
                    if let Some(values) = current_home_index.get(&byte) {
                        candidates.extend(values.keys().copied());
                    }
                }
                for value in candidates {
                    for home in &current_homes[&value] {
                        if !home.state.observed_bits.overlaps(written_bits)
                            && memory_ssa.snapshot_at(point_before, home.state.load)?
                                == home.state.snapshot
                        {
                            preserved_homes.push((value, home.clone()));
                        }
                    }
                }
                let mut fragment_candidates = BTreeSet::<VReg>::new();
                for byte in insert
                    .load
                    .bytes()
                    .expect("validated preserving write has a finite byte range")
                {
                    if let Some(values) = current_fragment_index.get(&byte) {
                        fragment_candidates.extend(values.keys().copied());
                    }
                }
                for value in fragment_candidates {
                    for fragment in &current_fragments[&value] {
                        if !fragment.state.observed_bits.overlaps(written_bits)
                            && memory_ssa.snapshot_at(point_before, fragment.state.load)?
                                == fragment.state.snapshot
                        {
                            preserved_fragments.push((value, fragment.clone()));
                        }
                    }
                }
            }

            for (value, mut home) in preserved_homes {
                let snapshot = memory_ssa.snapshot_at(point_after, home.state.load)?;
                if snapshot == home.state.snapshot {
                    continue;
                }
                home.state.snapshot = snapshot;
                index_store_home(&mut current_home_index, value, &home);
                current_homes.entry(value).or_default().push(home);
                home_pushes.push(value);
            }

            for (value, mut fragment) in preserved_fragments {
                let snapshot = memory_ssa.snapshot_at(point_after, fragment.state.load)?;
                if snapshot == fragment.state.snapshot {
                    continue;
                }
                fragment.state.snapshot = snapshot;
                index_fragment_home(&mut current_fragment_index, value, &fragment);
                current_fragments.entry(value).or_default().push(fragment);
                fragment_pushes.push(value);
            }

            if let Some(homes) = store_homes.get(&(block, instruction)) {
                for home in homes {
                    let stored = StoreHome {
                        state: StateRecipe {
                            load: home.load,
                            snapshot: memory_ssa.snapshot_at(point_after, home.load)?,
                            observed_bits: home.observed_bits,
                        },
                        steps: home.steps.clone(),
                    };
                    index_store_home(&mut current_home_index, home.value, &stored);
                    current_homes.entry(home.value).or_default().push(stored);
                    home_pushes.push(home.value);
                }
            }
            if let Some(fragment) = fragment_homes.get(&(block, instruction)).copied() {
                let stored = StateFragmentHome {
                    state: StateRecipe {
                        load: fragment.load,
                        snapshot: memory_ssa.snapshot_at(point_after, fragment.load)?,
                        observed_bits: fragment.observed_bits,
                    },
                    value_bit_offset: fragment.value_bit_offset,
                    state_bit_offset: fragment.bit_offset,
                    width_bits: fragment.width_bits,
                };
                index_fragment_home(&mut current_fragment_index, fragment.value, &stored);
                current_fragments
                    .entry(fragment.value)
                    .or_default()
                    .push(stored);
                fragment_pushes.push(fragment.value);
            }
        }

        let block_exit = memory_ssa.versions.block_exit(block).ok_or_else(|| {
            ReloadRecipeError::new(
                "RELOAD_RECIPE.MEMORY_BLOCK_EXIT",
                Some(block_id),
                None,
                None,
                "MemorySSA has no block-exit coordinate",
            )
        })?;
        for &successor in &cfg.successors[block] {
            let successor_id = func.blocks[successor].id;
            for phi in &func.blocks[successor].phis {
                if !relevant_values.contains(&phi.dst) {
                    continue;
                }
                let Some((_, value)) = phi
                    .sources
                    .iter()
                    .find(|(predecessor, _)| *predecessor == block_id)
                else {
                    continue;
                };
                let edge = EdgeUse {
                    predecessor: block_id,
                    successor: successor_id,
                    value: *value,
                };
                if let Some(recipe) = available_recipe(
                    *value,
                    recipes,
                    pure_recipes,
                    &current_homes,
                    block_exit,
                    memory_ssa,
                )? {
                    edge_recipes.insert(edge, recipe);
                    valid_edge_uses.insert(edge);
                }
                if collect_fragment_homes
                    && let Some(recipe) = available_fragment_recipe(
                        *value,
                        canonical_bits,
                        &current_fragments,
                        block_exit,
                        memory_ssa,
                    )?
                {
                    edge_fragment_recipes.insert(edge, recipe);
                }
            }
        }

        actions.push(Action::Exit {
            home_pushes,
            fragment_pushes,
        });
        actions.extend(children[block].iter().rev().copied().map(Action::Enter));
    }
    Ok(())
}
