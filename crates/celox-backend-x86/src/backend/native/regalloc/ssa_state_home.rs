//! Assign allocator-planned W/S homes to packed simulation-state words.
//!
//! This is deliberately a thin extension of the production Braun--Hack
//! allocator.  It does not run the interval allocator and it does not invent
//! a second liveness fixed point.  The existing spill plan decides where a
//! home is established and consumed; this module only proves that replacing
//! one stack home by one physical SimState word is valid at every reload.

use std::collections::{BTreeMap, BTreeSet};

use crate::HashMap;
use crate::native::mir::{BaseReg, BlockId, MFunction, MInst, PackedStateHome, SpillKind, VReg};

use super::cfg::NormalizedCfg;
use super::reload::{PointUse, ResolvedBase, ResolvedRecipe};
use super::spill_plan::{LogicalValue, PlannedEdgeOp, PlannedOp, SpillHome, SpillPlan};

type ReloadKey = (BlockId, usize, LogicalValue);
type SpillInsertion = (LogicalValue, SpillHome);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PlannedSpillInsertion {
    pub block: usize,
    pub instruction: usize,
    pub value: LogicalValue,
    pub home: SpillHome,
    /// Reconstruction emits this store as part of an atomic source-home to
    /// destination-home transfer; consumers still count the destination home
    /// definition, but must not emit a second standalone store.
    pub edge_transfer: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StateHomeError {
    pub rule: &'static str,
    pub block: Option<BlockId>,
    pub instruction: Option<usize>,
    pub values: Vec<VReg>,
    pub message: String,
}

impl StateHomeError {
    fn new(
        rule: &'static str,
        block: Option<BlockId>,
        instruction: Option<usize>,
        values: Vec<VReg>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            rule,
            block,
            instruction,
            values,
            message: message.into(),
        }
    }

    fn reload(error: super::reload::ReloadRecipeError) -> Self {
        Self::new(
            error.rule,
            error.block,
            error.instruction,
            error.value.into_iter().collect(),
            error.message,
        )
    }
}

struct Probe {
    func: MFunction,
    points: BTreeMap<ReloadKey, PointUse>,
}

