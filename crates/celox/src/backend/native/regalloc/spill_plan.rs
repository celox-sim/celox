//! Braun--Hack sections 4.2 and 4.3: W/S states and coupling plan.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::backend::native::mir::{BlockId, MFunction, MInst, PackedStateHome, VReg};

use super::assignment::clobbers;
use super::cfg::NormalizedCfg;
use super::next_use::{NextUseAnalysis, NextUseDistance};
use super::reload::{EdgeUse, PlanningRecipes, PointUse, ReloadRecipeAnalysis, ResolvedRecipe};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct LogicalValue(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct SpillHome(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PointSide {
    Before,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ProgramPoint {
    pub block: BlockId,
    pub instruction: usize,
    pub side: PointSide,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PlannedOp {
    Spill {
        value: LogicalValue,
        home: SpillHome,
    },
    Reload {
        value: LogicalValue,
        home: SpillHome,
    },
    SpillPhi {
        value: LogicalValue,
        home: SpillHome,
    },
}

/// Materialization on one CFG edge.
///
/// A point operation reads or writes the home of one logical SSA value.  A
/// phi edge is different: it transfers a predecessor value into the
/// successor's logical identity.  Keeping both identities explicit prevents
/// a reload from accidentally reading the successor home when only the
/// predecessor home is valid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PlannedEdgeOp {
    Spill {
        source: LogicalValue,
        destination: LogicalValue,
        destination_home: SpillHome,
    },
    Reload {
        source: LogicalValue,
        source_home: SpillHome,
        destination: LogicalValue,
    },
}

#[derive(Debug)]
pub(super) struct SpillPlan {
    pub logical: LogicalValues,
    pub homes: SpillHomes,
    pub point_ops: Vec<(ProgramPoint, PlannedOp)>,
    pub edge_ops: BTreeMap<(usize, usize), Vec<PlannedEdgeOp>>,
    /// Point reloads whose value is supplied by an independently verified
    /// path-specific MemorySSA recipe rather than a persistent spill home.
    pub recipe_reloads: BTreeSet<(BlockId, usize, LogicalValue)>,
    /// Phi-congruence homes whose complete selected reload set is supplied by
    /// exact rematerialization recipes instead of a stack slot.
    pub recipe_homes: BTreeSet<SpillHome>,
    /// Phi-congruence homes assigned to allocator-managed packed SimState
    /// words.  These homes remain ordinary W/S homes: unlike recipe-only
    /// homes, every path must execute the planned spill before a reload.
    pub state_homes: BTreeMap<SpillHome, PackedStateHome>,
    /// Exact MemorySSA recipe for every reload assigned to `state_homes`.
    /// Keys retain the pre-reconstruction insertion point; reconstruction
    /// independently proves the emitted load against final MIR.
    pub state_reload_recipes: BTreeMap<(BlockId, usize, LogicalValue), ResolvedRecipe>,
    pub w_entry: Vec<BTreeSet<LogicalValue>>,
    pub w_exit: Vec<BTreeSet<LogicalValue>>,
    pub s_entry: Vec<BTreeSet<LogicalValue>>,
    pub s_exit: Vec<BTreeSet<LogicalValue>>,
}

#[derive(Debug)]
struct BlockTransition {
    point_ops: Vec<(ProgramPoint, PlannedOp)>,
    recipe_reloads: BTreeSet<(BlockId, usize, LogicalValue)>,
    w_exit: BTreeSet<LogicalValue>,
    s_exit: BTreeSet<LogicalValue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SpillPlanError {
    pub rule: &'static str,
    pub block: Option<BlockId>,
    pub instruction: Option<usize>,
    pub values: Vec<VReg>,
    pub message: String,
}

impl SpillPlanError {
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
}

#[derive(Debug)]
pub(super) struct LogicalValues {
    count: u32,
}

impl LogicalValues {
    fn build(func: &MFunction) -> Self {
        Self {
            count: func.vregs.count(),
        }
    }

    pub(super) fn of(&self, value: VReg) -> LogicalValue {
        LogicalValue(value.0)
    }

    fn checked_of(
        &self,
        value: VReg,
        block: Option<BlockId>,
        instruction: Option<usize>,
    ) -> Result<LogicalValue, SpillPlanError> {
        if value.0 >= self.count {
            return Err(SpillPlanError::new(
                "SPILL_PLAN.VALUE_RANGE",
                block,
                instruction,
                vec![value],
                format!(
                    "v{} is outside the spill plan's {} logical values",
                    value.0, self.count
                ),
            ));
        }
        Ok(LogicalValue(value.0))
    }
}

#[derive(Debug)]
pub(super) struct SpillHomes {
    count: u32,
}

impl SpillHomes {
    fn build(func: &MFunction) -> Result<Self, SpillPlanError> {
        let count = func.vregs.count() as usize;
        for block in &func.blocks {
            for phi in &block.phis {
                if phi.dst.0 as usize >= count {
                    return Err(SpillPlanError::new(
                        "SPILL_PLAN.VALUE_RANGE",
                        Some(block.id),
                        None,
                        vec![phi.dst],
                        format!(
                            "phi destination v{} is outside the function's {count} virtual registers",
                            phi.dst.0
                        ),
                    ));
                }
                for &(_, source) in &phi.sources {
                    if source.0 as usize >= count {
                        return Err(SpillPlanError::new(
                            "SPILL_PLAN.VALUE_RANGE",
                            Some(block.id),
                            None,
                            vec![source],
                            format!(
                                "phi source v{} is outside the function's {count} virtual registers",
                                source.0
                            ),
                        ));
                    }
                }
            }
        }
        Ok(Self {
            count: count as u32,
        })
    }

    pub(super) fn of_vreg(&self, value: VReg) -> SpillHome {
        debug_assert!(value.0 < self.count);
        SpillHome(value.0)
    }

    pub(super) fn of_logical(&self, value: LogicalValue) -> SpillHome {
        debug_assert!(value.0 < self.count);
        SpillHome(value.0)
    }

    pub(super) fn members(&self, home: SpillHome) -> impl Iterator<Item = VReg> + '_ {
        (home.0 < self.count).then_some(VReg(home.0)).into_iter()
    }
}

#[derive(Debug, Default)]
struct EdgeTranslation {
    to_successor: HashMap<LogicalValue, Vec<LogicalValue>>,
    to_predecessor: HashMap<LogicalValue, LogicalValue>,
}

/// Indexed logical-value translation across normalized phi edges.
///
/// Building the two directions in one pass over phi operands avoids rescanning
/// every phi (and its predecessor list) for every member of W/S. A destination
/// has exactly one source on an edge. A legal non-CSSA source may feed several
/// phi destinations, so the forward relation is one-to-many.
#[derive(Debug)]
struct EdgeTranslations {
    by_edge: HashMap<(usize, usize), EdgeTranslation>,
}

impl EdgeTranslations {
    fn build(
        func: &MFunction,
        cfg: &NormalizedCfg,
        logical: &LogicalValues,
    ) -> Result<Self, SpillPlanError> {
        let mut by_edge = HashMap::<(usize, usize), EdgeTranslation>::new();
        for (successor, block) in func.blocks.iter().enumerate() {
            for phi in &block.phis {
                let destination = logical.checked_of(phi.dst, Some(block.id), None)?;
                for &(predecessor_id, source) in &phi.sources {
                    let Some(&predecessor) = cfg.block_index.get(&predecessor_id) else {
                        return Err(SpillPlanError::new(
                            "SPILL_PLAN.PHI_PREDECESSOR",
                            Some(block.id),
                            None,
                            vec![source, phi.dst],
                            format!(
                                "phi source predecessor {predecessor_id} is absent from the normalized CFG"
                            ),
                        ));
                    };
                    if !cfg
                        .successors
                        .get(predecessor)
                        .is_some_and(|successors| successors.contains(&successor))
                    {
                        return Err(SpillPlanError::new(
                            "SPILL_PLAN.EDGE_EXISTS",
                            Some(predecessor_id),
                            None,
                            vec![source, phi.dst],
                            format!(
                                "phi edge {predecessor_id} -> {} is absent from the normalized CFG",
                                block.id
                            ),
                        ));
                    }
                    let source = logical.checked_of(source, Some(predecessor_id), None)?;
                    let translation = by_edge.entry((predecessor, successor)).or_default();
                    translation
                        .to_successor
                        .entry(source)
                        .or_default()
                        .push(destination);
                    if translation
                        .to_predecessor
                        .insert(destination, source)
                        .is_some()
                    {
                        return Err(SpillPlanError::new(
                            "SPILL_PLAN.PHI_DESTINATION_UNIQUE",
                            Some(predecessor_id),
                            None,
                            vec![VReg(source.0), VReg(destination.0)],
                            format!(
                                "phi destination v{} has duplicate source for {predecessor_id}",
                                destination.0
                            ),
                        ));
                    }
                }
            }
        }
        Ok(Self { by_edge })
    }

    fn to_successors(
        &self,
        predecessor: usize,
        successor: usize,
        value: LogicalValue,
    ) -> impl Iterator<Item = LogicalValue> + '_ {
        let destinations = self
            .by_edge
            .get(&(predecessor, successor))
            .and_then(|translation| translation.to_successor.get(&value))
            .map(Vec::as_slice)
            .unwrap_or_default();
        destinations
            .iter()
            .copied()
            .chain(destinations.is_empty().then_some(value))
    }

    fn to_predecessor(
        &self,
        predecessor: usize,
        successor: usize,
        value: LogicalValue,
    ) -> LogicalValue {
        self.by_edge
            .get(&(predecessor, successor))
            .and_then(|translation| translation.to_predecessor.get(&value))
            .copied()
            .unwrap_or(value)
    }
}

#[cfg(test)]
pub(super) fn plan(
    func: &MFunction,
    cfg: &NormalizedCfg,
    next_use: &NextUseAnalysis,
    registers: usize,
) -> Result<SpillPlan, SpillPlanError> {
    let planning_recipes = PlanningRecipes::stack_only(func.vregs.count());
    plan_with_recipe_costs(func, cfg, next_use, &planning_recipes, registers)
}

#[cfg(test)]
pub(super) fn plan_with_recipe_costs(
    func: &MFunction,
    cfg: &NormalizedCfg,
    next_use: &NextUseAnalysis,
    planning_recipes: &PlanningRecipes,
    registers: usize,
) -> Result<SpillPlan, SpillPlanError> {
    let mut working = func.clone();
    plan_internal(
        &mut working,
        cfg,
        next_use,
        planning_recipes,
        registers,
        None,
    )
}

pub(super) fn plan_with_integrated_schedule(
    func: &mut MFunction,
    cfg: &NormalizedCfg,
    next_use: &NextUseAnalysis,
    planning_recipes: &PlanningRecipes,
    registers: usize,
    constraints: &super::constraints::ConstraintModel,
) -> Result<SpillPlan, SpillPlanError> {
    plan_internal(
        func,
        cfg,
        next_use,
        planning_recipes,
        registers,
        Some(constraints),
    )
}

fn plan_internal(
    func: &mut MFunction,
    cfg: &NormalizedCfg,
    next_use: &NextUseAnalysis,
    planning_recipes: &PlanningRecipes,
    registers: usize,
    constraints: Option<&super::constraints::ConstraintModel>,
) -> Result<SpillPlan, SpillPlanError> {
    let logical = LogicalValues::build(func);
    let homes = SpillHomes::build(func)?;
    let edge_translations = EdgeTranslations::build(func, cfg, &logical)?;
    let mut result = SpillPlan {
        logical,
        homes,
        point_ops: Vec::new(),
        edge_ops: BTreeMap::new(),
        recipe_reloads: BTreeSet::new(),
        recipe_homes: BTreeSet::new(),
        state_homes: BTreeMap::new(),
        state_reload_recipes: BTreeMap::new(),
        w_entry: vec![BTreeSet::new(); func.blocks.len()],
        w_exit: vec![BTreeSet::new(); func.blocks.len()],
        s_entry: vec![BTreeSet::new(); func.blocks.len()],
        s_exit: vec![BTreeSet::new(); func.blocks.len()],
    };
    for block in 0..func.blocks.len() {
        let entry = if let Some(region) = next_use.region_at_entry(block) {
            init_loop_region(func, next_use, &result, block, region, registers)?
        } else {
            init_usual(
                func,
                cfg,
                next_use,
                planning_recipes,
                &result,
                &edge_translations,
                block,
                registers,
            )
        };
        let mut entry = entry;
        let live_entry = next_use.entry[block]
            .keys()
            .copied()
            .map(|value| {
                result
                    .logical
                    .checked_of(value, Some(func.blocks[block].id), Some(0))
            })
            .collect::<Result<BTreeSet<_>, _>>()?;
        let exit_reload_costs = constraints
            .map(|_| {
                exit_reload_costs(
                    func,
                    cfg,
                    next_use,
                    planning_recipes,
                    &result.logical,
                    &edge_translations,
                    block,
                )
            })
            .transpose()?
            .unwrap_or_default();
        let (spilled, transition, order) = loop {
            // S means that a valid home exists on every path.  Every live
            // value omitted from W_entry therefore requires a home; edge
            // coupling below materializes any missing predecessor store.  A
            // resident value keeps an existing home only when every
            // predecessor already has one.
            let spilled =
                spilled_at_entry(cfg, &result, &edge_translations, block, &live_entry, &entry);
            let (transition, order) = if let Some(constraints) = constraints {
                let instruction_constraints =
                    constraints.instructions.get(block).ok_or_else(|| {
                        SpillPlanError::new(
                            "SPILL_PLAN.SCHEDULE_ORDER",
                            Some(func.blocks[block].id),
                            None,
                            Vec::new(),
                            "allocation constraints do not cover this block",
                        )
                    })?;
                let (transition, order) = plan_scheduled_block_transition(
                    func,
                    next_use,
                    planning_recipes,
                    &result.logical,
                    &result.homes,
                    block,
                    registers,
                    &entry,
                    spilled.clone(),
                    instruction_constraints,
                    &exit_reload_costs,
                )?;
                (transition, Some(order))
            } else {
                (
                    plan_block_transition(
                        func,
                        next_use,
                        planning_recipes,
                        &result.logical,
                        &result.homes,
                        block,
                        registers,
                        &entry,
                        spilled.clone(),
                    )?,
                    None,
                )
            };
            let rejected = entry_residents_evicted_before_first_use(
                func,
                next_use,
                block,
                &entry,
                &transition,
                order.as_deref(),
            );
            if rejected.is_empty() {
                break (spilled, transition, order);
            }
            entry.retain(|value| !rejected.contains(value));
        };
        if let Some(order) = order {
            let original = func.blocks[block].insts.clone();
            func.blocks[block].insts = order
                .into_iter()
                .map(|source| original[source].clone())
                .collect();
        }
        result.w_entry[block] = entry;
        result.s_entry[block] = spilled;
        result.point_ops.extend(transition.point_ops);
        result.recipe_reloads.extend(transition.recipe_reloads);
        result.w_exit[block] = transition.w_exit;
        result.s_exit[block] = transition.s_exit;
    }

    // Section 4.3.  Delaying this until every W/S exit is known is equivalent
    // to the paper's deferred handling of not-yet-processed backedges.
    let spilled_phis = result
        .point_ops
        .iter()
        .filter_map(|(_, operation)| match operation {
            PlannedOp::SpillPhi { value, .. } => Some(*value),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    for successor in 0..func.blocks.len() {
        for &predecessor in &cfg.predecessors[successor] {
            let mut resident_spills = Vec::new();
            let mut home_transfers = Vec::new();
            let mut scratch_reloads = Vec::new();
            let mut resident_reloads = Vec::new();
            let predecessor_w = result.w_exit[predecessor].clone();
            let predecessor_s = result.s_exit[predecessor].clone();
            for &successor_value in &result.w_entry[successor] {
                let value =
                    edge_translations.to_predecessor(predecessor, successor, successor_value);
                if !predecessor_w.contains(&value) {
                    resident_reloads.push(PlannedEdgeOp::Reload {
                        source: value,
                        source_home: result.homes.of_logical(value),
                        destination: successor_value,
                    });
                }
            }
            for &successor_value in &result.s_entry[successor] {
                let value =
                    edge_translations.to_predecessor(predecessor, successor, successor_value);
                let source_home = result.homes.of_logical(value);
                let destination_home = result.homes.of_logical(successor_value);
                if source_home == destination_home && predecessor_s.contains(&value) {
                    continue;
                }
                if predecessor_w.contains(&value) {
                    resident_spills.push(PlannedEdgeOp::Spill {
                        source: value,
                        destination: successor_value,
                        destination_home,
                    });
                } else if predecessor_s.contains(&value) {
                    // A phi transfer between independent homes is a short
                    // edge-local reload/store pair.  Keeping the predecessor
                    // SSA value live merely to copy its successor home would
                    // recreate the phi-web live range this representation is
                    // intended to remove.
                    home_transfers.push(PlannedEdgeOp::Reload {
                        source: value,
                        source_home,
                        destination: successor_value,
                    });
                    home_transfers.push(PlannedEdgeOp::Spill {
                        source: successor_value,
                        destination: successor_value,
                        destination_home,
                    });
                }
            }
            for phi in &func.blocks[successor].phis {
                let destination = result.logical.of(phi.dst);
                if !spilled_phis.contains(&destination) {
                    continue;
                }
                let source = edge_translations.to_predecessor(predecessor, successor, destination);
                let source_home = result.homes.of_logical(source);
                let destination_home = result.homes.of_logical(destination);
                if source_home == destination_home && predecessor_s.contains(&source) {
                    continue;
                }
                if predecessor_w.contains(&source) {
                    resident_spills.push(PlannedEdgeOp::Spill {
                        source,
                        destination,
                        destination_home,
                    });
                } else if predecessor_s.contains(&source) {
                    home_transfers.push(PlannedEdgeOp::Reload {
                        source,
                        source_home,
                        destination,
                    });
                    home_transfers.push(PlannedEdgeOp::Spill {
                        source: destination,
                        destination,
                        destination_home,
                    });
                }
            }
            // A reload/store home transfer needs one transient register.
            // When every successor-resident value already survives in
            // predecessor W, no ordinary edge spill or reload creates that
            // slot.  Explicitly park one such value across all home
            // transfers.  Treating edge operations as free parallel copies
            // here produces NUM_REGS + 1 live values after reconstruction.
            if !home_transfers.is_empty() {
                let surviving_residents = result.w_entry[successor]
                    .iter()
                    .copied()
                    .filter_map(|destination| {
                        let source =
                            edge_translations.to_predecessor(predecessor, successor, destination);
                        predecessor_w
                            .contains(&source)
                            .then_some((source, destination))
                    })
                    .collect::<Vec<_>>();
                if surviving_residents.len() == registers {
                    let (source, destination) = surviving_residents[0];
                    let destination_home = result.homes.of_logical(destination);
                    if !resident_spills.iter().any(|operation| {
                        matches!(
                            operation,
                            PlannedEdgeOp::Spill {
                                source: spill_source,
                                destination: spill_destination,
                                destination_home: spill_home,
                            } if *spill_source == source
                                && *spill_destination == destination
                                && *spill_home == destination_home
                        )
                    }) {
                        resident_spills.push(PlannedEdgeOp::Spill {
                            source,
                            destination,
                            destination_home,
                        });
                    }
                    scratch_reloads.push(PlannedEdgeOp::Reload {
                        source: destination,
                        source_home: destination_home,
                        destination,
                    });
                }
            }
            // Consume edge-resident values before introducing reload
            // temporaries or successor live-ins.  Otherwise one transient
            // transfer can raise an already-full W_exit above capacity.
            let mut operations = resident_spills;
            operations.extend(home_transfers);
            operations.extend(scratch_reloads);
            operations.extend(resident_reloads);
            if !operations.is_empty() {
                result.edge_ops.insert((predecessor, successor), operations);
            }
        }
    }
    Ok(result)
}

#[allow(clippy::too_many_arguments)]
fn exit_reload_costs(
    func: &MFunction,
    cfg: &NormalizedCfg,
    next_use: &NextUseAnalysis,
    planning_recipes: &PlanningRecipes,
    logical: &LogicalValues,
    edge_translations: &EdgeTranslations,
    block: usize,
) -> Result<HashMap<LogicalValue, u32>, SpillPlanError> {
    let mut costs = HashMap::<LogicalValue, u32>::new();
    for &successor in &cfg.successors[block] {
        let mut demanded = BTreeSet::<LogicalValue>::new();
        for &value in next_use.entry[successor].keys() {
            let destination =
                logical.checked_of(value, Some(func.blocks[successor].id), Some(0))?;
            demanded.insert(edge_translations.to_predecessor(block, successor, destination));
        }
        for phi in &func.blocks[successor].phis {
            if let Some((_, source)) = phi
                .sources
                .iter()
                .find(|(predecessor, _)| *predecessor == func.blocks[block].id)
            {
                demanded.insert(logical.checked_of(
                    *source,
                    Some(func.blocks[block].id),
                    Some(func.blocks[block].insts.len()),
                )?);
            }
        }
        for value in demanded {
            let reload = u32::from(reload_cost_on_edge(
                func,
                planning_recipes,
                block,
                successor,
                value,
            ));
            costs
                .entry(value)
                .and_modify(|cost| *cost = cost.saturating_add(reload))
                .or_insert(reload);
        }
    }
    Ok(costs)
}

fn spilled_at_entry(
    cfg: &NormalizedCfg,
    plan: &SpillPlan,
    edge_translations: &EdgeTranslations,
    block: usize,
    live_entry: &BTreeSet<LogicalValue>,
    resident: &BTreeSet<LogicalValue>,
) -> BTreeSet<LogicalValue> {
    let mut spilled = live_entry
        .difference(resident)
        .copied()
        .collect::<BTreeSet<_>>();
    if !cfg.predecessors[block].is_empty() {
        spilled.extend(resident.iter().copied().filter(|value| {
            cfg.predecessors[block].iter().all(|predecessor| {
                let predecessor_value =
                    edge_translations.to_predecessor(*predecessor, block, *value);
                plan.s_exit[*predecessor].contains(&predecessor_value)
            })
        }));
    }
    spilled
}

/// Remove optimistic entry residents that do not survive to their first local
/// use.  Keeping such a value in W moves its inevitable store from the incoming
/// edge into the block and occupies a register before providing any use.  S is
/// therefore no more expensive on the executed path and can reuse an already
/// valid predecessor home.
///
/// Replanning is bounded by the initial W size, which is at most the target's
/// fixed register count.  Each iteration removes at least one value, so this
/// remains linear in MIR size for a fixed ISA and needs no CFG-sized copy.
fn entry_residents_evicted_before_first_use(
    func: &MFunction,
    next_use: &NextUseAnalysis,
    block: usize,
    resident: &BTreeSet<LogicalValue>,
    transition: &BlockTransition,
    order: Option<&[usize]>,
) -> BTreeSet<LogicalValue> {
    let first_use = |value: LogicalValue| {
        order.map_or_else(
            || next_use.next_local_use(block, 0, VReg(value.0)),
            |order| {
                order.iter().position(|&source| {
                    func.blocks[block].insts[source]
                        .uses()
                        .contains(&VReg(value.0))
                })
            },
        )
    };
    transition
        .point_ops
        .iter()
        .filter_map(|(point, operation)| match *operation {
            PlannedOp::Spill { value, .. }
                if resident.contains(&value)
                    && first_use(value).is_none_or(|first_use| point.instruction < first_use) =>
            {
                Some(value)
            }
            PlannedOp::Reload { value, .. }
                if resident.contains(&value)
                    && !transition.recipe_reloads.contains(&(
                        func.blocks[block].id,
                        point.instruction,
                        value,
                    ))
                    && first_use(value) == Some(point.instruction) =>
            {
                Some(value)
            }
            _ => None,
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn plan_block_transition(
    func: &MFunction,
    next_use: &NextUseAnalysis,
    planning_recipes: &PlanningRecipes,
    logical: &LogicalValues,
    homes: &SpillHomes,
    block: usize,
    registers: usize,
    w_entry: &BTreeSet<LogicalValue>,
    spilled: BTreeSet<LogicalValue>,
) -> Result<BlockTransition, SpillPlanError> {
    let mut planner = BlockTransitionPlanner::new(
        func,
        next_use,
        planning_recipes,
        logical,
        homes,
        block,
        registers,
        w_entry,
        spilled,
    )?;
    for (instruction, inst) in func.blocks[block].insts.iter().enumerate() {
        planner.step(
            TransitionPoint {
                output: instruction,
                source: instruction,
            },
            inst,
        )?;
    }
    planner.finish()
}

#[derive(Debug, Clone, Copy)]
struct TransitionPoint {
    /// Instruction position in the final block order.
    output: usize,
    /// Stable position in the order consumed by next-use and planning-recipe
    /// analysis.
    source: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct AllocationCandidateScore {
    blocked_by_deferred_reload: bool,
    continuation_tie: std::cmp::Reverse<bool>,
    resident_operand_tie: std::cmp::Reverse<usize>,
    operand_tie: std::cmp::Reverse<usize>,
    materialization_cost: u32,
    pressure_delta: isize,
    preferred_rank: usize,
    source: usize,
}

struct AllocationReadyQueue {
    ordered: BTreeSet<(AllocationCandidateScore, usize)>,
    by_source: BTreeSet<(usize, usize)>,
    scores: Vec<Option<AllocationCandidateScore>>,
}

impl AllocationReadyQueue {
    fn new(instructions: usize) -> Self {
        Self {
            ordered: BTreeSet::new(),
            by_source: BTreeSet::new(),
            scores: vec![None; instructions],
        }
    }

    fn insert(&mut self, instruction: usize, score: AllocationCandidateScore) {
        debug_assert!(self.scores[instruction].is_none());
        self.ordered.insert((score, instruction));
        self.by_source.insert((score.source, instruction));
        self.scores[instruction] = Some(score);
    }

    fn refresh(&mut self, instruction: usize, score: AllocationCandidateScore) {
        let Some(previous) = self.scores[instruction].replace(score) else {
            return;
        };
        self.ordered.remove(&(previous, instruction));
        self.ordered.insert((score, instruction));
    }

    fn pop(
        &mut self,
        current_resident: usize,
        register_capacity: usize,
        demanded: Option<usize>,
    ) -> Option<(AllocationCandidateScore, usize)> {
        let &(source, source_instruction) = self.by_source.first()?;
        let source_score = self.scores[source_instruction]?;
        debug_assert_eq!(source, source_score.source);
        let source_projected = if source_score.pressure_delta < 0 {
            current_resident.saturating_sub(source_score.pressure_delta.unsigned_abs())
        } else {
            current_resident.saturating_add(source_score.pressure_delta as usize)
        };
        let &(best_score, best_instruction) = self.ordered.first()?;
        let demanded = demanded.and_then(|instruction| {
            let score = self.scores[instruction]?;
            let projected = if score.pressure_delta < 0 {
                current_resident.saturating_sub(score.pressure_delta.unsigned_abs())
            } else {
                current_resident.saturating_add(score.pressure_delta as usize)
            };
            (!score.blocked_by_deferred_reload && projected <= register_capacity)
                .then_some((score, instruction))
        });
        let (score, instruction) = if let Some(demanded) = demanded {
            demanded
        } else if !best_score.blocked_by_deferred_reload && best_score.continuation_tie.0 {
            (best_score, best_instruction)
        } else if !source_score.blocked_by_deferred_reload && source_projected <= register_capacity
        {
            (source_score, source_instruction)
        } else {
            (best_score, best_instruction)
        };
        self.ordered.remove(&(score, instruction));
        self.by_source.remove(&(score.source, instruction));
        self.scores[instruction] = None;
        Some((score, instruction))
    }

    fn contains(&self, instruction: usize) -> bool {
        self.scores.get(instruction).is_some_and(Option::is_some)
    }
}

struct DependencyDemandFrame {
    instruction: usize,
    next_dependency: usize,
}

struct DependencyDemand {
    target: usize,
    stack: Vec<DependencyDemandFrame>,
}

impl DependencyDemand {
    fn new(target: usize) -> Self {
        Self {
            target,
            stack: vec![DependencyDemandFrame {
                instruction: target,
                next_dependency: 0,
            }],
        }
    }

    /// Follow one unfinished sink packet backwards until its next ready
    /// prerequisite. Each dependency edge in this demand is scanned once.
    fn next_ready(&mut self, region: &super::schedule::ForwardReadyRegion) -> Option<usize> {
        loop {
            let frame_index = self.stack.len().checked_sub(1)?;
            let instruction = self.stack[frame_index].instruction;
            if region.is_emitted(instruction) {
                self.stack.pop();
                continue;
            }
            let dependency = {
                let dependencies = region.dependencies(instruction);
                let frame = &mut self.stack[frame_index];
                let mut dependency = None;
                while frame.next_dependency != dependencies.len() {
                    let candidate = dependencies[frame.next_dependency];
                    frame.next_dependency += 1;
                    if !region.is_emitted(candidate) {
                        dependency = Some(candidate);
                        break;
                    }
                }
                dependency
            };
            if let Some(dependency) = dependency {
                self.stack.push(DependencyDemandFrame {
                    instruction: dependency,
                    next_dependency: 0,
                });
                continue;
            }
            return region.is_ready(instruction).then_some(instruction);
        }
    }
}

struct ScheduledStepDelta {
    resident: Vec<LogicalValue>,
    deferred: Vec<LogicalValue>,
    remaining_uses: Vec<LogicalValue>,
    continuation: Vec<LogicalValue>,
}

#[allow(clippy::too_many_arguments)]
fn plan_scheduled_block_transition(
    func: &MFunction,
    next_use: &NextUseAnalysis,
    planning_recipes: &PlanningRecipes,
    logical: &LogicalValues,
    homes: &SpillHomes,
    block: usize,
    registers: usize,
    w_entry: &BTreeSet<LogicalValue>,
    spilled: BTreeSet<LogicalValue>,
    constraints: &[super::constraints::InstructionConstraints],
    exit_reload_costs: &HashMap<LogicalValue, u32>,
) -> Result<(BlockTransition, Vec<usize>), SpillPlanError> {
    let instructions = &func.blocks[block].insts;
    if instructions.len() != constraints.len() {
        return Err(SpillPlanError::new(
            "SPILL_PLAN.SCHEDULE_ORDER",
            Some(func.blocks[block].id),
            None,
            Vec::new(),
            "allocation constraints do not cover every block instruction",
        ));
    }
    let mut remaining =
        RemainingBlockUses::build(func, next_use, logical, block, exit_reload_costs)?;
    let mut planner = BlockTransitionPlanner::new(
        func,
        next_use,
        planning_recipes,
        logical,
        homes,
        block,
        registers,
        w_entry,
        spilled,
    )?;
    let mut order = Vec::with_capacity(instructions.len());
    let mut cursor = 0usize;
    while cursor != instructions.len() {
        if !super::schedule::is_allocation_schedulable_at(instructions, constraints, cursor) {
            let output = order.len();
            planner.step_scheduled(
                TransitionPoint {
                    output,
                    source: cursor,
                },
                &instructions[cursor],
                &mut remaining,
            )?;
            order.push(cursor);
            cursor += 1;
            continue;
        }

        let start = cursor;
        while cursor != instructions.len()
            && super::schedule::is_allocation_schedulable_at(instructions, constraints, cursor)
        {
            cursor += 1;
        }
        let end = cursor;
        let region = &instructions[start..end];
        let mut ready = super::schedule::ForwardReadyRegion::build(region).ok_or_else(|| {
            SpillPlanError::new(
                "SPILL_PLAN.SCHEDULE_DEPENDENCY",
                Some(func.blocks[block].id),
                Some(start),
                Vec::new(),
                "movable region does not have a forward dependency order",
            )
        })?;
        let mut queue = AllocationReadyQueue::new(region.len());
        let sinks = ready.sinks().to_vec();
        let mut next_sink = 0usize;
        let mut demand = sinks.first().copied().map(DependencyDemand::new);
        for &local in ready.ready() {
            let source = start + local;
            queue.insert(
                local,
                planner.candidate_score(source, &instructions[source], &remaining)?,
            );
        }
        while !ready.is_complete() {
            let demanded = demand.as_mut().and_then(|demand| demand.next_ready(&ready));
            let Some((score, local)) =
                queue.pop(planner.resident.len(), planner.registers, demanded)
            else {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.SCHEDULE_DEPENDENCY",
                    Some(func.blocks[block].id),
                    Some(start),
                    Vec::new(),
                    "movable region has no dependency-ready allocation candidate",
                ));
            };
            let source = start + local;
            if score.blocked_by_deferred_reload {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.RECIPE_RELOAD_ORDER",
                    Some(func.blocks[block].id),
                    Some(source),
                    instructions[source].uses().to_vec(),
                    "every dependency-ready instruction precedes an allocator-selected recipe reload point",
                ));
            }
            let output = order.len();
            let delta = planner.step_scheduled(
                TransitionPoint { output, source },
                &instructions[source],
                &mut remaining,
            )?;
            let newly_ready = ready.emit(local).ok_or_else(|| {
                SpillPlanError::new(
                    "SPILL_PLAN.SCHEDULE_DEPENDENCY",
                    Some(func.blocks[block].id),
                    Some(source),
                    Vec::new(),
                    "selected instruction was not dependency-ready",
                )
            })?;
            order.push(source);
            if demand.as_ref().is_some_and(|demand| demand.target == local) {
                next_sink += 1;
                while sinks
                    .get(next_sink)
                    .is_some_and(|&sink| ready.is_emitted(sink))
                {
                    next_sink += 1;
                }
                demand = sinks.get(next_sink).copied().map(DependencyDemand::new);
            }

            let mut refresh = BTreeSet::<usize>::new();
            for local in newly_ready {
                refresh.insert(local);
            }
            for value in delta
                .resident
                .into_iter()
                .chain(delta.deferred)
                .chain(delta.remaining_uses)
                .chain(delta.continuation)
            {
                for &candidate in ready.use_candidates(VReg(value.0)) {
                    if ready.is_ready(candidate) {
                        refresh.insert(candidate);
                    }
                }
            }
            for local in refresh {
                let source = start + local;
                let candidate =
                    planner.candidate_score(source, &instructions[source], &remaining)?;
                if queue.contains(local) {
                    queue.refresh(local, candidate);
                } else {
                    queue.insert(local, candidate);
                }
            }
        }
    }
    Ok((planner.finish()?, order))
}

struct BlockTransitionPlanner<'a> {
    func: &'a MFunction,
    next_use: &'a NextUseAnalysis,
    planning_recipes: &'a PlanningRecipes,
    logical: &'a LogicalValues,
    homes: &'a SpillHomes,
    block: usize,
    registers: usize,
    transition: BlockTransition,
    resident: BTreeSet<LogicalValue>,
    spilled: BTreeSet<LogicalValue>,
    deferred_recipe_reloads: BTreeMap<LogicalValue, PointUse>,
    last_definition: Option<LogicalValue>,
}

impl<'a> BlockTransitionPlanner<'a> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        func: &'a MFunction,
        next_use: &'a NextUseAnalysis,
        planning_recipes: &'a PlanningRecipes,
        logical: &'a LogicalValues,
        homes: &'a SpillHomes,
        block: usize,
        registers: usize,
        w_entry: &BTreeSet<LogicalValue>,
        mut spilled: BTreeSet<LogicalValue>,
    ) -> Result<Self, SpillPlanError> {
        let mut transition = BlockTransition {
            point_ops: Vec::new(),
            recipe_reloads: BTreeSet::new(),
            w_exit: BTreeSet::new(),
            s_exit: BTreeSet::new(),
        };
        let resident = w_entry.clone();
        for phi in &func.blocks[block].phis {
            let value = logical.checked_of(phi.dst, Some(func.blocks[block].id), Some(0))?;
            if !resident.contains(&value) {
                transition.point_ops.push((
                    ProgramPoint {
                        block: func.blocks[block].id,
                        instruction: 0,
                        side: PointSide::Before,
                    },
                    PlannedOp::SpillPhi {
                        value,
                        home: homes.of_logical(value),
                    },
                ));
                spilled.insert(value);
            }
        }
        Ok(Self {
            func,
            next_use,
            planning_recipes,
            logical,
            homes,
            block,
            registers,
            transition,
            resident,
            spilled,
            deferred_recipe_reloads: BTreeMap::new(),
            last_definition: None,
        })
    }

    fn step(&mut self, point: TransitionPoint, inst: &MInst) -> Result<(), SpillPlanError> {
        let before = LinearFutureUses {
            func: self.func,
            next_use: self.next_use,
            block: self.block,
            instruction: point.source,
        };
        let uses = self.begin_step(point, inst, &before)?;
        let after = LinearFutureUses {
            func: self.func,
            next_use: self.next_use,
            block: self.block,
            instruction: point.source + 1,
        };
        self.finish_step(point, inst, &uses, &after)
    }

    fn step_scheduled(
        &mut self,
        point: TransitionPoint,
        inst: &MInst,
        remaining: &mut RemainingBlockUses,
    ) -> Result<ScheduledStepDelta, SpillPlanError> {
        let resident_before = self.resident.clone();
        let deferred_before = self.deferred_recipe_reloads.clone();
        let last_definition_before = self.last_definition;
        let uses = {
            let before = DynamicFutureUses(remaining);
            self.begin_step(point, inst, &before)?
        };
        let remaining_uses = remaining.emit(point.source, inst, self.logical)?;
        {
            let after = DynamicFutureUses(remaining);
            self.finish_step(point, inst, &uses, &after)?;
        }
        let resident = resident_before
            .symmetric_difference(&self.resident)
            .copied()
            .collect();
        let deferred = deferred_before
            .keys()
            .chain(self.deferred_recipe_reloads.keys())
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter(|value| deferred_before.get(value) != self.deferred_recipe_reloads.get(value))
            .collect();
        let continuation = [last_definition_before, self.last_definition]
            .into_iter()
            .flatten()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        Ok(ScheduledStepDelta {
            resident,
            deferred,
            remaining_uses,
            continuation,
        })
    }

    fn candidate_score(
        &self,
        source: usize,
        inst: &MInst,
        remaining: &RemainingBlockUses,
    ) -> Result<AllocationCandidateScore, SpillPlanError> {
        let block_id = self.func.blocks[self.block].id;
        let uses = inst
            .uses()
            .iter()
            .copied()
            .map(|value| self.logical.checked_of(value, Some(block_id), Some(source)))
            .collect::<Result<BTreeSet<_>, _>>()?;
        let mut blocked_by_deferred_reload = false;
        let mut missing = 0usize;
        let mut resident_operands = 0usize;
        let mut materialization_cost = 0u32;
        let costs = super::cost::MachineSpillCosts::with_recipes(self.func, self.planning_recipes);
        for &value in &uses {
            if self.resident.contains(&value) {
                resident_operands += 1;
                continue;
            }
            missing += 1;
            let point = PointUse {
                block: block_id,
                instruction: source,
                value: VReg(value.0),
            };
            if let Some(&expected) = self.deferred_recipe_reloads.get(&value) {
                if expected != point {
                    blocked_by_deferred_reload = true;
                    continue;
                }
                materialization_cost = materialization_cost.saturating_add(u32::from(
                    self.planning_recipes
                        .point_specific_materialization_cost(point)
                        .unwrap_or_else(|| costs.persistent_reload(VReg(value.0))),
                ));
            } else {
                materialization_cost = materialization_cost
                    .saturating_add(u32::from(costs.persistent_reload(VReg(value.0))));
            }
        }
        let dying = uses
            .iter()
            .filter(|&&value| remaining.remaining_uses(value) == 1 && !remaining.is_live_out(value))
            .count();
        let definition_live = inst
            .def()
            .map(|value| self.logical.checked_of(value, Some(block_id), Some(source)))
            .transpose()?
            .is_some_and(|value| {
                remaining.remaining_uses(value) != 0 || remaining.is_live_out(value)
            });
        let pressure_delta = isize::try_from(missing + usize::from(definition_live))
            .unwrap_or(isize::MAX)
            .saturating_sub(isize::try_from(dying).unwrap_or(isize::MAX));
        Ok(AllocationCandidateScore {
            blocked_by_deferred_reload,
            continuation_tie: std::cmp::Reverse(
                self.last_definition
                    .is_some_and(|value| uses.contains(&value)),
            ),
            // Reusing an operand already in W closes the residency cluster
            // which made that value worth keeping.  Rank this before a new
            // root with no operands; otherwise a stream of cheap Loads fills
            // W, spills those very results, and only then visits their uses.
            resident_operand_tie: std::cmp::Reverse(resident_operands),
            // Once W is full, an operand-bearing instruction closes existing
            // work (resident, spilled, or rematerializable).  A zero-input
            // root only creates another value which must displace such work.
            operand_tie: std::cmp::Reverse(uses.len()),
            materialization_cost,
            pressure_delta,
            // Preserve the incoming ISel order as the deterministic final
            // tie-breaker after dependency locality and the current W/S
            // transition have made no distinction.
            preferred_rank: source,
            source,
        })
    }

    fn begin_step(
        &mut self,
        point: TransitionPoint,
        inst: &MInst,
        future_uses: &impl FutureUses,
    ) -> Result<BTreeSet<LogicalValue>, SpillPlanError> {
        let block_id = self.func.blocks[self.block].id;
        let uses = inst
            .uses()
            .into_iter()
            .map(|value| {
                self.logical
                    .checked_of(value, Some(block_id), Some(point.output))
            })
            .collect::<Result<BTreeSet<_>, _>>()?;
        for &value in &uses {
            if self.resident.insert(value) {
                if let Some(expected) = self.deferred_recipe_reloads.remove(&value) {
                    let actual = PointUse {
                        block: block_id,
                        instruction: point.source,
                        value: VReg(value.0),
                    };
                    if expected != actual {
                        return Err(SpillPlanError::new(
                            "SPILL_PLAN.RECIPE_RELOAD_POINT",
                            Some(block_id),
                            Some(point.output),
                            vec![VReg(value.0)],
                            format!(
                                "deferred recipe reload expected {expected:?} but reached {actual:?}"
                            ),
                        ));
                    }
                    self.transition
                        .recipe_reloads
                        .insert((block_id, point.output, value));
                }
                self.transition.point_ops.push((
                    ProgramPoint {
                        block: block_id,
                        instruction: point.output,
                        side: PointSide::Before,
                    },
                    PlannedOp::Reload {
                        value,
                        home: self.homes.of_logical(value),
                    },
                ));
            }
        }
        limit(
            self.func,
            self.planning_recipes,
            self.homes,
            &mut self.transition.point_ops,
            self.block,
            point.output,
            self.registers,
            &uses,
            &mut self.resident,
            &mut self.spilled,
            &mut self.deferred_recipe_reloads,
            future_uses,
        )?;
        Ok(uses)
    }

    fn finish_step(
        &mut self,
        point: TransitionPoint,
        inst: &MInst,
        uses: &BTreeSet<LogicalValue>,
        future_uses: &impl FutureUses,
    ) -> Result<(), SpillPlanError> {
        let block_id = self.func.blocks[self.block].id;
        let clobbered = clobbers(inst).len();
        if clobbered > self.registers {
            return Err(SpillPlanError::new(
                "SPILL_PLAN.CLOBBER_CAPACITY",
                Some(block_id),
                Some(point.output),
                inst.uses().to_vec(),
                format!(
                    "instruction clobbers {clobbered} registers but the allocator has only {}",
                    self.registers
                ),
            ));
        }
        if clobbered != 0 {
            limit_live_through_clobber(
                self.func,
                self.planning_recipes,
                self.homes,
                &mut self.transition.point_ops,
                self.block,
                point.output,
                self.registers.saturating_sub(clobbered),
                &mut self.resident,
                &mut self.spilled,
                &mut self.deferred_recipe_reloads,
                future_uses,
            )?;
        }
        if let Some(definition) = inst.def() {
            let definition =
                self.logical
                    .checked_of(definition, Some(block_id), Some(point.output))?;
            if !self.resident.contains(&definition) && self.resident.len() == self.registers {
                let Some(maximum) = self.registers.checked_sub(1) else {
                    return Err(SpillPlanError::new(
                        "SPILL_PLAN.OPERAND_PRESSURE",
                        Some(block_id),
                        Some(point.output),
                        vec![VReg(definition.0)],
                        "an instruction result requires a register but no registers are available",
                    ));
                };
                limit(
                    self.func,
                    self.planning_recipes,
                    self.homes,
                    &mut self.transition.point_ops,
                    self.block,
                    point.output,
                    maximum,
                    uses,
                    &mut self.resident,
                    &mut self.spilled,
                    &mut self.deferred_recipe_reloads,
                    future_uses,
                )?;
            }
            self.resident.insert(definition);
            self.last_definition = Some(definition);
        } else {
            self.last_definition = None;
        }
        self.resident
            .retain(|value| !future_uses.distance(*value).is_dead());
        Ok(())
    }

    fn finish(mut self) -> Result<BlockTransition, SpillPlanError> {
        if !self.deferred_recipe_reloads.is_empty() {
            return Err(SpillPlanError::new(
                "SPILL_PLAN.RECIPE_RELOAD_POINT",
                Some(self.func.blocks[self.block].id),
                None,
                self.deferred_recipe_reloads
                    .keys()
                    .map(|value| VReg(value.0))
                    .collect(),
                "deferred recipe reload did not reach its final local use",
            ));
        }
        self.transition.w_exit = self.resident;
        self.transition.s_exit = self.spilled;
        Ok(self.transition)
    }
}

#[allow(clippy::too_many_arguments)]
fn limit_live_through_clobber(
    func: &MFunction,
    planning_recipes: &PlanningRecipes,
    homes: &SpillHomes,
    point_ops: &mut Vec<(ProgramPoint, PlannedOp)>,
    block: usize,
    point_instruction: usize,
    capacity: usize,
    resident: &mut BTreeSet<LogicalValue>,
    spilled: &mut BTreeSet<LogicalValue>,
    deferred_recipe_reloads: &mut BTreeMap<LogicalValue, PointUse>,
    future_uses: &impl FutureUses,
) -> Result<(), SpillPlanError> {
    let mut live_through = resident
        .iter()
        .copied()
        .filter(|value| !future_uses.distance(*value).is_dead())
        .collect::<BTreeSet<_>>();
    while live_through.len() > capacity {
        let Some(victim) = live_through.iter().copied().max_by(|left, right| {
            compare_eviction_candidates(
                func,
                planning_recipes,
                spilled,
                future_uses,
                (*left, future_uses.distance(*left)),
                (*right, future_uses.distance(*right)),
            )
        }) else {
            return Err(SpillPlanError::new(
                "SPILL_PLAN.MIN_VICTIM",
                Some(func.blocks[block].id),
                Some(point_instruction),
                Vec::new(),
                "clobber pressure exceeded capacity but MIN had no live-through victim",
            ));
        };
        evict_value(
            func,
            planning_recipes,
            homes,
            point_ops,
            block,
            point_instruction,
            victim,
            spilled,
            deferred_recipe_reloads,
            future_uses,
        )?;
        live_through.remove(&victim);
        resident.remove(&victim);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn init_usual(
    func: &MFunction,
    cfg: &NormalizedCfg,
    next_use: &NextUseAnalysis,
    planning_recipes: &PlanningRecipes,
    plan: &SpillPlan,
    edge_translations: &EdgeTranslations,
    block: usize,
    registers: usize,
) -> BTreeSet<LogicalValue> {
    let processed = cfg.predecessors[block]
        .iter()
        .copied()
        .filter(|predecessor| *predecessor < block)
        .collect::<Vec<_>>();
    if processed.is_empty() {
        return BTreeSet::new();
    }
    if processed.len() == 1 && cfg.predecessors[block].len() == 1 {
        let predecessor = processed[0];
        return plan.w_exit[predecessor]
            .iter()
            .copied()
            .flat_map(|value| edge_translations.to_successors(predecessor, block, value))
            .collect();
    }
    let mut frequency = HashMap::<LogicalValue, usize>::new();
    for predecessor in &processed {
        for &value in &plan.w_exit[*predecessor] {
            for value in edge_translations.to_successors(*predecessor, block, value) {
                *frequency.entry(value).or_default() += 1;
            }
        }
    }
    let mut candidates = frequency
        .keys()
        .copied()
        .filter_map(|value| {
            let mut keep_cost = 0u128;
            let mut drop_cost = 0u128;
            for &predecessor in &processed {
                let predecessor_value = edge_translations.to_predecessor(predecessor, block, value);
                if plan.w_exit[predecessor].contains(&predecessor_value) {
                    if !plan.s_exit[predecessor].contains(&predecessor_value) {
                        drop_cost = drop_cost
                            .saturating_add(u128::from(spill_cost(func, predecessor_value)));
                    }
                } else {
                    keep_cost = keep_cost.saturating_add(u128::from(reload_cost_on_edge(
                        func,
                        planning_recipes,
                        predecessor,
                        block,
                        predecessor_value,
                    )));
                }
            }
            if next_use.anticipated_at_entry(block, VReg(value.0)) {
                drop_cost = drop_cost.saturating_add((processed.len() as u128).saturating_mul(
                    local_use_cluster_cost(func, next_use, planning_recipes, block, 0, value),
                ));
            }
            let savings = drop_cost
                .checked_sub(keep_cost)
                .filter(|saving| *saving != 0)?;
            Some((
                value,
                savings,
                logical_entry_distance(func, next_use, block, value),
            ))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| compare_join_retention_candidates(*left, *right));
    candidates
        .into_iter()
        .take(registers)
        .map(|(value, _, _)| value)
        .collect()
}

/// Prefer the values for which entry residency avoids the most coupling and
/// guaranteed-use work per instruction of live-range occupancy.  Loop exits
/// dominate straight-line distance, matching global next-use ordering.
fn compare_join_retention_candidates(
    left: (LogicalValue, u128, NextUseDistance),
    right: (LogicalValue, u128, NextUseDistance),
) -> Ordering {
    match (left.2, right.2) {
        (NextUseDistance::Dead, NextUseDistance::Dead) => left.0.cmp(&right.0),
        (NextUseDistance::Dead, _) => Ordering::Greater,
        (_, NextUseDistance::Dead) => Ordering::Less,
        (
            NextUseDistance::Finite {
                loop_exits: left_exits,
                instructions: left_instructions,
            },
            NextUseDistance::Finite {
                loop_exits: right_exits,
                instructions: right_instructions,
            },
        ) => left_exits.cmp(&right_exits).then_with(|| {
            let left_span = left_instructions as u128 + 1;
            let right_span = right_instructions as u128 + 1;
            right
                .1
                .saturating_mul(left_span)
                .cmp(&left.1.saturating_mul(right_span))
                .then_with(|| left_instructions.cmp(&right_instructions))
                .then_with(|| left.0.cmp(&right.0))
        }),
    }
}

fn init_loop_region(
    func: &MFunction,
    next_use: &NextUseAnalysis,
    plan: &SpillPlan,
    block: usize,
    region: usize,
    registers: usize,
) -> Result<BTreeSet<LogicalValue>, SpillPlanError> {
    let mut alive = next_use.entry[block]
        .keys()
        .copied()
        .map(|value| {
            plan.logical
                .checked_of(value, Some(func.blocks[block].id), Some(0))
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    for phi in &func.blocks[block].phis {
        alive.insert(
            plan.logical
                .checked_of(phi.dst, Some(func.blocks[block].id), Some(0))?,
        );
    }
    let Some(facts) = next_use.loop_regions.get(region) else {
        return Err(SpillPlanError::new(
            "SPILL_PLAN.NEXT_USE_REGION",
            Some(func.blocks[block].id),
            Some(0),
            Vec::new(),
            format!("next-use analysis references absent loop region {region}"),
        ));
    };
    let (mut candidates, mut live_through): (Vec<_>, Vec<_>) = alive
        .into_iter()
        .partition(|value| next_use.used_in_region(region, VReg(value.0)));
    candidates.sort_by_key(|value| logical_entry_distance(func, next_use, block, *value));
    if candidates.len() >= registers {
        return Ok(candidates.into_iter().take(registers).collect());
    }
    let internal_pressure = facts.max_pressure.saturating_sub(live_through.len());
    let free_loop = registers.saturating_sub(internal_pressure);
    live_through.sort_by_key(|value| logical_entry_distance(func, next_use, block, *value));
    Ok(candidates
        .into_iter()
        .chain(live_through.into_iter().take(free_loop))
        .take(registers)
        .collect())
}

trait FutureUses {
    fn distance(&self, value: LogicalValue) -> NextUseDistance;
    fn next_point(&self, value: LogicalValue) -> Option<PointUse>;
    fn exit_reload_cost(&self, _value: LogicalValue) -> u32 {
        0
    }
}

struct LinearFutureUses<'a> {
    func: &'a MFunction,
    next_use: &'a NextUseAnalysis,
    block: usize,
    instruction: usize,
}

impl FutureUses for LinearFutureUses<'_> {
    fn distance(&self, value: LogicalValue) -> NextUseDistance {
        self.next_use
            .distance_at(self.func, self.block, self.instruction, VReg(value.0))
    }

    fn next_point(&self, value: LogicalValue) -> Option<PointUse> {
        let instruction =
            self.next_use
                .next_local_use(self.block, self.instruction, VReg(value.0))?;
        Some(PointUse {
            block: self.func.blocks[self.block].id,
            instruction,
            value: VReg(value.0),
        })
    }
}

struct RemainingBlockUses {
    block: BlockId,
    preferred_rank: Vec<usize>,
    remaining: HashMap<LogicalValue, BTreeSet<(usize, usize)>>,
    exit: HashMap<LogicalValue, NextUseDistance>,
    exit_reload_costs: HashMap<LogicalValue, u32>,
    emitted: Vec<bool>,
    emitted_count: usize,
}

impl RemainingBlockUses {
    fn build(
        func: &MFunction,
        next_use: &NextUseAnalysis,
        logical: &LogicalValues,
        block: usize,
        exit_reload_costs: &HashMap<LogicalValue, u32>,
    ) -> Result<Self, SpillPlanError> {
        let instructions = func.blocks[block].insts.len();
        let preferred_rank = (0..instructions).collect::<Vec<_>>();
        let mut remaining = HashMap::<LogicalValue, BTreeSet<(usize, usize)>>::new();
        for (source, inst) in func.blocks[block].insts.iter().enumerate() {
            let mut uses = inst.uses().to_vec();
            uses.sort_unstable();
            uses.dedup();
            for value in uses {
                let value = logical.checked_of(value, Some(func.blocks[block].id), Some(source))?;
                remaining
                    .entry(value)
                    .or_default()
                    .insert((preferred_rank[source], source));
            }
        }
        let exit = next_use.exit[block]
            .iter()
            .map(|(&value, &distance)| {
                logical
                    .checked_of(value, Some(func.blocks[block].id), Some(instructions))
                    .map(|value| (value, distance))
            })
            .collect::<Result<HashMap<_, _>, _>>()?;
        Ok(Self {
            block: func.blocks[block].id,
            preferred_rank,
            remaining,
            exit,
            exit_reload_costs: exit_reload_costs.clone(),
            emitted: vec![false; instructions],
            emitted_count: 0,
        })
    }

    fn emit(
        &mut self,
        source: usize,
        inst: &MInst,
        logical: &LogicalValues,
    ) -> Result<Vec<LogicalValue>, SpillPlanError> {
        if source >= self.emitted.len() || std::mem::replace(&mut self.emitted[source], true) {
            return Err(SpillPlanError::new(
                "SPILL_PLAN.SCHEDULE_ORDER",
                Some(self.block),
                Some(source),
                Vec::new(),
                "a source instruction was committed more than once",
            ));
        }
        self.emitted_count += 1;
        let mut uses = inst.uses().to_vec();
        uses.sort_unstable();
        uses.dedup();
        let mut changed = Vec::with_capacity(uses.len());
        for value in uses {
            let value = logical.checked_of(value, Some(self.block), Some(source))?;
            let points = self.remaining.get_mut(&value).ok_or_else(|| {
                SpillPlanError::new(
                    "SPILL_PLAN.SCHEDULE_ORDER",
                    Some(self.block),
                    Some(source),
                    vec![VReg(value.0)],
                    "committed use has no remaining-use entry",
                )
            })?;
            if !points.remove(&(self.preferred_rank[source], source)) {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.SCHEDULE_ORDER",
                    Some(self.block),
                    Some(source),
                    vec![VReg(value.0)],
                    "committed use was absent from its remaining-use set",
                ));
            }
            changed.push(value);
        }
        Ok(changed)
    }

    fn remaining_uses(&self, value: LogicalValue) -> usize {
        self.remaining.get(&value).map_or(0, BTreeSet::len)
    }

    fn is_live_out(&self, value: LogicalValue) -> bool {
        self.exit.contains_key(&value)
    }

    fn distance(&self, value: LogicalValue) -> NextUseDistance {
        if let Some(&(rank, _)) = self.remaining.get(&value).and_then(BTreeSet::first) {
            return NextUseDistance::Finite {
                loop_exits: 0,
                instructions: rank.saturating_sub(self.emitted_count),
            };
        }
        let remaining_instructions = self.emitted.len().saturating_sub(self.emitted_count);
        match self.exit.get(&value).copied() {
            Some(NextUseDistance::Finite {
                loop_exits,
                instructions,
            }) => NextUseDistance::Finite {
                loop_exits,
                instructions: instructions.saturating_add(remaining_instructions),
            },
            _ => NextUseDistance::Dead,
        }
    }

    fn next_point(&self, value: LogicalValue) -> Option<PointUse> {
        let &(_, instruction) = self.remaining.get(&value)?.first()?;
        Some(PointUse {
            block: self.block,
            instruction,
            value: VReg(value.0),
        })
    }

    fn exit_reload_cost(&self, value: LogicalValue) -> u32 {
        self.exit_reload_costs.get(&value).copied().unwrap_or(0)
    }
}

struct DynamicFutureUses<'a>(&'a RemainingBlockUses);

impl FutureUses for DynamicFutureUses<'_> {
    fn distance(&self, value: LogicalValue) -> NextUseDistance {
        self.0.distance(value)
    }

    fn next_point(&self, value: LogicalValue) -> Option<PointUse> {
        self.0.next_point(value)
    }

    fn exit_reload_cost(&self, value: LogicalValue) -> u32 {
        self.0.exit_reload_cost(value)
    }
}

