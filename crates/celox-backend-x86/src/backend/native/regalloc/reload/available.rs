//! The recipe available for a value at a program point.

use super::*;

pub(super) fn available_recipe(
    value: VReg,
    recipes: &[ReloadRecipe],
    pure_recipes: &[PureRecipe],
    current_homes: &BTreeMap<VReg, Vec<StoreHome>>,
    at: VersionId,
    memory_ssa: &mut ReloadMemorySsa,
) -> Result<Option<ResolvedRecipe>, ReloadRecipeError> {
    let mut current_value = value;
    let mut reverse_steps = Vec::<PureStep>::new();
    let mut seen = BTreeSet::<VReg>::new();
    loop {
        if !seen.insert(current_value) {
            return Err(ReloadRecipeError::new(
                "RELOAD_RECIPE.PURE_CYCLE",
                None,
                None,
                Some(value),
                format!("pure recipe dependency cycles through {current_value}"),
            ));
        }

        // Prefer the nearest exact state snapshot of the value currently
        // being reconstructed.  In particular, a pure MIR definition may be
        // stored to SimState after it is computed.  Walking through the pure
        // expression first would lose that stronger, one-load home and try to
        // reconstruct the expression from its source instead.
        if let Some(recipe) =
            available_store_home(current_value, &reverse_steps, current_homes, at, memory_ssa)?
        {
            return Ok(Some(recipe));
        }

        match recipes.get(current_value.0 as usize) {
            Some(ReloadRecipe::Constant { value }) => {
                reverse_steps.reverse();
                return Ok(Some(ResolvedRecipe {
                    base: ResolvedBase::Constant(*value),
                    steps: reverse_steps,
                }));
            }
            Some(ReloadRecipe::StateVersion(recipe)) => {
                if memory_ssa.snapshot_at(at, recipe.load)? != recipe.snapshot {
                    return available_store_home(
                        current_value,
                        &reverse_steps,
                        current_homes,
                        at,
                        memory_ssa,
                    );
                }
                reverse_steps.reverse();
                return Ok(Some(ResolvedRecipe {
                    base: ResolvedBase::State(recipe.clone()),
                    steps: reverse_steps,
                }));
            }
            Some(ReloadRecipe::Pure { expression }) => {
                let Some(expression) = pure_recipes.get(expression.0 as usize).copied() else {
                    return Err(ReloadRecipeError::new(
                        "RELOAD_RECIPE.PURE_EXPRESSION",
                        None,
                        None,
                        Some(current_value),
                        "pure recipe identifier is outside the expression table",
                    ));
                };
                reverse_steps.push(expression.step());
                current_value = expression.source();
            }
            Some(ReloadRecipe::Stack) => {
                return available_store_home(
                    current_value,
                    &reverse_steps,
                    current_homes,
                    at,
                    memory_ssa,
                );
            }
            None => {
                return Err(ReloadRecipeError::new(
                    "RELOAD_RECIPE.VALUE_COVERAGE",
                    None,
                    None,
                    Some(current_value),
                    "recipe table does not cover the requested VReg",
                ));
            }
        }
    }
}

pub(super) fn available_store_home(
    value: VReg,
    reverse_steps: &[PureStep],
    current_homes: &BTreeMap<VReg, Vec<StoreHome>>,
    at: VersionId,
    memory_ssa: &mut ReloadMemorySsa,
) -> Result<Option<ResolvedRecipe>, ReloadRecipeError> {
    let Some(homes) = current_homes.get(&value) else {
        return Ok(None);
    };
    for home in homes.iter().rev() {
        if memory_ssa.snapshot_at(at, home.state.load)? == home.state.snapshot {
            let mut steps = home.steps.clone();
            let mut suffix = reverse_steps.to_vec();
            suffix.reverse();
            steps.extend(suffix);
            return Ok(Some(ResolvedRecipe {
                base: ResolvedBase::State(home.state.clone()),
                steps,
            }));
        }
    }
    Ok(None)
}