/// Select physical homes with two sparse MemorySSA probes.
///
/// Candidate discovery and probe construction are O(V + I + P), where P is
/// the number of planned spill/reload operations.  Each probe delegates to
/// the existing sparse byte MemorySSA.  At most one cloned MIR and one probe
/// analysis are live at a time; there is no cell-by-block or layer-by-range
/// dense product.
pub(super) fn select(
    func: &MFunction,
    cfg: &NormalizedCfg,
    plan: &mut SpillPlan,
) -> Result<(), StateHomeError> {
    plan.state_homes.clear();
    plan.state_reload_recipes.clear();

    let reloads = reload_locations(func, cfg, plan)?;
    let candidates = candidate_homes(func, plan, reloads.keys().copied());
    if candidates.is_empty() {
        return Ok(());
    }

    // Probe all candidate stores together.  Physically overlapping homes are
    // selected as one component: accepting only part of such a component
    // would make validity depend on stores that selection subsequently
    // removes.
    let first = build_probe(func, cfg, plan, &candidates, &reloads)?;
    let requested = first.points.values().copied().collect::<BTreeSet<_>>();
    let analysis = super::reload::analyze_with_queries(&first.func, cfg, &requested)
        .map_err(StateHomeError::reload)?;
    let individually_valid = candidates
        .iter()
        .filter_map(|(&home, &physical)| {
            let uses = reloads.get(&home)?;
            (!uses.is_empty()
                && uses.iter().all(|key| {
                    first.points.get(key).is_some_and(|point| {
                        analysis
                            .resolved_recipe_at_point(*point)
                            .is_some_and(|recipe| direct_home_recipe(recipe, physical))
                    })
                }))
            .then_some(home)
        })
        .collect::<BTreeSet<_>>();

    let mut selected = BTreeMap::<SpillHome, PackedStateHome>::new();
    for component in overlap_components(&candidates) {
        if component
            .iter()
            .all(|home| individually_valid.contains(home))
        {
            for home in component {
                selected.insert(home, candidates[&home]);
            }
        }
    }
    drop(analysis);
    drop(first);
    if selected.is_empty() {
        return Ok(());
    }

    // Removing rejected stores changes per-block MemorySSA write ordinals.
    // Rebuild once with exactly the stores reconstruction will emit and retain
    // those recipes, rather than adjusting structural versions heuristically.
    let final_probe = build_probe(func, cfg, plan, &selected, &reloads)?;
    let requested = final_probe
        .points
        .values()
        .copied()
        .collect::<BTreeSet<_>>();
    let final_analysis = super::reload::analyze_with_queries(&final_probe.func, cfg, &requested)
        .map_err(StateHomeError::reload)?;
    let mut recipes = BTreeMap::<ReloadKey, ResolvedRecipe>::new();
    for (&home, &physical) in &selected {
        let uses = &reloads[&home];
        for &key in uses {
            let point = final_probe.points.get(&key).copied().ok_or_else(|| {
                StateHomeError::new(
                    "STATE_HOME.RELOAD_POINT",
                    Some(key.0),
                    Some(key.1),
                    vec![VReg(key.2.0)],
                    "selected state-home reload has no probe point",
                )
            })?;
            let recipe = final_analysis
                .resolved_recipe_at_point(point)
                .filter(|recipe| direct_home_recipe(recipe, physical))
                .cloned()
                .ok_or_else(|| {
                    StateHomeError::new(
                        "STATE_HOME.FINAL_MEMORY_SSA",
                        Some(key.0),
                        Some(key.1),
                        vec![VReg(key.2.0)],
                        format!(
                            "state home {home:?} at offset {} ({:?}) is not the exact reaching definition",
                            physical.offset, physical.size
                        ),
                    )
                })?;
            recipes.insert(key, recipe);
        }
    }

    plan.state_homes = selected;
    plan.state_reload_recipes = recipes;
    verify(func, cfg, plan)
}

/// Fall back allocator-managed state homes which can overwrite a reload
/// recipe at the same serial edge insertion point. Overlapping homes are
/// selected and retired as one component so a retained home never depends on
/// a store removed from the component proof.
pub(super) fn fallback_to_stack(
    func: &MFunction,
    cfg: &NormalizedCfg,
    plan: &mut SpillPlan,
    hazardous: &BTreeSet<SpillHome>,
) -> Result<(), StateHomeError> {
    if hazardous.is_empty() {
        return Ok(());
    }
    let removed = overlap_components(&plan.state_homes)
        .into_iter()
        .filter(|component| component.iter().any(|home| hazardous.contains(home)))
        .flatten()
        .collect::<BTreeSet<_>>();
    if removed.is_empty() {
        return Ok(());
    }

    let reloads = reload_locations(func, cfg, plan)?;
    let removed_reload_keys = removed
        .iter()
        .flat_map(|home| reloads.get(home).into_iter().flatten().copied())
        .collect::<BTreeSet<_>>();
    for home in &removed {
        plan.state_homes.remove(home);
    }
    plan.state_reload_recipes
        .retain(|key, _| !removed_reload_keys.contains(key));
    verify(func, cfg, plan)
}

