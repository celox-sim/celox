//! Braun--Hack sections 4.2 and 4.3: W/S states and coupling plan.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::backend::native::mir::{BlockId, MFunction, PackedStateHome, VReg};

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

#[derive(Debug)]
pub(super) struct SpillPlan {
    pub logical: LogicalValues,
    pub homes: PhiCongruenceClasses,
    pub point_ops: Vec<(ProgramPoint, PlannedOp)>,
    pub edge_ops: BTreeMap<(usize, usize), Vec<PlannedOp>>,
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
pub(super) struct PhiCongruenceClasses {
    home_for_vreg: Vec<SpillHome>,
    nontrivial_members: HashMap<SpillHome, Vec<VReg>>,
}

impl PhiCongruenceClasses {
    fn build(func: &MFunction) -> Result<Self, SpillPlanError> {
        let count = func.vregs.count() as usize;
        let mut classes = DisjointSets::new(count);
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
                    classes.union(phi.dst.0, source.0);
                }
            }
        }
        let home_for_vreg = (0..count as u32)
            .map(|value| SpillHome(classes.minimum(value)))
            .collect::<Vec<_>>();
        let mut counts = vec![0u32; count];
        for &home in &home_for_vreg {
            counts[home.0 as usize] += 1;
        }
        let mut nontrivial_members = HashMap::<SpillHome, Vec<VReg>>::new();
        for (value, &home) in home_for_vreg.iter().enumerate() {
            if counts[home.0 as usize] > 1 {
                nontrivial_members
                    .entry(home)
                    .or_default()
                    .push(VReg(value as u32));
            }
        }
        Ok(Self {
            home_for_vreg,
            nontrivial_members,
        })
    }

    pub(super) fn of_vreg(&self, value: VReg) -> SpillHome {
        self.home_for_vreg[value.0 as usize]
    }

    pub(super) fn of_logical(&self, value: LogicalValue) -> SpillHome {
        self.home_for_vreg[value.0 as usize]
    }

    pub(super) fn members(&self, home: SpillHome) -> impl Iterator<Item = VReg> + '_ {
        self.nontrivial_members
            .get(&home)
            .into_iter()
            .flatten()
            .copied()
            .chain((!self.nontrivial_members.contains_key(&home)).then_some(VReg(home.0)))
    }
}

/// Iterative union--find whose balanced tree shape is independent from the
/// externally visible (minimum-VReg) spill-home identifier.
struct DisjointSets {
    parent: Vec<u32>,
    size: Vec<u32>,
    minimum: Vec<u32>,
}

impl DisjointSets {
    fn new(count: usize) -> Self {
        Self {
            parent: (0..count as u32).collect(),
            size: vec![1; count],
            minimum: (0..count as u32).collect(),
        }
    }

    fn root(&mut self, value: u32) -> u32 {
        let mut root = value;
        while self.parent[root as usize] != root {
            root = self.parent[root as usize];
        }

        let mut current = value;
        while self.parent[current as usize] != current {
            let next = self.parent[current as usize];
            self.parent[current as usize] = root;
            current = next;
        }
        root
    }

    fn union(&mut self, left: u32, right: u32) {
        let mut left = self.root(left);
        let mut right = self.root(right);
        if left == right {
            return;
        }
        if self.size[left as usize] < self.size[right as usize] {
            std::mem::swap(&mut left, &mut right);
        }
        let combined_size = self.size[left as usize] + self.size[right as usize];
        let combined_minimum = self.minimum[left as usize].min(self.minimum[right as usize]);
        self.parent[right as usize] = left;
        self.size[left as usize] = combined_size;
        self.minimum[left as usize] = combined_minimum;
    }

    fn minimum(&mut self, value: u32) -> u32 {
        let root = self.root(value);
        self.minimum[root as usize]
    }
}

#[derive(Debug, Default)]
struct EdgeTranslation {
    to_successor: HashMap<LogicalValue, LogicalValue>,
    to_predecessor: HashMap<LogicalValue, LogicalValue>,
}