pub(super) fn fragment_materialization_cost(fragment: &StateFragmentHome) -> u16 {
    let mut cost = 1u16; // physical load
    if fragment.state_bit_offset != 0 {
        cost = cost.saturating_add(1);
    }
    if fragment.width_bits < 64 {
        cost = cost.saturating_add(if fragment.width_bits <= 32 { 1 } else { 2 });
    }
    if fragment.value_bit_offset != 0 {
        cost = cost.saturating_add(1);
    }
    cost
}

/// Select a complete, minimum-machine-operation cover of the significant bits
/// of `value`.  Every selected physical fragment has an independently checked
/// MemorySSA version at this exact point.  The cover is allowed to overlap but
/// never to contain a hole.
pub(super) fn available_fragment_recipe(
    value: VReg,
    canonical_bits: &[u8],
    current_fragments: &BTreeMap<VReg, Vec<StateFragmentHome>>,
    at: VersionId,
    memory_ssa: &mut ReloadMemorySsa,
) -> Result<Option<CompositeStateRecipe>, ReloadRecipeError> {
    let Some(&required_bits) = canonical_bits.get(value.0 as usize) else {
        return Err(ReloadRecipeError::new(
            "RELOAD_RECIPE.VALUE_COVERAGE",
            None,
            None,
            Some(value),
            "fragment recipe value is outside the canonical-value table",
        ));
    };
    let required_bits = usize::from(required_bits);
    if required_bits == 0 || required_bits > 64 {
        return Ok(None);
    }
    let Some(homes) = current_fragments.get(&value) else {
        return Ok(None);
    };
    let mut available = Vec::<&StateFragmentHome>::new();
    for home in homes {
        let Some(end) = home.value_bit_offset.checked_add(home.width_bits) else {
            continue;
        };
        if home.value_bit_offset >= required_bits || end <= home.value_bit_offset {
            continue;
        }
        if memory_ssa.snapshot_at(at, home.state.load)? == home.state.snapshot {
            available.push(home);
        }
    }
    available.sort_by_key(|home| {
        (
            home.value_bit_offset,
            std::cmp::Reverse(home.value_bit_offset.saturating_add(home.width_bits)),
            fragment_materialization_cost(home),
            home.state.load.offset,
            home.state.load.size.bytes(),
            home.state_bit_offset,
            home.width_bits,
        )
    });

    #[derive(Clone)]
    struct Cover {
        cost: u16,
        fragments: Vec<usize>,
    }
    let mut best = vec![None::<Cover>; required_bits + 1];
    best[0] = Some(Cover {
        cost: 0,
        fragments: Vec::new(),
    });
    for covered in 0..required_bits {
        let Some(prefix) = best[covered].clone() else {
            continue;
        };
        for (fragment_index, fragment) in available.iter().enumerate() {
            let end = fragment
                .value_bit_offset
                .saturating_add(fragment.width_bits)
                .min(required_bits);
            if fragment.value_bit_offset > covered || end <= covered {
                continue;
            }
            let mut candidate = prefix.clone();
            candidate.cost = candidate
                .cost
                .saturating_add(fragment_materialization_cost(fragment))
                .saturating_add(u16::from(!candidate.fragments.is_empty()));
            candidate.fragments.push(fragment_index);
            let replace = best[end].as_ref().is_none_or(|existing| {
                (candidate.cost, candidate.fragments.as_slice())
                    < (existing.cost, existing.fragments.as_slice())
            });
            if replace {
                best[end] = Some(candidate);
            }
        }
    }
    let Some(cover) = best[required_bits].take() else {
        return Ok(None);
    };
    let fragments = cover
        .fragments
        .into_iter()
        .map(|index| {
            let fragment = available[index];
            StateFragmentRecipe {
                state: fragment.state.clone(),
                value_bit_offset: fragment.value_bit_offset,
                state_bit_offset: fragment.state_bit_offset,
                width_bits: fragment.width_bits,
            }
        })
        .collect::<Vec<_>>()
        .into_boxed_slice();
    Ok(Some(CompositeStateRecipe { fragments }))
}