#[allow(clippy::too_many_arguments)]
fn limit(
    func: &MFunction,
    planning_recipes: &PlanningRecipes,
    homes: &SpillHomes,
    point_ops: &mut Vec<(ProgramPoint, PlannedOp)>,
    block: usize,
    point_instruction: usize,
    maximum: usize,
    pinned: &BTreeSet<LogicalValue>,
    resident: &mut BTreeSet<LogicalValue>,
    spilled: &mut BTreeSet<LogicalValue>,
    deferred_recipe_reloads: &mut BTreeMap<LogicalValue, PointUse>,
    future_uses: &impl FutureUses,
) -> Result<(), SpillPlanError> {
    while resident.len() > maximum {
        let Some(victim) = resident
            .iter()
            .copied()
            .filter(|value| !pinned.contains(value))
            .max_by(|left, right| {
                compare_eviction_candidates(
                    func,
                    planning_recipes,
                    spilled,
                    future_uses,
                    (*left, future_uses.distance(*left)),
                    (*right, future_uses.distance(*right)),
                )
            })
        else {
            return Err(SpillPlanError::new(
                "SPILL_PLAN.OPERAND_PRESSURE",
                Some(func.blocks[block].id),
                Some(point_instruction),
                pinned.iter().map(|value| VReg(value.0)).collect(),
                format!(
                    "{} simultaneously pinned operands exceed the {maximum}-register capacity",
                    pinned.len()
                ),
            ));
        };
        evict_value(
            func,
            planning_recipes,
            homes,
            point_ops,
            block,
            point_instruction,
            victim,
            spilled,
            deferred_recipe_reloads,
            future_uses,
        )?;
        resident.remove(&victim);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn evict_value(
    func: &MFunction,
    planning_recipes: &PlanningRecipes,
    homes: &SpillHomes,
    point_ops: &mut Vec<(ProgramPoint, PlannedOp)>,
    block: usize,
    point_instruction: usize,
    value: LogicalValue,
    spilled: &mut BTreeSet<LogicalValue>,
    deferred_recipe_reloads: &mut BTreeMap<LogicalValue, PointUse>,
    future_uses: &impl FutureUses,
) -> Result<(), SpillPlanError> {
    if spilled.contains(&value) {
        return Ok(());
    }
    if let Some((point, recipe_cost)) = next_use_point_recipe(planning_recipes, value, future_uses)
    {
        let stack_cost =
            spill_cost(func, value).saturating_add(reload_cost(func, planning_recipes, value));
        if recipe_cost < stack_cost {
            if let Some(previous) = deferred_recipe_reloads.insert(value, point) {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.RECIPE_RELOAD_POINT",
                    Some(func.blocks[block].id),
                    Some(point_instruction),
                    vec![VReg(value.0)],
                    format!("logical value already had deferred recipe reload {previous:?}"),
                ));
            }
            return Ok(());
        }
    }
    spilled.insert(value);
    point_ops.push((
        ProgramPoint {
            block: func.blocks[block].id,
            instruction: point_instruction,
            side: PointSide::Before,
        },
        PlannedOp::Spill {
            value,
            home: homes.of_logical(value),
        },
    ));
    Ok(())
}

