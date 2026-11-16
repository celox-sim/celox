//! Deriving the recipe of each value from its definition.

use super::*;

pub(super) fn analyze_unverified_with_queries(
    func: &MFunction,
    cfg: &NormalizedCfg,
    requested_points: &BTreeSet<PointUse>,
    collect_all_uses: bool,
    collect_fragment_homes: bool,
) -> Result<ReloadRecipeAnalysis, ReloadRecipeError> {
    if func.blocks.len() != cfg.predecessors.len()
        || func.blocks.len() != cfg.successors.len()
        || func.blocks.len() != cfg.idom.len()
    {
        return Err(ReloadRecipeError::new(
            "RELOAD_RECIPE.MODEL_SHAPE",
            None,
            None,
            None,
            "CFG tables do not cover every MIR block",
        ));
    }

    let canonical_bits = canonical_value_bits(func)?;
    let mut state_loads = vec![None; func.vregs.count() as usize];
    let mut recipes = vec![ReloadRecipe::Stack; func.vregs.count() as usize];
    let mut pure_recipes = Vec::<PureRecipe>::new();
    for block in &func.blocks {
        for (instruction, inst) in block.insts.iter().enumerate() {
            let Some(definition) = inst.def() else {
                continue;
            };
            let Some(slot) = recipes.get_mut(definition.0 as usize) else {
                return Err(ReloadRecipeError::new(
                    "RELOAD_RECIPE.VALUE_RANGE",
                    Some(block.id),
                    Some(instruction),
                    Some(definition),
                    "MIR definition is outside the VReg recipe side table",
                ));
            };
            match inst {
                MInst::LoadImm { value, .. } => {
                    *slot = ReloadRecipe::Constant { value: *value };
                }
                MInst::Load {
                    base: BaseReg::SimState,
                    offset,
                    size,
                    ..
                } => {
                    let load = StateLoad {
                        offset: *offset,
                        size: *size,
                    };
                    if load.bytes().is_none() {
                        return Err(ReloadRecipeError::new(
                            "RELOAD_RECIPE.STATE_RANGE",
                            Some(block.id),
                            Some(instruction),
                            Some(definition),
                            "state load byte range overflows i64",
                        ));
                    }
                    state_loads[definition.0 as usize] = Some(load);
                }
                _ => {
                    if let Some(expression) = pure_expression(inst) {
                        let id = u32::try_from(pure_recipes.len()).map_err(|_| {
                            ReloadRecipeError::new(
                                "RELOAD_RECIPE.PURE_ID_RANGE",
                                Some(block.id),
                                Some(instruction),
                                Some(definition),
                                "pure-expression recipe count exceeds u32",
                            )
                        })?;
                        pure_recipes.push(expression);
                        *slot = ReloadRecipe::Pure {
                            expression: PureRecipeId(id),
                        };
                    }
                }
            }
        }
    }

    let _recipe_bases = resolve_recipe_bases(&recipes, &pure_recipes, &state_loads)?;
    let relevant_values = relevant_recipe_values(
        func,
        &recipes,
        &pure_recipes,
        requested_points,
        collect_all_uses,
    )?;
    let mut store_homes = HashMap::<(usize, usize), Vec<StoreHomeSpec>>::default();
    let mut preserving_writes = HashMap::<(usize, usize), ValidatedStateFragment>::default();
    let mut fragment_homes = HashMap::<(usize, usize), ValidatedStateFragment>::default();
    for (block, mir_block) in func.blocks.iter().enumerate() {
        for (instruction, inst) in mir_block.insts.iter().enumerate() {
            if let Some(insert) = validated_state_fragment(func, inst) {
                if insert.complete_value || collect_fragment_homes {
                    preserving_writes.insert((block, instruction), insert);
                }
                if collect_fragment_homes && relevant_values.contains(&insert.value) {
                    fragment_homes.insert((block, instruction), insert);
                }
            }
            let homes = store_home_specs(func, inst, &canonical_bits)
                .into_iter()
                .filter(|home| relevant_values.contains(&home.value))
                .collect::<Vec<_>>();
            if !homes.is_empty() {
                store_homes.insert((block, instruction), homes);
            }
        }
    }
    let mut memory_ssa = build_reload_memory_ssa(func, cfg)?;
    let mut point_recipes = BTreeMap::new();
    let mut edge_recipes = BTreeMap::new();
    let mut point_fragment_recipes = BTreeMap::new();
    let mut edge_fragment_recipes = BTreeMap::new();
    let mut valid_point_uses = BTreeSet::new();
    let mut valid_edge_uses = BTreeSet::new();
    rename_memory_ssa(
        func,
        cfg,
        &state_loads,
        &pure_recipes,
        &mut recipes,
        &mut memory_ssa,
        &store_homes,
        &fragment_homes,
        &preserving_writes,
        &canonical_bits,
        collect_fragment_homes,
        &relevant_values,
        requested_points,
        collect_all_uses,
        &mut point_recipes,
        &mut edge_recipes,
        &mut point_fragment_recipes,
        &mut edge_fragment_recipes,
        &mut valid_point_uses,
        &mut valid_edge_uses,
    )?;
    Ok(ReloadRecipeAnalysis {
        recipes,
        pure_recipes,
        requested_points: requested_points.clone(),
        point_recipes,
        edge_recipes,
        point_fragment_recipes,
        edge_fragment_recipes,
        valid_point_uses,
        valid_edge_uses,
        collect_all_uses,
        collect_fragment_homes,
    })
}

