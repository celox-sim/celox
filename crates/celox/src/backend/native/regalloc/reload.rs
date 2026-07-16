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
        ordinal: usize,
        variable: MemoryVariable,
    },
    Phi {
        block: usize,
        variable: MemoryVariable,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StateVersion {
    unknown_alias: MemoryVersion,
    bytes: Box<[MemoryVersion]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StateRecipe {
    pub load: StateLoad,
    version: StateVersion,
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

/// Recipe shape used by spill planning before concrete reload points exist.
/// It deliberately contains no MemorySSA versions: exact validity is proved
/// only for reloads that the planner actually selects.
#[derive(Debug)]
pub(super) struct PlanningRecipes {
    recipes: Vec<PlanningRecipe>,
    pure_recipes: Vec<PureRecipe>,
}

impl PlanningRecipes {
    pub fn global_materialization_costs(&self) -> Result<Vec<Option<u16>>, ReloadRecipeError> {
        let mut costs = vec![None::<Option<u16>>; self.recipes.len()];
        for start in 0..self.recipes.len() {
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
                match self.recipes[current] {
                    PlanningRecipe::Constant | PlanningRecipe::State => {
                        costs[current] = Some(Some(1));
                        break Some(1);
                    }
                    PlanningRecipe::Stack => {
                        costs[current] = Some(None);
                        break None;
                    }
                    PlanningRecipe::Pure { expression } => {
                        let Some(recipe) = self.pure_recipes.get(expression.0 as usize) else {
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
                        if current >= self.recipes.len() {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ResolvedBase {
    Constant(u64),
    State(StateRecipe),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ResolvedRecipe {
    pub base: ResolvedBase,
    pub steps: Vec<PureStep>,
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

pub(super) fn analyze_for_planning(func: &MFunction) -> Result<PlanningRecipes, ReloadRecipeError> {
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
    Ok(PlanningRecipes {
        recipes,
        pure_recipes,
    })
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
    let mut exact_store_homes = HashMap::new();
    for (block, mir_block) in func.blocks.iter().enumerate() {
        for (instruction, inst) in mir_block.insts.iter().enumerate() {
            if let Some(home) = exact_store_home(inst, &canonical_bits) {
                exact_store_homes.insert((block, instruction), home);
            }
        }
    }
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
    exact_store_homes.retain(|_, (value, _)| relevant_values.contains(value));
    let mut tracked_bytes = BTreeSet::<i64>::new();
    for &value in &relevant_values {
        if let Some(load) = state_loads.get(value.0 as usize).copied().flatten() {
            tracked_bytes.extend(load.bytes().expect("state-load range was validated"));
        }
    }
    for &(value, load) in exact_store_homes.values() {
        let Some(bytes) = load.bytes() else {
            return Err(ReloadRecipeError::new(
                "RELOAD_RECIPE.STATE_RANGE",
                None,
                None,
                Some(value),
                "exact store-home byte range overflows i64",
            ));
        };
        tracked_bytes.extend(bytes);
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
        &exact_store_homes,
        &relevant_values,
        requested_points,
        collect_all_uses,
        &mut point_recipes,
        &mut edge_recipes,
        &mut valid_point_uses,
        &mut valid_edge_uses,
    )?;
    verify_memory_phis(func, cfg, &memory_ssa)?;

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

fn exact_store_home(inst: &MInst, canonical_bits: &[u8]) -> Option<(VReg, StateLoad)> {
    let MInst::Store {
        base: BaseReg::SimState,
        offset,
        src,
        size,
    } = inst
    else {
        return None;
    };
    let stored_bits = (size.bytes() * 8) as u8;
    if canonical_bits.get(src.0 as usize).copied()? > stored_bits {
        return None;
    }
    Some((
        *src,
        StateLoad {
            offset: *offset,
            size: *size,
        },
    ))
}

/// Prove how many low bits may be nonzero from MIR semantics alone.
///
/// This is a local analysis side table, not a VReg type: machine registers
/// remain 64-bit values and no HDL width is attached to them.  A narrow store
/// is a reload home only when its source is already exactly the zero-extended
/// value produced by a load of the same machine width.
fn canonical_value_bits(func: &MFunction) -> Result<Vec<u8>, ReloadRecipeError> {
    let mut bits = vec![64u8; func.vregs.count() as usize];
    let get = |value: VReg, bits: &[u8], block: BlockId, instruction: Option<usize>| {
        bits.get(value.0 as usize).copied().ok_or_else(|| {
            ReloadRecipeError::new(
                "RELOAD_RECIPE.VALUE_RANGE",
                Some(block),
                instruction,
                Some(value),
                "MIR operand is outside the canonical-value side table",
            )
        })
    };
    for block in &func.blocks {
        for phi in &block.phis {
            let mut width = 0u8;
            for &(_, source) in &phi.sources {
                width = width.max(get(source, &bits, block.id, None)?);
            }
            let Some(destination) = bits.get_mut(phi.dst.0 as usize) else {
                return Err(ReloadRecipeError::new(
                    "RELOAD_RECIPE.VALUE_RANGE",
                    Some(block.id),
                    None,
                    Some(phi.dst),
                    "MIR phi destination is outside the canonical-value side table",
                ));
            };
            *destination = width;
        }
        for (instruction, inst) in block.insts.iter().enumerate() {
            let Some(destination) = inst.def() else {
                continue;
            };
            let operand = |value| get(value, &bits, block.id, Some(instruction));
            let width = match inst {
                MInst::Mov { src, .. } => operand(*src)?,
                MInst::Mov32 { src, .. } => operand(*src)?.min(32),
                MInst::LoadImm { value, .. } => significant_bits(*value),
                MInst::Load { size, .. }
                | MInst::LoadPtr { size, .. }
                | MInst::LoadIndexed { size, .. }
                | MInst::LoadPtrIndexed { size, .. } => (size.bytes() * 8) as u8,
                MInst::Add32 { .. } | MInst::Sub32 { .. } | MInst::Mul32 { .. } => 32,
                MInst::And { lhs, rhs, .. } => operand(*lhs)?.min(operand(*rhs)?),
                MInst::And32 { lhs, rhs, .. } => operand(*lhs)?.min(operand(*rhs)?).min(32),
                MInst::Or { lhs, rhs, .. } | MInst::Xor { lhs, rhs, .. } => {
                    operand(*lhs)?.max(operand(*rhs)?)
                }
                MInst::Or32 { lhs, rhs, .. } | MInst::Xor32 { lhs, rhs, .. } => {
                    operand(*lhs)?.max(operand(*rhs)?).min(32)
                }
                MInst::AndImm { src, imm, .. } => operand(*src)?.min(significant_bits(*imm)),
                MInst::AndImm32 { src, imm, .. } => operand(*src)?
                    .min(significant_bits(u64::from(*imm)))
                    .min(32),
                MInst::OrImm { src, imm, .. } => operand(*src)?.max(significant_bits(*imm)),
                MInst::ShrImm { src, imm, .. } => operand(*src)?.saturating_sub(*imm),
                MInst::ShlImm { src, imm, .. } => operand(*src)?.saturating_add(*imm).min(64),
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
                } => operand(*true_val)?.max(operand(*false_val)?),
                _ => 64,
            };
            let Some(slot) = bits.get_mut(destination.0 as usize) else {
                return Err(ReloadRecipeError::new(
                    "RELOAD_RECIPE.VALUE_RANGE",
                    Some(block.id),
                    Some(instruction),
                    Some(destination),
                    "MIR definition is outside the canonical-value side table",
                ));
            };
            *slot = width;
        }
    }
    Ok(bits)
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
    let mut write_ordinal = 0usize;
    for (block, mir_block) in func.blocks.iter().enumerate() {
        for (instruction, inst) in mir_block.insts.iter().enumerate() {
            let effect = memory_effect::writes(inst);
            let affects_sim_state = effect
                .unknown_base()
                .is_some_and(|base| base.is_none_or(|base| base == BaseReg::SimState))
                || effect.ranges().any(|range| range.base == BaseReg::SimState);
            let ordinal = if affects_sim_state {
                let ordinal = write_ordinal;
                write_ordinal = write_ordinal.checked_add(1).ok_or_else(|| {
                    ReloadRecipeError::new(
                        "RELOAD_RECIPE.WRITE_ORDINAL_RANGE",
                        Some(mir_block.id),
                        Some(instruction),
                        None,
                        "MemorySSA write ordinal exceeds addressable MIR size",
                    )
                })?;
                Some(ordinal)
            } else {
                None
            };
            let affected = affected_variables(inst, tracked_bytes)?;
            for variable in affected {
                definition_blocks.entry(variable).or_default().insert(block);
                write_versions.insert(
                    (block, instruction, variable),
                    MemoryVersion::Write {
                        ordinal: ordinal.expect("affected variables belong to a memory write"),
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
                        block: frontier,
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
    if effect
        .unknown_base()
        .is_some_and(|base| base.is_none_or(|base| base == BaseReg::SimState))
    {
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
    exact_store_homes: &HashMap<(usize, usize), (VReg, StateLoad)>,
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
    let mut current_homes = BTreeMap::<VReg, Vec<StateRecipe>>::new();
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
                    homes.pop();
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
        // exact SimState slot has the MemorySSA phi for that slot as a home.
        // Derive this only after every forward predecessor edge has supplied
        // an independently validated recipe.  Missing backedge facts keep a
        // loop phi on its stack fallback; they are never guessed.
        for phi in &func.blocks[block].phis {
            if !relevant_values.contains(&phi.dst) {
                continue;
            }
            let mut common_load = None::<StateLoad>;
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
                // Method-I CSSA gives each phi operand an edge-local Mov.
                // A 64-bit copy is the identity in MIR, so any chain made
                // solely from Copy64 still denotes the exact stored value.
                // Width-changing Copy32 and all arithmetic steps must remain
                // explicit and therefore cannot define a direct phi home.
                if !steps.iter().all(|step| matches!(step, PureStep::Copy64)) {
                    complete = false;
                    break;
                }
                match common_load {
                    Some(load) if load != state.load => {
                        complete = false;
                        break;
                    }
                    Some(_) => {}
                    None => common_load = Some(state.load),
                }
            }
            if complete {
                let Some(load) = common_load else {
                    continue;
                };
                let Some(recipe) = recipes.get_mut(phi.dst.0 as usize) else {
                    return Err(ReloadRecipeError::new(
                        "RELOAD_RECIPE.VALUE_RANGE",
                        Some(block_id),
                        None,
                        Some(phi.dst),
                        "MIR phi destination is outside the VReg recipe side table",
                    ));
                };
                *recipe = ReloadRecipe::StateVersion(StateRecipe {
                    load,
                    version: current_state_version(load, &current, memory_ssa)?,
                });
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
                });
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

            if let Some(&(value, load)) = exact_store_homes.get(&(block, instruction)) {
                let recipe = StateRecipe {
                    load,
                    version: current_state_version(load, &current, memory_ssa)?,
                };
                current_homes.entry(value).or_default().push(recipe);
                home_pushes.push(value);
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
                    "an overlapping or unknown state write makes a pure recipe's state base stale",
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
    current_homes: &BTreeMap<VReg, Vec<StateRecipe>>,
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
    current_homes: &BTreeMap<VReg, Vec<StateRecipe>>,
    current: &BTreeMap<MemoryVariable, MemoryVersion>,
    memory_ssa: &MemorySsa,
) -> Result<Option<ResolvedRecipe>, ReloadRecipeError> {
    let Some(homes) = current_homes.get(&value) else {
        return Ok(None);
    };
    for home in homes.iter().rev() {
        if current_state_version(home.load, current, memory_ssa)? == home.version {
            let mut steps = reverse_steps.to_vec();
            steps.reverse();
            return Ok(Some(ResolvedRecipe {
                base: ResolvedBase::State(home.clone()),
                steps,
            }));
        }
    }
    Ok(None)
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

    fn function_with_values(count: usize) -> (MFunction, Vec<VReg>) {
        let mut vregs = VRegAllocator::new();
        let values = (0..count).map(|_| vregs.alloc()).collect::<Vec<_>>();
        (
            MFunction::new(vregs, vec![SpillDesc::transient(); count]),
            values,
        )
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
        let (func, _, analysis) = analyze_function(func);

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
            analyze_for_planning(&func)
                .unwrap()
                .global_materialization_costs()
                .unwrap(),
            vec![Some(1), Some(2)]
        );
    }

    #[test]
    fn sparse_mark_preserves_only_nonoverlapping_state_recipes() {
        fn fixture(load_offset: i32) -> (VReg, ReloadRecipeAnalysis) {
            let (mut func, values) = function_with_values(2);
            let mut block = MBlock::new(BlockId(0));
            block.push(MInst::Load {
                dst: values[0],
                base: BaseReg::SimState,
                offset: load_offset,
                size: OpSize::S64,
            });
            block.push(MInst::SparseMarkActive {
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
            instruction: 2,
            value: unrelated,
        }));

        let (metadata, metadata_analysis) = fixture(100);
        assert!(!metadata_analysis.state_valid_at_point(PointUse {
            block: BlockId(0),
            instruction: 2,
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