/// Check the selected-home table independently from candidate selection.
pub(super) fn verify(
    func: &MFunction,
    cfg: &NormalizedCfg,
    plan: &SpillPlan,
) -> Result<(), StateHomeError> {
    if let Some(home) = plan
        .state_homes
        .keys()
        .find(|home| plan.recipe_homes.contains(home))
    {
        return Err(StateHomeError::new(
            "STATE_HOME.EXCLUSIVE_KIND",
            None,
            None,
            Vec::new(),
            format!("spill home {home:?} is both state-backed and recipe-only"),
        ));
    }

    let reloads = reload_locations(func, cfg, plan)?;
    let candidates = candidate_homes(func, plan, plan.state_homes.keys().copied());
    for (&home, &physical) in &plan.state_homes {
        if candidates.get(&home) != Some(&physical) {
            return Err(StateHomeError::new(
                "STATE_HOME.DESCRIPTOR_MATCH",
                None,
                None,
                plan.homes.members(home).collect(),
                format!("selected state home {home:?} no longer matches its MIR descriptors"),
            ));
        }
        let Some(uses) = reloads.get(&home) else {
            return Err(StateHomeError::new(
                "STATE_HOME.HAS_RELOAD",
                None,
                None,
                Vec::new(),
                format!("selected state home {home:?} has no planned reload"),
            ));
        };
        for &key in uses {
            let Some(recipe) = plan.state_reload_recipes.get(&key) else {
                return Err(StateHomeError::new(
                    "STATE_HOME.RECIPE_COVERS_RELOAD",
                    Some(key.0),
                    Some(key.1),
                    vec![VReg(key.2.0)],
                    format!("selected state home {home:?} has an uncovered reload"),
                ));
            };
            if !direct_home_recipe(recipe, physical) {
                return Err(StateHomeError::new(
                    "STATE_HOME.RECIPE_SHAPE",
                    Some(key.0),
                    Some(key.1),
                    vec![VReg(key.2.0)],
                    format!("selected state home {home:?} has a non-direct reload recipe"),
                ));
            }
        }
    }
    for (&key, recipe) in &plan.state_reload_recipes {
        let matching = reloads.iter().find_map(|(&home, uses)| {
            (uses.contains(&key) && plan.state_homes.contains_key(&home)).then_some(home)
        });
        let Some(home) = matching else {
            return Err(StateHomeError::new(
                "STATE_HOME.RECIPE_NAMES_RELOAD",
                Some(key.0),
                Some(key.1),
                vec![VReg(key.2.0)],
                "state-home recipe has no matching selected reload",
            ));
        };
        if !direct_home_recipe(recipe, plan.state_homes[&home]) {
            return Err(StateHomeError::new(
                "STATE_HOME.RECIPE_SHAPE",
                Some(key.0),
                Some(key.1),
                vec![VReg(key.2.0)],
                format!("state-home recipe does not load selected home {home:?}"),
            ));
        }
    }
    Ok(())
}

fn candidate_homes(
    func: &MFunction,
    plan: &SpillPlan,
    homes: impl IntoIterator<Item = SpillHome>,
) -> BTreeMap<SpillHome, PackedStateHome> {
    let mut result = BTreeMap::new();
    for home in homes {
        let mut physical = None::<PackedStateHome>;
        let mut compatible = true;
        for member in plan.homes.members(home) {
            let Some(descriptor) = func.spill_desc(member) else {
                continue;
            };
            if matches!(descriptor.kind, SpillKind::Remat { .. }) {
                compatible = false;
                break;
            }
            let Some(candidate) = descriptor.deferred_state_home else {
                continue;
            };
            if candidate.live_on_entry || candidate.byte_range().is_none() {
                compatible = false;
                break;
            }
            match physical {
                Some(previous)
                    if previous.offset != candidate.offset || previous.size != candidate.size =>
                {
                    compatible = false;
                    break;
                }
                Some(previous) => {
                    if candidate.id < previous.id {
                        physical = Some(PackedStateHome {
                            id: candidate.id,
                            ..previous
                        });
                    }
                }
                None => physical = Some(candidate),
            }
        }
        if compatible && let Some(physical) = physical {
            result.insert(home, physical);
        }
    }
    result
}