#[derive(Debug, Clone, Copy)]
pub(super) struct ValidatedStateFragment {
    pub(super) value: VReg,
    pub(super) load: StateLoad,
    pub(super) value_bit_offset: usize,
    pub(super) bit_offset: usize,
    pub(super) width_bits: usize,
    pub(super) complete_value: bool,
    pub(super) observed_bits: StateBitRange,
}

pub(super) fn validated_state_fragment(
    func: &MFunction,
    inst: &MInst,
) -> Option<ValidatedStateFragment> {
    let MInst::Store {
        base: BaseReg::SimState,
        offset,
        src,
        size,
    } = inst
    else {
        return None;
    };
    let stored_bits = usize::try_from(size.bytes()).ok()?.checked_mul(8)?;
    let load = StateLoad {
        offset: *offset,
        size: *size,
    };
    let insert = func.spill_desc(*src)?.state_insert?;
    let end_bit = insert.bit_offset.checked_add(insert.width_bits)?;
    let value_end_bit = insert.value_bit_offset.checked_add(insert.width_bits)?;
    if insert.width_bits == 0
        || insert.width_bits > 64
        || end_bit > stored_bits
        || value_end_bit > 64
        || usize::try_from(insert.value.0).ok()? >= func.vregs.count() as usize
    {
        return None;
    }
    Some(ValidatedStateFragment {
        value: insert.value,
        load,
        value_bit_offset: insert.value_bit_offset,
        bit_offset: insert.bit_offset,
        width_bits: insert.width_bits,
        complete_value: insert.complete_value,
        observed_bits: StateBitRange::inserted(load, insert.bit_offset, insert.width_bits)?,
    })
}

pub(super) fn store_home_specs(
    func: &MFunction,
    inst: &MInst,
    canonical_bits: &[u8],
) -> Vec<StoreHomeSpec> {
    let MInst::Store {
        base: BaseReg::SimState,
        offset,
        src,
        size,
    } = inst
    else {
        return Vec::new();
    };
    let stored_bits = (size.bytes() * 8) as u8;
    let load = StateLoad {
        offset: *offset,
        size: *size,
    };
    let full_bits = StateBitRange::from_load(load)
        .expect("a fixed-width i32-addressed state load has a valid bit range");
    let mut homes = Vec::with_capacity(2);
    if canonical_bits
        .get(src.0 as usize)
        .is_some_and(|bits| *bits <= stored_bits)
    {
        homes.push(StoreHomeSpec {
            value: *src,
            load,
            steps: Vec::new(),
            observed_bits: full_bits,
        });
    }

    let Some(insert) = validated_state_fragment(func, inst) else {
        return homes;
    };
    if !insert.complete_value
        || insert.value_bit_offset != 0
        || canonical_bits
            .get(insert.value.0 as usize)
            .is_none_or(|bits| usize::from(*bits) > insert.width_bits)
    {
        return homes;
    }
    let mut steps = Vec::with_capacity(2);
    if insert.bit_offset != 0 {
        let Ok(immediate) = u8::try_from(insert.bit_offset) else {
            return homes;
        };
        steps.push(PureStep::ShrImm64 { immediate });
    }
    if insert.width_bits < 64 {
        if insert.width_bits <= 32 {
            steps.push(PureStep::AndImm32 {
                immediate: u32::MAX >> (32 - insert.width_bits),
            });
        } else {
            let clear_bits = (64 - insert.width_bits) as u8;
            steps.push(PureStep::ShlImm64 {
                immediate: clear_bits,
            });
            steps.push(PureStep::ShrImm64 {
                immediate: clear_bits,
            });
        }
    }
    let inserted = StoreHomeSpec {
        value: insert.value,
        load: insert.load,
        steps,
        observed_bits: insert.observed_bits,
    };
    if !homes.contains(&inserted) {
        homes.push(inserted);
    }
    homes
}