/// O(1) logical-value translation across normalized phi edges.
///
/// Building the two directions in one pass over phi operands avoids rescanning
/// every phi (and its predecessor list) for every member of W/S.  Method-I CSSA
/// gives each phi operand a fresh edge-local name, so both maps are one-to-one.
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
                    if translation
                        .to_successor
                        .insert(source, destination)
                        .is_some()
                    {
                        return Err(SpillPlanError::new(
                            "SPILL_PLAN.PHI_SOURCE_UNIQUE",
                            Some(predecessor_id),
                            None,
                            vec![VReg(source.0), VReg(destination.0)],
                            format!(
                                "Method-I CSSA edge {predecessor_id} -> {} reuses phi source v{}",
                                block.id, source.0
                            ),
                        ));
                    }
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

    fn to_successor(
        &self,
        predecessor: usize,
        successor: usize,
        value: LogicalValue,
    ) -> LogicalValue {
        self.by_edge
            .get(&(predecessor, successor))
            .and_then(|translation| translation.to_successor.get(&value))
            .copied()
            .unwrap_or(value)
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

pub(super) fn plan_with_recipe_costs(
    func: &MFunction,
    cfg: &NormalizedCfg,
    next_use: &NextUseAnalysis,
    planning_recipes: &PlanningRecipes,
    registers: usize,
) -> Result<SpillPlan, SpillPlanError> {
    let logical = LogicalValues::build(func);
    let homes = PhiCongruenceClasses::build(func)?;
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
        result.w_entry[block] = entry;
        let live_entry = next_use.entry[block]
            .keys()
            .copied()
            .map(|value| {
                result
                    .logical
                    .checked_of(value, Some(func.blocks[block].id), Some(0))
            })
            .collect::<Result<BTreeSet<_>, _>>()?;
        // S means that a valid home exists on every path.  Every live value
        // omitted from W_entry therefore requires a home; edge coupling below
        // materializes any missing predecessor store.  A resident value keeps
        // an existing home only when every predecessor already has one.
        let mut spilled = live_entry
            .difference(&result.w_entry[block])
            .copied()
            .collect::<BTreeSet<_>>();
        if !cfg.predecessors[block].is_empty() {
            spilled.extend(result.w_entry[block].iter().copied().filter(|value| {
                cfg.predecessors[block].iter().all(|predecessor| {
                    let predecessor_value =
                        edge_translations.to_predecessor(*predecessor, block, *value);
                    result.s_exit[*predecessor].contains(&predecessor_value)
                })
            }));
        }
        result.s_entry[block] = spilled.clone();
        let transition = plan_block_transition(
            func,
            next_use,
            planning_recipes,
            &result.logical,
            &result.homes,
            block,
            registers,
            &result.w_entry[block],
            spilled,
        )?;
        result.point_ops.extend(transition.point_ops);
        result.recipe_reloads.extend(transition.recipe_reloads);
        result.w_exit[block] = transition.w_exit;
        result.s_exit[block] = transition.s_exit;
    }

    // Section 4.3.  Delaying this until every W/S exit is known is equivalent
    // to the paper's deferred handling of not-yet-processed backedges.
    for successor in 0..func.blocks.len() {
        for &predecessor in &cfg.predecessors[successor] {
            let mut operations = Vec::new();
            let predecessor_w = result.w_exit[predecessor].clone();
            let predecessor_s = result.s_exit[predecessor].clone();
            for &successor_value in &result.w_entry[successor] {
                let value =
                    edge_translations.to_predecessor(predecessor, successor, successor_value);
                if !predecessor_w.contains(&value) {
                    operations.push(PlannedOp::Reload {
                        value,
                        home: result.homes.of_logical(successor_value),
                    });
                }
            }
            for &successor_value in &result.s_entry[successor] {
                let value =
                    edge_translations.to_predecessor(predecessor, successor, successor_value);
                if !predecessor_s.contains(&value) && predecessor_w.contains(&value) {
                    operations.push(PlannedOp::Spill {
                        value,
                        home: result.homes.of_logical(successor_value),
                    });
                }
            }
            if !operations.is_empty() {
                result.edge_ops.insert((predecessor, successor), operations);
            }
        }
    }
    Ok(result)
}

