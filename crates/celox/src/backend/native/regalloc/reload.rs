//! Reload recipes whose validity is proved against physical MIR memory effects.
//!
//! A simulation-state recipe is deliberately expressed as the exact MIR load
//! that produced the value.  It never reconstructs an address or an operand
//! width from SIR metadata.  The accompanying memory version consists of one
//! unknown-alias epoch and one sparse MemorySSA version for every byte read by
//! that load.  A recipe is usable only where all of those versions still
//! match.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::fmt;

use crate::backend::native::memory_effect;
use crate::backend::native::mir::{BaseReg, BlockId, MFunction, MInst, OpSize, VReg};

use super::cfg::NormalizedCfg;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct StateLoad {
    pub offset: i32,
    pub size: OpSize,
}

impl StateLoad {
    fn bytes(self) -> Option<std::ops::Range<i64>> {
        let start = i64::from(self.offset);
        let end = start.checked_add(i64::from(self.size.bytes()))?;
        Some(start..end)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum MemoryVariable {
    UnknownAlias,
    Byte(i64),
}

/// Structural MemorySSA identity, independent of unrelated tracked ranges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum MemoryVersion {
    Entry(MemoryVariable),
    Write {
        block: BlockId,
        ordinal: usize,
        variable: MemoryVariable,
    },
    Phi {
        block: BlockId,
        variable: MemoryVariable,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct StateVersion {
    unknown_alias: MemoryVersion,
    bytes: Box<[MemoryVersion]>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct StateRecipe {
    pub load: StateLoad,
    version: StateVersion,
    observed_bits: StateBitRange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct StateBitRange {
    start: i64,
    end: i64,
}

impl StateBitRange {
    fn from_load(load: StateLoad) -> Option<Self> {
        let bytes = load.bytes()?;
        Some(Self {
            start: bytes.start.checked_mul(8)?,
            end: bytes.end.checked_mul(8)?,
        })
    }

    fn inserted(load: StateLoad, bit_offset: usize, width_bits: usize) -> Option<Self> {
        if width_bits == 0 {
            return None;
        }
        let base = i64::from(load.offset).checked_mul(8)?;
        let bit_offset = i64::try_from(bit_offset).ok()?;
        let width_bits = i64::try_from(width_bits).ok()?;
        let start = base.checked_add(bit_offset)?;
        let end = start.checked_add(width_bits)?;
        let range = Self { start, end };
        let physical = Self::from_load(load)?;
        (range.start >= physical.start && range.end <= physical.end).then_some(range)
    }

    fn overlaps(self, other: Self) -> bool {
        self.start < other.end && other.start < self.end
    }
}

/// A value's preferred reload mechanism before a concrete spill plan chooses
/// materialization sites.
//
// `Pure` is represented explicitly now so pure-expression rematerialization
// does not get folded back into storage metadata when it is enabled in the
// allocator-owned splitting slice.  Step 3b initially constructs only the
// constant and state forms; the stack form remains the correctness fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ReloadRecipe {
    Constant { value: u64 },
    StateVersion(StateRecipe),
    Pure { expression: PureRecipeId },
    Stack,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlanningRecipe {
    Constant,
    State,
    Pure { expression: PureRecipeId },
    Stack,
}

/// Reload costs used by spill planning.
///
/// Globally valid constants and state loads have one value-wide cost.  Values
/// committed after their SSA definition instead use sparse, MemorySSA-proved
/// costs at actual MIR uses and phi edges.  Reconstruction independently
/// rebuilds and verifies every recipe which the planner ultimately selects;
/// these cost facts can influence placement but cannot authorize emission.
#[derive(Debug)]
pub(super) struct PlanningRecipes {
    global_costs: Vec<Option<u16>>,
    point_costs: BTreeMap<PointUse, u16>,
    edge_costs: BTreeMap<EdgeUse, u16>,
}

impl PlanningRecipes {
    #[cfg(test)]
    pub fn global_materialization_costs(&self) -> Result<Vec<Option<u16>>, ReloadRecipeError> {
        Ok(self.global_costs.clone())
    }

    pub(super) fn global_materialization_cost(&self, value: VReg) -> Option<u16> {
        self.global_costs.get(value.0 as usize).copied().flatten()
    }

    pub(super) fn materialization_cost_at_point(&self, point: PointUse) -> Option<u16> {
        minimum_cost(
            self.global_materialization_cost(point.value),
            self.point_costs.get(&point).copied(),
        )
    }

    /// Cost of a path-specific MemorySSA recipe, excluding globally available
    /// rematerialization. Spill placement uses this distinction only when it
    /// can omit creation of a persistent stack home altogether.
    pub(super) fn point_specific_materialization_cost(&self, point: PointUse) -> Option<u16> {
        self.point_costs.get(&point).copied()
    }

    pub(super) fn materialization_cost_on_edge(&self, edge: EdgeUse) -> Option<u16> {
        minimum_cost(
            self.global_materialization_cost(edge.value),
            self.edge_costs.get(&edge).copied(),
        )
    }

    #[cfg(test)]
    pub(super) fn stack_only(value_count: u32) -> Self {
        Self::with_global_costs(vec![None; value_count as usize])
    }

    #[cfg(test)]
    pub(super) fn with_global_costs(global_costs: Vec<Option<u16>>) -> Self {
        Self {
            global_costs,
            point_costs: BTreeMap::new(),
            edge_costs: BTreeMap::new(),
        }
    }
}

fn minimum_cost(left: Option<u16>, right: Option<u16>) -> Option<u16> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(cost), None) | (None, Some(cost)) => Some(cost),
        (None, None) => None,
    }
}

fn global_materialization_costs(
    recipes: &[PlanningRecipe],
    pure_recipes: &[PureRecipe],
) -> Result<Vec<Option<u16>>, ReloadRecipeError> {
    let mut costs = vec![None::<Option<u16>>; recipes.len()];
    for start in 0..recipes.len() {
        if costs[start].is_some() {
            continue;
        }
        let mut path = Vec::<usize>::new();
        let mut seen = BTreeSet::<usize>::new();
        let mut current = start;
        let mut cost = loop {
            if let Some(cost) = costs[current] {
                break cost;
            }
            if !seen.insert(current) {
                return Err(ReloadRecipeError::new(
                    "RELOAD_RECIPE.PURE_CYCLE",
                    None,
                    None,
                    Some(VReg(current as u32)),
                    "planning recipe graph contains a cycle",
                ));
            }
            match recipes[current] {
                PlanningRecipe::Constant | PlanningRecipe::State => {
                    costs[current] = Some(Some(1));
                    break Some(1);
                }
                PlanningRecipe::Stack => {
                    costs[current] = Some(None);
                    break None;
                }
                PlanningRecipe::Pure { expression } => {
                    let Some(recipe) = pure_recipes.get(expression.0 as usize) else {
                        return Err(ReloadRecipeError::new(
                            "RELOAD_RECIPE.PURE_EXPRESSION",
                            None,
                            None,
                            Some(VReg(current as u32)),
                            "planning pure-expression identifier is outside its table",
                        ));
                    };
                    path.push(current);
                    current = recipe.source().0 as usize;
                    if current >= recipes.len() {
                        return Err(ReloadRecipeError::new(
                            "RELOAD_RECIPE.VALUE_COVERAGE",
                            None,
                            None,
                            Some(VReg(current as u32)),
                            "planning pure recipe source is outside the VReg table",
                        ));
                    }
                }
            }
        };
        for value in path.into_iter().rev() {
            cost = cost.map(|value| value.saturating_add(1));
            costs[value] = Some(cost);
        }
    }
    Ok(costs
        .into_iter()
        .map(|cost| cost.expect("every planning recipe cost is visited"))
        .collect())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct PureRecipeId(pub u32);

/// One target operation which may be recomputed if its operand recipes are
/// available at the chosen split point.  Width-changing x86 operations are
/// separate variants; no arbitrary HDL bit width is attached to a VReg.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PureRecipe {
    Copy64 { source: VReg },
    Copy32 { source: VReg },
    AndImm64 { source: VReg, immediate: u64 },
    AndImm32 { source: VReg, immediate: u32 },
    OrImm64 { source: VReg, immediate: u64 },
    ShrImm64 { source: VReg, immediate: u8 },
    ShlImm64 { source: VReg, immediate: u8 },
    SarImm64 { source: VReg, immediate: u8 },
    AddImm64 { source: VReg, immediate: i32 },
    SubImm64 { source: VReg, immediate: i32 },
    BitNot64 { source: VReg },
    Neg64 { source: VReg },
}

impl PureRecipe {
    fn source(self) -> VReg {
        match self {
            Self::Copy64 { source }
            | Self::Copy32 { source }
            | Self::AndImm64 { source, .. }
            | Self::AndImm32 { source, .. }
            | Self::OrImm64 { source, .. }
            | Self::ShrImm64 { source, .. }
            | Self::ShlImm64 { source, .. }
            | Self::SarImm64 { source, .. }
            | Self::AddImm64 { source, .. }
            | Self::SubImm64 { source, .. }
            | Self::BitNot64 { source }
            | Self::Neg64 { source } => source,
        }
    }

    fn step(self) -> PureStep {
        match self {
            Self::Copy64 { .. } => PureStep::Copy64,
            Self::Copy32 { .. } => PureStep::Copy32,
            Self::AndImm64 { immediate, .. } => PureStep::AndImm64 { immediate },
            Self::AndImm32 { immediate, .. } => PureStep::AndImm32 { immediate },
            Self::OrImm64 { immediate, .. } => PureStep::OrImm64 { immediate },
            Self::ShrImm64 { immediate, .. } => PureStep::ShrImm64 { immediate },
            Self::ShlImm64 { immediate, .. } => PureStep::ShlImm64 { immediate },
            Self::SarImm64 { immediate, .. } => PureStep::SarImm64 { immediate },
            Self::AddImm64 { immediate, .. } => PureStep::AddImm64 { immediate },
            Self::SubImm64 { immediate, .. } => PureStep::SubImm64 { immediate },
            Self::BitNot64 { .. } => PureStep::BitNot64,
            Self::Neg64 { .. } => PureStep::Neg64,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum PureStep {
    Copy64,
    Copy32,
    AndImm64 { immediate: u64 },
    AndImm32 { immediate: u32 },
    OrImm64 { immediate: u64 },
    ShrImm64 { immediate: u8 },
    ShlImm64 { immediate: u8 },
    SarImm64 { immediate: u8 },
    AddImm64 { immediate: i32 },
    SubImm64 { immediate: i32 },
    BitNot64,
    Neg64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum ResolvedBase {
    Constant(u64),
    State(StateRecipe),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct ResolvedRecipe {
    pub base: ResolvedBase,
    pub steps: Vec<PureStep>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StoreHomeSpec {
    value: VReg,
    load: StateLoad,
    steps: Vec<PureStep>,
    observed_bits: StateBitRange,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StoreHome {
    state: StateRecipe,
    steps: Vec<PureStep>,
}

type StoreHomeIndex = BTreeMap<i64, BTreeMap<VReg, usize>>;

fn index_store_home(index: &mut StoreHomeIndex, value: VReg, home: &StoreHome) {
    let bytes = home
        .state
        .load
        .bytes()
        .expect("validated store-home load has a finite byte range");
    for byte in bytes {
        *index.entry(byte).or_default().entry(value).or_default() += 1;
    }
}

fn unindex_store_home(index: &mut StoreHomeIndex, value: VReg, home: &StoreHome) {
    let bytes = home
        .state
        .load
        .bytes()
        .expect("validated store-home load has a finite byte range");
    for byte in bytes {
        let remove_byte = {
            let values = index
                .get_mut(&byte)
                .expect("popped store home is present in its byte index");
            let count = values
                .get_mut(&value)
                .expect("popped store home value is present in its byte index");
            *count -= 1;
            if *count == 0 {
                values.remove(&value);
            }
            values.is_empty()
        };
        if remove_byte {
            index.remove(&byte);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct PointUse {
    pub block: BlockId,
    pub instruction: usize,
    pub value: VReg,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct EdgeUse {
    pub predecessor: BlockId,
    pub successor: BlockId,
    pub value: VReg,
}

/// Sparse recipe facts retained only at actual uses.  Unlike global next-use
/// maps, this does not materialize a `(block, value)` entry merely because a
/// value is live through that block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ReloadRecipeAnalysis {
    recipes: Vec<ReloadRecipe>,
    pure_recipes: Vec<PureRecipe>,
    requested_points: BTreeSet<PointUse>,
    point_recipes: BTreeMap<PointUse, ResolvedRecipe>,
    edge_recipes: BTreeMap<EdgeUse, ResolvedRecipe>,
    valid_point_uses: BTreeSet<PointUse>,
    valid_edge_uses: BTreeSet<EdgeUse>,
    collect_all_uses: bool,
}

impl ReloadRecipeAnalysis {
    pub fn recipe(&self, value: VReg) -> Option<&ReloadRecipe> {
        self.recipes.get(value.0 as usize)
    }

    pub fn state_recipe(&self, value: VReg) -> Option<&StateRecipe> {
        match self.recipe(value)? {
            ReloadRecipe::StateVersion(recipe) => Some(recipe),
            _ => None,
        }
    }

    pub fn pure_recipe(&self, value: VReg) -> Option<PureRecipe> {
        let ReloadRecipe::Pure { expression } = self.recipe(value)? else {
            return None;
        };
        self.pure_recipes.get(expression.0 as usize).copied()
    }

    pub fn state_valid_at_point(&self, point: PointUse) -> bool {
        self.valid_point_uses.contains(&point)
    }

    pub fn state_valid_on_edge(&self, edge: EdgeUse) -> bool {
        self.valid_edge_uses.contains(&edge)
    }

    pub fn resolved_recipe_at_point(&self, point: PointUse) -> Option<&ResolvedRecipe> {
        self.point_recipes.get(&point)
    }

    #[cfg(test)]
    pub fn point_recipe_uses_store_home(&self, point: PointUse) -> bool {
        let Some(selected) = self.point_recipes.get(&point) else {
            return false;
        };
        self.resolved_recipe(point.value).ok().flatten().as_ref() != Some(selected)
    }

    pub fn resolved_recipe(
        &self,
        value: VReg,
    ) -> Result<Option<ResolvedRecipe>, ReloadRecipeError> {
        let mut current = value;
        let mut reverse_steps = Vec::<PureStep>::new();
        let mut seen = BTreeSet::<VReg>::new();
        loop {
            if !seen.insert(current) {
                return Err(ReloadRecipeError::new(
                    "RELOAD_RECIPE.PURE_CYCLE",
                    None,
                    None,
                    Some(value),
                    format!("pure recipe dependency cycles through {current}"),
                ));
            }
            match self.recipe(current) {
                Some(ReloadRecipe::Constant { value }) => {
                    reverse_steps.reverse();
                    return Ok(Some(ResolvedRecipe {
                        base: ResolvedBase::Constant(*value),
                        steps: reverse_steps,
                    }));
                }
                Some(ReloadRecipe::StateVersion(recipe)) => {
                    reverse_steps.reverse();
                    return Ok(Some(ResolvedRecipe {
                        base: ResolvedBase::State(recipe.clone()),
                        steps: reverse_steps,
                    }));
                }
                Some(ReloadRecipe::Pure { .. }) => {
                    let Some(recipe) = self.pure_recipe(current) else {
                        return Err(ReloadRecipeError::new(
                            "RELOAD_RECIPE.PURE_EXPRESSION",
                            None,
                            None,
                            Some(current),
                            "pure recipe identifier is outside the expression table",
                        ));
                    };
                    reverse_steps.push(recipe.step());
                    current = recipe.source();
                }
                Some(ReloadRecipe::Stack) => return Ok(None),
                None => {
                    return Err(ReloadRecipeError::new(
                        "RELOAD_RECIPE.VALUE_COVERAGE",
                        None,
                        None,
                        Some(current),
                        "pure recipe dependency is outside the VReg recipe table",
                    ));
                }
            }
        }
    }

    /// Rebuild every fact from MIR and compare it with the producer's result.
    /// Materialized-reload verification calls the same builder afresh and does
    /// not trust a transform's cached validity decision.
    pub fn verify(&self, func: &MFunction, cfg: &NormalizedCfg) -> Result<(), ReloadRecipeError> {
        let rebuilt = analyze_unverified_with_queries(
            func,
            cfg,
            &self.requested_points,
            self.collect_all_uses,
        )?;
        for index in 0..self.recipes.len() {
            let value = VReg(index as u32);
            match self.recipe(value) {
                Some(ReloadRecipe::StateVersion(_)) if self.state_recipe(value).is_none() => {
                    return Err(ReloadRecipeError::new(
                        "RELOAD_RECIPE.STATE_ACCESSOR",
                        None,
                        None,
                        Some(value),
                        "state recipe cannot be resolved through the recipe table",
                    ));
                }
                Some(ReloadRecipe::Pure { .. }) if self.pure_recipe(value).is_none() => {
                    return Err(ReloadRecipeError::new(
                        "RELOAD_RECIPE.PURE_ACCESSOR",
                        None,
                        None,
                        Some(value),
                        "pure recipe identifier is outside the expression table",
                    ));
                }
                Some(_) => {}
                None => {
                    return Err(ReloadRecipeError::new(
                        "RELOAD_RECIPE.VALUE_COVERAGE",
                        None,
                        None,
                        Some(value),
                        "recipe table does not cover every VReg",
                    ));
                }
            }
        }
        if self.recipes != rebuilt.recipes {
            return Err(ReloadRecipeError::new(
                "RELOAD_RECIPE.RECIPES_MATCH_MIR",
                None,
                None,
                None,
                "cached reload recipes differ from independently rebuilt MIR recipes",
            ));
        }
        if self.pure_recipes != rebuilt.pure_recipes {
            return Err(ReloadRecipeError::new(
                "RELOAD_RECIPE.PURE_EXPRESSIONS_MATCH_MIR",
                None,
                None,
                None,
                "cached pure-expression recipes differ from independently rebuilt MIR recipes",
            ));
        }
        if self.point_recipes != rebuilt.point_recipes {
            return Err(ReloadRecipeError::new(
                "RELOAD_RECIPE.POINT_RECIPES_MATCH_MEMORY_SSA",
                None,
                None,
                None,
                "cached point recipes differ from independently rebuilt MemorySSA",
            ));
        }
        if self.edge_recipes != rebuilt.edge_recipes {
            return Err(ReloadRecipeError::new(
                "RELOAD_RECIPE.EDGE_RECIPES_MATCH_MEMORY_SSA",
                None,
                None,
                None,
                "cached edge recipes differ from independently rebuilt MemorySSA",
            ));
        }
        if self.valid_point_uses != rebuilt.valid_point_uses {
            return Err(ReloadRecipeError::new(
                "RELOAD_RECIPE.POINT_VALIDITY_MATCHES_MEMORY_SSA",
                None,
                None,
                None,
                "cached point validity differs from independently rebuilt MemorySSA",
            ));
        }
        if rebuilt
            .valid_point_uses
            .iter()
            .any(|point| !self.state_valid_at_point(*point))
        {
            return Err(ReloadRecipeError::new(
                "RELOAD_RECIPE.POINT_ACCESSOR",
                None,
                None,
                None,
                "valid point use cannot be resolved through the recipe analysis",
            ));
        }
        if self.valid_edge_uses != rebuilt.valid_edge_uses {
            return Err(ReloadRecipeError::new(
                "RELOAD_RECIPE.EDGE_VALIDITY_MATCHES_MEMORY_SSA",
                None,
                None,
                None,
                "cached edge validity differs from independently rebuilt MemorySSA",
            ));
        }
        if rebuilt
            .valid_edge_uses
            .iter()
            .any(|edge| !self.state_valid_on_edge(*edge))
        {
            return Err(ReloadRecipeError::new(
                "RELOAD_RECIPE.EDGE_ACCESSOR",
                None,
                None,
                None,
                "valid edge use cannot be resolved through the recipe analysis",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ReloadRecipeError {
    pub rule: &'static str,
    pub block: Option<BlockId>,
    pub instruction: Option<usize>,
    pub value: Option<VReg>,
    pub message: String,
}

impl ReloadRecipeError {
    fn new(
        rule: &'static str,
        block: Option<BlockId>,
        instruction: Option<usize>,
        value: Option<VReg>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            rule,
            block,
            instruction,
            value,
            message: message.into(),
        }
    }
}

impl fmt::Display for ReloadRecipeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.rule)?;
        if let Some(block) = self.block {
            write!(f, " at {block}")?;
        }
        if let Some(instruction) = self.instruction {
            write!(f, "/i{instruction}")?;
        }
        if let Some(value) = self.value {
            write!(f, " value={value}")?;
        }
        write!(f, ": {}", self.message)
    }
}

impl std::error::Error for ReloadRecipeError {}

pub(super) fn analyze_for_planning(
    func: &MFunction,
    cfg: &NormalizedCfg,
) -> Result<PlanningRecipes, ReloadRecipeError> {
    let mut recipes = vec![PlanningRecipe::Stack; func.vregs.count() as usize];
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
                    "MIR definition is outside the planning recipe table",
                ));
            };
            *slot = match inst {
                MInst::LoadImm { .. } => PlanningRecipe::Constant,
                MInst::Load {
                    base: BaseReg::SimState,
                    ..
                } => PlanningRecipe::State,
                _ => {
                    let Some(expression) = pure_expression(inst) else {
                        continue;
                    };
                    let id = u32::try_from(pure_recipes.len()).map_err(|_| {
                        ReloadRecipeError::new(
                            "RELOAD_RECIPE.PURE_ID_RANGE",
                            Some(block.id),
                            Some(instruction),
                            Some(definition),
                            "planning pure-expression count exceeds u32",
                        )
                    })?;
                    pure_recipes.push(expression);
                    PlanningRecipe::Pure {
                        expression: PureRecipeId(id),
                    }
                }
            };
        }
    }
    let global_costs = global_materialization_costs(&recipes, &pure_recipes)?;
    let candidates = point_specific_recipe_candidates(func, &recipes, &pure_recipes)?;
    let requested_points = func
        .blocks
        .iter()
        .flat_map(|block| {
            block
                .insts
                .iter()
                .enumerate()
                .flat_map(move |(instruction, inst)| {
                    inst.uses().into_iter().map(move |value| PointUse {
                        block: block.id,
                        instruction,
                        value,
                    })
                })
        })
        .filter(|point| {
            candidates.contains(&point.value)
                && global_costs
                    .get(point.value.0 as usize)
                    .is_some_and(Option::is_none)
        })
        .collect::<BTreeSet<_>>();
    let (point_costs, edge_costs) = if requested_points.is_empty() {
        (BTreeMap::new(), BTreeMap::new())
    } else {
        let exact = analyze_unverified_with_queries(func, cfg, &requested_points, false)?;
        (
            exact
                .point_recipes
                .iter()
                .map(|(point, recipe)| (*point, resolved_recipe_cost(recipe)))
                .collect(),
            exact
                .edge_recipes
                .iter()
                .map(|(edge, recipe)| (*edge, resolved_recipe_cost(recipe)))
                .collect(),
        )
    };
    Ok(PlanningRecipes {
        global_costs,
        point_costs,
        edge_costs,
    })
}

fn resolved_recipe_cost(recipe: &ResolvedRecipe) -> u16 {
    u16::try_from(recipe.steps.len().saturating_add(1)).unwrap_or(u16::MAX)
}

/// Conservatively identify SSA names which may acquire a path-specific
/// SimState home.  This is only a sparse query filter: MemorySSA still proves
/// the exact byte version at every retained use.
fn point_specific_recipe_candidates(
    func: &MFunction,
    recipes: &[PlanningRecipe],
    pure_recipes: &[PureRecipe],
) -> Result<BTreeSet<VReg>, ReloadRecipeError> {
    fn seed_candidate(
        value: VReg,
        value_count: usize,
        candidates: &mut BTreeSet<VReg>,
        queue: &mut VecDeque<VReg>,
    ) -> Result<(), ReloadRecipeError> {
        if value.0 as usize >= value_count {
            return Err(ReloadRecipeError::new(
                "RELOAD_RECIPE.VALUE_COVERAGE",
                None,
                None,
                Some(value),
                "point-specific recipe candidate is outside the VReg table",
            ));
        }
        if candidates.insert(value) {
            queue.push_back(value);
        }
        Ok(())
    }

    #[derive(Clone, Copy)]
    enum Dependent {
        Pure(VReg),
        Phi(usize),
    }

    struct PhiCandidate {
        destination: VReg,
        remaining_sources: usize,
    }

    let value_count = func.vregs.count() as usize;
    let mut dependents = vec![Vec::<Dependent>::new(); value_count];
    for (destination, recipe) in recipes.iter().copied().enumerate() {
        let PlanningRecipe::Pure { expression } = recipe else {
            continue;
        };
        let Some(expression) = pure_recipes.get(expression.0 as usize) else {
            return Err(ReloadRecipeError::new(
                "RELOAD_RECIPE.PURE_EXPRESSION",
                None,
                None,
                Some(VReg(destination as u32)),
                "planning pure-expression identifier is outside its table",
            ));
        };
        let source = expression.source();
        let Some(users) = dependents.get_mut(source.0 as usize) else {
            return Err(ReloadRecipeError::new(
                "RELOAD_RECIPE.PURE_SOURCE_RANGE",
                None,
                None,
                Some(source),
                "planning pure recipe source is outside the VReg table",
            ));
        };
        users.push(Dependent::Pure(VReg(destination as u32)));
    }

    let mut phis = Vec::<PhiCandidate>::new();
    for block in &func.blocks {
        for phi in &block.phis {
            let sources = phi
                .sources
                .iter()
                .map(|(_, source)| *source)
                .collect::<BTreeSet<_>>();
            if sources.is_empty() {
                continue;
            }
            let index = phis.len();
            phis.push(PhiCandidate {
                destination: phi.dst,
                remaining_sources: sources.len(),
            });
            for source in sources {
                let Some(users) = dependents.get_mut(source.0 as usize) else {
                    return Err(ReloadRecipeError::new(
                        "RELOAD_RECIPE.VALUE_COVERAGE",
                        Some(block.id),
                        None,
                        Some(source),
                        "phi source is outside the VReg table",
                    ));
                };
                users.push(Dependent::Phi(index));
            }
        }
    }

    let canonical_bits = canonical_value_bits(func)?;
    let mut candidates = BTreeSet::<VReg>::new();
    let mut queue = VecDeque::<VReg>::new();
    for (value, recipe) in recipes.iter().enumerate() {
        if matches!(recipe, PlanningRecipe::State) {
            seed_candidate(VReg(value as u32), value_count, &mut candidates, &mut queue)?;
        }
    }
    for block in &func.blocks {
        for inst in &block.insts {
            for home in store_home_specs(func, inst, &canonical_bits) {
                seed_candidate(home.value, value_count, &mut candidates, &mut queue)?;
            }
        }
    }

    while let Some(value) = queue.pop_front() {
        for dependent in dependents[value.0 as usize].iter().copied() {
            match dependent {
                Dependent::Pure(destination) => {
                    seed_candidate(destination, value_count, &mut candidates, &mut queue)?
                }
                Dependent::Phi(index) => {
                    let phi = &mut phis[index];
                    phi.remaining_sources = phi.remaining_sources.saturating_sub(1);
                    if phi.remaining_sources == 0 {
                        seed_candidate(phi.destination, value_count, &mut candidates, &mut queue)?;
                    }
                }
            }
        }
    }
    Ok(candidates)
}

#[cfg(test)]
pub(super) fn analyze(
    func: &MFunction,
    cfg: &NormalizedCfg,
) -> Result<ReloadRecipeAnalysis, ReloadRecipeError> {
    let analysis = analyze_unverified_with_queries(func, cfg, &BTreeSet::new(), true)?;
    analysis.verify(func, cfg)?;
    Ok(analysis)
}

pub(super) fn analyze_with_queries(
    func: &MFunction,
    cfg: &NormalizedCfg,
    requested_points: &BTreeSet<PointUse>,
) -> Result<ReloadRecipeAnalysis, ReloadRecipeError> {
    let analysis = analyze_unverified_with_queries(func, cfg, requested_points, false)?;
    analysis.verify(func, cfg)?;
    Ok(analysis)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecipeBase {
    Constant,
    State(VReg),
}

#[derive(Debug)]
struct MemoryPhi {
    block: usize,
    variable: MemoryVariable,
    version: MemoryVersion,
    inputs: Vec<(usize, MemoryVersion)>,
}

#[derive(Debug)]
struct MemorySsa {
    tracked_bytes: BTreeSet<i64>,
    entry_versions: BTreeMap<MemoryVariable, MemoryVersion>,
    write_versions: HashMap<(usize, usize, MemoryVariable), MemoryVersion>,
    phis: Vec<MemoryPhi>,
    phis_by_block: Vec<Vec<(MemoryVariable, usize)>>,
}

fn analyze_unverified_with_queries(
    func: &MFunction,
    cfg: &NormalizedCfg,
    requested_points: &BTreeSet<PointUse>,
    collect_all_uses: bool,
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
    let mut store_homes = HashMap::<(usize, usize), Vec<StoreHomeSpec>>::new();
    let mut preserving_writes = HashMap::<(usize, usize), ValidatedStateInsert>::new();
    for (block, mir_block) in func.blocks.iter().enumerate() {
        for (instruction, inst) in mir_block.insts.iter().enumerate() {
            if let Some(insert) = validated_state_insert(func, inst, &canonical_bits) {
                preserving_writes.insert((block, instruction), insert);
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
    let mut tracked_bytes = BTreeSet::<i64>::new();
    for &value in &relevant_values {
        if let Some(load) = state_loads.get(value.0 as usize).copied().flatten() {
            tracked_bytes.extend(load.bytes().expect("state-load range was validated"));
        }
    }
    for homes in store_homes.values() {
        for home in homes {
            let Some(bytes) = home.load.bytes() else {
                return Err(ReloadRecipeError::new(
                    "RELOAD_RECIPE.STATE_RANGE",
                    None,
                    None,
                    Some(home.value),
                    "store-home byte range overflows i64",
                ));
            };
            tracked_bytes.extend(bytes);
        }
    }

    let mut memory_ssa = build_memory_ssa(func, cfg, &tracked_bytes)?;
    let mut point_recipes = BTreeMap::new();
    let mut edge_recipes = BTreeMap::new();
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
        &preserving_writes,
        &relevant_values,
        requested_points,
        collect_all_uses,
        &mut point_recipes,
        &mut edge_recipes,
        &mut valid_point_uses,
        &mut valid_edge_uses,
    )?;
    verify_memory_phis(func, cfg, &memory_ssa)?;
    let phi_aliases = trivial_memory_phi_aliases(&memory_ssa);
    canonicalize_reload_recipes(
        &mut recipes,
        &mut point_recipes,
        &mut edge_recipes,
        &phi_aliases,
    );

    Ok(ReloadRecipeAnalysis {
        recipes,
        pure_recipes,
        requested_points: requested_points.clone(),
        point_recipes,
        edge_recipes,
        valid_point_uses,
        valid_edge_uses,
        collect_all_uses,
    })
}

#[derive(Debug, Clone, Copy)]
struct ValidatedStateInsert {
    value: VReg,
    load: StateLoad,
    bit_offset: usize,
    width_bits: usize,
    observed_bits: StateBitRange,
}

fn validated_state_insert(
    func: &MFunction,
    inst: &MInst,
    canonical_bits: &[u8],
) -> Option<ValidatedStateInsert> {
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
    if insert.width_bits == 0
        || insert.width_bits > 64
        || end_bit > stored_bits
        || canonical_bits
            .get(insert.value.0 as usize)
            .is_none_or(|bits| usize::from(*bits) > insert.width_bits)
    {
        return None;
    }
    Some(ValidatedStateInsert {
        value: insert.value,
        load,
        bit_offset: insert.bit_offset,
        width_bits: insert.width_bits,
        observed_bits: StateBitRange::inserted(load, insert.bit_offset, insert.width_bits)?,
    })
}

fn store_home_specs(func: &MFunction, inst: &MInst, canonical_bits: &[u8]) -> Vec<StoreHomeSpec> {
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

    let Some(insert) = validated_state_insert(func, inst, canonical_bits) else {
        return homes;
    };
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
fn canonical_value_bits(func: &MFunction) -> Result<Vec<u8>, ReloadRecipeError> {
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

fn canonical_instruction_bits(
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
                    crate::backend::native::mir::SpillKind::SimState { width_bits, .. }
                    | crate::backend::native::mir::SpillKind::SimStateAlias {
                        width_bits, ..
                    } => u8::try_from(width_bits).ok(),
                    crate::backend::native::mir::SpillKind::Stack
                    | crate::backend::native::mir::SpillKind::Remat { .. } => None,
                });
            logical.map_or(physical, |logical| logical.min(physical))
        }
        MInst::LoadPtr { size, .. }
        | MInst::LoadIndexed { size, .. }
        | MInst::LoadPtrIndexed { size, .. } => (size.bytes() * 8) as u8,
        MInst::Add32 { .. } | MInst::Sub32 { .. } | MInst::Mul32 { .. } => 32,
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
        MInst::Bsr { .. } | MInst::BsrOr { .. } => 6,
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

fn significant_bits(value: u64) -> u8 {
    (u64::BITS - value.leading_zeros()) as u8
}

fn resolve_recipe_bases(
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

fn relevant_recipe_values(
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

fn pure_expression(inst: &MInst) -> Option<PureRecipe> {
    match inst {
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
        MInst::BitNot { src, .. } => Some(PureRecipe::BitNot64 { source: *src }),
        MInst::Neg { src, .. } => Some(PureRecipe::Neg64 { source: *src }),
        _ => None,
    }
}

fn build_memory_ssa(
    func: &MFunction,
    cfg: &NormalizedCfg,
    tracked_bytes: &BTreeSet<i64>,
) -> Result<MemorySsa, ReloadRecipeError> {
    let variables = std::iter::once(MemoryVariable::UnknownAlias)
        .chain(tracked_bytes.iter().copied().map(MemoryVariable::Byte))
        .collect::<Vec<_>>();
    let mut entry_versions = BTreeMap::new();
    for variable in variables {
        entry_versions.insert(variable, MemoryVersion::Entry(variable));
    }

    let mut definition_blocks = BTreeMap::<MemoryVariable, BTreeSet<usize>>::new();
    let mut write_versions = HashMap::new();
    for (block, mir_block) in func.blocks.iter().enumerate() {
        let mut write_ordinal = 0usize;
        for (instruction, inst) in mir_block.insts.iter().enumerate() {
            let effect = memory_effect::writes(inst);
            let affects_sim_state =
                matches!(
                    effect.unknown_memory(),
                    Some(memory_effect::UnknownMemory::Direct(BaseReg::SimState))
                ) || effect.ranges().any(|range| range.base == BaseReg::SimState);
            let ordinal = if affects_sim_state {
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
                Some(ordinal)
            } else {
                None
            };
            let affected = affected_variables(inst, tracked_bytes)?;
            if affected.is_empty() {
                continue;
            }
            let ordinal = ordinal.ok_or_else(|| {
                ReloadRecipeError::new(
                    "RELOAD_RECIPE.WRITE_ORDINAL_MISSING",
                    Some(mir_block.id),
                    Some(instruction),
                    None,
                    "MemorySSA found affected SimState variables for an instruction without a SimState write ordinal",
                )
            })?;
            for variable in affected {
                definition_blocks.entry(variable).or_default().insert(block);
                write_versions.insert(
                    (block, instruction, variable),
                    MemoryVersion::Write {
                        block: mir_block.id,
                        ordinal,
                        variable,
                    },
                );
            }
        }
    }

    let mut phis = Vec::<MemoryPhi>::new();
    let mut phis_by_block = vec![Vec::new(); func.blocks.len()];
    for (variable, original_definitions) in definition_blocks {
        let mut definitions = original_definitions;
        let mut queue = definitions.iter().copied().collect::<VecDeque<_>>();
        let mut placed = BTreeSet::<usize>::new();
        while let Some(definition) = queue.pop_front() {
            for &frontier in &cfg.dominance_frontier[definition] {
                if frontier == 0 || !placed.insert(frontier) {
                    continue;
                }
                let id = phis.len();
                phis.push(MemoryPhi {
                    block: frontier,
                    variable,
                    version: MemoryVersion::Phi {
                        block: func.blocks[frontier].id,
                        variable,
                    },
                    inputs: Vec::with_capacity(cfg.predecessors[frontier].len()),
                });
                phis_by_block[frontier].push((variable, id));
                if definitions.insert(frontier) {
                    queue.push_back(frontier);
                }
            }
        }
    }
    for entries in &mut phis_by_block {
        entries.sort_unstable_by_key(|(variable, _)| *variable);
    }

    Ok(MemorySsa {
        tracked_bytes: tracked_bytes.clone(),
        entry_versions,
        write_versions,
        phis,
        phis_by_block,
    })
}

fn affected_variables(
    inst: &MInst,
    tracked_bytes: &BTreeSet<i64>,
) -> Result<Vec<MemoryVariable>, ReloadRecipeError> {
    let effect = memory_effect::writes(inst);
    if matches!(
        effect.unknown_memory(),
        Some(memory_effect::UnknownMemory::Direct(BaseReg::SimState))
    ) {
        return Ok(vec![MemoryVariable::UnknownAlias]);
    }
    let mut affected = BTreeSet::new();
    for range in effect
        .ranges()
        .filter(|range| range.base == BaseReg::SimState)
    {
        let Some(end) = range.end() else {
            return Ok(vec![MemoryVariable::UnknownAlias]);
        };
        affected.extend(
            tracked_bytes
                .range(range.offset..end)
                .copied()
                .map(MemoryVariable::Byte),
        );
    }
    Ok(affected.into_iter().collect())
}

#[allow(clippy::too_many_arguments)]
fn rename_memory_ssa(
    func: &MFunction,
    cfg: &NormalizedCfg,
    state_loads: &[Option<StateLoad>],
    pure_recipes: &[PureRecipe],
    recipes: &mut [ReloadRecipe],
    memory_ssa: &mut MemorySsa,
    store_homes: &HashMap<(usize, usize), Vec<StoreHomeSpec>>,
    preserving_writes: &HashMap<(usize, usize), ValidatedStateInsert>,
    relevant_values: &BTreeSet<VReg>,
    requested_points: &BTreeSet<PointUse>,
    collect_all_uses: bool,
    point_recipes: &mut BTreeMap<PointUse, ResolvedRecipe>,
    edge_recipes: &mut BTreeMap<EdgeUse, ResolvedRecipe>,
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
            memory_changes: Vec<(MemoryVariable, Option<MemoryVersion>)>,
            home_pushes: Vec<VReg>,
        },
    }

    let mut current = BTreeMap::<MemoryVariable, MemoryVersion>::new();
    let mut current_homes = BTreeMap::<VReg, Vec<StoreHome>>::new();
    let mut current_home_index = StoreHomeIndex::new();
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
                memory_changes,
                home_pushes,
            } => {
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
                for (variable, previous) in memory_changes.into_iter().rev() {
                    if let Some(previous) = previous {
                        current.insert(variable, previous);
                    } else {
                        current.remove(&variable);
                    }
                }
                continue;
            }
            Action::Enter(block) => block,
        };
        let mut memory_changes = Vec::new();
        let mut home_pushes = Vec::new();
        for &(variable, phi) in &memory_ssa.phis_by_block[block] {
            set_current(
                &mut current,
                &mut memory_changes,
                variable,
                memory_ssa.phis[phi].version,
            );
        }

        let block_id = func.blocks[block].id;
        // A register phi that merges values already committed to the same
        // exact SimState expression has the MemorySSA phi for that slot as a
        // home. Identity copies introduced by CSSA do not change the shape.
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
                        version: current_state_version(load, &current, memory_ssa)?,
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
                        &current,
                        memory_ssa,
                    )? {
                        point_recipes.insert(point, recipe);
                        valid_point_uses.insert(point);
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
                        &current,
                        memory_ssa,
                    )? {
                        point_recipes.insert(point, recipe);
                    }
                }
            }

            if let Some(definition) = inst.def()
                && relevant_values.contains(&definition)
                && let Some(load) = state_loads.get(definition.0 as usize).copied().flatten()
            {
                recipes[definition.0 as usize] = ReloadRecipe::StateVersion(StateRecipe {
                    load,
                    version: current_state_version(load, &current, memory_ssa)?,
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
                            && current_state_version(home.state.load, &current, memory_ssa)?
                                == home.state.version
                        {
                            preserved_homes.push((value, home.clone()));
                        }
                    }
                }
            }

            for variable in affected_variables(inst, &memory_ssa.tracked_bytes)? {
                let Some(&version) = memory_ssa
                    .write_versions
                    .get(&(block, instruction, variable))
                else {
                    return Err(ReloadRecipeError::new(
                        "RELOAD_RECIPE.WRITE_VERSION",
                        Some(block_id),
                        Some(instruction),
                        None,
                        "memory write has no MemorySSA definition",
                    ));
                };
                set_current(&mut current, &mut memory_changes, variable, version);
            }

            for (value, mut home) in preserved_homes {
                let version = current_state_version(home.state.load, &current, memory_ssa)?;
                if version == home.state.version {
                    continue;
                }
                home.state.version = version;
                index_store_home(&mut current_home_index, value, &home);
                current_homes.entry(value).or_default().push(home);
                home_pushes.push(value);
            }

            if let Some(homes) = store_homes.get(&(block, instruction)) {
                for home in homes {
                    let stored = StoreHome {
                        state: StateRecipe {
                            load: home.load,
                            version: current_state_version(home.load, &current, memory_ssa)?,
                            observed_bits: home.observed_bits,
                        },
                        steps: home.steps.clone(),
                    };
                    index_store_home(&mut current_home_index, home.value, &stored);
                    current_homes.entry(home.value).or_default().push(stored);
                    home_pushes.push(home.value);
                }
            }
        }

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
                    &current,
                    memory_ssa,
                )? {
                    edge_recipes.insert(edge, recipe);
                    valid_edge_uses.insert(edge);
                }
            }
            for &(_, phi) in &memory_ssa.phis_by_block[successor] {
                let variable = memory_ssa.phis[phi].variable;
                let version = current_version(variable, &current, memory_ssa)?;
                memory_ssa.phis[phi].inputs.push((block, version));
            }
        }

        actions.push(Action::Exit {
            memory_changes,
            home_pushes,
        });
        actions.extend(children[block].iter().rev().copied().map(Action::Enter));
    }
    Ok(())
}

/// A reload selected after spill planning.  The expected recipe comes from a
/// verified pre-reconstruction MemorySSA snapshot; the final verifier rebuilds
/// MemorySSA and compares the emitted machine operation and reaching version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ExpectedMaterializedReload {
    pub reload: VReg,
    pub expected: ResolvedRecipe,
}