/// Prove how many low bits may be nonzero from MIR semantics alone.
///
/// This is a local analysis side table, not a VReg type: machine registers
/// remain 64-bit values and no HDL width is attached to them.  A narrow store
/// is a reload home only when its source is already exactly the zero-extended
/// value produced by a load of the same machine width.
pub(in super::super) fn canonical_value_bits(
    func: &MFunction,
) -> Result<Vec<u8>, ReloadRecipeError> {
    #[derive(Clone, Copy)]
    enum Definition {
        Phi { block: usize, phi: usize },
        Instruction { block: usize, instruction: usize },
    }

    let value_count = func.vregs.count() as usize;
    let mut definitions = vec![None::<Definition>; value_count];
    let mut dependent_counts = vec![0usize; value_count];
    let value_index = |value: VReg,
                       block: BlockId,
                       instruction: Option<usize>,
                       role: &'static str|
     -> Result<usize, ReloadRecipeError> {
        let index = value.0 as usize;
        (index < value_count).then_some(index).ok_or_else(|| {
            ReloadRecipeError::new(
                "RELOAD_RECIPE.VALUE_RANGE",
                Some(block),
                instruction,
                Some(value),
                format!("MIR {role} is outside the canonical-value side table"),
            )
        })
    };

    for (block_index, block) in func.blocks.iter().enumerate() {
        for (phi_index, phi) in block.phis.iter().enumerate() {
            let destination = value_index(phi.dst, block.id, None, "phi destination")?;
            definitions[destination] = Some(Definition::Phi {
                block: block_index,
                phi: phi_index,
            });
            for &(_, source) in &phi.sources {
                let source = value_index(source, block.id, None, "phi source")?;
                dependent_counts[source] = dependent_counts[source].saturating_add(1);
            }
        }
        for (instruction_index, inst) in block.insts.iter().enumerate() {
            let Some(destination) = inst.def() else {
                continue;
            };
            let destination =
                value_index(destination, block.id, Some(instruction_index), "definition")?;
            definitions[destination] = Some(Definition::Instruction {
                block: block_index,
                instruction: instruction_index,
            });
            for source in inst.uses() {
                let source = value_index(source, block.id, Some(instruction_index), "operand")?;
                dependent_counts[source] = dependent_counts[source].saturating_add(1);
            }
        }
    }

    // Store the def-use graph in CSR form. A Vec per VReg is prohibitively
    // expensive on the fused Linux function even though the number of actual
    // operand edges is linear in MIR size.
    let mut dependent_offsets = Vec::with_capacity(value_count + 1);
    dependent_offsets.push(0usize);
    for count in dependent_counts {
        let next = dependent_offsets
            .last()
            .copied()
            .unwrap_or(0)
            .saturating_add(count);
        dependent_offsets.push(next);
    }
    let mut dependent_cursor = dependent_offsets[..value_count].to_vec();
    let mut dependents = vec![VReg(0); dependent_offsets[value_count]];
    let mut record_dependency = |source: VReg, destination: VReg| {
        let source = source.0 as usize;
        let cursor = &mut dependent_cursor[source];
        dependents[*cursor] = destination;
        *cursor += 1;
    };
    for block in &func.blocks {
        for phi in &block.phis {
            for &(_, source) in &phi.sources {
                record_dependency(source, phi.dst);
            }
        }
        for inst in &block.insts {
            if let Some(destination) = inst.def() {
                for source in inst.uses() {
                    record_dependency(source, destination);
                }
            }
        }
    }

    // None is the unreachable/not-yet-proved bottom element. Phi nodes merge
    // the values already reachable from processed predecessors; a backedge can
    // raise that bound later. The worklist therefore reaches the least fixed
    // point without rescanning every instruction for every loop iteration.
    let mut bits = vec![None::<u8>; value_count];
    let mut queued = vec![false; value_count];
    let mut worklist = VecDeque::with_capacity(value_count);
    for (value, definition) in definitions.iter().enumerate() {
        if definition.is_some() {
            queued[value] = true;
            worklist.push_back(VReg(value as u32));
        }
    }
    while let Some(value) = worklist.pop_front() {
        let value_index = value.0 as usize;
        queued[value_index] = false;
        let Some(definition) = definitions[value_index] else {
            continue;
        };
        let proved = match definition {
            Definition::Phi { block, phi } => func.blocks[block].phis[phi]
                .sources
                .iter()
                .filter_map(|(_, source)| bits[source.0 as usize])
                .max(),
            Definition::Instruction { block, instruction } => canonical_instruction_bits(
                func,
                &func.blocks[block].insts[instruction],
                &bits,
                func.blocks[block].id,
                instruction,
            )?,
        };
        let Some(proved) = proved else {
            continue;
        };
        if bits[value_index].is_some_and(|previous| previous >= proved) {
            continue;
        }
        bits[value_index] = Some(proved);
        for &dependent in
            &dependents[dependent_offsets[value_index]..dependent_offsets[value_index + 1]]
        {
            let dependent = dependent.0 as usize;
            if !queued[dependent] {
                queued[dependent] = true;
                worklist.push_back(VReg(dependent as u32));
            }
        }
    }

    Ok(bits.into_iter().map(|width| width.unwrap_or(64)).collect())
}