/// Return the exact MemorySSA recipe at the next local use.
///
/// One eviction-to-reload interval is an independent residency cluster.  The
/// value does not need a persistent stack home merely because it has another
/// use after that reload: once reloaded it is resident again, and a later
/// eviction makes a fresh home decision against the MemorySSA version at that
/// later use.
fn next_use_point_recipe(
    planning_recipes: &PlanningRecipes,
    value: LogicalValue,
    future_uses: &impl FutureUses,
) -> Option<(PointUse, u16)> {
    let point = future_uses.next_point(value)?;
    planning_recipes
        .point_specific_materialization_cost(point)
        .map(|cost| (point, cost))
}

/// Compare two possible split points by the cost density of keeping the value
/// resident until its next use.  Braun--Hack MIN is the equal-cost special
/// case: with equal spill/reload costs, the farther next use is still evicted.
/// For target values with different rematerialization and memory costs, the
/// numerator is the machine-instruction cost avoided by retaining the value,
/// and the denominator is the register occupancy until that use.
fn compare_eviction_candidates(
    func: &MFunction,
    planning_recipes: &PlanningRecipes,
    spilled: &BTreeSet<LogicalValue>,
    future_uses: &impl FutureUses,
    left: (LogicalValue, NextUseDistance),
    right: (LogicalValue, NextUseDistance),
) -> Ordering {
    match (left.1, right.1) {
        (NextUseDistance::Dead, NextUseDistance::Dead) => left.0.cmp(&right.0),
        (NextUseDistance::Dead, _) => Ordering::Greater,
        (_, NextUseDistance::Dead) => Ordering::Less,
        (
            NextUseDistance::Finite {
                loop_exits: left_exits,
                instructions: left_instructions,
            },
            NextUseDistance::Finite {
                loop_exits: right_exits,
                instructions: right_instructions,
            },
        ) => left_exits.cmp(&right_exits).then_with(|| {
            let left_cost =
                eviction_cost(func, planning_recipes, spilled, left.0, future_uses) as u128;
            let right_cost =
                eviction_cost(func, planning_recipes, spilled, right.0, future_uses) as u128;
            let left_span = left_instructions as u128 + 1;
            let right_span = right_instructions as u128 + 1;
            // Lower avoided-cost density is the better eviction candidate.
            // Cross multiplication keeps the decision deterministic and free
            // of floating-point rounding.
            (right_cost * left_span)
                .cmp(&(left_cost * right_span))
                .then_with(|| left_instructions.cmp(&right_instructions))
                .then_with(|| left.0.cmp(&right.0))
        }),
    }
}