pub(super) fn verify_expected_materialized_reloads(
    func: &MFunction,
    cfg: &NormalizedCfg,
    reloads: &[ExpectedMaterializedReload],
) -> Result<(), ReloadRecipeError> {
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
        verify_resolved_recipe_match(materialization.reload, &materialization.expected, &actual)?;
    }
    Ok(())
}

fn verify_resolved_recipe_match(
    reload: VReg,
    original: &ResolvedRecipe,
    materialized: &ResolvedRecipe,
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
            if left.version != right.version {
                return Err(ReloadRecipeError::new(
                    "RELOAD_RECIPE.STATE_VERSION_CURRENT",
                    None,
                    None,
                    Some(reload),
                    format!(
                        "an overlapping or unknown state write changed the selected version {:?} to {:?}",
                        left.version, right.version
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

fn set_current(
    current: &mut BTreeMap<MemoryVariable, MemoryVersion>,
    changes: &mut Vec<(MemoryVariable, Option<MemoryVersion>)>,
    variable: MemoryVariable,
    version: MemoryVersion,
) {
    let previous = current.insert(variable, version);
    changes.push((variable, previous));
}

fn current_version(
    variable: MemoryVariable,
    current: &BTreeMap<MemoryVariable, MemoryVersion>,
    memory_ssa: &MemorySsa,
) -> Result<MemoryVersion, ReloadRecipeError> {
    current
        .get(&variable)
        .copied()
        .or_else(|| memory_ssa.entry_versions.get(&variable).copied())
        .ok_or_else(|| {
            ReloadRecipeError::new(
                "RELOAD_RECIPE.ENTRY_VERSION",
                None,
                None,
                None,
                format!("MemorySSA variable {variable:?} has no entry definition"),
            )
        })
}

fn current_state_version(
    load: StateLoad,
    current: &BTreeMap<MemoryVariable, MemoryVersion>,
    memory_ssa: &MemorySsa,
) -> Result<StateVersion, ReloadRecipeError> {
    let unknown_alias = current_version(MemoryVariable::UnknownAlias, current, memory_ssa)?;
    let Some(bytes) = load.bytes() else {
        return Err(ReloadRecipeError::new(
            "RELOAD_RECIPE.STATE_RANGE",
            None,
            None,
            None,
            "state load byte range overflows i64",
        ));
    };
    let bytes = bytes
        .map(|byte| current_version(MemoryVariable::Byte(byte), current, memory_ssa))
        .collect::<Result<Box<[_]>, _>>()?;
    Ok(StateVersion {
        unknown_alias,
        bytes,
    })
}

fn available_recipe(
    value: VReg,
    recipes: &[ReloadRecipe],
    pure_recipes: &[PureRecipe],
    current_homes: &BTreeMap<VReg, Vec<StoreHome>>,
    current: &BTreeMap<MemoryVariable, MemoryVersion>,
    memory_ssa: &MemorySsa,
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
        if let Some(recipe) = available_store_home(
            current_value,
            &reverse_steps,
            current_homes,
            current,
            memory_ssa,
        )? {
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
                if current_state_version(recipe.load, current, memory_ssa)? != recipe.version {
                    return available_store_home(
                        current_value,
                        &reverse_steps,
                        current_homes,
                        current,
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
                    current,
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

fn available_store_home(
    value: VReg,
    reverse_steps: &[PureStep],
    current_homes: &BTreeMap<VReg, Vec<StoreHome>>,
    current: &BTreeMap<MemoryVariable, MemoryVersion>,
    memory_ssa: &MemorySsa,
) -> Result<Option<ResolvedRecipe>, ReloadRecipeError> {
    let Some(homes) = current_homes.get(&value) else {
        return Ok(None);
    };
    for home in homes.iter().rev() {
        if current_state_version(home.state.load, current, memory_ssa)? == home.state.version {
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

/// Collapse MemorySSA phis whose complete SCC has one external version.
///
/// Iterated dominance-frontier placement is intentionally structural and can
/// create a wrapper phi after CFG tail merging even when every incoming edge
/// carries the same version.  Treating that wrapper as a new state value would
/// reject a valid reload moved from those edges into their shared block.  SCC
/// condensation handles loop phis without recursion: a component is trivial
/// only when all of its external operands canonicalize to one version.
fn trivial_memory_phi_aliases(memory_ssa: &MemorySsa) -> HashMap<MemoryVersion, MemoryVersion> {
    let count = memory_ssa.phis.len();
    if count == 0 {
        return HashMap::new();
    }
    let phi_index = memory_ssa
        .phis
        .iter()
        .enumerate()
        .map(|(index, phi)| (phi.version, index))
        .collect::<HashMap<_, _>>();
    let mut dependencies = vec![Vec::<usize>::new(); count];
    let mut users = vec![Vec::<usize>::new(); count];
    for (phi, node) in memory_ssa.phis.iter().enumerate() {
        let inputs = node
            .inputs
            .iter()
            .filter_map(|(_, version)| phi_index.get(version).copied())
            .collect::<BTreeSet<_>>();
        dependencies[phi].extend(inputs.iter().copied());
        for input in inputs {
            users[input].push(phi);
        }
    }

    let mut visited = vec![false; count];
    let mut postorder = Vec::with_capacity(count);
    for root in 0..count {
        if visited[root] {
            continue;
        }
        let mut stack = vec![(root, false)];
        while let Some((phi, expanded)) = stack.pop() {
            if expanded {
                postorder.push(phi);
                continue;
            }
            if std::mem::replace(&mut visited[phi], true) {
                continue;
            }
            stack.push((phi, true));
            stack.extend(
                dependencies[phi]
                    .iter()
                    .rev()
                    .copied()
                    .map(|dependency| (dependency, false)),
            );
        }
    }

    let mut component_of = vec![usize::MAX; count];
    let mut components = Vec::<Vec<usize>>::new();
    for root in postorder.into_iter().rev() {
        if component_of[root] != usize::MAX {
            continue;
        }
        let component = components.len();
        let mut members = Vec::new();
        let mut stack = vec![root];
        component_of[root] = component;
        while let Some(phi) = stack.pop() {
            members.push(phi);
            for &user in &users[phi] {
                if component_of[user] == usize::MAX {
                    component_of[user] = component;
                    stack.push(user);
                }
            }
        }
        members.sort_unstable();
        components.push(members);
    }

    let mut component_dependencies = vec![BTreeSet::<usize>::new(); components.len()];
    let mut component_users = vec![BTreeSet::<usize>::new(); components.len()];
    for phi in 0..count {
        let component = component_of[phi];
        for &dependency in &dependencies[phi] {
            let dependency = component_of[dependency];
            if dependency != component && component_dependencies[component].insert(dependency) {
                component_users[dependency].insert(component);
            }
        }
    }
    let mut pending = component_dependencies
        .iter()
        .map(BTreeSet::len)
        .collect::<Vec<_>>();
    let mut ready = pending
        .iter()
        .enumerate()
        .filter_map(|(component, pending)| (*pending == 0).then_some(component))
        .collect::<BTreeSet<_>>();
    let mut aliases = HashMap::<MemoryVersion, MemoryVersion>::new();
    while let Some(component) = ready.pop_first() {
        let mut external = BTreeSet::<MemoryVersion>::new();
        for &phi in &components[component] {
            for &(_, version) in &memory_ssa.phis[phi].inputs {
                if phi_index
                    .get(&version)
                    .is_some_and(|input| component_of[*input] == component)
                {
                    continue;
                }
                external.insert(canonical_memory_version(version, &aliases));
            }
        }
        let representative = if external.len() == 1 {
            external.first().copied()
        } else if external.is_empty() {
            components[component]
                .iter()
                .map(|phi| memory_ssa.phis[*phi].version)
                .min()
        } else {
            None
        };
        if let Some(representative) = representative {
            for &phi in &components[component] {
                let version = memory_ssa.phis[phi].version;
                if version != representative {
                    aliases.insert(version, representative);
                }
            }
        }
        for &user in &component_users[component] {
            pending[user] -= 1;
            if pending[user] == 0 {
                ready.insert(user);
            }
        }
    }
    aliases
}

fn canonical_memory_version(
    mut version: MemoryVersion,
    aliases: &HashMap<MemoryVersion, MemoryVersion>,
) -> MemoryVersion {
    while let Some(&canonical) = aliases.get(&version) {
        version = canonical;
    }
    version
}

fn canonicalize_state_recipe(
    recipe: &mut StateRecipe,
    aliases: &HashMap<MemoryVersion, MemoryVersion>,
) {
    recipe.version.unknown_alias = canonical_memory_version(recipe.version.unknown_alias, aliases);
    for version in &mut recipe.version.bytes {
        *version = canonical_memory_version(*version, aliases);
    }
}

fn canonicalize_resolved_recipe(
    recipe: &mut ResolvedRecipe,
    aliases: &HashMap<MemoryVersion, MemoryVersion>,
) {
    if let ResolvedBase::State(state) = &mut recipe.base {
        canonicalize_state_recipe(state, aliases);
    }
}

fn canonicalize_reload_recipes(
    recipes: &mut [ReloadRecipe],
    point_recipes: &mut BTreeMap<PointUse, ResolvedRecipe>,
    edge_recipes: &mut BTreeMap<EdgeUse, ResolvedRecipe>,
    aliases: &HashMap<MemoryVersion, MemoryVersion>,
) {
    if aliases.is_empty() {
        return;
    }
    for recipe in recipes {
        if let ReloadRecipe::StateVersion(state) = recipe {
            canonicalize_state_recipe(state, aliases);
        }
    }
    for recipe in point_recipes.values_mut() {
        canonicalize_resolved_recipe(recipe, aliases);
    }
    for recipe in edge_recipes.values_mut() {
        canonicalize_resolved_recipe(recipe, aliases);
    }
}

fn verify_memory_phis(
    func: &MFunction,
    cfg: &NormalizedCfg,
    memory_ssa: &MemorySsa,
) -> Result<(), ReloadRecipeError> {
    for phi in &memory_ssa.phis {
        let expected = cfg.predecessors[phi.block]
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        let actual = phi
            .inputs
            .iter()
            .map(|(predecessor, _)| *predecessor)
            .collect::<BTreeSet<_>>();
        if expected != actual || phi.inputs.len() != expected.len() {
            return Err(ReloadRecipeError::new(
                "RELOAD_RECIPE.PHI_INPUTS",
                Some(func.blocks[phi.block].id),
                None,
                None,
                format!(
                    "MemorySSA phi for {:?} has predecessor inputs {actual:?}, expected {expected:?}",
                    phi.variable
                ),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::native::mir::{
        MBlock, MemoryAliasRange, PhiNode, SpillDesc, VRegAllocator,
    };
    use crate::ir::{InstanceId, RegionedAbsoluteAddr, STABLE_REGION};
    use veryl_analyzer::ir::VarId;

    fn function_with_values(count: usize) -> (MFunction, Vec<VReg>) {
        let mut vregs = VRegAllocator::new();
        let values = (0..count).map(|_| vregs.alloc()).collect::<Vec<_>>();
        (
            MFunction::new(vregs, vec![SpillDesc::transient(); count]),
            values,
        )
    }

    #[test]
    fn trivial_memory_phi_aliases_cover_wrappers_and_cycles() {
        let variable = MemoryVariable::Byte(80);
        let entry = MemoryVersion::Entry(variable);
        let write = MemoryVersion::Write {
            block: BlockId(0),
            ordinal: 0,
            variable,
        };
        let nontrivial = MemoryVersion::Phi {
            block: BlockId(1),
            variable,
        };
        let wrapper = MemoryVersion::Phi {
            block: BlockId(2),
            variable,
        };
        let cycle_left = MemoryVersion::Phi {
            block: BlockId(3),
            variable,
        };
        let cycle_right = MemoryVersion::Phi {
            block: BlockId(4),
            variable,
        };
        let memory_ssa = MemorySsa {
            tracked_bytes: BTreeSet::new(),
            entry_versions: BTreeMap::new(),
            write_versions: HashMap::new(),
            phis: vec![
                MemoryPhi {
                    block: 1,
                    variable,
                    version: nontrivial,
                    inputs: vec![(0, entry), (1, write)],
                },
                MemoryPhi {
                    block: 2,
                    variable,
                    version: wrapper,
                    inputs: vec![(0, nontrivial), (1, nontrivial)],
                },
                MemoryPhi {
                    block: 3,
                    variable,
                    version: cycle_left,
                    inputs: vec![(0, entry), (1, cycle_right)],
                },
                MemoryPhi {
                    block: 4,
                    variable,
                    version: cycle_right,
                    inputs: vec![(0, cycle_left)],
                },
            ],
            phis_by_block: Vec::new(),
        };

        let aliases = trivial_memory_phi_aliases(&memory_ssa);

        assert!(!aliases.contains_key(&nontrivial));
        assert_eq!(aliases.get(&wrapper), Some(&nontrivial));
        assert_eq!(aliases.get(&cycle_left), Some(&entry));
        assert_eq!(aliases.get(&cycle_right), Some(&entry));
    }

    fn analyze_function(mut func: MFunction) -> (MFunction, NormalizedCfg, ReloadRecipeAnalysis) {
        let cfg = super::super::cfg::normalize(&mut func).unwrap();
        let analysis = analyze(&func, &cfg).unwrap();
        (func, cfg, analysis)
    }

    #[test]
    fn exact_physical_load_shape_becomes_recipe() {
        let (mut func, values) = function_with_values(2);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::Load {
            dst: values[0],
            base: BaseReg::SimState,
            offset: 19,
            size: OpSize::S32,
        });
        block.push(MInst::Mov32 {
            dst: values[1],
            src: values[0],
        });
        block.push(MInst::Return);
        func.push_block(block);
        let (func, cfg, analysis) = analyze_function(func);

        assert_eq!(
            analysis.state_recipe(values[0]).map(|recipe| recipe.load),
            Some(StateLoad {
                offset: 19,
                size: OpSize::S32,
            })
        );
        assert!(analysis.state_valid_at_point(PointUse {
            block: BlockId(0),
            instruction: 1,
            value: values[0],
        }));
        assert!(matches!(
            analysis.recipe(values[1]),
            Some(ReloadRecipe::Pure { .. })
        ));
        assert_eq!(
            analysis.pure_recipe(values[1]),
            Some(PureRecipe::Copy32 { source: values[0] })
        );
        assert_eq!(
            analysis.resolved_recipe(values[1]).unwrap(),
            Some(ResolvedRecipe {
                base: ResolvedBase::State(analysis.state_recipe(values[0]).unwrap().clone()),
                steps: vec![PureStep::Copy32],
            })
        );
        assert_eq!(
            analyze_for_planning(&func, &cfg)
                .unwrap()
                .global_materialization_costs()
                .unwrap(),
            vec![Some(1), Some(2)]
        );
    }

    #[test]
    fn sparse_mark_preserves_only_nonoverlapping_state_recipes() {
        fn fixture(load_offset: i32) -> (VReg, ReloadRecipeAnalysis) {
            let (mut func, values) = function_with_values(3);
            let mut block = MBlock::new(BlockId(0));
            block.push(MInst::Load {
                dst: values[0],
                base: BaseReg::SimState,
                offset: load_offset,
                size: OpSize::S64,
            });
            block.push(MInst::Scratch { dst: values[2] });
            block.push(MInst::SparseMarkActive {
                scratch: values[2],
                active_index: 3,
                active_count_offset: 100,
                active_flags_offset: 200,
                active_list_offset: 300,
                active_capacity: 16,
            });
            block.push(MInst::Mov {
                dst: values[1],
                src: values[0],
            });
            block.push(MInst::Return);
            func.push_block(block);
            let (_, _, analysis) = analyze_function(func);
            (values[0], analysis)
        }

        let (unrelated, unrelated_analysis) = fixture(40);
        assert!(unrelated_analysis.state_valid_at_point(PointUse {
            block: BlockId(0),
            instruction: 3,
            value: unrelated,
        }));

        let (metadata, metadata_analysis) = fixture(100);
        assert!(!metadata_analysis.state_valid_at_point(PointUse {
            block: BlockId(0),
            instruction: 3,
            value: metadata,
        }));
    }

    #[test]
    fn exact_s64_store_establishes_a_post_store_home() {
        let (mut func, values) = function_with_values(3);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::Load {
            dst: values[0],
            base: BaseReg::StackFrame,
            offset: 0,
            size: OpSize::S64,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 40,
            src: values[0],
            size: OpSize::S64,
        });
        block.push(MInst::Mov {
            dst: values[1],
            src: values[0],
        });
        block.push(MInst::Store {
            base: BaseReg::StackFrame,
            offset: 8,
            src: values[1],
            size: OpSize::S64,
        });
        block.push(MInst::LoadImm {
            dst: values[2],
            value: 0,
        });
        block.push(MInst::Return);
        func.push_block(block);
        let (_, _, analysis) = analyze_function(func);

        assert!(
            analysis
                .resolved_recipe_at_point(PointUse {
                    block: BlockId(0),
                    instruction: 1,
                    value: values[0],
                })
                .is_none(),
            "a store cannot use the home which it has not established yet"
        );
        let recipe = analysis
            .resolved_recipe_at_point(PointUse {
                block: BlockId(0),
                instruction: 2,
                value: values[0],
            })
            .unwrap();
        assert!(matches!(
            &recipe.base,
            ResolvedBase::State(StateRecipe {
                load: StateLoad {
                    offset: 40,
                    size: OpSize::S64,
                },
                ..
            })
        ));
        assert!(recipe.steps.is_empty());
    }

    #[test]
    fn planning_cost_is_exact_at_each_use_and_falls_back_after_overwrite() {
        let (mut func, values) = function_with_values(4);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::Load {
            dst: values[0],
            base: BaseReg::StackFrame,
            offset: 0,
            size: OpSize::S64,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 40,
            src: values[0],
            size: OpSize::S64,
        });
        block.push(MInst::Mov {
            dst: values[1],
            src: values[0],
        });
        block.push(MInst::LoadImm {
            dst: values[2],
            value: 9,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 40,
            src: values[2],
            size: OpSize::S64,
        });
        block.push(MInst::Mov {
            dst: values[3],
            src: values[0],
        });
        block.push(MInst::Return);
        func.push_block(block);
        let cfg = super::super::cfg::normalize(&mut func).unwrap();

        let costs = analyze_for_planning(&func, &cfg).unwrap();

        assert_eq!(costs.global_materialization_cost(values[0]), None);
        assert_eq!(
            costs.materialization_cost_at_point(PointUse {
                block: BlockId(0),
                instruction: 2,
                value: values[0],
            }),
            Some(1)
        );
        assert_eq!(
            costs.materialization_cost_at_point(PointUse {
                block: BlockId(0),
                instruction: 5,
                value: values[0],
            }),
            None,
            "an overwritten MemorySSA version must use the stack fallback"
        );
    }

    #[test]
    fn planning_cost_preserves_exact_phi_edge_homes() {
        let (mut func, values) = function_with_values(5);
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: values[0],
            value: 1,
        });
        entry.push(MInst::Branch {
            cond: values[0],
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        });
        let mut left = MBlock::new(BlockId(1));
        left.push(MInst::Load {
            dst: values[1],
            base: BaseReg::StackFrame,
            offset: 0,
            size: OpSize::S64,
        });
        left.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 40,
            src: values[1],
            size: OpSize::S64,
        });
        left.push(MInst::Jump { target: BlockId(3) });
        let mut right = MBlock::new(BlockId(2));
        right.push(MInst::Load {
            dst: values[2],
            base: BaseReg::StackFrame,
            offset: 8,
            size: OpSize::S64,
        });
        right.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 40,
            src: values[2],
            size: OpSize::S64,
        });
        right.push(MInst::Jump { target: BlockId(3) });
        let mut join = MBlock::new(BlockId(3));
        join.phis.push(PhiNode {
            dst: values[3],
            sources: vec![(BlockId(1), values[1]), (BlockId(2), values[2])],
        });
        join.push(MInst::Mov {
            dst: values[4],
            src: values[3],
        });
        join.push(MInst::Return);
        func.blocks = vec![entry, left, right, join];
        let cfg = super::super::cfg::normalize(&mut func).unwrap();

        let costs = analyze_for_planning(&func, &cfg).unwrap();

        assert_eq!(
            costs.materialization_cost_at_point(PointUse {
                block: BlockId(3),
                instruction: 0,
                value: values[3],
            }),
            Some(1)
        );
        for (predecessor, value) in [(BlockId(1), values[1]), (BlockId(2), values[2])] {
            assert_eq!(
                costs.materialization_cost_on_edge(EdgeUse {
                    predecessor,
                    successor: BlockId(3),
                    value,
                }),
                Some(1)
            );
        }
    }

    #[test]
    fn exact_store_of_pure_result_precedes_expression_reconstruction() {
        let (mut func, values) = function_with_values(3);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::Load {
            dst: values[0],
            base: BaseReg::StackFrame,
            offset: 0,
            size: OpSize::S64,
        });
        block.push(MInst::AddImm {
            dst: values[1],
            src: values[0],
            imm: 7,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 40,
            src: values[1],
            size: OpSize::S64,
        });
        block.push(MInst::Mov {
            dst: values[2],
            src: values[1],
        });
        block.push(MInst::Return);
        func.push_block(block);
        let (_, _, analysis) = analyze_function(func);

        let point = PointUse {
            block: BlockId(0),
            instruction: 3,
            value: values[1],
        };
        let recipe = analysis.resolved_recipe_at_point(point).unwrap();
        assert!(analysis.point_recipe_uses_store_home(point));
        assert!(matches!(
            &recipe.base,
            ResolvedBase::State(StateRecipe {
                load: StateLoad {
                    offset: 40,
                    size: OpSize::S64,
                },
                ..
            })
        ));
        assert!(
            recipe.steps.is_empty(),
            "the stored pure result is already the requested value"
        );
    }

    #[test]
    fn proved_zero_extended_narrow_store_is_an_exact_home() {
        let (mut func, values) = function_with_values(3);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::Load {
            dst: values[0],
            base: BaseReg::StackFrame,
            offset: 0,
            size: OpSize::S64,
        });
        block.push(MInst::AndImm {
            dst: values[1],
            src: values[0],
            imm: 0xff,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 40,
            src: values[1],
            size: OpSize::S8,
        });
        block.push(MInst::Mov {
            dst: values[2],
            src: values[1],
        });
        block.push(MInst::Return);
        func.push_block(block);
        let (_, _, analysis) = analyze_function(func);

        let recipe = analysis
            .resolved_recipe_at_point(PointUse {
                block: BlockId(0),
                instruction: 3,
                value: values[1],
            })
            .unwrap();
        assert!(matches!(
            &recipe.base,
            ResolvedBase::State(StateRecipe {
                load: StateLoad {
                    offset: 40,
                    size: OpSize::S8,
                },
                ..
            })
        ));
        assert!(recipe.steps.is_empty());
    }

    #[test]
    fn potentially_overflowing_value_is_not_a_narrow_store_home() {
        let (mut func, values) = function_with_values(4);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::Load {
            dst: values[0],
            base: BaseReg::StackFrame,
            offset: 0,
            size: OpSize::S64,
        });
        block.push(MInst::AndImm {
            dst: values[1],
            src: values[0],
            imm: 0xff,
        });
        block.push(MInst::AddImm {
            dst: values[2],
            src: values[1],
            imm: 1,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 40,
            src: values[2],
            size: OpSize::S8,
        });
        block.push(MInst::Mov {
            dst: values[3],
            src: values[2],
        });
        block.push(MInst::Return);
        func.push_block(block);
        let (_, _, analysis) = analyze_function(func);

        assert!(
            analysis
                .resolved_recipe_at_point(PointUse {
                    block: BlockId(0),
                    instruction: 4,
                    value: values[2],
                })
                .is_none()
        );
    }

    fn phi_of_committed_values(right_offset: i32) -> (MFunction, VReg) {
        let (mut func, values) = function_with_values(7);
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: values[0],
            value: 1,
        });
        entry.push(MInst::Branch {
            cond: values[0],
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        });

        let mut left = MBlock::new(BlockId(1));
        left.push(MInst::Load {
            dst: values[1],
            base: BaseReg::StackFrame,
            offset: 0,
            size: OpSize::S64,
        });
        left.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 40,
            src: values[1],
            size: OpSize::S64,
        });
        left.push(MInst::Mov {
            dst: values[3],
            src: values[1],
        });
        left.push(MInst::Jump { target: BlockId(3) });

        let mut right = MBlock::new(BlockId(2));
        right.push(MInst::Load {
            dst: values[2],
            base: BaseReg::StackFrame,
            offset: 8,
            size: OpSize::S64,
        });
        right.push(MInst::Store {
            base: BaseReg::SimState,
            offset: right_offset,
            src: values[2],
            size: OpSize::S64,
        });
        right.push(MInst::Mov {
            dst: values[4],
            src: values[2],
        });
        right.push(MInst::Jump { target: BlockId(3) });

        let mut join = MBlock::new(BlockId(3));
        join.phis.push(PhiNode {
            dst: values[5],
            sources: vec![(BlockId(1), values[3]), (BlockId(2), values[4])],
        });
        join.push(MInst::Mov {
            dst: values[6],
            src: values[5],
        });
        join.push(MInst::Return);
        func.blocks = vec![entry, left, right, join];
        (func, values[5])
    }

    #[test]
    fn memory_phi_is_an_exact_home_for_matching_register_phi() {
        let (func, phi) = phi_of_committed_values(40);
        let (_, _, analysis) = analyze_function(func);

        let recipe = analysis
            .resolved_recipe_at_point(PointUse {
                block: BlockId(3),
                instruction: 0,
                value: phi,
            })
            .unwrap();
        assert!(matches!(
            &recipe.base,
            ResolvedBase::State(StateRecipe {
                load: StateLoad {
                    offset: 40,
                    size: OpSize::S64,
                },
                ..
            })
        ));
        assert!(recipe.steps.is_empty());
    }

    #[test]
    fn logical_state_load_width_proves_a_narrow_store_phi_home() {
        let (mut func, values) = function_with_values(5);
        let address = RegionedAbsoluteAddr {
            region: STABLE_REGION,
            instance_id: InstanceId(0),
            var_id: VarId::default(),
        };
        func.spill_descs[values[1].0 as usize] = SpillDesc::sim_state(address, 0, 5, false);
        func.spill_descs[values[2].0 as usize] = SpillDesc::sim_state(address, 0, 5, false);

        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: values[0],
            value: 1,
        });
        entry.push(MInst::Branch {
            cond: values[0],
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        });

        let mut left = MBlock::new(BlockId(1));
        left.push(MInst::Load {
            dst: values[1],
            base: BaseReg::SimState,
            offset: 8,
            size: OpSize::S64,
        });
        left.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 40,
            src: values[1],
            size: OpSize::S8,
        });
        left.push(MInst::Jump { target: BlockId(3) });

        let mut right = MBlock::new(BlockId(2));
        right.push(MInst::Load {
            dst: values[2],
            base: BaseReg::SimState,
            offset: 16,
            size: OpSize::S64,
        });
        right.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 40,
            src: values[2],
            size: OpSize::S8,
        });
        right.push(MInst::Jump { target: BlockId(3) });

        let mut join = MBlock::new(BlockId(3));
        join.phis.push(PhiNode {
            dst: values[3],
            sources: vec![(BlockId(1), values[1]), (BlockId(2), values[2])],
        });
        join.push(MInst::Mov {
            dst: values[4],
            src: values[3],
        });
        join.push(MInst::Return);

        // Keep the CFG valid but place the phi before its source definitions
        // in the block vector. A single linear width scan cannot prove it.
        func.blocks = vec![entry, join, left, right];
        let bits = canonical_value_bits(&func).unwrap();
        assert_eq!(bits[values[3].0 as usize], 5);

        let (_, _, analysis) = analyze_function(func);
        let recipe = analysis
            .resolved_recipe_at_point(PointUse {
                block: BlockId(3),
                instruction: 0,
                value: values[3],
            })
            .unwrap();
        assert!(matches!(
            &recipe.base,
            ResolvedBase::State(StateRecipe {
                load: StateLoad {
                    offset: 40,
                    size: OpSize::S8,
                },
                ..
            })
        ));
        assert!(recipe.steps.is_empty());
    }

    #[test]
    fn partial_rmw_store_phi_reconstructs_inserted_value_from_state() {
        let (mut func, values) = function_with_values(11);
        let low_mask = (1u64 << 52) - 1;
        let high_mask = !low_mask;
        func.spill_descs[values[4].0 as usize] =
            SpillDesc::transient().with_state_insert(values[1], 0, 52);
        func.spill_descs[values[8].0 as usize] =
            SpillDesc::transient().with_state_insert(values[5], 0, 52);

        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: values[0],
            value: 1,
        });
        entry.push(MInst::Branch {
            cond: values[0],
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        });

        let mut left = MBlock::new(BlockId(1));
        left.push(MInst::LoadImm {
            dst: values[1],
            value: 0x123,
        });
        left.push(MInst::Load {
            dst: values[2],
            base: BaseReg::SimState,
            offset: 40,
            size: OpSize::S64,
        });
        left.push(MInst::AndImm {
            dst: values[3],
            src: values[2],
            imm: high_mask,
        });
        left.push(MInst::Or {
            dst: values[4],
            lhs: values[3],
            rhs: values[1],
        });
        left.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 40,
            src: values[4],
            size: OpSize::S64,
        });
        left.push(MInst::Jump { target: BlockId(3) });

        let mut right = MBlock::new(BlockId(2));
        right.push(MInst::LoadImm {
            dst: values[5],
            value: 0x456,
        });
        right.push(MInst::Load {
            dst: values[6],
            base: BaseReg::SimState,
            offset: 40,
            size: OpSize::S64,
        });
        right.push(MInst::AndImm {
            dst: values[7],
            src: values[6],
            imm: high_mask,
        });
        right.push(MInst::Or {
            dst: values[8],
            lhs: values[7],
            rhs: values[5],
        });
        right.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 40,
            src: values[8],
            size: OpSize::S64,
        });
        right.push(MInst::Jump { target: BlockId(3) });

        let mut join = MBlock::new(BlockId(3));
        join.phis.push(PhiNode {
            dst: values[9],
            sources: vec![(BlockId(1), values[1]), (BlockId(2), values[5])],
        });
        join.push(MInst::Mov {
            dst: values[10],
            src: values[9],
        });
        join.push(MInst::Return);
        func.blocks = vec![entry, left, right, join];

        let (_, _, analysis) = analyze_function(func);
        let recipe = analysis
            .resolved_recipe_at_point(PointUse {
                block: BlockId(3),
                instruction: 0,
                value: values[9],
            })
            .unwrap();
        assert!(matches!(
            &recipe.base,
            ResolvedBase::State(StateRecipe {
                load: StateLoad {
                    offset: 40,
                    size: OpSize::S64,
                },
                ..
            })
        ));
        assert_eq!(
            recipe.steps,
            vec![
                PureStep::ShlImm64 { immediate: 12 },
                PureStep::ShrImm64 { immediate: 12 },
            ]
        );
    }

    fn two_partial_rmw_stores(second_bit: usize) -> (MFunction, VReg, PointUse) {
        let (mut func, values) = function_with_values(12);
        func.spill_descs[values[4].0 as usize] =
            SpillDesc::transient().with_state_insert(values[1], 0, 1);
        func.spill_descs[values[10].0 as usize] =
            SpillDesc::transient().with_state_insert(values[6], second_bit, 1);

        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::Load {
            dst: values[0],
            base: BaseReg::StackFrame,
            offset: 0,
            size: OpSize::S64,
        });
        block.push(MInst::AndImm {
            dst: values[1],
            src: values[0],
            imm: 1,
        });
        block.push(MInst::Load {
            dst: values[2],
            base: BaseReg::SimState,
            offset: 40,
            size: OpSize::S8,
        });
        block.push(MInst::AndImm {
            dst: values[3],
            src: values[2],
            imm: !1,
        });
        block.push(MInst::Or {
            dst: values[4],
            lhs: values[3],
            rhs: values[1],
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 40,
            src: values[4],
            size: OpSize::S8,
        });
        block.push(MInst::Load {
            dst: values[5],
            base: BaseReg::StackFrame,
            offset: 8,
            size: OpSize::S64,
        });
        block.push(MInst::AndImm {
            dst: values[6],
            src: values[5],
            imm: 1,
        });
        block.push(MInst::ShlImm {
            dst: values[7],
            src: values[6],
            imm: second_bit as u8,
        });
        block.push(MInst::Load {
            dst: values[8],
            base: BaseReg::SimState,
            offset: 40,
            size: OpSize::S8,
        });
        block.push(MInst::AndImm {
            dst: values[9],
            src: values[8],
            imm: !(1u64 << second_bit),
        });
        block.push(MInst::Or {
            dst: values[10],
            lhs: values[9],
            rhs: values[7],
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 40,
            src: values[10],
            size: OpSize::S8,
        });
        block.push(MInst::Mov {
            dst: values[11],
            src: values[1],
        });
        block.push(MInst::Return);
        func.push_block(block);
        let point = PointUse {
            block: BlockId(0),
            instruction: 13,
            value: values[1],
        };
        (func, values[1], point)
    }

    #[test]
    fn disjoint_partial_rmw_preserves_an_existing_bit_home() {
        let (func, value, point) = two_partial_rmw_stores(1);
        let (_, _, analysis) = analyze_function(func);

        let recipe = analysis.resolved_recipe_at_point(point).unwrap();
        assert!(analysis.point_recipe_uses_store_home(point));
        assert!(matches!(
            &recipe.base,
            ResolvedBase::State(StateRecipe {
                load: StateLoad {
                    offset: 40,
                    size: OpSize::S8,
                },
                observed_bits: StateBitRange {
                    start: 320,
                    end: 321,
                },
                ..
            })
        ));
        assert_eq!(recipe.steps, vec![PureStep::AndImm32 { immediate: 1 }]);
        assert_eq!(point.value, value);
    }

    #[test]
    fn overlapping_partial_rmw_invalidates_an_existing_bit_home() {
        let (func, _, point) = two_partial_rmw_stores(0);
        let (_, _, analysis) = analyze_function(func);

        assert!(analysis.resolved_recipe_at_point(point).is_none());
    }

    #[test]
    fn register_phi_with_different_state_slots_keeps_stack_fallback() {
        let (func, phi) = phi_of_committed_values(48);
        let (_, _, analysis) = analyze_function(func);

        assert!(
            analysis
                .resolved_recipe_at_point(PointUse {
                    block: BlockId(3),
                    instruction: 0,
                    value: phi,
                })
                .is_none()
        );
    }

    #[test]
    fn requested_terminator_point_observes_a_post_store_home() {
        let (mut func, values) = function_with_values(1);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::Load {
            dst: values[0],
            base: BaseReg::StackFrame,
            offset: 0,
            size: OpSize::S64,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 40,
            src: values[0],
            size: OpSize::S64,
        });
        block.push(MInst::Return);
        func.push_block(block);
        let cfg = super::super::cfg::normalize(&mut func).unwrap();
        let query = PointUse {
            block: BlockId(0),
            instruction: 2,
            value: values[0],
        };
        let analysis = analyze_with_queries(&func, &cfg, &BTreeSet::from([query])).unwrap();

        let recipe = analysis.resolved_recipe_at_point(query).unwrap();
        assert!(analysis.point_recipe_uses_store_home(query));
        assert!(matches!(
            &recipe.base,
            ResolvedBase::State(StateRecipe {
                load: StateLoad {
                    offset: 40,
                    size: OpSize::S64,
                },
                ..
            })
        ));
        assert!(recipe.steps.is_empty());
    }

    #[test]
    fn sparse_memory_ssa_matches_full_analysis_at_requested_points() {
        let (mut func, values) = function_with_values(5);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::Load {
            dst: values[0],
            base: BaseReg::SimState,
            offset: 8,
            size: OpSize::S64,
        });
        block.push(MInst::Load {
            dst: values[1],
            base: BaseReg::SimState,
            offset: 40,
            size: OpSize::S64,
        });
        block.push(MInst::LoadImm {
            dst: values[2],
            value: 9,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 8,
            src: values[2],
            size: OpSize::S64,
        });
        block.push(MInst::Mov {
            dst: values[3],
            src: values[1],
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 44,
            src: values[2],
            size: OpSize::S8,
        });
        block.push(MInst::Mov {
            dst: values[4],
            src: values[1],
        });
        block.push(MInst::Return);
        func.push_block(block);
        let cfg = super::super::cfg::normalize(&mut func).unwrap();
        let before_overlap = PointUse {
            block: BlockId(0),
            instruction: 4,
            value: values[1],
        };
        let after_overlap = PointUse {
            block: BlockId(0),
            instruction: 6,
            value: values[1],
        };
        let requested = BTreeSet::from([before_overlap, after_overlap]);

        let full = analyze(&func, &cfg).unwrap();
        let sparse = analyze_with_queries(&func, &cfg, &requested).unwrap();

        assert_eq!(full.recipe(values[1]), sparse.recipe(values[1]));
        for point in requested {
            assert_eq!(
                full.resolved_recipe_at_point(point),
                sparse.resolved_recipe_at_point(point),
                "sparse MemorySSA changed the selected reload recipe at {point:?}"
            );
            assert_eq!(
                full.state_valid_at_point(point),
                sparse.resolved_recipe_at_point(point).is_some(),
                "sparse MemorySSA changed reload validity at {point:?}"
            );
        }
        assert!(sparse.resolved_recipe_at_point(before_overlap).is_some());
        assert!(sparse.resolved_recipe_at_point(after_overlap).is_none());
    }

    #[test]
    fn narrow_store_is_not_an_unproved_full_register_home() {
        let (mut func, values) = function_with_values(2);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::Load {
            dst: values[0],
            base: BaseReg::StackFrame,
            offset: 0,
            size: OpSize::S64,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 40,
            src: values[0],
            size: OpSize::S32,
        });
        block.push(MInst::Mov {
            dst: values[1],
            src: values[0],
        });
        block.push(MInst::Return);
        func.push_block(block);
        let (_, _, analysis) = analyze_function(func);

        assert!(
            analysis
                .resolved_recipe_at_point(PointUse {
                    block: BlockId(0),
                    instruction: 2,
                    value: values[0],
                })
                .is_none()
        );
    }

    #[test]
    fn overlapping_write_kills_a_store_backed_home() {
        let (mut func, values) = function_with_values(3);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::Load {
            dst: values[0],
            base: BaseReg::StackFrame,
            offset: 0,
            size: OpSize::S64,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 40,
            src: values[0],
            size: OpSize::S64,
        });
        block.push(MInst::LoadImm {
            dst: values[1],
            value: 7,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 43,
            src: values[1],
            size: OpSize::S8,
        });
        block.push(MInst::Mov {
            dst: values[2],
            src: values[0],
        });
        block.push(MInst::Return);
        func.push_block(block);
        let (_, _, analysis) = analyze_function(func);

        assert!(
            analysis
                .resolved_recipe_at_point(PointUse {
                    block: BlockId(0),
                    instruction: 4,
                    value: values[0],
                })
                .is_none()
        );
    }

    #[test]
    fn overlapping_partial_store_invalidates_recipe() {
        let (mut func, values) = function_with_values(3);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::Load {
            dst: values[0],
            base: BaseReg::SimState,
            offset: 16,
            size: OpSize::S64,
        });
        block.push(MInst::LoadImm {
            dst: values[1],
            value: 0xaa,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 19,
            src: values[1],
            size: OpSize::S8,
        });
        block.push(MInst::Mov {
            dst: values[2],
            src: values[0],
        });
        block.push(MInst::Return);
        func.push_block(block);
        let (_, _, analysis) = analyze_function(func);

        assert!(!analysis.state_valid_at_point(PointUse {
            block: BlockId(0),
            instruction: 3,
            value: values[0],
        }));
    }

    #[test]
    fn disjoint_and_stack_stores_preserve_recipe() {
        let (mut func, values) = function_with_values(3);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::Load {
            dst: values[0],
            base: BaseReg::SimState,
            offset: 16,
            size: OpSize::S32,
        });
        block.push(MInst::LoadImm {
            dst: values[1],
            value: 7,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 24,
            src: values[1],
            size: OpSize::S64,
        });
        block.push(MInst::Store {
            base: BaseReg::StackFrame,
            offset: 0,
            src: values[1],
            size: OpSize::S64,
        });
        block.push(MInst::Mov {
            dst: values[2],
            src: values[0],
        });
        block.push(MInst::Return);
        func.push_block(block);
        let (_, _, analysis) = analyze_function(func);

        assert!(analysis.state_valid_at_point(PointUse {
            block: BlockId(0),
            instruction: 4,
            value: values[0],
        }));
    }

    #[test]
    fn indirect_runtime_store_preserves_direct_state_recipe() {
        let (mut func, values) = function_with_values(4);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::Load {
            dst: values[0],
            base: BaseReg::SimState,
            offset: 16,
            size: OpSize::S32,
        });
        block.push(MInst::LoadImm {
            dst: values[1],
            value: 0,
        });
        block.push(MInst::LoadImm {
            dst: values[2],
            value: 7,
        });
        block.push(MInst::StorePtr {
            ptr: values[1],
            offset: 0,
            src: values[2],
            size: OpSize::S64,
        });
        block.push(MInst::Mov {
            dst: values[3],
            src: values[0],
        });
        block.push(MInst::Return);
        func.push_block(block);
        let (_, _, analysis) = analyze_function(func);

        assert!(analysis.state_valid_at_point(PointUse {
            block: BlockId(0),
            instruction: 4,
            value: values[0],
        }));
    }

    #[test]
    fn indexed_state_store_kills_every_state_recipe() {
        let (mut func, values) = function_with_values(4);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::Load {
            dst: values[0],
            base: BaseReg::SimState,
            offset: 128,
            size: OpSize::S64,
        });
        block.push(MInst::LoadImm {
            dst: values[1],
            value: 0,
        });
        block.push(MInst::LoadImm {
            dst: values[2],
            value: 1,
        });
        block.push(MInst::StoreIndexed {
            base: BaseReg::SimState,
            offset: 0,
            index: values[1],
            src: values[2],
            size: OpSize::S8,
            alias_range: None,
        });
        block.push(MInst::Mov {
            dst: values[3],
            src: values[0],
        });
        block.push(MInst::Return);
        func.push_block(block);
        let (_, _, analysis) = analyze_function(func);

        assert!(!analysis.state_valid_at_point(PointUse {
            block: BlockId(0),
            instruction: 4,
            value: values[0],
        }));
    }

    #[test]
    fn bounded_indexed_store_preserves_only_nonoverlapping_state_recipes() {
        fn fixture(load_offset: i32) -> (VReg, ReloadRecipeAnalysis) {
            let (mut func, values) = function_with_values(4);
            let mut block = MBlock::new(BlockId(0));
            block.push(MInst::Load {
                dst: values[0],
                base: BaseReg::SimState,
                offset: load_offset,
                size: OpSize::S64,
            });
            block.push(MInst::LoadImm {
                dst: values[1],
                value: 0,
            });
            block.push(MInst::LoadImm {
                dst: values[2],
                value: 1,
            });
            block.push(MInst::StoreIndexed {
                base: BaseReg::SimState,
                offset: 16,
                index: values[1],
                src: values[2],
                size: OpSize::S8,
                alias_range: MemoryAliasRange::new(16, 64),
            });
            block.push(MInst::Mov {
                dst: values[3],
                src: values[0],
            });
            block.push(MInst::Return);
            func.push_block(block);
            let (_, _, analysis) = analyze_function(func);
            (values[0], analysis)
        }

        let (overlapping, overlapping_analysis) = fixture(32);
        assert!(!overlapping_analysis.state_valid_at_point(PointUse {
            block: BlockId(0),
            instruction: 4,
            value: overlapping,
        }));

        let (disjoint, disjoint_analysis) = fixture(128);
        assert!(disjoint_analysis.state_valid_at_point(PointUse {
            block: BlockId(0),
            instruction: 4,
            value: disjoint,
        }));
    }

    fn diamond(overlap_left: bool) -> (MFunction, VReg, VReg) {
        let (mut func, values) = function_with_values(4);
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: values[0],
            value: 1,
        });
        entry.push(MInst::Load {
            dst: values[1],
            base: BaseReg::SimState,
            offset: 64,
            size: OpSize::S64,
        });
        entry.push(MInst::LoadImm {
            dst: values[2],
            value: 9,
        });
        entry.push(MInst::Branch {
            cond: values[0],
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        });
        let mut left = MBlock::new(BlockId(1));
        left.push(MInst::Store {
            base: BaseReg::SimState,
            offset: if overlap_left { 68 } else { 80 },
            src: values[2],
            size: OpSize::S32,
        });
        left.push(MInst::Jump { target: BlockId(3) });
        let mut right = MBlock::new(BlockId(2));
        right.push(MInst::Jump { target: BlockId(3) });
        let mut join = MBlock::new(BlockId(3));
        join.push(MInst::Mov {
            dst: values[3],
            src: values[1],
        });
        join.push(MInst::Return);
        func.blocks = vec![entry, left, right, join];
        (func, values[1], values[3])
    }

    #[test]
    fn write_on_one_diamond_arm_invalidates_join_recipe() {
        let (func, loaded, _) = diamond(true);
        let (_, _, analysis) = analyze_function(func);
        assert!(!analysis.state_valid_at_point(PointUse {
            block: BlockId(3),
            instruction: 0,
            value: loaded,
        }));
    }

    #[test]
    fn disjoint_write_on_one_diamond_arm_preserves_join_recipe() {
        let (func, loaded, _) = diamond(false);
        let (_, _, analysis) = analyze_function(func);
        assert!(analysis.state_valid_at_point(PointUse {
            block: BlockId(3),
            instruction: 0,
            value: loaded,
        }));
    }

    #[test]
    fn loop_write_invalidates_header_use_on_later_iterations() {
        let (mut func, values) = function_with_values(5);
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::Load {
            dst: values[0],
            base: BaseReg::SimState,
            offset: 0,
            size: OpSize::S64,
        });
        entry.push(MInst::LoadImm {
            dst: values[1],
            value: 1,
        });
        entry.push(MInst::LoadImm {
            dst: values[2],
            value: 2,
        });
        entry.push(MInst::Jump { target: BlockId(1) });
        let mut header = MBlock::new(BlockId(1));
        header.push(MInst::Mov {
            dst: values[3],
            src: values[0],
        });
        header.push(MInst::Branch {
            cond: values[1],
            true_bb: BlockId(2),
            false_bb: BlockId(3),
        });
        let mut body = MBlock::new(BlockId(2));
        body.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 0,
            src: values[2],
            size: OpSize::S64,
        });
        body.push(MInst::Jump { target: BlockId(1) });
        let mut exit = MBlock::new(BlockId(3));
        exit.push(MInst::Mov {
            dst: values[4],
            src: values[3],
        });
        exit.push(MInst::Return);
        func.blocks = vec![entry, header, body, exit];
        let (_, _, analysis) = analyze_function(func);

        assert!(!analysis.state_valid_at_point(PointUse {
            block: BlockId(1),
            instruction: 0,
            value: values[0],
        }));
    }

    #[test]
    fn phi_edge_use_observes_predecessor_memory_version() {
        let (mut func, values) = function_with_values(4);
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::Load {
            dst: values[0],
            base: BaseReg::SimState,
            offset: 32,
            size: OpSize::S32,
        });
        entry.push(MInst::LoadImm {
            dst: values[1],
            value: 1,
        });
        entry.push(MInst::Branch {
            cond: values[1],
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        });
        let mut left = MBlock::new(BlockId(1));
        left.push(MInst::Jump { target: BlockId(3) });
        let mut right = MBlock::new(BlockId(2));
        right.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 32,
            src: values[1],
            size: OpSize::S8,
        });
        right.push(MInst::Jump { target: BlockId(3) });
        let mut join = MBlock::new(BlockId(3));
        join.phis.push(PhiNode {
            dst: values[2],
            sources: vec![(BlockId(1), values[0]), (BlockId(2), values[0])],
        });
        join.push(MInst::Mov {
            dst: values[3],
            src: values[2],
        });
        join.push(MInst::Return);
        func.blocks = vec![entry, left, right, join];
        let (_, _, analysis) = analyze_function(func);

        assert!(analysis.state_valid_on_edge(EdgeUse {
            predecessor: BlockId(1),
            successor: BlockId(3),
            value: values[0],
        }));
        assert!(!analysis.state_valid_on_edge(EdgeUse {
            predecessor: BlockId(2),
            successor: BlockId(3),
            value: values[0],
        }));
    }

    #[test]
    fn memcopy_destination_invalidates_only_overlapping_recipe() {
        let (mut func, values) = function_with_values(3);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::Load {
            dst: values[0],
            base: BaseReg::SimState,
            offset: 100,
            size: OpSize::S16,
        });
        block.push(MInst::MemCopy {
            src_offset: 0,
            dst_offset: 101,
            byte_len: 4,
        });
        block.push(MInst::Mov {
            dst: values[1],
            src: values[0],
        });
        block.push(MInst::LoadImm {
            dst: values[2],
            value: 0,
        });
        block.push(MInst::Return);
        func.push_block(block);
        let (_, _, analysis) = analyze_function(func);
        assert!(!analysis.state_valid_at_point(PointUse {
            block: BlockId(0),
            instruction: 2,
            value: values[0],
        }));
    }

    #[test]
    fn independent_verifier_accepts_same_version_reload() {
        let (mut func, values) = function_with_values(3);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::Load {
            dst: values[0],
            base: BaseReg::SimState,
            offset: 40,
            size: OpSize::S64,
        });
        block.push(MInst::Load {
            dst: values[1],
            base: BaseReg::SimState,
            offset: 40,
            size: OpSize::S64,
        });
        block.push(MInst::Mov {
            dst: values[2],
            src: values[1],
        });
        block.push(MInst::Return);
        func.push_block(block);
        let (func, cfg, analysis) = analyze_function(func);
        let expected = analysis.resolved_recipe(values[0]).unwrap().unwrap();

        verify_expected_materialized_reloads(
            &func,
            &cfg,
            &[ExpectedMaterializedReload {
                reload: values[1],
                expected,
            }],
        )
        .unwrap();
    }

    #[test]
    fn independent_verifier_rejects_stale_reload() {
        let (mut func, values) = function_with_values(3);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::Load {
            dst: values[0],
            base: BaseReg::SimState,
            offset: 40,
            size: OpSize::S64,
        });
        block.push(MInst::LoadImm {
            dst: values[1],
            value: 9,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 44,
            src: values[1],
            size: OpSize::S8,
        });
        block.push(MInst::Load {
            dst: values[2],
            base: BaseReg::SimState,
            offset: 40,
            size: OpSize::S64,
        });
        block.push(MInst::Return);
        func.push_block(block);
        let (func, cfg, analysis) = analyze_function(func);
        let expected = analysis.resolved_recipe(values[0]).unwrap().unwrap();

        let error = verify_expected_materialized_reloads(
            &func,
            &cfg,
            &[ExpectedMaterializedReload {
                reload: values[2],
                expected,
            }],
        )
        .unwrap_err();
        assert_eq!(error.rule, "RELOAD_RECIPE.STATE_VERSION_CURRENT");
    }

    #[test]
    fn independent_verifier_rejects_changed_machine_width() {
        let (mut func, values) = function_with_values(2);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::Load {
            dst: values[0],
            base: BaseReg::SimState,
            offset: 40,
            size: OpSize::S64,
        });
        block.push(MInst::Load {
            dst: values[1],
            base: BaseReg::SimState,
            offset: 40,
            size: OpSize::S32,
        });
        block.push(MInst::Return);
        func.push_block(block);
        let (func, cfg, analysis) = analyze_function(func);
        let expected = analysis.resolved_recipe(values[0]).unwrap().unwrap();

        let error = verify_expected_materialized_reloads(
            &func,
            &cfg,
            &[ExpectedMaterializedReload {
                reload: values[1],
                expected,
            }],
        )
        .unwrap_err();
        assert_eq!(error.rule, "RELOAD_RECIPE.PHYSICAL_SHAPE_MATCHES");
    }
}