pub(super) fn canonical_instruction_bits(
    func: &MFunction,
    inst: &MInst,
    bits: &[Option<u8>],
    block: BlockId,
    instruction: usize,
) -> Result<Option<u8>, ReloadRecipeError> {
    let operand = |value: VReg| {
        bits.get(value.0 as usize).copied().ok_or_else(|| {
            ReloadRecipeError::new(
                "RELOAD_RECIPE.VALUE_RANGE",
                Some(block),
                Some(instruction),
                Some(value),
                "MIR operand is outside the canonical-value side table",
            )
        })
    };
    for source in inst.uses() {
        if operand(source)?.is_none() {
            return Ok(None);
        }
    }
    let known = |value: VReg| -> Result<u8, ReloadRecipeError> {
        Ok(operand(value)?.expect("all instruction operands were proved above"))
    };
    let width = match inst {
        MInst::Mov { src, .. } => known(*src)?,
        MInst::Mov32 { src, .. } => known(*src)?.min(32),
        MInst::LoadImm { value, .. } => significant_bits(*value),
        MInst::Load {
            dst, base, size, ..
        } => {
            let physical = (size.bytes() * 8) as u8;
            let logical = (*base == BaseReg::SimState)
                .then(|| func.spill_desc(*dst))
                .flatten()
                .and_then(|descriptor| match descriptor.kind {
                    crate::native::mir::SpillKind::SimState { width_bits, .. }
                    | crate::native::mir::SpillKind::SimStateAlias { width_bits, .. } => {
                        u8::try_from(width_bits).ok()
                    }
                    crate::native::mir::SpillKind::Stack
                    | crate::native::mir::SpillKind::Remat { .. } => None,
                });
            logical.map_or(physical, |logical| logical.min(physical))
        }
        MInst::LoadPtr { size, .. }
        | MInst::LoadIndexed { size, .. }
        | MInst::LoadPtrIndexed { size, .. } => (size.bytes() * 8) as u8,
        MInst::Add32 { .. }
        | MInst::Sub32 { .. }
        | MInst::Mul32 { .. }
        | MInst::MulImm32 { .. } => 32,
        MInst::And { lhs, rhs, .. } => known(*lhs)?.min(known(*rhs)?),
        MInst::And32 { lhs, rhs, .. } => known(*lhs)?.min(known(*rhs)?).min(32),
        MInst::Or { lhs, rhs, .. } | MInst::Xor { lhs, rhs, .. } => known(*lhs)?.max(known(*rhs)?),
        MInst::Or32 { lhs, rhs, .. } | MInst::Xor32 { lhs, rhs, .. } => {
            known(*lhs)?.max(known(*rhs)?).min(32)
        }
        MInst::AndImm { src, imm, .. } => known(*src)?.min(significant_bits(*imm)),
        MInst::AndImm32 { src, imm, .. } => {
            known(*src)?.min(significant_bits(u64::from(*imm))).min(32)
        }
        MInst::OrImm { src, imm, .. } => known(*src)?.max(significant_bits(*imm)),
        MInst::ShrImm { src, imm, .. } => known(*src)?.saturating_sub(*imm),
        MInst::ShlImm { src, imm, .. } => known(*src)?.saturating_add(*imm).min(64),
        MInst::Cmp { .. } | MInst::CmpImm { .. } => 1,
        MInst::Popcnt { .. } => 7,
        MInst::Bsf { .. } | MInst::Bsr { .. } | MInst::BsrOr { .. } => 6,
        MInst::Select {
            true_val,
            false_val,
            ..
        }
        | MInst::CmpSelect {
            true_val,
            false_val,
            ..
        }
        | MInst::CmpImmSelect {
            true_val,
            false_val,
            ..
        }
        | MInst::GuardedCmpSelect {
            true_val,
            false_val,
            ..
        } => known(*true_val)?.max(known(*false_val)?),
        _ => 64,
    };
    Ok(Some(width))
}