fn eviction_cost(
    func: &MFunction,
    planning_recipes: &PlanningRecipes,
    spilled: &BTreeSet<LogicalValue>,
    value: LogicalValue,
    future_uses: &impl FutureUses,
) -> u32 {
    let has_persistent_home = spilled.contains(&value);
    let local_reload =
        reload_cost_at_next_local_use(func, planning_recipes, value, future_uses).map(u32::from);
    let reload_cost = local_reload.unwrap_or_else(|| {
        let exit_cost = future_uses.exit_reload_cost(value);
        if exit_cost == 0 {
            u32::from(reload_cost(func, planning_recipes, value))
        } else {
            exit_cost
        }
    });
    let persistent_cost = reload_cost.saturating_add(if has_persistent_home {
        0
    } else {
        u32::from(spill_cost(func, value))
    });
    if has_persistent_home {
        return persistent_cost;
    }
    next_use_point_recipe(planning_recipes, value, future_uses)
        .map_or(persistent_cost, |(_, recipe_cost)| {
            persistent_cost.min(u32::from(recipe_cost))
        })
}

fn reload_cost_at_next_local_use(
    func: &MFunction,
    planning_recipes: &PlanningRecipes,
    value: LogicalValue,
    future_uses: &impl FutureUses,
) -> Option<u16> {
    let costs = super::cost::MachineSpillCosts::with_recipes(func, planning_recipes);
    future_uses
        .next_point(value)
        .map(|point| costs.reload_at_point(point))
}

/// Cost the complete guaranteed straight-line use cluster for a value which
/// is absent at block entry.  Point-specific MemorySSA recipes may differ
/// between uses, so every concrete point is queried independently.  If the
/// first guaranteed use lies beyond this block, retain the existing
/// cross-block fallback rather than assigning a zero cost.
fn local_use_cluster_cost(
    func: &MFunction,
    next_use: &NextUseAnalysis,
    planning_recipes: &PlanningRecipes,
    block: usize,
    instruction: usize,
    value: LogicalValue,
) -> u128 {
    let value = VReg(value.0);
    let uses = next_use.local_uses_from(block, instruction, value);
    if uses.is_empty() {
        return u128::from(reload_cost(func, planning_recipes, LogicalValue(value.0)));
    }
    let costs = super::cost::MachineSpillCosts::with_recipes(func, planning_recipes);
    uses.iter().fold(0u128, |cost, &instruction| {
        let reload = costs.reload_at_point(PointUse {
            block: func.blocks[block].id,
            instruction,
            value,
        });
        cost.saturating_add(u128::from(reload))
    })
}

fn reload_cost_on_edge(
    func: &MFunction,
    planning_recipes: &PlanningRecipes,
    predecessor: usize,
    successor: usize,
    value: LogicalValue,
) -> u16 {
    let value = VReg(value.0);
    super::cost::MachineSpillCosts::with_recipes(func, planning_recipes).reload_on_edge(EdgeUse {
        predecessor: func.blocks[predecessor].id,
        successor: func.blocks[successor].id,
        value,
    })
}

fn reload_cost(func: &MFunction, planning_recipes: &PlanningRecipes, value: LogicalValue) -> u16 {
    super::cost::MachineSpillCosts::with_recipes(func, planning_recipes)
        .persistent_reload(VReg(value.0))
}

