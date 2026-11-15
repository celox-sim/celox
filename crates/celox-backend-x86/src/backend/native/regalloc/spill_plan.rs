//! Braun--Hack sections 4.2 and 4.3: W/S states and coupling plan.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};

use crate::HashMap;
use crate::native::mir::{BlockId, MFunction, MInst, PackedStateHome, VReg};

use super::cfg::NormalizedCfg;
use super::next_use::{DistanceMap, NextUseAnalysis, NextUseDistance};
use super::reload::{EdgeUse, PlanningRecipes, PointUse, ReloadRecipeAnalysis, ResolvedRecipe};

mod eviction;
mod logical_set;
mod transition;
mod verify;
use eviction::*;
pub(super) use logical_set::*;
use transition::*;

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

pub(super) fn edge_reload_uses_transferred_home(
    operations: &[PlannedEdgeOp],
    reload_index: usize,
) -> bool {
    let Some(PlannedEdgeOp::Reload {
        source,
        source_home,
        destination,
    }) = operations.get(reload_index).copied()
    else {
        return false;
    };
    operations[..reload_index].iter().any(|operation| {
        matches!(
            operation,
            PlannedEdgeOp::Spill {
                source: spill_source,
                destination: spill_destination,
                destination_home,
            } if *spill_source == source
                && *spill_destination == destination
                && *destination_home == source_home
        )
    })
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
    pub w_entry: Vec<LogicalSet>,
    pub w_exit: Vec<LogicalSet>,
    pub s_entry: Vec<LogicalSet>,
    pub s_exit: Vec<LogicalSet>,
}

#[derive(Debug)]
struct BlockTransition {
    point_ops: Vec<(ProgramPoint, PlannedOp)>,
    recipe_reloads: BTreeSet<(BlockId, usize, LogicalValue)>,
    w_exit: LogicalSet,
    s_exit: LogicalSet,
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
/// has exactly one source on an edge. One source may feed several phi
/// destinations, so the forward relation is one-to-many.
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
        let mut by_edge = HashMap::<(usize, usize), EdgeTranslation>::default();
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
        w_entry: vec![LogicalSet::new(); func.blocks.len()],
        w_exit: vec![LogicalSet::new(); func.blocks.len()],
        s_entry: vec![LogicalSet::new(); func.blocks.len()],
        s_exit: vec![LogicalSet::new(); func.blocks.len()],
    };
    #[expect(
        clippy::disallowed_types,
        reason = "single-threaded: local lazily built logical-value key list, created only when a block's spill sets are first frozen"
    )]
    let logical_keys = std::cell::OnceCell::<std::sync::Arc<[LogicalValue]>>::new();
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
            .collect::<Result<LogicalSet, _>>()?;
        let mut exit_reload_costs = LazyExitReloadCosts {
            func,
            cfg,
            next_use,
            recipes: planning_recipes,
            translations: &edge_translations,
            block,
            cache: HashMap::default(),
        };
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
                    &mut exit_reload_costs,
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
        if let Some(order) = order
            && !order.iter().copied().eq(0..func.blocks[block].insts.len())
        {
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
        if result.s_entry[block].len() >= 64 || result.s_exit[block].len() >= 64 {
            let logical_keys =
                logical_keys.get_or_init(|| (0..result.logical.count).map(LogicalValue).collect());
            let same_spilled = result.s_entry[block] == result.s_exit[block];
            result.s_entry[block].freeze(logical_keys);
            if same_spilled && matches!(result.s_entry[block], LogicalSet::Frozen(_)) {
                result.s_exit[block] = result.s_entry[block].clone();
            } else {
                result.s_exit[block].freeze(logical_keys);
            }
        }
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
        .collect::<LogicalSet>();
    for successor in 0..func.blocks.len() {
        for &predecessor in &cfg.predecessors[successor] {
            let mut resident_spills = Vec::new();
            let mut home_transfers = Vec::new();
            let mut scratch_reloads = Vec::new();
            let mut resident_reloads = Vec::new();
            let predecessor_w = result.w_exit[predecessor].clone();
            let predecessor_s = result.s_exit[predecessor].clone();
            let translation = edge_translations.by_edge.get(&(predecessor, successor));
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
            let renamed = translation.is_some_and(|edge| {
                edge.to_predecessor
                    .iter()
                    .any(|(destination, source)| destination != source)
            });
            if renamed || result.s_entry[successor] != predecessor_s {
                let mut predecessor_membership =
                    OrderedMembership::new(&predecessor_s, result.s_entry[successor].len());
                for &successor_value in &result.s_entry[successor] {
                    let value = translation
                        .and_then(|edge| edge.to_predecessor.get(&successor_value))
                        .copied()
                        .unwrap_or(successor_value);
                    let source_home = result.homes.of_logical(value);
                    let destination_home = result.homes.of_logical(successor_value);
                    // Only unchanged logical values query the merge cursor. Phi
                    // sources can arrive in any order and retain direct lookup.
                    if source_home == destination_home
                        && predecessor_membership.contains(&successor_value)
                    {
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
            // slot. Explicitly park one such value across all home transfers.
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
                        // The reload is physically read from the successor
                        // home, but it must split the predecessor logical live
                        // range across every home transfer.
                        source,
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

#[cfg(test)]
mod tests;
