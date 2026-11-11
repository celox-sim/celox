//! Reload recipes whose validity is proved against physical MIR memory effects.
//!
//! A simulation-state recipe is deliberately expressed as the exact MIR load
//! that produced the value. It never reconstructs an address or an operand
//! width from SIR metadata. Its `MemorySnapshot` is a query-specific view of
//! the shared access-based MemorySSA graph. A recipe is usable only where the
//! same clobber graph reaches that exact physical load.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use celox_analysis::memory::{MemoryEffect, effects_may_alias};
use celox_analysis::memory_versions::{
    self, MemoryVersion, MemoryVersionEvent, MemoryVersions, RangeWrite, VersionId, VersionSegment,
};

use crate::native::memory_effect::{self, MemoryObject, analysis_effects};
use crate::native::mir::{BaseReg, BlockId, CmpKind, MFunction, MInst, OpSize, VReg};
use crate::{HashMap, HashSet};

use super::cfg::NormalizedCfg;

mod available;
mod memory_ssa;
mod recipe;
mod recipe_analysis;
mod snapshot;
mod verify;
use available::*;
use memory_ssa::*;
pub(super) use recipe::*;
pub(super) use recipe_analysis::*;
pub(super) use snapshot::*;
pub(super) use verify::*;

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
    point_fragment_recipes: BTreeMap<PointUse, CompositeStateRecipe>,
    edge_fragment_recipes: BTreeMap<EdgeUse, CompositeStateRecipe>,
    valid_point_uses: BTreeSet<PointUse>,
    valid_edge_uses: BTreeSet<EdgeUse>,
    collect_all_uses: bool,
    collect_fragment_homes: bool,
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
            self.collect_fragment_homes,
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
        if self.point_fragment_recipes != rebuilt.point_fragment_recipes {
            return Err(ReloadRecipeError::new(
                "RELOAD_RECIPE.POINT_FRAGMENTS_MATCH_MEMORY_SSA",
                None,
                None,
                None,
                "cached point fragment recipes differ from independently rebuilt MemorySSA",
            ));
        }
        if self.edge_fragment_recipes != rebuilt.edge_fragment_recipes {
            return Err(ReloadRecipeError::new(
                "RELOAD_RECIPE.EDGE_FRAGMENTS_MATCH_MEMORY_SSA",
                None,
                None,
                None,
                "cached edge fragment recipes differ from independently rebuilt MemorySSA",
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
        let exact = analyze_unverified_with_queries(func, cfg, &requested_points, false, false)?;
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
    let analysis = analyze_unverified_with_queries(func, cfg, &BTreeSet::new(), true, false)?;
    analysis.verify(func, cfg)?;
    Ok(analysis)
}

pub(super) fn analyze_with_queries(
    func: &MFunction,
    cfg: &NormalizedCfg,
    requested_points: &BTreeSet<PointUse>,
) -> Result<ReloadRecipeAnalysis, ReloadRecipeError> {
    analyze_with_queries_checked(func, cfg, requested_points, true)
}

/// [`analyze_with_queries`], rebuilding every fact a second time to check
/// the analysis against itself only when `verify` is set: the check costs as
/// much as the analysis.
pub(super) fn analyze_with_queries_checked(
    func: &MFunction,
    cfg: &NormalizedCfg,
    requested_points: &BTreeSet<PointUse>,
    verify: bool,
) -> Result<ReloadRecipeAnalysis, ReloadRecipeError> {
    let analysis = analyze_unverified_with_queries(func, cfg, requested_points, false, false)?;
    if verify {
        analysis.verify(func, cfg)?;
    }
    Ok(analysis)
}

#[cfg(test)]
mod tests;