fn reload_locations(
    func: &MFunction,
    cfg: &NormalizedCfg,
    plan: &SpillPlan,
) -> Result<BTreeMap<SpillHome, BTreeSet<ReloadKey>>, StateHomeError> {
    let mut result = BTreeMap::<SpillHome, BTreeSet<ReloadKey>>::new();
    for &(point, operation) in &plan.point_ops {
        let PlannedOp::Reload { value, home } = operation else {
            continue;
        };
        result
            .entry(home)
            .or_default()
            .insert((point.block, point.instruction, value));
    }
    for (&(predecessor, successor), operations) in &plan.edge_ops {
        let predecessor_block = func.blocks.get(predecessor).ok_or_else(|| {
            StateHomeError::new(
                "STATE_HOME.EDGE_PREDECESSOR",
                None,
                None,
                Vec::new(),
                format!("edge predecessor index {predecessor} is outside MIR"),
            )
        })?;
        if func.blocks.get(successor).is_none() {
            return Err(StateHomeError::new(
                "STATE_HOME.EDGE_SUCCESSOR",
                Some(predecessor_block.id),
                None,
                Vec::new(),
                format!("edge successor index {successor} is outside MIR"),
            ));
        }
        let insertion = super::cfg::edge_insertion_point(func, cfg, predecessor, successor)
            .ok_or_else(|| {
                StateHomeError::new(
                    "STATE_HOME.EDGE_POINT",
                    Some(predecessor_block.id),
                    None,
                    Vec::new(),
                    "edge reload has no single-edge materialization point",
                )
            })?;
        let block = &func.blocks[insertion.block];
        for &operation in operations {
            if let PlannedEdgeOp::Reload {
                source,
                source_home,
                ..
            } = operation
            {
                result.entry(source_home).or_default().insert((
                    block.id,
                    insertion.instruction,
                    source,
                ));
            }
        }
    }
    Ok(result)
}