#[allow(clippy::too_many_arguments)]
fn plan_block_transition(
    func: &MFunction,
    next_use: &NextUseAnalysis,
    planning_recipes: &PlanningRecipes,
    logical: &LogicalValues,
    homes: &PhiCongruenceClasses,
    block: usize,
    registers: usize,
    w_entry: &BTreeSet<LogicalValue>,
    mut spilled: BTreeSet<LogicalValue>,
) -> Result<BlockTransition, SpillPlanError> {
    let mut transition = BlockTransition {
        point_ops: Vec::new(),
        recipe_reloads: BTreeSet::new(),
        w_exit: BTreeSet::new(),
        s_exit: BTreeSet::new(),
    };
    let mut resident = w_entry.clone();
    let mut deferred_recipe_reloads = BTreeMap::<LogicalValue, PointUse>::new();

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

    for (instruction, inst) in func.blocks[block].insts.iter().enumerate() {
        let uses = inst
            .uses()
            .into_iter()
            .map(|value| logical.checked_of(value, Some(func.blocks[block].id), Some(instruction)))
            .collect::<Result<BTreeSet<_>, _>>()?;
        for &value in &uses {
            if resident.insert(value) {
                if let Some(expected) = deferred_recipe_reloads.remove(&value) {
                    let actual = PointUse {
                        block: func.blocks[block].id,
                        instruction,
                        value: VReg(value.0),
                    };
                    if expected != actual {
                        return Err(SpillPlanError::new(
                            "SPILL_PLAN.RECIPE_RELOAD_POINT",
                            Some(func.blocks[block].id),
                            Some(instruction),
                            vec![VReg(value.0)],
                            format!(
                                "deferred recipe reload expected {expected:?} but reached {actual:?}"
                            ),
                        ));
                    }
                    transition
                        .recipe_reloads
                        .insert((func.blocks[block].id, instruction, value));
                }
                transition.point_ops.push((
                    ProgramPoint {
                        block: func.blocks[block].id,
                        instruction,
                        side: PointSide::Before,
                    },
                    PlannedOp::Reload {
                        value,
                        home: homes.of_logical(value),
                    },
                ));
            }
        }
        limit(
            func,
            next_use,
            planning_recipes,
            logical,
            homes,
            &mut transition.point_ops,
            block,
            instruction,
            instruction,
            registers,
            &uses,
            &mut resident,
            &mut spilled,
            &mut deferred_recipe_reloads,
        )?;
        let clobbered = clobbers(inst).len();
        if clobbered > registers {
            return Err(SpillPlanError::new(
                "SPILL_PLAN.CLOBBER_CAPACITY",
                Some(func.blocks[block].id),
                Some(instruction),
                inst.uses().to_vec(),
                format!(
                    "instruction clobbers {clobbered} registers but the allocator has only {registers}"
                ),
            ));
        }
        if clobbered != 0 {
            limit_live_through_clobber(
                func,
                next_use,
                planning_recipes,
                logical,
                homes,
                &mut transition.point_ops,
                block,
                instruction,
                registers.saturating_sub(clobbered),
                &mut resident,
                &mut spilled,
                &mut deferred_recipe_reloads,
            )?;
        }
        if let Some(definition) = inst.def() {
            let definition =
                logical.checked_of(definition, Some(func.blocks[block].id), Some(instruction))?;
            if !resident.contains(&definition) && resident.len() == registers {
                let Some(maximum) = registers.checked_sub(1) else {
                    return Err(SpillPlanError::new(
                        "SPILL_PLAN.OPERAND_PRESSURE",
                        Some(func.blocks[block].id),
                        Some(instruction),
                        vec![VReg(definition.0)],
                        "an instruction result requires a register but no registers are available",
                    ));
                };
                limit(
                    func,
                    next_use,
                    planning_recipes,
                    logical,
                    homes,
                    &mut transition.point_ops,
                    block,
                    instruction,
                    instruction + 1,
                    maximum,
                    &uses,
                    &mut resident,
                    &mut spilled,
                    &mut deferred_recipe_reloads,
                )?;
            }
            resident.insert(definition);
        }
        resident.retain(|value| {
            !logical_distance_at(func, next_use, logical, block, instruction + 1, *value).is_dead()
        });
    }
    if !deferred_recipe_reloads.is_empty() {
        return Err(SpillPlanError::new(
            "SPILL_PLAN.RECIPE_RELOAD_POINT",
            Some(func.blocks[block].id),
            None,
            deferred_recipe_reloads
                .keys()
                .map(|value| VReg(value.0))
                .collect(),
            "deferred recipe reload did not reach its final local use",
        ));
    }
    transition.w_exit = resident;
    transition.s_exit = spilled;
    Ok(transition)
}