fn spill_cost(func: &MFunction, value: LogicalValue) -> u16 {
    super::cost::MachineSpillCosts::from_descriptors(func).spill(VReg(value.0))
}

fn logical_entry_distance(
    func: &MFunction,
    next_use: &NextUseAnalysis,
    block: usize,
    value: LogicalValue,
) -> NextUseDistance {
    next_use.distance_at(func, block, 0, VReg(value.0))
}

impl SpillPlan {
    /// Finalize whole-home rematerialization after the W/S plan has exposed
    /// every concrete point and edge reload.  Reconstruction must materialize
    /// this decision; it may no longer infer a different home kind on its own.
    pub(super) fn select_recipe_homes(
        &mut self,
        func: &MFunction,
        cfg: &NormalizedCfg,
        analysis: &ReloadRecipeAnalysis,
    ) -> Result<(), SpillPlanError> {
        let base_costs = super::cost::MachineSpillCosts::from_descriptors(func);
        let mut candidates = BTreeSet::<SpillHome>::new();
        let mut rejected = BTreeSet::<SpillHome>::new();
        let mut baseline_costs = BTreeMap::<SpillHome, u128>::new();
        let mut recipe_costs = BTreeMap::<SpillHome, u128>::new();
        for &(point, operation) in &self.point_ops {
            match operation {
                PlannedOp::Reload { value, home } => {
                    candidates.insert(home);
                    let query = PointUse {
                        block: point.block,
                        instruction: point.instruction,
                        value: VReg(value.0),
                    };
                    if let Some(recipe) = analysis.resolved_recipe_at_point(query) {
                        let cost = u128::try_from(recipe.steps.len().saturating_add(1))
                            .unwrap_or(u128::MAX);
                        let baseline = baseline_costs.entry(home).or_default();
                        *baseline = baseline.saturating_add(
                            if self.recipe_reloads.contains(&(
                                point.block,
                                point.instruction,
                                value,
                            )) {
                                cost
                            } else {
                                u128::from(base_costs.persistent_reload(VReg(value.0)))
                            },
                        );
                        let total = recipe_costs.entry(home).or_default();
                        *total = total.saturating_add(cost);
                    } else {
                        let baseline = baseline_costs.entry(home).or_default();
                        *baseline = baseline.saturating_add(u128::from(
                            base_costs.persistent_reload(VReg(value.0)),
                        ));
                        rejected.insert(home);
                    }
                }
                PlannedOp::Spill { value, home } => {
                    let total = baseline_costs.entry(home).or_default();
                    *total = total.saturating_add(u128::from(spill_cost(func, value)));
                }
                PlannedOp::SpillPhi { value, home } => {
                    let Some(&block) = cfg.block_index.get(&point.block) else {
                        return Err(SpillPlanError::new(
                            "SPILL_PLAN.RECIPE_HOME_PHI",
                            Some(point.block),
                            Some(point.instruction),
                            vec![VReg(value.0)],
                            "recipe-home SpillPhi block is outside the normalized CFG",
                        ));
                    };
                    let Some(phi) = func.blocks[block]
                        .phis
                        .iter()
                        .find(|phi| phi.dst.0 == value.0)
                    else {
                        return Err(SpillPlanError::new(
                            "SPILL_PLAN.RECIPE_HOME_PHI",
                            Some(point.block),
                            Some(point.instruction),
                            vec![VReg(value.0)],
                            "recipe-home SpillPhi has no matching MIR phi",
                        ));
                    };
                    for &(predecessor, source) in &phi.sources {
                        let Some(&predecessor) = cfg.block_index.get(&predecessor) else {
                            return Err(SpillPlanError::new(
                                "SPILL_PLAN.RECIPE_HOME_PHI",
                                Some(point.block),
                                Some(point.instruction),
                                vec![VReg(value.0), source],
                                "recipe-home SpillPhi source is outside the normalized CFG",
                            ));
                        };
                        let source = LogicalValue(source.0);
                        if !self.s_exit[predecessor].contains(&source) {
                            let total = baseline_costs.entry(home).or_default();
                            *total = total.saturating_add(u128::from(spill_cost(func, source)));
                        }
                    }
                }
            }
        }
        for (&(predecessor, successor), operations) in &self.edge_ops {
            let Some(predecessor_block) = func.blocks.get(predecessor) else {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.RECIPE_HOME_EDGE",
                    None,
                    None,
                    Vec::new(),
                    format!("recipe-home predecessor index {predecessor} is outside function"),
                ));
            };
            if func.blocks.get(successor).is_none() {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.RECIPE_HOME_EDGE",
                    Some(predecessor_block.id),
                    None,
                    Vec::new(),
                    format!("recipe-home successor index {successor} is outside function"),
                ));
            }
            let insertion = super::cfg::edge_insertion_point(func, cfg, predecessor, successor)
                .ok_or_else(|| {
                    SpillPlanError::new(
                        "SPILL_PLAN.RECIPE_HOME_EDGE",
                        Some(predecessor_block.id),
                        None,
                        Vec::new(),
                        "recipe-home edge has no single-edge materialization point",
                    )
                })?;
            let insertion_block = &func.blocks[insertion.block];
            if insertion.instruction >= insertion_block.insts.len() {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.RECIPE_HOME_EDGE",
                    Some(insertion_block.id),
                    Some(insertion.instruction),
                    Vec::new(),
                    "recipe-home edge insertion point is outside its MIR block",
                ));
            }
            for &operation in operations {
                match operation {
                    PlannedEdgeOp::Reload {
                        source,
                        source_home,
                        ..
                    } => {
                        candidates.insert(source_home);
                        let total = baseline_costs.entry(source_home).or_default();
                        *total = total.saturating_add(u128::from(
                            base_costs.persistent_reload(VReg(source.0)),
                        ));
                        let query = PointUse {
                            block: insertion_block.id,
                            instruction: insertion.instruction,
                            value: VReg(source.0),
                        };
                        if let Some(recipe) = analysis.resolved_recipe_at_point(query) {
                            let cost = u128::try_from(recipe.steps.len().saturating_add(1))
                                .unwrap_or(u128::MAX);
                            let total = recipe_costs.entry(source_home).or_default();
                            *total = total.saturating_add(cost);
                        } else {
                            rejected.insert(source_home);
                        }
                    }
                    PlannedEdgeOp::Spill {
                        source,
                        destination_home,
                        ..
                    } => {
                        let total = baseline_costs.entry(destination_home).or_default();
                        *total = total.saturating_add(u128::from(spill_cost(func, source)));
                    }
                }
            }
        }
        candidates.retain(|home| {
            !self.state_homes.contains_key(home)
                && !rejected.contains(home)
                && recipe_costs.get(home).copied().unwrap_or(u128::MAX)
                    < baseline_costs.get(home).copied().unwrap_or_default()
        });
        self.recipe_homes = candidates;
        Ok(())
    }

    /// Independently prove that every reload assigned to a recipe-only home
    /// has an exact recipe at its final insertion point.  This verifier does
    /// not trust the candidate/rejection sets used by selection.
    pub(super) fn verify_recipe_homes(
        &self,
        func: &MFunction,
        cfg: &NormalizedCfg,
        analysis: &ReloadRecipeAnalysis,
    ) -> Result<(), SpillPlanError> {
        for &home in &self.recipe_homes {
            let mut reloads = 0usize;
            for &(point, operation) in &self.point_ops {
                let PlannedOp::Reload {
                    value,
                    home: reload_home,
                } = operation
                else {
                    continue;
                };
                if reload_home != home {
                    continue;
                }
                reloads += 1;
                let query = PointUse {
                    block: point.block,
                    instruction: point.instruction,
                    value: VReg(value.0),
                };
                if analysis.resolved_recipe_at_point(query).is_none() {
                    return Err(SpillPlanError::new(
                        "SPILL_PLAN.RECIPE_HOME_POINT",
                        Some(point.block),
                        Some(point.instruction),
                        vec![VReg(value.0)],
                        format!("recipe home {home:?} has a point reload without an exact recipe"),
                    ));
                }
            }
            for (&(predecessor, successor), operations) in &self.edge_ops {
                let Some(predecessor_block) = func.blocks.get(predecessor) else {
                    return Err(SpillPlanError::new(
                        "SPILL_PLAN.RECIPE_HOME_EDGE",
                        None,
                        None,
                        Vec::new(),
                        format!("recipe home {home:?} references absent predecessor {predecessor}"),
                    ));
                };
                let Some(successor_block) = func.blocks.get(successor) else {
                    return Err(SpillPlanError::new(
                        "SPILL_PLAN.RECIPE_HOME_EDGE",
                        Some(predecessor_block.id),
                        None,
                        Vec::new(),
                        format!("recipe home {home:?} references absent successor {successor}"),
                    ));
                };
                let insertion = super::cfg::edge_insertion_point(func, cfg, predecessor, successor)
                    .ok_or_else(|| {
                        SpillPlanError::new(
                            "SPILL_PLAN.RECIPE_HOME_EDGE",
                            Some(predecessor_block.id),
                            None,
                            Vec::new(),
                            "recipe-home edge has no single-edge materialization point",
                        )
                    })?;
                let insertion_block = &func.blocks[insertion.block];
                for &operation in operations {
                    let PlannedEdgeOp::Reload {
                        source,
                        source_home: reload_home,
                        ..
                    } = operation
                    else {
                        continue;
                    };
                    if reload_home != home {
                        continue;
                    }
                    reloads += 1;
                    let query = PointUse {
                        block: insertion_block.id,
                        instruction: insertion.instruction,
                        value: VReg(source.0),
                    };
                    if analysis.resolved_recipe_at_point(query).is_none() {
                        return Err(SpillPlanError::new(
                            "SPILL_PLAN.RECIPE_HOME_EDGE",
                            Some(predecessor_block.id),
                            None,
                            vec![VReg(source.0)],
                            format!(
                                "recipe home {home:?} has no exact recipe on edge {} -> {}",
                                predecessor_block.id, successor_block.id
                            ),
                        ));
                    }
                }
            }
            if reloads == 0 {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.RECIPE_HOME_RELOAD",
                    None,
                    None,
                    Vec::new(),
                    format!("recipe home {home:?} has no selected reload"),
                ));
            }
        }
        Ok(())
    }

    pub(super) fn verify(
        &self,
        func: &MFunction,
        cfg: &NormalizedCfg,
        registers: usize,
    ) -> Result<(), SpillPlanError> {
        let block_count = func.blocks.len();
        if self.w_entry.len() != block_count
            || self.w_exit.len() != block_count
            || self.s_entry.len() != block_count
            || self.s_exit.len() != block_count
        {
            return Err(SpillPlanError::new(
                "SPILL_PLAN.STATE_SHAPE",
                None,
                None,
                Vec::new(),
                format!(
                    "spill-plan state tables must all contain {block_count} rows (W_entry={}, W_exit={}, S_entry={}, S_exit={})",
                    self.w_entry.len(),
                    self.w_exit.len(),
                    self.s_entry.len(),
                    self.s_exit.len()
                ),
            ));
        }
        if self.logical.count != func.vregs.count() || self.homes.count != self.logical.count {
            return Err(SpillPlanError::new(
                "SPILL_PLAN.STATE_SHAPE",
                None,
                None,
                Vec::new(),
                format!(
                    "spill-plan value tables cover {} logical values and {} homes, but the function has {} virtual registers",
                    self.logical.count,
                    self.homes.count,
                    func.vregs.count()
                ),
            ));
        }

        for (block, state) in self.w_entry.iter().enumerate() {
            if state.len() > registers {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.PRESSURE",
                    Some(func.blocks[block].id),
                    Some(0),
                    state.iter().map(|value| VReg(value.0)).collect(),
                    format!(
                        "W_entry contains {} residents but only {registers} registers are available",
                        state.len()
                    ),
                ));
            }
        }
        for (block, state) in self.w_exit.iter().enumerate() {
            if state.len() > registers {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.PRESSURE",
                    Some(func.blocks[block].id),
                    Some(func.blocks[block].insts.len()),
                    state.iter().map(|value| VReg(value.0)).collect(),
                    format!(
                        "W_exit contains {} residents but only {registers} registers are available",
                        state.len()
                    ),
                ));
            }
        }

        for (block, states) in (0..block_count).map(|block| {
            (
                block,
                [
                    &self.w_entry[block],
                    &self.w_exit[block],
                    &self.s_entry[block],
                    &self.s_exit[block],
                ],
            )
        }) {
            for state in states {
                if let Some(value) = state.iter().find(|value| value.0 >= self.logical.count) {
                    return Err(SpillPlanError::new(
                        "SPILL_PLAN.VALUE_RANGE",
                        Some(func.blocks[block].id),
                        None,
                        vec![VReg(value.0)],
                        format!(
                            "spill-plan state references logical value {} but the plan contains {} values",
                            value.0, self.logical.count
                        ),
                    ));
                }
            }
        }

        for (&(predecessor, successor), operations) in &self.edge_ops {
            let Some(predecessor_block) = func.blocks.get(predecessor) else {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.EDGE_EXISTS",
                    None,
                    None,
                    Vec::new(),
                    format!("edge operation predecessor index {predecessor} is out of range"),
                ));
            };
            let Some(successor_block) = func.blocks.get(successor) else {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.EDGE_EXISTS",
                    Some(predecessor_block.id),
                    None,
                    Vec::new(),
                    format!("edge operation successor index {successor} is out of range"),
                ));
            };
            if !cfg.successors[predecessor].contains(&successor) {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.EDGE_EXISTS",
                    Some(predecessor_block.id),
                    None,
                    Vec::new(),
                    format!(
                        "planned edge operation targets {}, which is not a CFG successor",
                        successor_block.id
                    ),
                ));
            }
            if super::cfg::edge_insertion_point(func, cfg, predecessor, successor).is_none() {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.EDGE_ISOLATED",
                    Some(predecessor_block.id),
                    None,
                    Vec::new(),
                    "edge operation has no single-edge materialization point",
                ));
            }
            if operations.is_empty() {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.EDGE_EXISTS",
                    Some(predecessor_block.id),
                    None,
                    Vec::new(),
                    format!(
                        "edge-operation list for {} -> {} is empty",
                        predecessor_block.id, successor_block.id
                    ),
                ));
            }
            for &operation in operations {
                self.verify_edge_operation(operation, Some(predecessor_block.id))?;
            }
        }
        for &(point, operation) in &self.point_ops {
            let Some(&block) = cfg.block_index.get(&point.block) else {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.POINT_RANGE",
                    Some(point.block),
                    Some(point.instruction),
                    Vec::new(),
                    "planned operation references a block absent from the normalized CFG",
                ));
            };
            if point.instruction > func.blocks[block].insts.len() {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.POINT_RANGE",
                    Some(point.block),
                    Some(point.instruction),
                    Vec::new(),
                    format!(
                        "planned operation is outside the block's {} instructions",
                        func.blocks[block].insts.len()
                    ),
                ));
            }
            self.verify_operation(operation, Some(point.block), Some(point.instruction))?;
        }
        for &(block, instruction, value) in &self.recipe_reloads {
            let matching_reload = self.point_ops.iter().any(|(point, operation)| {
                point.block == block
                    && point.instruction == instruction
                    && matches!(operation, PlannedOp::Reload { value: reload, .. } if *reload == value)
            });
            if !matching_reload {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.RECIPE_RELOAD_POINT",
                    Some(block),
                    Some(instruction),
                    vec![VReg(value.0)],
                    "recipe-reload annotation has no matching point reload",
                ));
            }
        }
        for &home in &self.recipe_homes {
            if home.0 >= self.logical.count {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.RECIPE_HOME_RANGE",
                    None,
                    None,
                    Vec::new(),
                    format!(
                        "recipe home {} is outside the plan's {} logical values",
                        home.0, self.logical.count
                    ),
                ));
            }
            let has_reload = self
                .point_ops
                .iter()
                .any(|(_, operation)| {
                    matches!(operation, PlannedOp::Reload { home: reload_home, .. } if *reload_home == home)
                })
                || self.edge_ops.values().flatten().any(|operation| {
                    matches!(operation, PlannedEdgeOp::Reload { source_home, .. } if *source_home == home)
                });
            if !has_reload {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.RECIPE_HOME_RELOAD",
                    None,
                    None,
                    Vec::new(),
                    format!("recipe home {home:?} has no selected reload"),
                ));
            }
        }
        Ok(())
    }

    fn verify_operation(
        &self,
        operation: PlannedOp,
        block: Option<BlockId>,
        instruction: Option<usize>,
    ) -> Result<(), SpillPlanError> {
        let (value, home) = match operation {
            PlannedOp::Spill { value, home }
            | PlannedOp::Reload { value, home }
            | PlannedOp::SpillPhi { value, home } => (value, home),
        };
        if value.0 >= self.logical.count {
            return Err(SpillPlanError::new(
                "SPILL_PLAN.VALUE_RANGE",
                block,
                instruction,
                vec![VReg(value.0)],
                format!(
                    "planned operation references logical value {} but the plan contains {} values",
                    value.0, self.logical.count
                ),
            ));
        }
        let expected = self.homes.of_logical(value);
        if home != expected {
            return Err(SpillPlanError::new(
                "SPILL_PLAN.HOME",
                block,
                instruction,
                vec![VReg(value.0)],
                format!(
                    "planned operation uses spill home {} but logical value {} belongs to home {}",
                    home.0, value.0, expected.0
                ),
            ));
        }
        Ok(())
    }

    fn verify_edge_operation(
        &self,
        operation: PlannedEdgeOp,
        block: Option<BlockId>,
    ) -> Result<(), SpillPlanError> {
        let (source, destination, home, expected_home) = match operation {
            PlannedEdgeOp::Reload {
                source,
                source_home,
                destination,
            } => (
                source,
                destination,
                source_home,
                self.homes.of_logical(source),
            ),
            PlannedEdgeOp::Spill {
                source,
                destination,
                destination_home,
            } => (
                source,
                destination,
                destination_home,
                self.homes.of_logical(destination),
            ),
        };
        for value in [source, destination] {
            if value.0 >= self.logical.count {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.VALUE_RANGE",
                    block,
                    None,
                    vec![VReg(value.0)],
                    format!(
                        "planned edge operation references logical value {} but the plan contains {} values",
                        value.0, self.logical.count
                    ),
                ));
            }
        }
        if home != expected_home {
            return Err(SpillPlanError::new(
                "SPILL_PLAN.HOME_CLASS",
                block,
                None,
                vec![VReg(source.0), VReg(destination.0)],
                format!("planned edge operation names home {home:?}, expected {expected_home:?}"),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::native::mir::{
        BaseReg, MBlock, MInst, OpSize, PhiNode, SpillDesc, VRegAllocator,
    };

    #[test]
    fn integrated_ready_walk_closes_resident_lanes_before_starting_new_roots() {
        const LANES: usize = 32;
        let mut vregs = VRegAllocator::new();
        let roots = (0..LANES).map(|_| vregs.alloc()).collect::<Vec<_>>();
        let results = (0..LANES).map(|_| vregs.alloc()).collect::<Vec<_>>();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); LANES * 2]);
        let mut block = MBlock::new(BlockId(0));
        for (lane, &root) in roots.iter().enumerate() {
            block.push(MInst::LoadImm {
                dst: root,
                value: lane as u64,
            });
        }
        for (&root, &result) in roots.iter().zip(&results) {
            block.push(MInst::AndImm {
                dst: result,
                src: root,
                imm: 1,
            });
        }
        block.push(MInst::Return);
        func.push_block(block);

        let cfg = super::super::cfg::normalize(&mut func).unwrap();
        let constraints = super::super::constraints::ConstraintModel::build(&func, &cfg).unwrap();
        let next_use = super::super::next_use::analyze(&func, &cfg).unwrap();
        let logical = LogicalValues::build(&func);
        let homes = SpillHomes::build(&func).unwrap();
        let recipes = PlanningRecipes::stack_only(func.vregs.count());
        let (_, order) = plan_scheduled_block_transition(
            &func,
            &next_use,
            &recipes,
            &logical,
            &homes,
            0,
            4,
            &BTreeSet::new(),
            BTreeSet::new(),
            &constraints.instructions[0],
            &HashMap::new(),
        )
        .unwrap();

        let mut outstanding = BTreeSet::new();
        assert!(
            order
                .iter()
                .position(|source| *source >= LANES)
                .is_some_and(|position| position <= 4),
            "a direct consumer must run no later than the resident-capacity boundary"
        );
        for &source in &order[..order.len() - 1] {
            if source < LANES {
                outstanding.insert(source);
            } else {
                assert!(
                    outstanding.remove(&(source - LANES)),
                    "a lane consumer must follow its root"
                );
            }
        }
        assert!(outstanding.is_empty());
        assert_eq!(order.last(), Some(&(LANES * 2)));
    }

    #[test]
    fn integrated_ready_walk_builds_the_earliest_bounded_sink_packet() {
        let mut vregs = VRegAllocator::new();
        let long_root = vregs.alloc();
        let short_root = vregs.alloc();
        let short_result = vregs.alloc();
        let filler_root = vregs.alloc();
        let filler_result = vregs.alloc();
        let long_result = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 6]);
        let mut block = MBlock::new(BlockId(0));
        // Root source order starts `long_root` and keeps it resident while
        // the independent short cluster executes. Sink-directed order starts
        // from the earliest result and first tries to close its dependency
        // cone without exceeding the physical register capacity.
        block.push(MInst::LoadImm {
            dst: long_root,
            value: 1,
        });
        block.push(MInst::LoadImm {
            dst: short_root,
            value: 2,
        });
        block.push(MInst::AndImm {
            dst: short_result,
            src: short_root,
            imm: 1,
        });
        block.push(MInst::LoadImm {
            dst: filler_root,
            value: 3,
        });
        block.push(MInst::AndImm {
            dst: filler_result,
            src: filler_root,
            imm: 1,
        });
        block.push(MInst::AndImm {
            dst: long_result,
            src: long_root,
            imm: 1,
        });
        block.push(MInst::Return);
        func.push_block(block);

        let cfg = super::super::cfg::normalize(&mut func).unwrap();
        let constraints = super::super::constraints::ConstraintModel::build(&func, &cfg).unwrap();
        let next_use = super::super::next_use::analyze(&func, &cfg).unwrap();
        let logical = LogicalValues::build(&func);
        let homes = SpillHomes::build(&func).unwrap();
        let recipes = PlanningRecipes::stack_only(func.vregs.count());
        let (_, order) = plan_scheduled_block_transition(
            &func,
            &next_use,
            &recipes,
            &logical,
            &homes,
            0,
            4,
            &BTreeSet::new(),
            BTreeSet::new(),
            &constraints.instructions[0],
            &HashMap::new(),
        )
        .unwrap();

        assert_eq!(&order[..2], &[1, 2]);
        assert!(
            order.iter().position(|source| *source == 0)
                < order.iter().position(|source| *source == 5)
        );
        assert_eq!(order.last(), Some(&6));
    }

    #[test]
    fn bounded_sink_packet_materializes_a_shared_producer_once_for_adjacent_sinks() {
        let mut vregs = VRegAllocator::new();
        let distant_root = vregs.alloc();
        let shared_root = vregs.alloc();
        let first_result = vregs.alloc();
        let second_result = vregs.alloc();
        let filler_root = vregs.alloc();
        let filler_result = vregs.alloc();
        let distant_result = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 7]);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::LoadImm {
            dst: distant_root,
            value: 1,
        });
        block.push(MInst::LoadImm {
            dst: shared_root,
            value: 2,
        });
        block.push(MInst::AndImm {
            dst: first_result,
            src: shared_root,
            imm: 1,
        });
        block.push(MInst::AndImm {
            dst: second_result,
            src: shared_root,
            imm: 2,
        });
        block.push(MInst::LoadImm {
            dst: filler_root,
            value: 3,
        });
        block.push(MInst::AndImm {
            dst: filler_result,
            src: filler_root,
            imm: 1,
        });
        block.push(MInst::AndImm {
            dst: distant_result,
            src: distant_root,
            imm: 1,
        });
        block.push(MInst::Return);
        func.push_block(block);

        let cfg = super::super::cfg::normalize(&mut func).unwrap();
        let constraints = super::super::constraints::ConstraintModel::build(&func, &cfg).unwrap();
        let next_use = super::super::next_use::analyze(&func, &cfg).unwrap();
        let logical = LogicalValues::build(&func);
        let homes = SpillHomes::build(&func).unwrap();
        let recipes = PlanningRecipes::stack_only(func.vregs.count());
        let (_, order) = plan_scheduled_block_transition(
            &func,
            &next_use,
            &recipes,
            &logical,
            &homes,
            0,
            4,
            &BTreeSet::new(),
            BTreeSet::new(),
            &constraints.instructions[0],
            &HashMap::new(),
        )
        .unwrap();

        assert_eq!(&order[..3], &[1, 2, 3]);
        assert_eq!(order.iter().filter(|&&source| source == 1).count(), 1);
        assert_eq!(order.last(), Some(&7));
    }

    #[test]
    fn exit_reload_price_is_deduplicated_per_edge_and_summed_across_edges() {
        let mut vregs = VRegAllocator::new();
        let condition = vregs.alloc();
        let source = vregs.alloc();
        let true_value = vregs.alloc();
        let false_value = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 4]);

        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: condition,
            value: 1,
        });
        entry.push(MInst::LoadImm {
            dst: source,
            value: 2,
        });
        entry.push(MInst::Branch {
            cond: condition,
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        });

        let mut true_block = MBlock::new(BlockId(1));
        true_block.phis.push(PhiNode {
            dst: true_value,
            sources: vec![(BlockId(0), source)],
        });
        true_block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 0,
            src: true_value,
            size: OpSize::S64,
        });
        true_block.push(MInst::Return);

        let mut false_block = MBlock::new(BlockId(2));
        false_block.phis.push(PhiNode {
            dst: false_value,
            sources: vec![(BlockId(0), source)],
        });
        false_block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 8,
            src: false_value,
            size: OpSize::S64,
        });
        false_block.push(MInst::Return);
        func.blocks = vec![entry, true_block, false_block];

        let cfg = super::super::cfg::normalize(&mut func).unwrap();
        let next_use = super::super::next_use::analyze(&func, &cfg).unwrap();
        let logical = LogicalValues::build(&func);
        let translations = EdgeTranslations::build(&func, &cfg, &logical).unwrap();
        let recipes = PlanningRecipes::stack_only(func.vregs.count());
        let entry = cfg.block_index[&BlockId(0)];
        let true_block = cfg.block_index[&BlockId(1)];
        let false_block = cfg.block_index[&BlockId(2)];
        let costs = exit_reload_costs(
            &func,
            &cfg,
            &next_use,
            &recipes,
            &logical,
            &translations,
            entry,
        )
        .unwrap();
        let source = LogicalValue(source.0);
        let expected = u32::from(reload_cost_on_edge(
            &func, &recipes, entry, true_block, source,
        ))
        .saturating_add(u32::from(reload_cost_on_edge(
            &func,
            &recipes,
            entry,
            false_block,
            source,
        )));

        assert_eq!(costs.get(&source), Some(&expected));
        assert_eq!(costs.len(), 1, "only the shared phi source is live out");
    }

    #[test]
    fn entry_value_evicted_before_first_use_is_planned_as_a_memory_phi() {
        let mut vregs = VRegAllocator::new();
        let initial = vregs.alloc();
        let merged = vregs.alloc();
        let pressure_a = vregs.alloc();
        let pressure_b = vregs.alloc();
        let next = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 5]);

        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: initial,
            value: 1,
        });
        entry.push(MInst::Jump { target: BlockId(1) });

        let mut header = MBlock::new(BlockId(1));
        header.phis.push(PhiNode {
            dst: merged,
            sources: vec![(BlockId(0), initial), (BlockId(2), next)],
        });
        header.push(MInst::LoadImm {
            dst: pressure_a,
            value: 2,
        });
        header.push(MInst::LoadImm {
            dst: pressure_b,
            value: 3,
        });
        header.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 0,
            src: pressure_a,
            size: OpSize::S64,
        });
        header.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 8,
            src: pressure_b,
            size: OpSize::S64,
        });
        header.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 16,
            src: merged,
            size: OpSize::S64,
        });
        header.push(MInst::Jump { target: BlockId(2) });

        let mut latch = MBlock::new(BlockId(2));
        latch.push(MInst::Mov {
            dst: next,
            src: merged,
        });
        latch.push(MInst::Branch {
            cond: merged,
            true_bb: BlockId(1),
            false_bb: BlockId(3),
        });

        let mut exit = MBlock::new(BlockId(3));
        exit.push(MInst::Return);
        func.blocks = vec![entry, header, latch, exit];

        let cfg = super::super::cfg::normalize(&mut func).unwrap();
        let next_use = super::super::next_use::analyze(&func, &cfg).unwrap();
        let plan = plan(&func, &cfg, &next_use, 2).unwrap();
        plan.verify(&func, &cfg, 2).unwrap();

        let header = cfg.block_index[&BlockId(1)];
        let merged = LogicalValue(merged.0);
        assert!(next_use.region_at_entry(header).is_some());
        assert!(!plan.w_entry[header].contains(&merged));
        assert!(plan.point_ops.iter().any(|(point, operation)| {
            point.block == BlockId(1)
                && matches!(operation, PlannedOp::SpillPhi { value, .. } if *value == merged)
        }));
        assert!(plan.point_ops.iter().any(|(point, operation)| {
            point.block == BlockId(1)
                && point.instruction == 4
                && matches!(operation, PlannedOp::Reload { value, .. } if *value == merged)
        }));
        assert!(plan.point_ops.iter().all(|(point, operation)| {
            point.block != BlockId(1)
                || !matches!(operation, PlannedOp::Spill { value, .. } if *value == merged)
        }));
        let merged_home = plan.homes.of_logical(merged);
        for &(predecessor_id, source) in &func.blocks[header].phis[0].sources {
            let predecessor = cfg.block_index[&predecessor_id];
            assert!(
                plan.edge_ops
                    .get(&(predecessor, header))
                    .is_some_and(|operations| operations.iter().any(|operation| {
                        matches!(
                            operation,
                            PlannedEdgeOp::Spill {
                                destination,
                                destination_home,
                                ..
                            } if *destination == merged && *destination_home == merged_home
                        )
                    })),
                "missing explicit transfer into the loop-phi home: {plan:#?}"
            );
            assert_ne!(plan.homes.of_vreg(source), merged_home);
        }
    }

    #[test]
    fn single_predecessor_inherits_residency_without_edge_reconciliation() {
        let mut vregs = VRegAllocator::new();
        let value = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient()]);

        let mut predecessor = MBlock::new(BlockId(0));
        predecessor.push(MInst::LoadImm {
            dst: value,
            value: 1,
        });
        predecessor.push(MInst::Jump { target: BlockId(1) });
        let mut successor = MBlock::new(BlockId(1));
        successor.push(MInst::Return);
        func.blocks = vec![predecessor, successor];

        let cfg = super::super::cfg::normalize(&mut func).unwrap();
        let next_use = super::super::next_use::analyze(&func, &cfg).unwrap();
        let logical = LogicalValues::build(&func);
        let homes = SpillHomes::build(&func).unwrap();
        let translations = EdgeTranslations::build(&func, &cfg, &logical).unwrap();
        let mut plan = SpillPlan {
            logical,
            homes,
            point_ops: Vec::new(),
            edge_ops: BTreeMap::new(),
            recipe_reloads: BTreeSet::new(),
            recipe_homes: BTreeSet::new(),
            state_homes: BTreeMap::new(),
            state_reload_recipes: BTreeMap::new(),
            w_entry: vec![BTreeSet::new(); func.blocks.len()],
            w_exit: vec![BTreeSet::new(); func.blocks.len()],
            s_entry: vec![BTreeSet::new(); func.blocks.len()],
            s_exit: vec![BTreeSet::new(); func.blocks.len()],
        };
        let predecessor = cfg.block_index[&BlockId(0)];
        let successor = cfg.block_index[&BlockId(1)];
        let logical_value = LogicalValue(value.0);
        plan.w_exit[predecessor].insert(logical_value);
        plan.s_exit[predecessor].insert(logical_value);

        assert!(!next_use.anticipated_at_entry(successor, value));
        let planning_recipes = PlanningRecipes::stack_only(func.vregs.count());
        let inherited = init_usual(
            &func,
            &cfg,
            &next_use,
            &planning_recipes,
            &plan,
            &translations,
            successor,
            1,
        );

        assert_eq!(inherited, BTreeSet::from([logical_value]));
    }

    #[test]
    fn join_retention_pays_for_guaranteed_use_but_delays_one_arm_use() {
        let mut vregs = VRegAllocator::new();
        let condition = vregs.alloc();
        let conditional = vregs.alloc();
        let guaranteed = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 3]);

        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: condition,
            value: 1,
        });
        entry.push(MInst::LoadImm {
            dst: conditional,
            value: 2,
        });
        entry.push(MInst::LoadImm {
            dst: guaranteed,
            value: 3,
        });
        entry.push(MInst::Branch {
            cond: condition,
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        });
        let mut first = MBlock::new(BlockId(1));
        first.push(MInst::Jump { target: BlockId(3) });
        let mut second = MBlock::new(BlockId(2));
        second.push(MInst::Jump { target: BlockId(3) });
        let mut join = MBlock::new(BlockId(3));
        join.push(MInst::Branch {
            cond: condition,
            true_bb: BlockId(4),
            false_bb: BlockId(5),
        });
        let mut use_arm = MBlock::new(BlockId(4));
        use_arm.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 0,
            src: conditional,
            size: OpSize::S64,
        });
        use_arm.push(MInst::Jump { target: BlockId(6) });
        let mut skip_arm = MBlock::new(BlockId(5));
        skip_arm.push(MInst::Jump { target: BlockId(6) });
        let mut tail = MBlock::new(BlockId(6));
        tail.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 8,
            src: guaranteed,
            size: OpSize::S64,
        });
        tail.push(MInst::Return);
        func.blocks = vec![entry, first, second, join, use_arm, skip_arm, tail];

        let cfg = super::super::cfg::normalize(&mut func).unwrap();
        let next_use = super::super::next_use::analyze(&func, &cfg).unwrap();
        next_use.verify(&func, &cfg).unwrap();
        let logical = LogicalValues::build(&func);
        let homes = SpillHomes::build(&func).unwrap();
        let translations = EdgeTranslations::build(&func, &cfg, &logical).unwrap();
        let mut plan = SpillPlan {
            logical,
            homes,
            point_ops: Vec::new(),
            edge_ops: BTreeMap::new(),
            recipe_reloads: BTreeSet::new(),
            recipe_homes: BTreeSet::new(),
            state_homes: BTreeMap::new(),
            state_reload_recipes: BTreeMap::new(),
            w_entry: vec![BTreeSet::new(); func.blocks.len()],
            w_exit: vec![BTreeSet::new(); func.blocks.len()],
            s_entry: vec![BTreeSet::new(); func.blocks.len()],
            s_exit: vec![BTreeSet::new(); func.blocks.len()],
        };
        let join = cfg.block_index[&BlockId(3)];
        let predecessors = &cfg.predecessors[join];
        assert_eq!(predecessors.len(), 2);
        assert!(predecessors.iter().all(|predecessor| *predecessor < join));
        plan.w_exit[predecessors[0]]
            .extend([LogicalValue(conditional.0), LogicalValue(guaranteed.0)]);

        assert!(!next_use.anticipated_at_entry(join, conditional));
        assert!(next_use.anticipated_at_entry(join, guaranteed));
        let planning_recipes = PlanningRecipes::stack_only(func.vregs.count());
        let retained = init_usual(
            &func,
            &cfg,
            &next_use,
            &planning_recipes,
            &plan,
            &translations,
            join,
            1,
        );

        assert_eq!(retained, BTreeSet::from([LogicalValue(guaranteed.0)]));
    }

    #[test]
    fn join_retention_prices_every_use_in_a_straight_line_cluster() {
        let mut vregs = VRegAllocator::new();
        let condition = vregs.alloc();
        let single_use = vregs.alloc();
        let repeated_use = vregs.alloc();
        let sum = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 4]);

        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: condition,
            value: 1,
        });
        entry.push(MInst::LoadImm {
            dst: single_use,
            value: 2,
        });
        entry.push(MInst::LoadImm {
            dst: repeated_use,
            value: 3,
        });
        entry.push(MInst::Branch {
            cond: condition,
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        });
        let mut first = MBlock::new(BlockId(1));
        first.push(MInst::Jump { target: BlockId(3) });
        let mut second = MBlock::new(BlockId(2));
        second.push(MInst::Jump { target: BlockId(3) });
        let mut join = MBlock::new(BlockId(3));
        // Both values have the same first-use point.  A first-use-only model
        // therefore chooses the lower VReg tie-break (`single_use`), while the
        // complete local cluster must retain `repeated_use`.
        join.push(MInst::Add {
            dst: sum,
            lhs: single_use,
            rhs: repeated_use,
        });
        join.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 0,
            src: repeated_use,
            size: OpSize::S64,
        });
        join.push(MInst::Return);
        func.blocks = vec![entry, first, second, join];

        let cfg = super::super::cfg::normalize(&mut func).unwrap();
        let next_use = super::super::next_use::analyze(&func, &cfg).unwrap();
        let logical = LogicalValues::build(&func);
        let homes = SpillHomes::build(&func).unwrap();
        let translations = EdgeTranslations::build(&func, &cfg, &logical).unwrap();
        let mut plan = SpillPlan {
            logical,
            homes,
            point_ops: Vec::new(),
            edge_ops: BTreeMap::new(),
            recipe_reloads: BTreeSet::new(),
            recipe_homes: BTreeSet::new(),
            state_homes: BTreeMap::new(),
            state_reload_recipes: BTreeMap::new(),
            w_entry: vec![BTreeSet::new(); func.blocks.len()],
            w_exit: vec![BTreeSet::new(); func.blocks.len()],
            s_entry: vec![BTreeSet::new(); func.blocks.len()],
            s_exit: vec![BTreeSet::new(); func.blocks.len()],
        };
        let join = cfg.block_index[&BlockId(3)];
        let predecessors = &cfg.predecessors[join];
        plan.w_exit[predecessors[0]]
            .extend([LogicalValue(single_use.0), LogicalValue(repeated_use.0)]);
        let planning_recipes = PlanningRecipes::stack_only(func.vregs.count());

        let retained = init_usual(
            &func,
            &cfg,
            &next_use,
            &planning_recipes,
            &plan,
            &translations,
            join,
            1,
        );

        assert_eq!(retained, BTreeSet::from([LogicalValue(repeated_use.0)]));
        next_use.verify(&func, &cfg).unwrap();
    }

    #[test]
    fn reused_non_cssa_edge_source_maps_to_every_destination() {
        let mut vregs = VRegAllocator::new();
        let source = vregs.alloc();
        let first = vregs.alloc();
        let second = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 3]);
        let mut predecessor = MBlock::new(BlockId(0));
        predecessor.push(MInst::LoadImm {
            dst: source,
            value: 1,
        });
        predecessor.push(MInst::Jump { target: BlockId(1) });
        let mut successor = MBlock::new(BlockId(1));
        successor.phis = vec![
            PhiNode {
                dst: first,
                sources: vec![(BlockId(0), source)],
            },
            PhiNode {
                dst: second,
                sources: vec![(BlockId(0), source)],
            },
        ];
        successor.push(MInst::Return);
        func.blocks = vec![predecessor, successor];
        let cfg = super::super::cfg::normalize(&mut func).unwrap();
        let logical = LogicalValues::build(&func);

        let translations = EdgeTranslations::build(&func, &cfg, &logical).unwrap();
        let mapped = translations
            .to_successors(0, 1, LogicalValue(source.0))
            .collect::<Vec<_>>();

        assert_eq!(mapped, [LogicalValue(first.0), LogicalValue(second.0)]);
        assert_eq!(
            translations.to_predecessor(0, 1, LogicalValue(first.0)),
            LogicalValue(source.0)
        );
        assert_eq!(
            translations.to_predecessor(0, 1, LogicalValue(second.0)),
            LogicalValue(source.0)
        );
    }

    #[test]
    fn excessive_operand_pressure_is_a_structured_error() {
        let mut vregs = VRegAllocator::new();
        let left = vregs.alloc();
        let right = vregs.alloc();
        let result = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 3]);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::LoadImm {
            dst: left,
            value: 1,
        });
        block.push(MInst::LoadImm {
            dst: right,
            value: 2,
        });
        block.push(MInst::Add {
            dst: result,
            lhs: left,
            rhs: right,
        });
        block.push(MInst::Return);
        func.blocks.push(block);
        let cfg = super::super::cfg::normalize(&mut func).unwrap();
        let next_use = super::super::next_use::analyze(&func, &cfg).unwrap();

        let error = plan(&func, &cfg, &next_use, 1).unwrap_err();

        assert_eq!(error.rule, "SPILL_PLAN.OPERAND_PRESSURE");
        assert_eq!(error.block, Some(BlockId(0)));
        assert_eq!(error.instruction, Some(2));
        assert_eq!(error.values, vec![left, right]);
    }

    #[test]
    fn excessive_clobber_pressure_is_a_structured_error() {
        let mut vregs = VRegAllocator::new();
        let input = vregs.alloc();
        let result = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 2]);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::LoadImm {
            dst: input,
            value: 1,
        });
        block.push(MInst::UDiv {
            dst: result,
            lhs: input,
            rhs: input,
        });
        block.push(MInst::Return);
        func.blocks.push(block);
        let cfg = super::super::cfg::normalize(&mut func).unwrap();
        let next_use = super::super::next_use::analyze(&func, &cfg).unwrap();

        let error = plan(&func, &cfg, &next_use, 1).unwrap_err();

        assert_eq!(error.rule, "SPILL_PLAN.CLOBBER_CAPACITY");
        assert_eq!(error.block, Some(BlockId(0)));
        assert_eq!(error.instruction, Some(1));
    }

    #[test]
    fn eviction_uses_target_cost_density_and_preserves_min_as_tie_breaker() {
        let mut vregs = VRegAllocator::new();
        let cheap = vregs.alloc();
        let costly = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::remat(1), SpillDesc::transient()]);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::Return);
        func.push_block(block);
        let cfg = super::super::cfg::normalize(&mut func).unwrap();
        let next_use = super::super::next_use::analyze(&func, &cfg).unwrap();
        let planning_recipes = PlanningRecipes::stack_only(func.vregs.count());
        let spilled = BTreeSet::new();
        let future = LinearFutureUses {
            func: &func,
            next_use: &next_use,
            block: 0,
            instruction: 0,
        };
        let local = |instructions| NextUseDistance::Finite {
            loop_exits: 0,
            instructions,
        };

        assert_eq!(
            compare_eviction_candidates(
                &func,
                &planning_recipes,
                &spilled,
                &future,
                (LogicalValue(cheap.0), local(1)),
                (LogicalValue(costly.0), local(1)),
            ),
            Ordering::Greater,
            "equal spans must evict the cheaper rematerializable value"
        );
        assert_eq!(
            compare_eviction_candidates(
                &func,
                &planning_recipes,
                &spilled,
                &future,
                (LogicalValue(cheap.0), local(1)),
                (LogicalValue(costly.0), local(15)),
            ),
            Ordering::Less,
            "a sufficiently long occupancy interval must outweigh a larger split cost"
        );

        let mut equal_cost = MFunction::new(
            func.vregs.clone(),
            vec![SpillDesc::transient(), SpillDesc::transient()],
        );
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::Return);
        equal_cost.push_block(block);
        let equal_cfg = super::super::cfg::normalize(&mut equal_cost).unwrap();
        let equal_next_use = super::super::next_use::analyze(&equal_cost, &equal_cfg).unwrap();
        let equal_recipes = PlanningRecipes::stack_only(equal_cost.vregs.count());
        let equal_future = LinearFutureUses {
            func: &equal_cost,
            next_use: &equal_next_use,
            block: 0,
            instruction: 0,
        };
        assert_eq!(
            compare_eviction_candidates(
                &equal_cost,
                &equal_recipes,
                &spilled,
                &equal_future,
                (LogicalValue(cheap.0), local(2)),
                (LogicalValue(costly.0), local(8)),
            ),
            Ordering::Less,
            "equal target costs must reduce to furthest-next-use MIN"
        );
        let exact_recipes = PlanningRecipes::with_global_costs(vec![Some(1), None]);
        assert_eq!(
            compare_eviction_candidates(
                &equal_cost,
                &exact_recipes,
                &spilled,
                &equal_future,
                (LogicalValue(cheap.0), local(2)),
                (LogicalValue(costly.0), local(2)),
            ),
            Ordering::Greater,
            "an exact reload recipe must override the stale transient descriptor cost"
        );
    }

    #[test]
    fn point_specific_memoryssa_cost_changes_the_allocator_owned_split() {
        let mut vregs = VRegAllocator::new();
        let state_backed = vregs.alloc();
        let stack_backed = vregs.alloc();
        let near_use = vregs.alloc();
        let pressure = vregs.alloc();
        let sum = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 5]);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::Load {
            dst: state_backed,
            base: BaseReg::StackFrame,
            offset: 0,
            size: OpSize::S64,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 40,
            src: state_backed,
            size: OpSize::S64,
        });
        block.push(MInst::Load {
            dst: stack_backed,
            base: BaseReg::StackFrame,
            offset: 8,
            size: OpSize::S64,
        });
        block.push(MInst::Load {
            dst: near_use,
            base: BaseReg::StackFrame,
            offset: 16,
            size: OpSize::S64,
        });
        block.push(MInst::LoadImm {
            dst: pressure,
            value: 0,
        });
        block.push(MInst::Store {
            base: BaseReg::StackFrame,
            offset: 24,
            src: near_use,
            size: OpSize::S64,
        });
        block.push(MInst::Add {
            dst: sum,
            lhs: state_backed,
            rhs: stack_backed,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 48,
            src: sum,
            size: OpSize::S64,
        });
        block.push(MInst::Return);
        func.push_block(block);
        let cfg = super::super::cfg::normalize(&mut func).unwrap();
        let next_use = super::super::next_use::analyze(&func, &cfg).unwrap();
        let exact = super::super::reload::analyze_for_planning(&func, &cfg).unwrap();
        let stack_only = PlanningRecipes::stack_only(func.vregs.count());

        let exact_plan = plan_with_recipe_costs(&func, &cfg, &next_use, &exact, 3).unwrap();
        let stack_plan = plan_with_recipe_costs(&func, &cfg, &next_use, &stack_only, 3).unwrap();
        let split_at_pressure = |plan: &SpillPlan| {
            plan.point_ops.iter().find_map(|(point, operation)| {
                (point.block == BlockId(0) && point.instruction == 4)
                    .then_some(operation)
                    .and_then(|operation| match operation {
                        PlannedOp::Spill { value, .. } => Some(*value),
                        _ => None,
                    })
            })
        };

        assert_eq!(split_at_pressure(&exact_plan), None);
        assert!(
            exact_plan
                .recipe_reloads
                .contains(&(BlockId(0), 6, LogicalValue(state_backed.0)))
        );
        assert!(exact_plan.point_ops.iter().any(|(point, operation)| {
            point.block == BlockId(0)
                && point.instruction == 6
                && matches!(
                    operation,
                    PlannedOp::Reload { value, .. }
                        if *value == LogicalValue(state_backed.0)
                )
        }));
        assert_eq!(
            split_at_pressure(&stack_plan),
            Some(LogicalValue(stack_backed.0)),
            "without a point recipe equal-cost MIN uses the deterministic VReg tie-break"
        );
    }

    #[test]
    fn point_recipe_splits_one_cluster_before_a_later_invalidated_use() {
        let mut vregs = VRegAllocator::new();
        let stored = vregs.alloc();
        let pressure = vregs.alloc();
        let overwrite = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 3]);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::Load {
            dst: stored,
            base: BaseReg::StackFrame,
            offset: 0,
            size: OpSize::S64,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 40,
            src: stored,
            size: OpSize::S64,
        });
        block.push(MInst::Load {
            dst: pressure,
            base: BaseReg::StackFrame,
            offset: 8,
            size: OpSize::S64,
        });
        block.push(MInst::Store {
            base: BaseReg::StackFrame,
            offset: 24,
            src: pressure,
            size: OpSize::S64,
        });
        block.push(MInst::Store {
            base: BaseReg::StackFrame,
            offset: 32,
            src: stored,
            size: OpSize::S64,
        });
        block.push(MInst::Load {
            dst: overwrite,
            base: BaseReg::StackFrame,
            offset: 16,
            size: OpSize::S64,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 40,
            src: overwrite,
            size: OpSize::S64,
        });
        block.push(MInst::Store {
            base: BaseReg::StackFrame,
            offset: 40,
            src: stored,
            size: OpSize::S64,
        });
        block.push(MInst::Return);
        func.push_block(block);
        let cfg = super::super::cfg::normalize(&mut func).unwrap();
        let next_use = super::super::next_use::analyze(&func, &cfg).unwrap();
        let recipes = super::super::reload::analyze_for_planning(&func, &cfg).unwrap();

        let plan = plan_with_recipe_costs(&func, &cfg, &next_use, &recipes, 1).unwrap();

        assert!(
            plan.recipe_reloads
                .contains(&(BlockId(0), 4, LogicalValue(stored.0)))
        );
        assert!(plan.point_ops.iter().any(|(point, operation)| {
            point.block == BlockId(0)
                && point.instruction == 5
                && matches!(
                    operation,
                    PlannedOp::Spill { value, .. } if *value == LogicalValue(stored.0)
                )
        }));
        assert!(!plan.point_ops.iter().any(|(point, operation)| {
            point.block == BlockId(0)
                && point.instruction == 2
                && matches!(
                    operation,
                    PlannedOp::Spill { value, .. } if *value == LogicalValue(stored.0)
                )
        }));
    }

    #[test]
    fn stable_recipe_identity_materializes_at_the_output_position() {
        let mut vregs = VRegAllocator::new();
        let stored = vregs.alloc();
        let pressure = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 2]);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::Load {
            dst: stored,
            base: BaseReg::StackFrame,
            offset: 0,
            size: OpSize::S64,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 40,
            src: stored,
            size: OpSize::S64,
        });
        block.push(MInst::Load {
            dst: pressure,
            base: BaseReg::StackFrame,
            offset: 8,
            size: OpSize::S64,
        });
        block.push(MInst::Store {
            base: BaseReg::StackFrame,
            offset: 16,
            src: pressure,
            size: OpSize::S64,
        });
        block.push(MInst::Store {
            base: BaseReg::StackFrame,
            offset: 24,
            src: stored,
            size: OpSize::S64,
        });
        block.push(MInst::Return);
        func.push_block(block);
        let cfg = super::super::cfg::normalize(&mut func).unwrap();
        let next_use = super::super::next_use::analyze(&func, &cfg).unwrap();
        let recipes = super::super::reload::analyze_for_planning(&func, &cfg).unwrap();
        let logical = LogicalValues::build(&func);
        let homes = SpillHomes::build(&func).unwrap();
        let mut planner = BlockTransitionPlanner::new(
            &func,
            &next_use,
            &recipes,
            &logical,
            &homes,
            0,
            1,
            &BTreeSet::new(),
            BTreeSet::new(),
        )
        .unwrap();
        for (source, inst) in func.blocks[0].insts.iter().enumerate() {
            planner
                .step(
                    TransitionPoint {
                        output: if source >= 4 { source + 3 } else { source },
                        source,
                    },
                    inst,
                )
                .unwrap();
        }
        let transition = planner.finish().unwrap();
        let stored = LogicalValue(stored.0);

        assert!(transition.recipe_reloads.contains(&(BlockId(0), 7, stored)));
        assert!(transition.point_ops.iter().any(|(point, operation)| {
            point.instruction == 7
                && matches!(operation, PlannedOp::Reload { value, .. } if *value == stored)
        }));
    }

    #[test]
    fn whole_recipe_home_requires_an_exact_recipe_at_every_selected_reload() {
        let mut vregs = VRegAllocator::new();
        let stored = vregs.alloc();
        let overwrite = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 2]);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::Load {
            dst: stored,
            base: BaseReg::StackFrame,
            offset: 0,
            size: OpSize::S64,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 40,
            src: stored,
            size: OpSize::S64,
        });
        block.push(MInst::Store {
            base: BaseReg::StackFrame,
            offset: 16,
            src: stored,
            size: OpSize::S64,
        });
        block.push(MInst::Load {
            dst: overwrite,
            base: BaseReg::StackFrame,
            offset: 8,
            size: OpSize::S64,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 40,
            src: overwrite,
            size: OpSize::S64,
        });
        block.push(MInst::Store {
            base: BaseReg::StackFrame,
            offset: 24,
            src: stored,
            size: OpSize::S64,
        });
        block.push(MInst::Return);
        func.push_block(block);
        let cfg = super::super::cfg::normalize(&mut func).unwrap();
        let next_use = super::super::next_use::analyze(&func, &cfg).unwrap();
        let mut plan = plan(&func, &cfg, &next_use, 2).unwrap();
        plan.point_ops.clear();
        plan.edge_ops.clear();
        plan.recipe_reloads.clear();
        plan.recipe_homes.clear();
        let logical = LogicalValue(stored.0);
        let home = plan.homes.of_logical(logical);
        let reload = |instruction| {
            (
                ProgramPoint {
                    block: BlockId(0),
                    instruction,
                    side: PointSide::Before,
                },
                PlannedOp::Reload {
                    value: logical,
                    home,
                },
            )
        };
        plan.point_ops.extend([reload(2), reload(5)]);
        let requested = super::super::ssa::planner_reload_queries(&func, &cfg, &plan).unwrap();
        let recipes = super::super::reload::analyze_with_queries(&func, &cfg, &requested).unwrap();

        plan.select_recipe_homes(&func, &cfg, &recipes).unwrap();
        assert!(plan.recipe_homes.is_empty());

        plan.point_ops.pop();
        plan.select_recipe_homes(&func, &cfg, &recipes).unwrap();
        assert_eq!(plan.recipe_homes, BTreeSet::from([home]));
        plan.verify_recipe_homes(&func, &cfg, &recipes).unwrap();

        plan.point_ops.push(reload(5));
        let error = plan.verify_recipe_homes(&func, &cfg, &recipes).unwrap_err();
        assert_eq!(error.rule, "SPILL_PLAN.RECIPE_HOME_POINT");
        assert_eq!(error.block, Some(BlockId(0)));
        assert_eq!(error.instruction, Some(5));
    }

    #[test]
    fn whole_recipe_home_uses_the_existing_mixed_plan_as_its_baseline() {
        let mut vregs = VRegAllocator::new();
        let base = vregs.alloc();
        let first = vregs.alloc();
        let value = vregs.alloc();
        let overwrite = vregs.alloc();
        let mut spill_descs = vec![SpillDesc::transient(); 4];
        spill_descs[value.0 as usize].spill_cost = 1;
        let mut func = MFunction::new(vregs, spill_descs);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::Load {
            dst: base,
            base: BaseReg::SimState,
            offset: 40,
            size: OpSize::S64,
        });
        block.push(MInst::BitNot {
            dst: first,
            src: base,
        });
        block.push(MInst::BitNot {
            dst: value,
            src: first,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 48,
            src: value,
            size: OpSize::S64,
        });
        block.push(MInst::Store {
            base: BaseReg::StackFrame,
            offset: 0,
            src: value,
            size: OpSize::S64,
        });
        block.push(MInst::Load {
            dst: overwrite,
            base: BaseReg::StackFrame,
            offset: 8,
            size: OpSize::S64,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 48,
            src: overwrite,
            size: OpSize::S64,
        });
        block.push(MInst::Store {
            base: BaseReg::StackFrame,
            offset: 16,
            src: value,
            size: OpSize::S64,
        });
        block.push(MInst::Return);
        func.push_block(block);

        let cfg = super::super::cfg::normalize(&mut func).unwrap();
        let next_use = super::super::next_use::analyze(&func, &cfg).unwrap();
        let mut plan = plan(&func, &cfg, &next_use, 4).unwrap();
        plan.point_ops.clear();
        plan.edge_ops.clear();
        plan.recipe_reloads.clear();
        plan.recipe_homes.clear();
        let logical = LogicalValue(value.0);
        let home = plan.homes.of_logical(logical);
        let point = |instruction| ProgramPoint {
            block: BlockId(0),
            instruction,
            side: PointSide::Before,
        };
        plan.point_ops.extend([
            (
                point(3),
                PlannedOp::Spill {
                    value: logical,
                    home,
                },
            ),
            (
                point(4),
                PlannedOp::Reload {
                    value: logical,
                    home,
                },
            ),
            (
                point(7),
                PlannedOp::Reload {
                    value: logical,
                    home,
                },
            ),
        ]);
        plan.recipe_reloads.insert((BlockId(0), 4, logical));
        let requested = super::super::ssa::planner_reload_queries(&func, &cfg, &plan).unwrap();
        let recipes = super::super::reload::analyze_with_queries(&func, &cfg, &requested).unwrap();
        let recipe_cost = |instruction| {
            recipes
                .resolved_recipe_at_point(PointUse {
                    block: BlockId(0),
                    instruction,
                    value,
                })
                .map(|recipe| recipe.steps.len() + 1)
        };
        assert_eq!(recipe_cost(4), Some(1));
        assert_eq!(recipe_cost(7), Some(3));

        plan.select_recipe_homes(&func, &cfg, &recipes).unwrap();
        assert!(
            plan.recipe_homes.is_empty(),
            "the all-recipe cost ties the selected point-recipe, stack-reload, and spill baseline"
        );
    }

    #[test]
    fn whole_recipe_home_compares_complete_stack_and_recipe_costs() {
        let mut vregs = VRegAllocator::new();
        let base = vregs.alloc();
        let first = vregs.alloc();
        let value = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 3]);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::Load {
            dst: base,
            base: BaseReg::SimState,
            offset: 40,
            size: OpSize::S64,
        });
        block.push(MInst::BitNot {
            dst: first,
            src: base,
        });
        block.push(MInst::BitNot {
            dst: value,
            src: first,
        });
        block.push(MInst::Store {
            base: BaseReg::StackFrame,
            offset: 0,
            src: value,
            size: OpSize::S64,
        });
        block.push(MInst::Return);
        func.push_block(block);
        let cfg = super::super::cfg::normalize(&mut func).unwrap();
        let next_use = super::super::next_use::analyze(&func, &cfg).unwrap();
        let mut plan = plan(&func, &cfg, &next_use, 3).unwrap();
        plan.point_ops.clear();
        plan.edge_ops.clear();
        plan.recipe_reloads.clear();
        plan.recipe_homes.clear();
        let logical = LogicalValue(value.0);
        let home = plan.homes.of_logical(logical);
        let point = ProgramPoint {
            block: BlockId(0),
            instruction: 3,
            side: PointSide::Before,
        };
        plan.point_ops.push((
            point,
            PlannedOp::Reload {
                value: logical,
                home,
            },
        ));
        let requested = super::super::ssa::planner_reload_queries(&func, &cfg, &plan).unwrap();
        let recipes = super::super::reload::analyze_with_queries(&func, &cfg, &requested).unwrap();

        plan.select_recipe_homes(&func, &cfg, &recipes).unwrap();
        assert!(
            plan.recipe_homes.is_empty(),
            "a three-instruction pure recipe must not replace a two-cost stack reload"
        );

        plan.point_ops.push((
            point,
            PlannedOp::Spill {
                value: logical,
                home,
            },
        ));
        plan.select_recipe_homes(&func, &cfg, &recipes).unwrap();
        assert_eq!(
            plan.recipe_homes,
            BTreeSet::from([home]),
            "avoiding the spill and reload makes the three-instruction recipe cheaper"
        );
        plan.verify_recipe_homes(&func, &cfg, &recipes).unwrap();
    }

    #[test]
    fn stale_state_table_is_a_structured_error() {
        let mut func = MFunction::new(VRegAllocator::new(), Vec::new());
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::Return);
        func.blocks.push(block);
        let cfg = super::super::cfg::normalize(&mut func).unwrap();
        let next_use = super::super::next_use::analyze(&func, &cfg).unwrap();
        let mut plan = plan(&func, &cfg, &next_use, 1).unwrap();
        plan.w_entry.pop();

        let error = plan.verify(&func, &cfg, 1).unwrap_err();

        assert_eq!(error.rule, "SPILL_PLAN.STATE_SHAPE");
        assert_eq!(error.block, None);
    }

    #[test]
    fn descending_large_phi_web_keeps_independent_homes_without_recursion() {
        const MEMBERS: u32 = 50_000;

        let mut vregs = VRegAllocator::new();
        for _ in 0..MEMBERS {
            vregs.alloc();
        }
        let mut func = MFunction::new(vregs, Vec::new());
        let mut block = MBlock::new(BlockId(0));
        for destination in (1..MEMBERS).rev() {
            block.phis.push(PhiNode {
                dst: VReg(destination),
                sources: vec![(BlockId(0), VReg(destination - 1))],
            });
        }
        func.blocks.push(block);

        let homes = SpillHomes::build(&func).unwrap();

        assert_eq!(homes.of_vreg(VReg(0)), SpillHome(0));
        assert_eq!(homes.of_vreg(VReg(MEMBERS - 1)), SpillHome(MEMBERS - 1));
        assert_eq!(homes.members(SpillHome(0)).count(), 1);
    }

    #[test]
    fn large_phi_join_is_indexed_once_in_both_directions() {
        const PREDECESSORS: usize = 64;
        const PHIS: usize = 512;
        const INTERNAL_BLOCKS: usize = PREDECESSORS - 1;
        const TREE_BLOCKS: usize = PREDECESSORS * 2 - 1;
        let join_id = BlockId(TREE_BLOCKS as u32);
        let mut vregs = VRegAllocator::new();
        let condition = vregs.alloc();
        let mut expected = Vec::with_capacity(PREDECESSORS * PHIS);
        let mut phis = Vec::with_capacity(PHIS);
        let mut leaf_definitions = (0..PREDECESSORS)
            .map(|_| Vec::with_capacity(PHIS))
            .collect::<Vec<_>>();
        for _ in 0..PHIS {
            let mut sources = Vec::with_capacity(PREDECESSORS);
            for (predecessor, definitions) in leaf_definitions.iter_mut().enumerate() {
                let source = vregs.alloc();
                let predecessor_id = BlockId((INTERNAL_BLOCKS + predecessor) as u32);
                sources.push((predecessor_id, source));
                definitions.push(source);
            }
            let destination = vregs.alloc();
            expected.extend(
                sources
                    .iter()
                    .map(|&(predecessor, source)| (predecessor, source, destination)),
            );
            phis.push(PhiNode {
                dst: destination,
                sources,
            });
        }

        let spill_descs = vec![SpillDesc::transient(); vregs.count() as usize];
        let mut func = MFunction::new(vregs, spill_descs);
        // A complete binary branch tree makes every one of the 64 eventual
        // join predecessors reachable from the single MIR entry block.
        for block_index in 0..INTERNAL_BLOCKS {
            let mut block = MBlock::new(BlockId(block_index as u32));
            if block_index == 0 {
                block.push(MInst::LoadImm {
                    dst: condition,
                    value: 1,
                });
            }
            block.push(MInst::Branch {
                cond: condition,
                true_bb: BlockId((block_index * 2 + 1) as u32),
                false_bb: BlockId((block_index * 2 + 2) as u32),
            });
            func.blocks.push(block);
        }
        for (predecessor, definitions) in leaf_definitions.iter().enumerate() {
            let predecessor_id = BlockId((INTERNAL_BLOCKS + predecessor) as u32);
            let mut block = MBlock::new(predecessor_id);
            for &source in definitions {
                block.push(MInst::LoadImm {
                    dst: source,
                    value: source.0 as u64,
                });
            }
            block.push(MInst::Jump { target: join_id });
            func.blocks.push(block);
        }
        let mut join = MBlock::new(join_id);
        join.phis = phis;
        join.push(MInst::Return);
        func.blocks.push(join);
        func.verify();
        let cfg = super::super::cfg::normalize(&mut func).unwrap();
        func.verify();

        let logical = LogicalValues::build(&func);
        let translations = EdgeTranslations::build(&func, &cfg, &logical).unwrap();

        let join = cfg.block_index[&join_id];
        for (predecessor_id, source, destination) in expected {
            let predecessor = cfg.block_index[&predecessor_id];
            assert!(
                translations
                    .to_successors(predecessor, join, LogicalValue(source.0))
                    .any(|value| value == LogicalValue(destination.0))
            );
            assert_eq!(
                translations.to_predecessor(predecessor, join, LogicalValue(destination.0),),
                LogicalValue(source.0)
            );
        }
    }

    #[test]
    fn irreducible_scc_entries_prioritize_values_used_in_the_region() {
        use crate::backend::native::mir::{BaseReg, OpSize, SpillDesc};
        use crate::backend::native::regalloc::next_use::{self, LoopRegionKind};

        let mut vregs = VRegAllocator::new();
        let hot = vregs.alloc();
        let live_through = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 2]);

        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm { dst: hot, value: 1 });
        entry.push(MInst::LoadImm {
            dst: live_through,
            value: 2,
        });
        entry.push(MInst::Branch {
            cond: hot,
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        });

        let mut left_entry = MBlock::new(BlockId(1));
        left_entry.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 0,
            src: hot,
            size: OpSize::S64,
        });
        left_entry.push(MInst::Branch {
            cond: hot,
            true_bb: BlockId(2),
            false_bb: BlockId(3),
        });

        let mut right_entry = MBlock::new(BlockId(2));
        right_entry.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 8,
            src: hot,
            size: OpSize::S64,
        });
        right_entry.push(MInst::Branch {
            cond: hot,
            true_bb: BlockId(1),
            false_bb: BlockId(3),
        });

        let mut exit = MBlock::new(BlockId(3));
        exit.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 16,
            src: live_through,
            size: OpSize::S64,
        });
        exit.push(MInst::Return);
        func.blocks = vec![entry, left_entry, right_entry, exit];

        let cfg = super::super::cfg::normalize(&mut func).unwrap();
        let next_use = next_use::analyze(&func, &cfg).unwrap();
        let plan = plan(&func, &cfg, &next_use, 1).unwrap();
        let left = cfg.block_index[&BlockId(1)];
        let right = cfg.block_index[&BlockId(2)];
        let region = next_use.region_at_entry(left).unwrap();
        assert_eq!(next_use.region_at_entry(right), Some(region));
        assert_eq!(
            next_use.loop_regions[region].kind,
            LoopRegionKind::IrreducibleScc
        );
        for entry in [left, right] {
            assert_eq!(plan.w_entry[entry], BTreeSet::from([LogicalValue(hot.0)]));
            assert!(!plan.w_entry[entry].contains(&LogicalValue(live_through.0)));
        }
    }
}