/// Expand the abstract W/S spill operations into their actual point stores.
/// Both the MemorySSA probe and reconstruction consume this one ordered list;
/// keeping two implementations here would make structural write versions
/// depend on subtly different SpillPhi expansion.
pub(super) fn planned_spills(
    func: &MFunction,
    cfg: &NormalizedCfg,
    plan: &SpillPlan,
) -> Result<Vec<PlannedSpillInsertion>, StateHomeError> {
    let mut result = Vec::new();
    let spilled_phis = plan
        .point_ops
        .iter()
        .filter_map(|(_, operation)| match operation {
            PlannedOp::SpillPhi { value, .. } => Some(*value),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    for (successor, block) in func.blocks.iter().enumerate() {
        for phi in &block.phis {
            let logical = plan.logical.of(phi.dst);
            if !spilled_phis.contains(&logical) {
                continue;
            }
            let home = plan.homes.of_vreg(phi.dst);
            if plan.recipe_homes.contains(&home) {
                continue;
            }
            for &(predecessor, source) in &phi.sources {
                let predecessor = cfg.block_index.get(&predecessor).copied().ok_or_else(|| {
                    StateHomeError::new(
                        "STATE_HOME.PHI_PREDECESSOR",
                        Some(block.id),
                        None,
                        vec![phi.dst, source],
                        "spilled phi predecessor is outside normalized CFG",
                    )
                })?;
                let source = plan.logical.of(source);
                let has_explicit_transfer = plan
                    .edge_ops
                    .get(&(predecessor, successor))
                    .is_some_and(|operations| {
                        operations.iter().any(|operation| {
                            matches!(
                                operation,
                                PlannedEdgeOp::Spill {
                                    destination,
                                    destination_home,
                                    ..
                                } if *destination == logical && *destination_home == home
                            )
                        })
                    });
                if has_explicit_transfer {
                    continue;
                }
                if plan.homes.of_logical(source) == home
                    && plan.s_exit[predecessor].contains(&source)
                {
                    continue;
                }
                let insertion = super::cfg::edge_insertion_point(func, cfg, predecessor, successor)
                    .ok_or_else(|| {
                        StateHomeError::new(
                            "STATE_HOME.PHI_EDGE_POINT",
                            Some(block.id),
                            None,
                            vec![phi.dst, VReg(source.0)],
                            "spilled phi edge has no single-edge materialization point",
                        )
                    })?;
                result.push(PlannedSpillInsertion {
                    block: insertion.block,
                    instruction: insertion.instruction,
                    value: source,
                    home,
                    edge_transfer: false,
                });
            }
        }
    }
    for &(point, operation) in &plan.point_ops {
        let PlannedOp::Spill { value, home } = operation else {
            continue;
        };
        let block = cfg.block_index.get(&point.block).copied().ok_or_else(|| {
            StateHomeError::new(
                "STATE_HOME.POINT_BLOCK",
                Some(point.block),
                Some(point.instruction),
                vec![VReg(value.0)],
                "spill point is outside normalized CFG",
            )
        })?;
        result.push(PlannedSpillInsertion {
            block,
            instruction: point.instruction,
            value,
            home,
            edge_transfer: false,
        });
    }
    for (&(predecessor, successor), operations) in &plan.edge_ops {
        let insertion = super::cfg::edge_insertion_point(func, cfg, predecessor, successor)
            .ok_or_else(|| {
                StateHomeError::new(
                    "STATE_HOME.EDGE_POINT",
                    func.blocks.get(predecessor).map(|block| block.id),
                    None,
                    Vec::new(),
                    "spill edge has no single-edge materialization point",
                )
            })?;
        for (operation_index, &operation) in operations.iter().enumerate() {
            match operation {
                PlannedEdgeOp::Spill {
                    source,
                    destination,
                    destination_home,
                } => {
                    let is_home_transfer = operation_index != 0
                        && matches!(
                            operations[operation_index - 1],
                            PlannedEdgeOp::Reload {
                                destination: reload_destination,
                                ..
                            } if reload_destination == destination && source == destination
                        );
                    result.push(PlannedSpillInsertion {
                        block: insertion.block,
                        instruction: insertion.instruction,
                        value: source,
                        home: destination_home,
                        edge_transfer: is_home_transfer,
                    });
                }
                PlannedEdgeOp::Reload { .. } => {}
            }
        }
    }
    Ok(result)
}

fn build_probe(
    func: &MFunction,
    cfg: &NormalizedCfg,
    plan: &SpillPlan,
    homes: &BTreeMap<SpillHome, PackedStateHome>,
    reloads: &BTreeMap<SpillHome, BTreeSet<ReloadKey>>,
) -> Result<Probe, StateHomeError> {
    let mut insertions = HashMap::<(usize, usize), Vec<SpillInsertion>>::default();
    for spill in planned_spills(func, cfg, plan)? {
        if spill.edge_transfer {
            // The probe has no synthetic VReg for the atomic source-home
            // reload.  Conservatively leave this destination out of the
            // deferred-state candidate proof instead of pretending its
            // successor logical value is resident on the predecessor edge.
            continue;
        }
        if homes.contains_key(&spill.home) {
            insertions
                .entry((spill.block, spill.instruction))
                .or_default()
                .push((spill.value, spill.home));
        }
    }

    let mut probe = func.clone();
    let mut shifted = vec![Vec::<usize>::new(); probe.blocks.len()];
    for (block, (probe_block, shifted_block)) in
        probe.blocks.iter_mut().zip(&mut shifted).enumerate()
    {
        let original = std::mem::take(&mut probe_block.insts);
        shifted_block.reserve(original.len());
        let extra = insertions
            .iter()
            .filter(|((owner, _), _)| *owner == block)
            .map(|(_, stores)| stores.len())
            .sum::<usize>();
        let mut rewritten = Vec::with_capacity(original.len().saturating_add(extra));
        for (instruction, inst) in original.into_iter().enumerate() {
            for &(value, home) in insertions.get(&(block, instruction)).into_iter().flatten() {
                let physical = homes[&home];
                rewritten.push(MInst::Store {
                    base: BaseReg::SimState,
                    offset: physical.offset,
                    src: VReg(value.0),
                    size: physical.size,
                });
            }
            shifted_block.push(rewritten.len());
            rewritten.push(inst);
        }
        probe_block.insts = rewritten;
    }
    if let Some((&(block, instruction), stores)) =
        insertions.iter().find(|((block, instruction), _)| {
            shifted
                .get(*block)
                .is_none_or(|indices| *instruction >= indices.len())
        })
    {
        return Err(StateHomeError::new(
            "STATE_HOME.SPILL_POINT_RANGE",
            func.blocks.get(block).map(|owner| owner.id),
            Some(instruction),
            stores.iter().map(|(value, _)| VReg(value.0)).collect(),
            "state-home spill point is outside its MIR block",
        ));
    }

    let mut points = BTreeMap::<ReloadKey, PointUse>::new();
    for home in homes.keys() {
        let Some(uses) = reloads.get(home) else {
            continue;
        };
        for &key in uses {
            let block = cfg.block_index.get(&key.0).copied().ok_or_else(|| {
                StateHomeError::new(
                    "STATE_HOME.RELOAD_BLOCK",
                    Some(key.0),
                    Some(key.1),
                    vec![VReg(key.2.0)],
                    "state-home reload block is outside normalized CFG",
                )
            })?;
            let instruction = shifted
                .get(block)
                .and_then(|indices| indices.get(key.1))
                .copied()
                .ok_or_else(|| {
                    StateHomeError::new(
                        "STATE_HOME.RELOAD_POINT_RANGE",
                        Some(key.0),
                        Some(key.1),
                        vec![VReg(key.2.0)],
                        "state-home reload point is outside its MIR block",
                    )
                })?;
            points.insert(
                key,
                PointUse {
                    block: key.0,
                    instruction,
                    value: VReg(key.2.0),
                },
            );
        }
    }
    Ok(Probe {
        func: probe,
        points,
    })
}

fn direct_home_recipe(recipe: &ResolvedRecipe, home: PackedStateHome) -> bool {
    recipe.steps.is_empty()
        && matches!(
            &recipe.base,
            ResolvedBase::State(state)
                if state.load.offset == home.offset && state.load.size == home.size
        )
}

fn overlap_components(homes: &BTreeMap<SpillHome, PackedStateHome>) -> Vec<Vec<SpillHome>> {
    let mut intervals = homes
        .iter()
        .filter_map(|(&home, &physical)| {
            physical
                .byte_range()
                .map(|range| (range.start, range.end, home))
        })
        .collect::<Vec<_>>();
    intervals.sort_unstable();
    let mut result = Vec::<Vec<SpillHome>>::new();
    let mut current = Vec::<SpillHome>::new();
    let mut current_end = i64::MIN;
    for (start, end, home) in intervals {
        if current.is_empty() || start < current_end {
            current.push(home);
            current_end = current_end.max(end);
        } else {
            result.push(std::mem::take(&mut current));
            current.push(home);
            current_end = end;
        }
    }
    if !current.is_empty() {
        result.push(current);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::mir::{MBlock, OpSize, SpillDesc, StateHomeId, VRegAllocator};
    use crate::native::regalloc::{cfg, next_use, reconstruct, reload, spill_plan};

    fn home(id: u32, offset: i32) -> PackedStateHome {
        PackedStateHome {
            id: StateHomeId(id),
            offset,
            size: OpSize::S64,
            live_on_entry: false,
        }
    }

    fn blank_plan(
        func: &MFunction,
        cfg: &NormalizedCfg,
        next_use: &super::super::next_use::NextUseAnalysis,
    ) -> SpillPlan {
        let mut plan = spill_plan::plan(func, cfg, next_use, 32).unwrap();
        plan.point_ops.clear();
        plan.edge_ops.clear();
        plan.recipe_reloads.clear();
        plan.recipe_homes.clear();
        plan.state_homes.clear();
        plan.state_reload_recipes.clear();
        plan
    }

    fn point(instruction: usize) -> super::super::spill_plan::ProgramPoint {
        super::super::spill_plan::ProgramPoint {
            block: BlockId(0),
            instruction,
            side: super::super::spill_plan::PointSide::Before,
        }
    }

    #[test]
    fn branch_edge_operations_use_the_single_predecessor_successor_entry() {
        let mut vregs = VRegAllocator::new();
        let condition = vregs.alloc();
        let value = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 2]);

        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: condition,
            value: 1,
        });
        entry.push(MInst::LoadImm {
            dst: value,
            value: 0x1234,
        });
        entry.push(MInst::Branch {
            cond: condition,
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        });
        let mut left = MBlock::new(BlockId(1));
        left.push(MInst::Return);
        let mut right = MBlock::new(BlockId(2));
        right.push(MInst::Return);
        func.blocks = vec![entry, left, right];

        let cfg = cfg::normalize(&mut func).unwrap();
        let next_use = next_use::analyze(&func, &cfg).unwrap();
        let mut plan = blank_plan(&func, &cfg, &next_use);
        let entry = cfg.block_index[&BlockId(0)];
        let successor = cfg.successors[entry][0];
        assert_eq!(cfg.predecessors[successor], [entry]);

        let logical = plan.logical.of(value);
        let spill_home = plan.homes.of_vreg(value);
        plan.edge_ops.insert(
            (entry, successor),
            vec![
                PlannedEdgeOp::Spill {
                    source: logical,
                    destination: logical,
                    destination_home: spill_home,
                },
                PlannedEdgeOp::Reload {
                    source: logical,
                    source_home: spill_home,
                    destination: logical,
                },
            ],
        );

        assert_eq!(
            planned_spills(&func, &cfg, &plan).unwrap(),
            vec![PlannedSpillInsertion {
                block: successor,
                instruction: 0,
                value: logical,
                home: spill_home,
                edge_transfer: false,
            }]
        );
        let queries = super::super::ssa::planner_reload_queries(&func, &cfg, &plan).unwrap();
        assert_eq!(
            queries,
            BTreeSet::from([PointUse {
                block: func.blocks[successor].id,
                instruction: 0,
                value: VReg(logical.0),
            }])
        );
    }

    #[test]
    fn selected_home_reconstructs_as_verified_state_store_and_load() {
        let mut vregs = VRegAllocator::new();
        let value = vregs.alloc();
        let marker = vregs.alloc();
        let mut func = MFunction::new(
            vregs,
            vec![
                SpillDesc::transient().with_deferred_state_home(home(0, 0)),
                SpillDesc::transient(),
            ],
        );
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::LoadImm {
            dst: value,
            value: 0x1234,
        });
        block.push(MInst::LoadImm {
            dst: marker,
            value: 1,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 16,
            src: value,
            size: OpSize::S64,
        });
        block.push(MInst::Return);
        func.blocks.push(block);

        let cfg = cfg::normalize(&mut func).unwrap();
        let next_use = next_use::analyze(&func, &cfg).unwrap();
        let mut plan = blank_plan(&func, &cfg, &next_use);
        let logical = plan.logical.of(value);
        let spill_home = plan.homes.of_vreg(value);
        plan.point_ops = vec![
            (
                point(1),
                PlannedOp::Spill {
                    value: logical,
                    home: spill_home,
                },
            ),
            (
                point(2),
                PlannedOp::Reload {
                    value: logical,
                    home: spill_home,
                },
            ),
        ];

        select(&func, &cfg, &mut plan).unwrap();
        assert_eq!(plan.state_homes.get(&spill_home), Some(&home(0, 0)));
        assert_eq!(plan.state_reload_recipes.len(), 1);

        let requested = super::super::ssa::planner_reload_queries(&func, &cfg, &plan).unwrap();
        let ordinary_recipes = reload::analyze_with_queries(&func, &cfg, &requested).unwrap();
        let result = reconstruct::reconstruct(
            &mut func,
            &cfg,
            &plan,
            &next_use,
            &ordinary_recipes,
            false,
            true,
        )
        .unwrap();
        assert_eq!(result.frame_size, 0);
        assert!(func.blocks[0].insts.iter().any(|inst| {
            matches!(
                inst,
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 0,
                    size: OpSize::S64,
                    ..
                }
            )
        }));
        assert!(func.blocks[0].insts.iter().any(|inst| {
            matches!(
                inst,
                MInst::Load {
                    base: BaseReg::SimState,
                    offset: 0,
                    size: OpSize::S64,
                    ..
                }
            )
        }));
        assert!(!func.blocks[0].insts.iter().any(|inst| {
            matches!(
                inst,
                MInst::Store {
                    base: BaseReg::StackFrame,
                    ..
                } | MInst::Load {
                    base: BaseReg::StackFrame,
                    ..
                }
            )
        }));
        super::super::materialized_state_home::verify_materialized_state_homes(
            &func,
            &cfg,
            &result.state_stores,
            &result.state_reloads,
        )
        .unwrap();
        reload::verify_expected_materialized_reloads(&func, &cfg, &result.recipe_reloads).unwrap();
    }

    #[test]
    fn hazardous_selected_home_falls_back_to_stack() {
        let mut vregs = VRegAllocator::new();
        let value = vregs.alloc();
        let marker = vregs.alloc();
        let mut func = MFunction::new(
            vregs,
            vec![
                SpillDesc::transient().with_deferred_state_home(home(0, 0)),
                SpillDesc::transient(),
            ],
        );
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::LoadImm {
            dst: value,
            value: 0x1234,
        });
        block.push(MInst::LoadImm {
            dst: marker,
            value: 1,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 16,
            src: value,
            size: OpSize::S64,
        });
        block.push(MInst::Return);
        func.blocks.push(block);

        let cfg = cfg::normalize(&mut func).unwrap();
        let next_use = next_use::analyze(&func, &cfg).unwrap();
        let mut plan = blank_plan(&func, &cfg, &next_use);
        let logical = plan.logical.of(value);
        let spill_home = plan.homes.of_vreg(value);
        plan.point_ops = vec![
            (
                point(1),
                PlannedOp::Spill {
                    value: logical,
                    home: spill_home,
                },
            ),
            (
                point(2),
                PlannedOp::Reload {
                    value: logical,
                    home: spill_home,
                },
            ),
        ];
        select(&func, &cfg, &mut plan).unwrap();
        assert!(plan.state_homes.contains_key(&spill_home));

        fallback_to_stack(&func, &cfg, &mut plan, &BTreeSet::from([spill_home])).unwrap();

        assert!(!plan.state_homes.contains_key(&spill_home));
        assert!(plan.state_reload_recipes.is_empty());
        let requested = super::super::ssa::planner_reload_queries(&func, &cfg, &plan).unwrap();
        let ordinary_recipes = reload::analyze_with_queries(&func, &cfg, &requested).unwrap();
        let result = reconstruct::reconstruct(
            &mut func,
            &cfg,
            &plan,
            &next_use,
            &ordinary_recipes,
            false,
            true,
        )
        .unwrap();
        assert_eq!(result.frame_size, 8);
        assert!(func.blocks[0].insts.iter().any(|inst| {
            matches!(
                inst,
                MInst::Store {
                    base: BaseReg::StackFrame,
                    ..
                }
            )
        }));
        assert!(func.blocks[0].insts.iter().any(|inst| {
            matches!(
                inst,
                MInst::Load {
                    base: BaseReg::StackFrame,
                    ..
                }
            )
        }));
    }

    #[test]
    fn overlapping_original_write_rejects_deferred_state_home() {
        let mut vregs = VRegAllocator::new();
        let value = vregs.alloc();
        let clobber = vregs.alloc();
        let mut func = MFunction::new(
            vregs,
            vec![
                SpillDesc::transient().with_deferred_state_home(home(0, 0)),
                SpillDesc::transient(),
            ],
        );
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::LoadImm {
            dst: value,
            value: 0x1234,
        });
        block.push(MInst::LoadImm {
            dst: clobber,
            value: 0x5678,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 0,
            src: clobber,
            size: OpSize::S64,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 16,
            src: value,
            size: OpSize::S64,
        });
        block.push(MInst::Return);
        func.blocks.push(block);

        let cfg = cfg::normalize(&mut func).unwrap();
        let next_use = next_use::analyze(&func, &cfg).unwrap();
        let mut plan = blank_plan(&func, &cfg, &next_use);
        let logical = plan.logical.of(value);
        let spill_home = plan.homes.of_vreg(value);
        plan.point_ops = vec![
            (
                point(1),
                PlannedOp::Spill {
                    value: logical,
                    home: spill_home,
                },
            ),
            (
                point(3),
                PlannedOp::Reload {
                    value: logical,
                    home: spill_home,
                },
            ),
        ];

        select(&func, &cfg, &mut plan).unwrap();
        assert!(plan.state_homes.is_empty());
        assert!(plan.state_reload_recipes.is_empty());
    }
}