#[allow(clippy::too_many_arguments)]
fn limit_live_through_clobber(
    func: &MFunction,
    next_use: &NextUseAnalysis,
    planning_recipes: &PlanningRecipes,
    logical: &LogicalValues,
    homes: &PhiCongruenceClasses,
    point_ops: &mut Vec<(ProgramPoint, PlannedOp)>,
    block: usize,
    instruction: usize,
    capacity: usize,
    resident: &mut BTreeSet<LogicalValue>,
    spilled: &mut BTreeSet<LogicalValue>,
    deferred_recipe_reloads: &mut BTreeMap<LogicalValue, PointUse>,
) -> Result<(), SpillPlanError> {
    let mut live_through = resident
        .iter()
        .copied()
        .filter(|value| {
            !logical_distance_at(func, next_use, logical, block, instruction + 1, *value).is_dead()
        })
        .collect::<BTreeSet<_>>();
    while live_through.len() > capacity {
        let Some(victim) = live_through.iter().copied().max_by(|left, right| {
            compare_eviction_candidates(
                func,
                next_use,
                planning_recipes,
                spilled,
                block,
                instruction + 1,
                (
                    *left,
                    logical_distance_at(func, next_use, logical, block, instruction + 1, *left),
                ),
                (
                    *right,
                    logical_distance_at(func, next_use, logical, block, instruction + 1, *right),
                ),
            )
        }) else {
            return Err(SpillPlanError::new(
                "SPILL_PLAN.MIN_VICTIM",
                Some(func.blocks[block].id),
                Some(instruction),
                Vec::new(),
                "clobber pressure exceeded capacity but MIN had no live-through victim",
            ));
        };
        evict_value(
            func,
            next_use,
            planning_recipes,
            homes,
            point_ops,
            block,
            instruction,
            instruction + 1,
            victim,
            spilled,
            deferred_recipe_reloads,
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
            .map(|value| edge_translations.to_successor(predecessor, block, value))
            .collect();
    }
    let mut frequency = HashMap::<LogicalValue, usize>::new();
    for predecessor in &processed {
        for &value in &plan.w_exit[*predecessor] {
            let value = edge_translations.to_successor(*predecessor, block, value);
            *frequency.entry(value).or_default() += 1;
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

#[allow(clippy::too_many_arguments)]
fn limit(
    func: &MFunction,
    next_use: &NextUseAnalysis,
    planning_recipes: &PlanningRecipes,
    logical: &LogicalValues,
    homes: &PhiCongruenceClasses,
    point_ops: &mut Vec<(ProgramPoint, PlannedOp)>,
    block: usize,
    point_instruction: usize,
    distance_instruction: usize,
    maximum: usize,
    pinned: &BTreeSet<LogicalValue>,
    resident: &mut BTreeSet<LogicalValue>,
    spilled: &mut BTreeSet<LogicalValue>,
    deferred_recipe_reloads: &mut BTreeMap<LogicalValue, PointUse>,
) -> Result<(), SpillPlanError> {
    while resident.len() > maximum {
        let Some(victim) = resident
            .iter()
            .copied()
            .filter(|value| !pinned.contains(value))
            .max_by(|left, right| {
                compare_eviction_candidates(
                    func,
                    next_use,
                    planning_recipes,
                    spilled,
                    block,
                    distance_instruction,
                    (
                        *left,
                        logical_distance_at(
                            func,
                            next_use,
                            logical,
                            block,
                            distance_instruction,
                            *left,
                        ),
                    ),
                    (
                        *right,
                        logical_distance_at(
                            func,
                            next_use,
                            logical,
                            block,
                            distance_instruction,
                            *right,
                        ),
                    ),
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
            next_use,
            planning_recipes,
            homes,
            point_ops,
            block,
            point_instruction,
            distance_instruction,
            victim,
            spilled,
            deferred_recipe_reloads,
        )?;
        resident.remove(&victim);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn evict_value(
    func: &MFunction,
    next_use: &NextUseAnalysis,
    planning_recipes: &PlanningRecipes,
    homes: &PhiCongruenceClasses,
    point_ops: &mut Vec<(ProgramPoint, PlannedOp)>,
    block: usize,
    point_instruction: usize,
    distance_instruction: usize,
    value: LogicalValue,
    spilled: &mut BTreeSet<LogicalValue>,
    deferred_recipe_reloads: &mut BTreeMap<LogicalValue, PointUse>,
) -> Result<(), SpillPlanError> {
    if spilled.contains(&value) {
        return Ok(());
    }
    if let Some((point, recipe_cost)) = next_use_point_recipe(
        func,
        next_use,
        planning_recipes,
        block,
        distance_instruction,
        value,
    ) {
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
    func: &MFunction,
    next_use: &NextUseAnalysis,
    planning_recipes: &PlanningRecipes,
    block: usize,
    instruction: usize,
    value: LogicalValue,
) -> Option<(PointUse, u16)> {
    let value = VReg(value.0);
    let instruction = next_use.next_local_use(block, instruction, value)?;
    let point = PointUse {
        block: func.blocks[block].id,
        instruction,
        value,
    };
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
    next_use: &NextUseAnalysis,
    planning_recipes: &PlanningRecipes,
    spilled: &BTreeSet<LogicalValue>,
    block: usize,
    instruction: usize,
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
            let left_cost = eviction_cost(
                func,
                next_use,
                planning_recipes,
                spilled,
                block,
                instruction,
                left.0,
            ) as u128;
            let right_cost = eviction_cost(
                func,
                next_use,
                planning_recipes,
                spilled,
                block,
                instruction,
                right.0,
            ) as u128;
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
    next_use: &NextUseAnalysis,
    planning_recipes: &PlanningRecipes,
    spilled: &BTreeSet<LogicalValue>,
    block: usize,
    instruction: usize,
    value: LogicalValue,
) -> u16 {
    let has_persistent_home = spilled.contains(&value);
    let persistent_cost =
        reload_cost_at_next_local_use(func, next_use, planning_recipes, block, instruction, value)
            .saturating_add(if has_persistent_home {
                0
            } else {
                spill_cost(func, value)
            });
    if has_persistent_home {
        return persistent_cost;
    }
    next_use_point_recipe(func, next_use, planning_recipes, block, instruction, value)
        .map_or(persistent_cost, |(_, recipe_cost)| {
            persistent_cost.min(recipe_cost)
        })
}

fn reload_cost_at_next_local_use(
    func: &MFunction,
    next_use: &NextUseAnalysis,
    planning_recipes: &PlanningRecipes,
    block: usize,
    instruction: usize,
    value: LogicalValue,
) -> u16 {
    let value = VReg(value.0);
    if let Some(instruction) = next_use.next_local_use(block, instruction, value)
        && let Some(cost) = planning_recipes.materialization_cost_at_point(PointUse {
            block: func.blocks[block].id,
            instruction,
            value,
        })
    {
        return cost;
    }
    reload_cost(func, planning_recipes, LogicalValue(value.0))
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
    uses.iter().fold(0u128, |cost, &instruction| {
        let reload = planning_recipes
            .materialization_cost_at_point(PointUse {
                block: func.blocks[block].id,
                instruction,
                value,
            })
            .unwrap_or_else(|| reload_cost(func, planning_recipes, LogicalValue(value.0)));
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
    if let Some(cost) = planning_recipes.materialization_cost_on_edge(EdgeUse {
        predecessor: func.blocks[predecessor].id,
        successor: func.blocks[successor].id,
        value,
    }) {
        return cost;
    }
    reload_cost(func, planning_recipes, LogicalValue(value.0))
}

fn reload_cost(func: &MFunction, planning_recipes: &PlanningRecipes, value: LogicalValue) -> u16 {
    if let Some(cost) = planning_recipes.global_materialization_cost(VReg(value.0)) {
        return cost;
    }
    stack_reload_cost(func, value)
}

fn stack_reload_cost(func: &MFunction, value: LogicalValue) -> u16 {
    let Some(desc) = func.spill_desc(VReg(value.0)) else {
        // A missing descriptor is rejected by MIR verification before this
        // phase.  Keep this helper total so malformed input remains a
        // structured verifier error rather than a planner panic.
        return u16::MAX;
    };
    u16::from(desc.reload_cost)
}

fn spill_cost(func: &MFunction, value: LogicalValue) -> u16 {
    func.spill_desc(VReg(value.0))
        .map_or(u16::MAX, |desc| u16::from(desc.spill_cost))
}

fn logical_entry_distance(
    func: &MFunction,
    next_use: &NextUseAnalysis,
    block: usize,
    value: LogicalValue,
) -> NextUseDistance {
    next_use.distance_at(func, block, 0, VReg(value.0))
}

fn logical_distance_at(
    func: &MFunction,
    next_use: &NextUseAnalysis,
    logical: &LogicalValues,
    block: usize,
    instruction: usize,
    value: LogicalValue,
) -> NextUseDistance {
    let _ = logical;
    next_use.distance_at(func, block, instruction, VReg(value.0))
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
                                u128::from(stack_reload_cost(func, value))
                            },
                        );
                        let total = recipe_costs.entry(home).or_default();
                        *total = total.saturating_add(cost);
                    } else {
                        let baseline = baseline_costs.entry(home).or_default();
                        *baseline =
                            baseline.saturating_add(u128::from(stack_reload_cost(func, value)));
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
            let Some(instruction) = predecessor_block.insts.len().checked_sub(1) else {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.RECIPE_HOME_EDGE",
                    Some(predecessor_block.id),
                    None,
                    Vec::new(),
                    "recipe-home edge predecessor is empty",
                ));
            };
            for &operation in operations {
                match operation {
                    PlannedOp::Reload { value, home } => {
                        candidates.insert(home);
                        let total = baseline_costs.entry(home).or_default();
                        *total = total.saturating_add(u128::from(stack_reload_cost(func, value)));
                        let query = PointUse {
                            block: predecessor_block.id,
                            instruction,
                            value: VReg(value.0),
                        };
                        if let Some(recipe) = analysis.resolved_recipe_at_point(query) {
                            let cost = u128::try_from(recipe.steps.len().saturating_add(1))
                                .unwrap_or(u128::MAX);
                            let total = recipe_costs.entry(home).or_default();
                            *total = total.saturating_add(cost);
                        } else {
                            rejected.insert(home);
                        }
                    }
                    PlannedOp::Spill { value, home } => {
                        let total = baseline_costs.entry(home).or_default();
                        *total = total.saturating_add(u128::from(spill_cost(func, value)));
                    }
                    PlannedOp::SpillPhi { .. } => {
                        return Err(SpillPlanError::new(
                            "SPILL_PLAN.RECIPE_HOME_EDGE",
                            Some(predecessor_block.id),
                            None,
                            Vec::new(),
                            "SpillPhi cannot be an edge operation",
                        ));
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
                let Some(instruction) = predecessor_block.insts.len().checked_sub(1) else {
                    return Err(SpillPlanError::new(
                        "SPILL_PLAN.RECIPE_HOME_EDGE",
                        Some(predecessor_block.id),
                        None,
                        Vec::new(),
                        "recipe-home edge predecessor is empty",
                    ));
                };
                for &operation in operations {
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
                        block: predecessor_block.id,
                        instruction,
                        value: VReg(value.0),
                    };
                    if analysis.resolved_recipe_at_point(query).is_none() {
                        return Err(SpillPlanError::new(
                            "SPILL_PLAN.RECIPE_HOME_EDGE",
                            Some(predecessor_block.id),
                            None,
                            vec![VReg(value.0)],
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
        if self.logical.count != func.vregs.count()
            || self.homes.home_for_vreg.len() != self.logical.count as usize
        {
            return Err(SpillPlanError::new(
                "SPILL_PLAN.STATE_SHAPE",
                None,
                None,
                Vec::new(),
                format!(
                    "spill-plan value tables cover {} logical values and {} homes, but the function has {} virtual registers",
                    self.logical.count,
                    self.homes.home_for_vreg.len(),
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

        for (value, &home) in self.homes.home_for_vreg.iter().enumerate() {
            if home.0 >= self.logical.count {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.VALUE_RANGE",
                    None,
                    None,
                    vec![VReg(value as u32)],
                    format!(
                        "logical value {value} has out-of-range spill home {}",
                        home.0
                    ),
                ));
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
            if cfg.successors[predecessor].len() != 1 {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.EDGE_ISOLATED",
                    Some(predecessor_block.id),
                    None,
                    Vec::new(),
                    "edge operation predecessor must be a dedicated insertion block",
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
                self.verify_operation(operation, Some(predecessor_block.id), None)?;
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
                .map(|(_, operation)| operation)
                .chain(self.edge_ops.values().flatten())
                .any(|operation| {
                    matches!(operation, PlannedOp::Reload { home: reload_home, .. } if *reload_home == home)
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::native::mir::{
        BaseReg, MBlock, MInst, OpSize, PhiNode, SpillDesc, VRegAllocator,
    };

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
        let homes = PhiCongruenceClasses::build(&func).unwrap();
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
        let homes = PhiCongruenceClasses::build(&func).unwrap();
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
        let homes = PhiCongruenceClasses::build(&func).unwrap();
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
    fn reused_cssa_edge_source_is_a_structured_error() {
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

        let error = EdgeTranslations::build(&func, &cfg, &logical).unwrap_err();

        assert_eq!(error.rule, "SPILL_PLAN.PHI_SOURCE_UNIQUE");
        assert_eq!(error.block, Some(BlockId(0)));
        assert_eq!(error.values, vec![source, second]);
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
        let local = |instructions| NextUseDistance::Finite {
            loop_exits: 0,
            instructions,
        };

        assert_eq!(
            compare_eviction_candidates(
                &func,
                &next_use,
                &planning_recipes,
                &spilled,
                0,
                0,
                (LogicalValue(cheap.0), local(1)),
                (LogicalValue(costly.0), local(1)),
            ),
            Ordering::Greater,
            "equal spans must evict the cheaper rematerializable value"
        );
        assert_eq!(
            compare_eviction_candidates(
                &func,
                &next_use,
                &planning_recipes,
                &spilled,
                0,
                0,
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
        assert_eq!(
            compare_eviction_candidates(
                &equal_cost,
                &equal_next_use,
                &equal_recipes,
                &spilled,
                0,
                0,
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
                &equal_next_use,
                &exact_recipes,
                &spilled,
                0,
                0,
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
        let requested = super::super::ssa::planner_reload_queries(&func, &plan).unwrap();
        let recipes = super::super::reload::analyze_with_queries(&func, &cfg, &requested).unwrap();

        plan.select_recipe_homes(&func, &cfg, &recipes).unwrap();
        assert!(plan.recipe_homes.is_empty());

        plan.point_ops.pop();
        plan.select_recipe_homes(&func, &cfg, &recipes).unwrap();
        assert_eq!(plan.recipe_homes, BTreeSet::from([home]));
        plan.verify_recipe_homes(&func, &recipes).unwrap();

        plan.point_ops.push(reload(5));
        let error = plan.verify_recipe_homes(&func, &recipes).unwrap_err();
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
        let requested = super::super::ssa::planner_reload_queries(&func, &plan).unwrap();
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
        let requested = super::super::ssa::planner_reload_queries(&func, &plan).unwrap();
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
        plan.verify_recipe_homes(&func, &recipes).unwrap();
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
    fn descending_large_phi_web_builds_one_stable_home_without_recursion() {
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

        let homes = PhiCongruenceClasses::build(&func).unwrap();

        assert_eq!(homes.of_vreg(VReg(0)), SpillHome(0));
        assert_eq!(homes.of_vreg(VReg(MEMBERS - 1)), SpillHome(0));
        assert_eq!(homes.members(SpillHome(0)).count(), MEMBERS as usize);
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
            assert_eq!(
                translations.to_successor(predecessor, join, LogicalValue(source.0),),
                LogicalValue(destination.0)
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