pub(super) fn significant_bits(value: u64) -> u8 {
    (u64::BITS - value.leading_zeros()) as u8
}

pub(super) fn resolve_recipe_bases(
    recipes: &[ReloadRecipe],
    pure_recipes: &[PureRecipe],
    state_loads: &[Option<StateLoad>],
) -> Result<Vec<Option<RecipeBase>>, ReloadRecipeError> {
    let mut resolved = vec![None::<Option<RecipeBase>>; recipes.len()];
    let mut marks = vec![0usize; recipes.len()];
    for start in 0..recipes.len() {
        if resolved[start].is_some() {
            continue;
        }
        let generation = start + 1;
        let mut path = Vec::<usize>::new();
        let mut current = start;
        let base = loop {
            if current >= recipes.len() {
                return Err(ReloadRecipeError::new(
                    "RELOAD_RECIPE.PURE_SOURCE_RANGE",
                    None,
                    None,
                    Some(VReg(current as u32)),
                    "pure recipe source is outside the VReg recipe table",
                ));
            }
            if let Some(base) = resolved[current] {
                break base;
            }
            if marks[current] == generation {
                return Err(ReloadRecipeError::new(
                    "RELOAD_RECIPE.PURE_CYCLE",
                    None,
                    None,
                    Some(VReg(start as u32)),
                    format!("pure recipe dependency cycles through v{current}"),
                ));
            }
            marks[current] = generation;
            path.push(current);
            if state_loads[current].is_some() {
                break Some(RecipeBase::State(VReg(current as u32)));
            }
            match recipes[current] {
                ReloadRecipe::Constant { .. } => break Some(RecipeBase::Constant),
                ReloadRecipe::Pure { expression } => {
                    let Some(expression) = pure_recipes.get(expression.0 as usize) else {
                        return Err(ReloadRecipeError::new(
                            "RELOAD_RECIPE.PURE_EXPRESSION",
                            None,
                            None,
                            Some(VReg(current as u32)),
                            "pure recipe identifier is outside the expression table",
                        ));
                    };
                    current = expression.source().0 as usize;
                }
                ReloadRecipe::StateVersion(_) => {
                    break Some(RecipeBase::State(VReg(current as u32)));
                }
                ReloadRecipe::Stack => break None,
            }
        };
        for member in path {
            resolved[member] = Some(base);
        }
    }
    Ok(resolved
        .into_iter()
        .map(|base| base.unwrap_or(None))
        .collect())
}

pub(super) fn relevant_recipe_values(
    func: &MFunction,
    recipes: &[ReloadRecipe],
    pure_recipes: &[PureRecipe],
    requested_points: &BTreeSet<PointUse>,
    collect_all_uses: bool,
) -> Result<BTreeSet<VReg>, ReloadRecipeError> {
    if collect_all_uses {
        return Ok((0..func.vregs.count()).map(VReg).collect());
    }

    let phi_sources = func
        .blocks
        .iter()
        .flat_map(|block| &block.phis)
        .map(|phi| {
            (
                phi.dst,
                phi.sources
                    .iter()
                    .map(|(_, source)| *source)
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<HashMap<_, _>>();
    let mut relevant = BTreeSet::<VReg>::new();
    let mut queue = requested_points
        .iter()
        .map(|point| point.value)
        .collect::<VecDeque<_>>();
    while let Some(value) = queue.pop_front() {
        if !relevant.insert(value) {
            continue;
        }
        let Some(recipe) = recipes.get(value.0 as usize) else {
            return Err(ReloadRecipeError::new(
                "RELOAD_RECIPE.VALUE_COVERAGE",
                None,
                None,
                Some(value),
                "requested reload is outside the VReg recipe table",
            ));
        };
        if let ReloadRecipe::Pure { expression } = recipe {
            let Some(expression) = pure_recipes.get(expression.0 as usize) else {
                return Err(ReloadRecipeError::new(
                    "RELOAD_RECIPE.PURE_EXPRESSION",
                    None,
                    None,
                    Some(value),
                    "relevant pure-expression identifier is outside its table",
                ));
            };
            queue.push_back(expression.source());
        }
        if let Some(sources) = phi_sources.get(&value) {
            queue.extend(sources.iter().copied());
        }
    }
    Ok(relevant)
}

pub(super) fn pure_expression(inst: &MInst) -> Option<PureRecipe> {
    match inst {
        MInst::MulImm { src, imm, .. } => Some(PureRecipe::MulImm64 {
            source: *src,
            immediate: *imm,
        }),
        MInst::MulImm32 { src, imm, .. } => Some(PureRecipe::MulImm32 {
            source: *src,
            immediate: *imm,
        }),
        MInst::Mov { src, .. } => Some(PureRecipe::Copy64 { source: *src }),
        MInst::Mov32 { src, .. } => Some(PureRecipe::Copy32 { source: *src }),
        MInst::AndImm { src, imm, .. } => Some(PureRecipe::AndImm64 {
            source: *src,
            immediate: *imm,
        }),
        MInst::AndImm32 { src, imm, .. } => Some(PureRecipe::AndImm32 {
            source: *src,
            immediate: *imm,
        }),
        MInst::OrImm { src, imm, .. } => Some(PureRecipe::OrImm64 {
            source: *src,
            immediate: *imm,
        }),
        MInst::ShrImm { src, imm, .. } => Some(PureRecipe::ShrImm64 {
            source: *src,
            immediate: *imm,
        }),
        MInst::ShlImm { src, imm, .. } => Some(PureRecipe::ShlImm64 {
            source: *src,
            immediate: *imm,
        }),
        MInst::SarImm { src, imm, .. } => Some(PureRecipe::SarImm64 {
            source: *src,
            immediate: *imm,
        }),
        MInst::AddImm { src, imm, .. } => Some(PureRecipe::AddImm64 {
            source: *src,
            immediate: *imm,
        }),
        MInst::SubImm { src, imm, .. } => Some(PureRecipe::SubImm64 {
            source: *src,
            immediate: *imm,
        }),
        MInst::CmpImm { lhs, imm, kind, .. } => Some(PureRecipe::CmpImm64 {
            source: *lhs,
            immediate: *imm,
            kind: *kind,
        }),
        MInst::BitNot { src, .. } => Some(PureRecipe::BitNot64 { source: *src }),
        MInst::Neg { src, .. } => Some(PureRecipe::Neg64 { source: *src }),
        _ => None,
    }
}
