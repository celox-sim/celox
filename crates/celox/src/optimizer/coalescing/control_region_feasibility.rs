//! Analysis-only reverse-if-conversion gate for profile-selected SIR blocks.
//!
//! HDL case statements can reach fused SIR as one large superblock: every
//! selector arm is evaluated, then Muxes select the observable values. This
//! probe specializes such a block for each exact selector value and computes
//! the closed backward slice which would remain. It does not rewrite SIR.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
#[cfg(test)]
use std::fmt;

use num_bigint::BigUint;
use num_traits::Zero;

use super::cost_model::estimate_clif_cost;
use super::shared::def_reg;
use super::sir_analysis::{UseSite, collect_uses, instruction_uses};
use super::state_ssa::StateSsa;
use crate::ir::cfg::SirCfg;
use crate::ir::{
    BinaryOp, BlockId, ExecutionUnit, RegionedAbsoluteAddr, RegisterId, SIRInstruction, SIROffset,
    SIRTerminator, SIRValue, UnaryOp,
};
use crate::{HashMap, HashSet};

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SelectorRegionFact {
    pub block: BlockId,
    pub samples: u64,
    pub selector: RegisterId,
    pub explicit_cases: usize,
    pub block_instructions: usize,
    pub baseline_cost: usize,
    pub worst_case_cost: usize,
    pub mean_case_cost: usize,
    pub minimum_skipped_instructions: usize,
    pub maximum_skipped_instructions: usize,
    pub live_outputs: usize,
    pub effects: usize,
    pub cross_block_affected_instructions: usize,
    pub cross_block_affected_blocks: usize,
    pub cross_block_effect_sinks: usize,
    pub cross_block_branch_conditions: usize,
    pub cross_block_effect_sites: Vec<(BlockId, usize)>,
    pub sink_recipes: Vec<SelectorSinkRecipeFact>,
    pub sink_recipe_pairs: Vec<SelectorSinkRecipePairFact>,
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SelectorSinkRecipeFact {
    pub sink: (BlockId, usize),
    pub effect: String,
    pub publication: String,
    pub source: RegisterId,
    pub recipe_instructions: Vec<(BlockId, usize)>,
    pub recipe_blocks: Vec<BlockId>,
    pub selector_control_blocks: Vec<BlockId>,
    pub entering_edges: Vec<(BlockId, BlockId)>,
    pub continuations: Vec<BlockId>,
    pub constant_frontier: Vec<RegisterId>,
    pub load_frontier: Vec<RegisterId>,
    pub shared_ssa_frontier: Vec<RegisterId>,
    pub external_frontier: Vec<RegisterId>,
    pub control_merges: Vec<RegisterId>,
    pub loop_cutoffs: Vec<RegisterId>,
    pub case_summary: SelectorSinkCaseSummaryFact,
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct SelectorSinkCaseSummaryFact {
    pub alternatives: usize,
    pub reachable_alternatives: usize,
    pub minimum_instructions: usize,
    pub maximum_instructions: usize,
    pub mean_instructions: usize,
    pub minimum_cost: usize,
    pub maximum_cost: usize,
    pub mean_cost: usize,
    pub maximum_blocks: usize,
    pub maximum_load_frontier: usize,
    pub maximum_dominating_ssa_frontier: usize,
    pub maximum_external_frontier: usize,
    pub maximum_control_merges: usize,
    pub maximum_non_dominating_control_merges: usize,
    pub maximum_loop_cutoffs: usize,
    pub all_load_frontier: Vec<RegisterId>,
    pub all_dominating_ssa_frontier: Vec<RegisterId>,
    pub all_external_frontier: Vec<RegisterId>,
    pub all_control_merges: Vec<RegisterId>,
    pub all_non_dominating_control_merges: Vec<RegisterId>,
    pub non_dominating_control_merge_cases: Vec<(String, Vec<RegisterId>)>,
    pub path_local_placements: Vec<PathLocalPlacementFact>,
    pub all_loop_cutoffs: Vec<RegisterId>,
    pub stable_load_frontier: Vec<RegisterId>,
    pub unstable_load_frontier: Vec<RegisterId>,
    pub unversioned_load_frontier: Vec<RegisterId>,
    pub maximum_unstable_loads_per_case: usize,
    pub maximum_unversioned_loads_per_case: usize,
    pub case_load_frontiers: Vec<Vec<RegisterId>>,
    pub maximum_instruction_case: String,
    pub maximum_instruction_recipe: Vec<(BlockId, usize)>,
    pub maximum_instruction_load_frontier: Vec<RegisterId>,
    pub maximum_instruction_dominating_ssa_frontier: Vec<RegisterId>,
    pub maximum_instruction_external_frontier: Vec<RegisterId>,
    pub maximum_instruction_control_merges: Vec<RegisterId>,
    pub maximum_instruction_loop_cutoffs: Vec<RegisterId>,
    pub maximum_cost_case: String,
    pub maximum_cost_instructions: usize,
    pub maximum_cost_recipe: Vec<(BlockId, usize)>,
    pub maximum_cost_load_frontier: Vec<RegisterId>,
    pub maximum_cost_dominating_ssa_frontier: Vec<RegisterId>,
    pub maximum_cost_external_frontier: Vec<RegisterId>,
    pub maximum_cost_control_merges: Vec<RegisterId>,
    pub maximum_cost_loop_cutoffs: Vec<RegisterId>,
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PathLocalPlacementFact {
    pub case: String,
    pub merge: RegisterId,
    pub insertion_block: BlockId,
    pub load_frontier: Vec<RegisterId>,
    pub unavailable_ssa_frontier: Vec<RegisterId>,
    pub stable_load_frontier: Vec<RegisterId>,
    pub unstable_load_frontier: Vec<RegisterId>,
    pub unversioned_load_frontier: Vec<RegisterId>,
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SelectorSinkRecipePairFact {
    pub left_sink: (BlockId, usize),
    pub right_sink: (BlockId, usize),
    pub same_publication: bool,
    pub same_continuation: bool,
    pub common_dominator: Option<BlockId>,
    pub common_postdominator: Option<BlockId>,
    pub common_instructions: usize,
    pub left_only_instructions: usize,
    pub right_only_instructions: usize,
    pub common_blocks: usize,
    pub common_frontier_values: usize,
}

#[cfg(test)]
impl SelectorRegionFact {
    fn worst_saving(&self) -> usize {
        self.baseline_cost.saturating_sub(self.worst_case_cost)
    }

    fn profile_weighted_worst_saving(&self) -> u128 {
        (self.worst_saving() as u128).saturating_mul(self.samples as u128)
    }
}

#[cfg(test)]
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct ControlRegionFeasibilityReport {
    pub selected_blocks: usize,
    pub selected_samples: u64,
    pub blocks_with_selector_groups: usize,
    pub profitable_regions: usize,
    pub profile_weighted_selected_cost: u128,
    pub profile_weighted_baseline_cost: u128,
    pub profile_weighted_worst_case_cost: u128,
    facts: Vec<SelectorRegionFact>,
}

#[cfg(test)]
impl fmt::Display for ControlRegionFeasibilityReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "selected_blocks={} selected_samples={} blocks_with_selector_groups={} \
             profitable_regions={} profile_weighted_baseline_cost={} \
             profile_weighted_worst_case_cost={} profile_weighted_worst_saving={} \
             profile_weighted_selected_cost={} selected_cost_saving_ppm={}",
            self.selected_blocks,
            self.selected_samples,
            self.blocks_with_selector_groups,
            self.profitable_regions,
            self.profile_weighted_baseline_cost,
            self.profile_weighted_worst_case_cost,
            self.profile_weighted_baseline_cost
                .saturating_sub(self.profile_weighted_worst_case_cost),
            self.profile_weighted_selected_cost,
            self.profile_weighted_baseline_cost
                .saturating_sub(self.profile_weighted_worst_case_cost)
                .saturating_mul(1_000_000)
                .checked_div(self.profile_weighted_selected_cost)
                .unwrap_or(0),
        )
    }
}

#[derive(Debug, Clone)]
struct SelectorGroup {
    selector: RegisterId,
    cases: BTreeSet<BigUint>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct DefinitionSite {
    block: BlockId,
    index: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct IncomingValue {
    predecessor: BlockId,
    target: BlockId,
    value: RegisterId,
}

#[derive(Debug)]
struct SpecializedBlock {
    cost: usize,
    needed_instructions: usize,
}

#[derive(Debug, Default)]
struct CrossBlockClosure {
    instructions: BTreeSet<(BlockId, usize)>,
    blocks: BTreeSet<BlockId>,
    effect_sinks: BTreeSet<(BlockId, usize)>,
    branch_conditions: BTreeSet<BlockId>,
}

#[derive(Debug, Default)]
struct SinkRecipe {
    instructions: BTreeSet<(BlockId, usize)>,
    blocks: BTreeSet<BlockId>,
    selector_control_blocks: BTreeSet<BlockId>,
    #[cfg(test)]
    entering_edges: BTreeSet<(BlockId, BlockId)>,
    #[cfg(test)]
    continuations: BTreeSet<BlockId>,
    constant_frontier: BTreeSet<RegisterId>,
    load_frontier: BTreeSet<RegisterId>,
    #[cfg(test)]
    shared_ssa_frontier: BTreeSet<RegisterId>,
    dominating_ssa_frontier: BTreeSet<RegisterId>,
    external_frontier: BTreeSet<RegisterId>,
    control_merges: BTreeSet<RegisterId>,
    non_dominating_control_merges: BTreeSet<RegisterId>,
    loop_cutoffs: BTreeSet<RegisterId>,
    clone_order: Vec<(BlockId, usize)>,
    aliases: BTreeMap<RegisterId, RegisterId>,
    known_values: BTreeMap<RegisterId, bool>,
}

#[cfg(test)]
#[derive(Debug)]
struct CaseSinkRecipe {
    label: String,
    selected_case: Option<BigUint>,
    recipe: SinkRecipe,
    cost: usize,
}

#[derive(Debug)]
struct SelectorCaseContext {
    #[cfg(test)]
    label: String,
    selected_case: Option<BigUint>,
    known: HashMap<RegisterId, bool>,
    reachable: BTreeSet<BlockId>,
}

#[derive(Clone, Debug)]
pub(crate) struct ExecutableCaseRecipe {
    pub selected_case: Option<BigUint>,
    pub source: RegisterId,
    pub clone_order: Vec<(BlockId, usize)>,
    pub aliases: BTreeMap<RegisterId, RegisterId>,
    pub known_values: BTreeMap<RegisterId, bool>,
}

#[derive(Clone, Debug)]
pub(crate) struct EffectSinkDispatchPlan {
    pub sink: (BlockId, usize),
    pub continuation: BlockId,
    pub cases: Vec<ExecutableCaseRecipe>,
}

#[derive(Clone, Debug)]
pub(crate) struct PathLocalEffectExitPlan {
    pub sink: (BlockId, usize),
    pub continuation: BlockId,
    pub insertion_block: BlockId,
    pub guard: RegisterId,
    pub recipe: ExecutableCaseRecipe,
}

#[derive(Clone, Debug)]
pub(crate) struct EffectCaseRewritePlan {
    pub origin: BlockId,
    pub selector: RegisterId,
    pub explicit_cases: Vec<BigUint>,
    pub sinks: Vec<EffectSinkDispatchPlan>,
    pub path_local_exits: Vec<PathLocalEffectExitPlan>,
    pub estimated_saving: usize,
}

#[cfg(test)]
pub(crate) fn analyze(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    profile_blocks: &[(BlockId, u64)],
) -> ControlRegionFeasibilityReport {
    let constants = collect_exact_constants(eu);
    let uses = collect_uses(eu);
    let edge_param_uses = collect_edge_param_uses(eu);
    let definitions = collect_definition_sites(eu);
    let parameter_blocks = collect_parameter_blocks(eu);
    let incoming_values = collect_incoming_values(eu);
    let predecessors = collect_predecessors(eu);
    let cfg = SirCfg::analyze_structure(eu).ok();
    let mut selected = BTreeMap::<BlockId, u64>::new();
    for &(block, samples) in profile_blocks {
        if eu.blocks.contains_key(&block) {
            let total = selected.entry(block).or_default();
            *total = total.saturating_add(samples);
        }
    }

    let mut report = ControlRegionFeasibilityReport {
        selected_blocks: selected.len(),
        selected_samples: selected.values().copied().sum(),
        ..ControlRegionFeasibilityReport::default()
    };
    for (block_id, samples) in selected {
        let block = &eu.blocks[&block_id];
        let baseline_cost = block
            .instructions
            .iter()
            .map(|instruction| estimate_clif_cost(instruction, &eu.register_map, false))
            .sum::<usize>();
        report.profile_weighted_selected_cost = report
            .profile_weighted_selected_cost
            .saturating_add((baseline_cost as u128).saturating_mul(samples as u128));
        let groups = selector_groups(block, &constants);
        if groups.is_empty() {
            continue;
        }
        report.blocks_with_selector_groups += 1;
        let live_outputs = block
            .instructions
            .iter()
            .filter_map(def_reg)
            .filter(|register| {
                uses.get(register)
                    .into_iter()
                    .flatten()
                    .any(|site| site.block() != block_id)
            })
            .count();
        let effects = block
            .instructions
            .iter()
            .filter(|instruction| def_reg(instruction).is_none())
            .count();

        let best = groups
            .into_iter()
            .filter_map(|group| {
                analyze_group(
                    eu,
                    block_id,
                    samples,
                    group,
                    &constants,
                    &uses,
                    &edge_param_uses,
                    &definitions,
                    &parameter_blocks,
                    &incoming_values,
                    &predecessors,
                    cfg.as_ref(),
                    baseline_cost,
                    live_outputs,
                    effects,
                )
            })
            .max_by_key(|fact| {
                (
                    fact.profile_weighted_worst_saving(),
                    fact.minimum_skipped_instructions,
                    fact.explicit_cases,
                )
            });
        let Some(fact) = best else {
            continue;
        };
        if fact.worst_case_cost >= fact.baseline_cost {
            continue;
        }
        report.profitable_regions += 1;
        report.profile_weighted_baseline_cost = report
            .profile_weighted_baseline_cost
            .saturating_add((fact.baseline_cost as u128).saturating_mul(samples as u128));
        report.profile_weighted_worst_case_cost = report
            .profile_weighted_worst_case_cost
            .saturating_add((fact.worst_case_cost as u128).saturating_mul(samples as u128));
        report.facts.push(fact);
    }
    report.facts.sort_unstable_by_key(|fact| {
        (
            std::cmp::Reverse(fact.profile_weighted_worst_saving()),
            fact.block,
        )
    });
    if let Some(cfg) = cfg.as_ref() {
        annotate_load_frontier_versions(eu, cfg, &definitions, &mut report.facts);
    }
    report
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LoadFrontierVersion {
    Stable,
    Unstable,
    Unversioned,
}

#[cfg(test)]
fn annotate_load_frontier_versions(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &SirCfg,
    definitions: &HashMap<RegisterId, DefinitionSite>,
    facts: &mut [SelectorRegionFact],
) {
    let mut loads_by_region = BTreeMap::<u32, HashSet<RegisterId>>::new();
    for register in facts
        .iter()
        .flat_map(|fact| &fact.sink_recipes)
        .flat_map(|recipe| &recipe.case_summary.all_load_frontier)
    {
        let Some(site) = definitions.get(register) else {
            continue;
        };
        let SIRInstruction::Load(_, address, ..) = &eu.blocks[&site.block].instructions[site.index]
        else {
            continue;
        };
        loads_by_region
            .entry(address.region)
            .or_default()
            .insert(*register);
    }
    let states = loads_by_region
        .into_iter()
        .filter_map(|(region, loads)| {
            StateSsa::analyze_selected_loads_two_state(eu, cfg, region, &loads)
                .ok()
                .map(|state| (region, state))
        })
        .collect::<BTreeMap<_, _>>();

    for recipe in facts.iter_mut().flat_map(|fact| &mut fact.sink_recipes) {
        let target = recipe.sink.0;
        let status = |register: RegisterId| {
            load_frontier_version(eu, definitions, &states, register, target)
        };
        recipe.case_summary.stable_load_frontier.clear();
        recipe.case_summary.unstable_load_frontier.clear();
        recipe.case_summary.unversioned_load_frontier.clear();
        for &register in &recipe.case_summary.all_load_frontier {
            match status(register) {
                LoadFrontierVersion::Stable => {
                    recipe.case_summary.stable_load_frontier.push(register);
                }
                LoadFrontierVersion::Unstable => {
                    recipe.case_summary.unstable_load_frontier.push(register);
                }
                LoadFrontierVersion::Unversioned => {
                    recipe.case_summary.unversioned_load_frontier.push(register);
                }
            }
        }
        recipe.case_summary.maximum_unstable_loads_per_case = recipe
            .case_summary
            .case_load_frontiers
            .iter()
            .map(|loads| {
                loads
                    .iter()
                    .filter(|&&register| status(register) == LoadFrontierVersion::Unstable)
                    .count()
            })
            .max()
            .unwrap_or(0);
        recipe.case_summary.maximum_unversioned_loads_per_case = recipe
            .case_summary
            .case_load_frontiers
            .iter()
            .map(|loads| {
                loads
                    .iter()
                    .filter(|&&register| status(register) == LoadFrontierVersion::Unversioned)
                    .count()
            })
            .max()
            .unwrap_or(0);
        for placement in &mut recipe.case_summary.path_local_placements {
            placement.stable_load_frontier.clear();
            placement.unstable_load_frontier.clear();
            placement.unversioned_load_frontier.clear();
            for &register in &placement.load_frontier {
                match load_frontier_version(
                    eu,
                    definitions,
                    &states,
                    register,
                    placement.insertion_block,
                ) {
                    LoadFrontierVersion::Stable => {
                        placement.stable_load_frontier.push(register);
                    }
                    LoadFrontierVersion::Unstable => {
                        placement.unstable_load_frontier.push(register);
                    }
                    LoadFrontierVersion::Unversioned => {
                        placement.unversioned_load_frontier.push(register);
                    }
                }
            }
        }
    }
}

fn load_frontier_version(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, DefinitionSite>,
    states: &BTreeMap<u32, StateSsa>,
    register: RegisterId,
    target: BlockId,
) -> LoadFrontierVersion {
    let Some(site) = definitions.get(&register) else {
        return LoadFrontierVersion::Unversioned;
    };
    let SIRInstruction::Load(_, address, ..) = &eu.blocks[&site.block].instructions[site.index]
    else {
        return LoadFrontierVersion::Unversioned;
    };
    let Some(state) = states.get(&address.region) else {
        return LoadFrontierVersion::Unversioned;
    };
    let Some((slot, original)) = state.read_version(site.block, site.index, register) else {
        return LoadFrontierVersion::Unversioned;
    };
    if state.entry_version(target, slot) == Some(original) {
        LoadFrontierVersion::Stable
    } else {
        LoadFrontierVersion::Unstable
    }
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn analyze_group(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block_id: BlockId,
    samples: u64,
    group: SelectorGroup,
    constants: &HashMap<RegisterId, SIRValue>,
    uses: &HashMap<RegisterId, Vec<UseSite>>,
    edge_param_uses: &HashMap<RegisterId, Vec<RegisterId>>,
    definitions: &HashMap<RegisterId, DefinitionSite>,
    parameter_blocks: &HashMap<RegisterId, BlockId>,
    incoming_values: &HashMap<RegisterId, Vec<IncomingValue>>,
    predecessors: &HashMap<BlockId, Vec<BlockId>>,
    cfg: Option<&SirCfg>,
    baseline_cost: usize,
    live_outputs: usize,
    effects: usize,
) -> Option<SelectorRegionFact> {
    // A binary condition belongs to the ordinary branch recovery path.
    if group.cases.len() < 2 {
        return None;
    }
    let cross_block = cross_block_selector_closure(
        eu,
        block_id,
        group.selector,
        &group.cases,
        constants,
        uses,
        edge_param_uses,
    );
    let case_contexts = build_selector_case_contexts(
        eu,
        block_id,
        group.selector,
        &group.cases,
        constants,
        &cross_block.effect_sinks,
    );
    let sink_recipes = cross_block
        .effect_sinks
        .iter()
        .filter_map(|&sink| {
            analyze_sink_recipe(
                eu,
                &case_contexts,
                sink,
                &cross_block,
                uses,
                definitions,
                parameter_blocks,
                incoming_values,
                predecessors,
                cfg,
            )
        })
        .collect::<Vec<_>>();
    let sink_recipe_pairs = compare_sink_recipes(&sink_recipes, cfg);
    let mut specializations = group
        .cases
        .iter()
        .cloned()
        .map(Some)
        .chain(std::iter::once(None))
        .map(|selected| {
            specialize_block(
                eu,
                block_id,
                group.selector,
                selected.as_ref(),
                constants,
                uses,
            )
        })
        .collect::<Vec<_>>();
    let dispatch_cost = selector_dispatch_cost(group.cases.len());
    for specialization in &mut specializations {
        specialization.cost = specialization.cost.saturating_add(dispatch_cost);
    }
    let block_instructions = eu.blocks[&block_id].instructions.len();
    Some(SelectorRegionFact {
        block: block_id,
        samples,
        selector: group.selector,
        explicit_cases: group.cases.len(),
        block_instructions,
        baseline_cost,
        worst_case_cost: specializations
            .iter()
            .map(|specialization| specialization.cost)
            .max()
            .unwrap_or(baseline_cost),
        mean_case_cost: specializations
            .iter()
            .map(|specialization| specialization.cost)
            .sum::<usize>()
            .div_ceil(specializations.len()),
        minimum_skipped_instructions: specializations
            .iter()
            .map(|specialization| {
                block_instructions.saturating_sub(specialization.needed_instructions)
            })
            .min()
            .unwrap_or(0),
        maximum_skipped_instructions: specializations
            .iter()
            .map(|specialization| {
                block_instructions.saturating_sub(specialization.needed_instructions)
            })
            .max()
            .unwrap_or(0),
        live_outputs,
        effects,
        cross_block_affected_instructions: cross_block.instructions.len(),
        cross_block_affected_blocks: cross_block.blocks.len(),
        cross_block_effect_sinks: cross_block.effect_sinks.len(),
        cross_block_branch_conditions: cross_block.branch_conditions.len(),
        cross_block_effect_sites: cross_block.effect_sinks.into_iter().collect(),
        sink_recipes,
        sink_recipe_pairs,
    })
}

#[cfg(test)]
fn selector_dispatch_cost(explicit_cases: usize) -> usize {
    // Selector normalization, bounds/table operation and indirect transfer.
    3usize.saturating_add(explicit_cases.ilog2() as usize)
}

fn collect_exact_constants(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
) -> HashMap<RegisterId, SIRValue> {
    eu.blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .filter_map(|instruction| match instruction {
            SIRInstruction::Imm(register, value) if value.mask.is_zero() => {
                Some((*register, value.clone()))
            }
            _ => None,
        })
        .collect()
}

fn selector_groups(
    block: &crate::ir::BasicBlock<RegionedAbsoluteAddr>,
    constants: &HashMap<RegisterId, SIRValue>,
) -> Vec<SelectorGroup> {
    let mut groups = BTreeMap::<RegisterId, BTreeSet<BigUint>>::new();
    for instruction in &block.instructions {
        let SIRInstruction::Binary(_, lhs, operation, rhs) = instruction else {
            continue;
        };
        if !matches!(operation, BinaryOp::Eq | BinaryOp::EqWildcard) {
            continue;
        }
        let (selector, constant) = match (constants.get(lhs), constants.get(rhs)) {
            (None, Some(constant)) => (*lhs, constant),
            (Some(constant), None) => (*rhs, constant),
            _ => continue,
        };
        groups
            .entry(selector)
            .or_default()
            .insert(constant.payload.clone());
    }
    groups
        .into_iter()
        .map(|(selector, cases)| SelectorGroup { selector, cases })
        .collect()
}

fn collect_edge_param_uses(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
) -> HashMap<RegisterId, Vec<RegisterId>> {
    let mut result = HashMap::<RegisterId, Vec<RegisterId>>::default();
    for block in eu.blocks.values() {
        let mut add_edge = |target: BlockId, arguments: &[RegisterId]| {
            let Some(target) = eu.blocks.get(&target) else {
                return;
            };
            for (&argument, &parameter) in arguments.iter().zip(&target.params) {
                result.entry(argument).or_default().push(parameter);
            }
        };
        match &block.terminator {
            SIRTerminator::Jump(target, arguments) => add_edge(*target, arguments),
            SIRTerminator::Branch {
                true_block,
                false_block,
                ..
            } => {
                add_edge(true_block.0, &true_block.1);
                add_edge(false_block.0, &false_block.1);
            }
            SIRTerminator::Switch { .. } | SIRTerminator::Return | SIRTerminator::Error(_) => {}
        }
    }
    for parameters in result.values_mut() {
        parameters.sort_unstable();
        parameters.dedup();
    }
    result
}

fn collect_definition_sites(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
) -> HashMap<RegisterId, DefinitionSite> {
    eu.blocks
        .values()
        .flat_map(|block| {
            block
                .instructions
                .iter()
                .enumerate()
                .filter_map(move |(index, instruction)| {
                    def_reg(instruction).map(|register| {
                        (
                            register,
                            DefinitionSite {
                                block: block.id,
                                index,
                            },
                        )
                    })
                })
        })
        .collect()
}

fn collect_parameter_blocks(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
) -> HashMap<RegisterId, BlockId> {
    eu.blocks
        .values()
        .flat_map(|block| {
            block
                .params
                .iter()
                .copied()
                .map(move |parameter| (parameter, block.id))
        })
        .collect()
}

fn collect_incoming_values(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
) -> HashMap<RegisterId, Vec<IncomingValue>> {
    let mut result = HashMap::<RegisterId, Vec<IncomingValue>>::default();
    for block in eu.blocks.values() {
        let mut add_edge = |target: BlockId, arguments: &[RegisterId]| {
            let Some(target_block) = eu.blocks.get(&target) else {
                return;
            };
            for (&value, &parameter) in arguments.iter().zip(&target_block.params) {
                result.entry(parameter).or_default().push(IncomingValue {
                    predecessor: block.id,
                    target,
                    value,
                });
            }
        };
        match &block.terminator {
            SIRTerminator::Jump(target, arguments) => add_edge(*target, arguments),
            SIRTerminator::Branch {
                true_block,
                false_block,
                ..
            } => {
                add_edge(true_block.0, &true_block.1);
                add_edge(false_block.0, &false_block.1);
            }
            SIRTerminator::Switch { .. } | SIRTerminator::Return | SIRTerminator::Error(_) => {}
        }
    }
    for values in result.values_mut() {
        values.sort_unstable_by_key(|incoming| (incoming.predecessor, incoming.value));
        values.dedup();
    }
    result
}

fn collect_predecessors(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
) -> HashMap<BlockId, Vec<BlockId>> {
    let mut result = HashMap::<BlockId, Vec<BlockId>>::default();
    for block in eu.blocks.values() {
        for successor in terminator_successors(&block.terminator) {
            result.entry(successor).or_default().push(block.id);
        }
    }
    for blocks in result.values_mut() {
        blocks.sort_unstable();
        blocks.dedup();
    }
    result
}

fn cross_block_selector_closure(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    origin: BlockId,
    selector: RegisterId,
    cases: &BTreeSet<BigUint>,
    constants: &HashMap<RegisterId, SIRValue>,
    uses: &HashMap<RegisterId, Vec<UseSite>>,
    edge_param_uses: &HashMap<RegisterId, Vec<RegisterId>>,
) -> CrossBlockClosure {
    let mut closure = CrossBlockClosure::default();
    let mut values = BTreeSet::<RegisterId>::new();
    let mut work = VecDeque::<RegisterId>::new();
    for (index, instruction) in eu.blocks[&origin].instructions.iter().enumerate() {
        let SIRInstruction::Binary(destination, lhs, BinaryOp::Eq | BinaryOp::EqWildcard, rhs) =
            instruction
        else {
            continue;
        };
        let constant = if *lhs == selector {
            constants.get(rhs)
        } else if *rhs == selector {
            constants.get(lhs)
        } else {
            None
        };
        if constant.is_some_and(|constant| cases.contains(&constant.payload))
            && values.insert(*destination)
        {
            closure.instructions.insert((origin, index));
            closure.blocks.insert(origin);
            work.push_back(*destination);
        }
    }

    while let Some(value) = work.pop_front() {
        for &parameter in edge_param_uses.get(&value).into_iter().flatten() {
            if values.insert(parameter) {
                work.push_back(parameter);
            }
        }
        for site in uses.get(&value).into_iter().flatten() {
            match *site {
                UseSite::Instruction { block, index } => {
                    let instruction = &eu.blocks[&block].instructions[index];
                    closure.blocks.insert(block);
                    if let Some(destination) = def_reg(instruction) {
                        if closure.instructions.insert((block, index)) && values.insert(destination)
                        {
                            work.push_back(destination);
                        }
                    } else {
                        closure.effect_sinks.insert((block, index));
                    }
                }
                UseSite::BranchCondition { block } => {
                    closure.blocks.insert(block);
                    closure.branch_conditions.insert(block);
                }
                UseSite::TrueEdgeArgument { .. }
                | UseSite::FalseEdgeArgument { .. }
                | UseSite::JumpArgument { .. } => {}
            }
        }
    }
    closure
}

#[derive(Clone, Copy)]
enum RecipeWalk {
    Enter(RegisterId),
    Exit(RegisterId),
}

#[allow(clippy::too_many_arguments)]
#[cfg(test)]
fn analyze_sink_recipe(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    case_contexts: &[SelectorCaseContext],
    sink: (BlockId, usize),
    selector_closure: &CrossBlockClosure,
    uses: &HashMap<RegisterId, Vec<UseSite>>,
    definitions: &HashMap<RegisterId, DefinitionSite>,
    parameter_blocks: &HashMap<RegisterId, BlockId>,
    incoming_values: &HashMap<RegisterId, Vec<IncomingValue>>,
    predecessors: &HashMap<BlockId, Vec<BlockId>>,
    cfg: Option<&SirCfg>,
) -> Option<SelectorSinkRecipeFact> {
    let instruction = eu.blocks.get(&sink.0)?.instructions.get(sink.1)?;
    let SIRInstruction::Store(address, offset, width, source, triggers, comb_capture_sites) =
        instruction
    else {
        return None;
    };

    let mut recipe = SinkRecipe::default();
    recipe.blocks.insert(sink.0);
    recipe
        .continuations
        .extend(terminator_successors(&eu.blocks[&sink.0].terminator));

    let mut active = BTreeSet::<RegisterId>::new();
    let mut complete = BTreeSet::<RegisterId>::new();
    let mut work = vec![RecipeWalk::Enter(*source)];
    while let Some(item) = work.pop() {
        let register = match item {
            RecipeWalk::Exit(register) => {
                active.remove(&register);
                complete.insert(register);
                continue;
            }
            RecipeWalk::Enter(register) => register,
        };
        if complete.contains(&register) {
            continue;
        }
        if !active.insert(register) {
            recipe.loop_cutoffs.insert(register);
            continue;
        }
        work.push(RecipeWalk::Exit(register));

        if let Some(incoming) = incoming_values.get(&register) {
            recipe.control_merges.insert(register);
            for incoming in incoming.iter().rev() {
                work.push(RecipeWalk::Enter(incoming.value));
            }
            continue;
        }

        let Some(site) = definitions.get(&register).copied() else {
            recipe.external_frontier.insert(register);
            continue;
        };
        let definition = &eu.blocks[&site.block].instructions[site.index];
        match definition {
            SIRInstruction::Imm(..) => {
                recipe.constant_frontier.insert(register);
            }
            SIRInstruction::Load(..) => {
                recipe.load_frontier.insert(register);
            }
            _ => {
                let selector_dependent = selector_closure
                    .instructions
                    .contains(&(site.block, site.index));
                let single_use = uses.get(&register).is_none_or(|sites| sites.len() <= 1);
                if !is_recipe_pure(definition)
                    || !selector_closure.blocks.contains(&site.block)
                    || (!selector_dependent && !single_use)
                {
                    recipe.shared_ssa_frontier.insert(register);
                    continue;
                }
                recipe.instructions.insert((site.block, site.index));
                recipe.blocks.insert(site.block);
                for operand in instruction_uses(definition).into_iter().rev() {
                    work.push(RecipeWalk::Enter(operand));
                }
            }
        }
    }

    let mut can_reach_sink = BTreeSet::new();
    let mut block_work = vec![sink.0];
    while let Some(block) = block_work.pop() {
        if !can_reach_sink.insert(block) {
            continue;
        }
        block_work.extend(
            predecessors
                .get(&block)
                .into_iter()
                .flatten()
                .filter(|predecessor| selector_closure.blocks.contains(predecessor))
                .copied(),
        );
    }
    recipe.selector_control_blocks.extend(
        selector_closure
            .branch_conditions
            .intersection(&can_reach_sink)
            .copied(),
    );
    recipe
        .blocks
        .extend(recipe.selector_control_blocks.iter().copied());
    for &block in &recipe.blocks {
        for &predecessor in predecessors.get(&block).into_iter().flatten() {
            if !recipe.blocks.contains(&predecessor) {
                recipe.entering_edges.insert((predecessor, block));
            }
        }
    }

    Some(SelectorSinkRecipeFact {
        sink,
        effect: instruction.to_string(),
        publication: format!(
            "addr={address},offset={offset},bits={width},triggers={triggers:?},\
             comb_capture_sites={comb_capture_sites:?}"
        ),
        source: *source,
        recipe_instructions: recipe.instructions.into_iter().collect(),
        recipe_blocks: recipe.blocks.into_iter().collect(),
        selector_control_blocks: recipe.selector_control_blocks.into_iter().collect(),
        entering_edges: recipe.entering_edges.into_iter().collect(),
        continuations: recipe.continuations.into_iter().collect(),
        constant_frontier: recipe.constant_frontier.into_iter().collect(),
        load_frontier: recipe.load_frontier.into_iter().collect(),
        shared_ssa_frontier: recipe.shared_ssa_frontier.into_iter().collect(),
        external_frontier: recipe.external_frontier.into_iter().collect(),
        control_merges: recipe.control_merges.into_iter().collect(),
        loop_cutoffs: recipe.loop_cutoffs.into_iter().collect(),
        case_summary: analyze_case_sink_recipes(
            eu,
            case_contexts,
            sink,
            selector_closure,
            definitions,
            parameter_blocks,
            incoming_values,
            predecessors,
            cfg,
        ),
    })
}

#[allow(clippy::too_many_arguments)]
#[cfg(test)]
fn analyze_case_sink_recipes(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    case_contexts: &[SelectorCaseContext],
    sink: (BlockId, usize),
    selector_closure: &CrossBlockClosure,
    definitions: &HashMap<RegisterId, DefinitionSite>,
    parameter_blocks: &HashMap<RegisterId, BlockId>,
    incoming_values: &HashMap<RegisterId, Vec<IncomingValue>>,
    predecessors: &HashMap<BlockId, Vec<BlockId>>,
    cfg: Option<&SirCfg>,
) -> SelectorSinkCaseSummaryFact {
    let mut recipes = case_contexts
        .iter()
        .filter_map(|context| {
            analyze_case_sink_recipe(
                eu,
                context,
                sink,
                selector_closure,
                definitions,
                parameter_blocks,
                incoming_values,
                predecessors,
                cfg,
            )
            .map(|(recipe, cost)| CaseSinkRecipe {
                label: context.label.clone(),
                selected_case: context.selected_case.clone(),
                recipe,
                cost,
            })
        })
        .collect::<Vec<_>>();
    recipes.sort_unstable_by_key(|recipe| {
        (
            recipe.recipe.instructions.len(),
            recipe.selected_case.clone(),
        )
    });

    let alternatives = case_contexts.len();
    let reachable_alternatives = recipes.len();
    let minimum = recipes.first();
    let maximum = recipes.last();
    let instruction_sum = recipes
        .iter()
        .map(|recipe| recipe.recipe.instructions.len())
        .sum::<usize>();
    let cost_sum = recipes.iter().map(|recipe| recipe.cost).sum::<usize>();
    let maximum_cost_recipe = recipes
        .iter()
        .max_by_key(|recipe| (recipe.cost, recipe.recipe.instructions.len()));
    let union = |select: fn(&SinkRecipe) -> &BTreeSet<RegisterId>| {
        recipes
            .iter()
            .flat_map(|recipe| select(&recipe.recipe))
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
    };
    let path_local_placements = cfg.map_or_else(Vec::new, |cfg| {
        recipes
            .iter()
            .filter_map(|case| {
                if case.recipe.non_dominating_control_merges.len() != 1 {
                    return None;
                }
                let merge = *case.recipe.non_dominating_control_merges.first()?;
                let insertion_block = *parameter_blocks.get(&merge)?;
                let mut unavailable = case
                    .recipe
                    .dominating_ssa_frontier
                    .iter()
                    .chain(&case.recipe.control_merges)
                    .chain(&case.recipe.external_frontier)
                    .copied()
                    .filter(|&register| {
                        !value_available_at_block_entry(
                            eu,
                            register,
                            insertion_block,
                            definitions,
                            parameter_blocks,
                            cfg,
                        )
                    })
                    .collect::<BTreeSet<_>>();
                for &load in &case.recipe.load_frontier {
                    let Some(site) = definitions.get(&load) else {
                        unavailable.insert(load);
                        continue;
                    };
                    for operand in
                        instruction_uses(&eu.blocks[&site.block].instructions[site.index])
                    {
                        if !value_available_at_block_entry(
                            eu,
                            operand,
                            insertion_block,
                            definitions,
                            parameter_blocks,
                            cfg,
                        ) {
                            unavailable.insert(operand);
                        }
                    }
                }
                Some(PathLocalPlacementFact {
                    case: case.label.clone(),
                    merge,
                    insertion_block,
                    load_frontier: case.recipe.load_frontier.iter().copied().collect(),
                    unavailable_ssa_frontier: unavailable.into_iter().collect(),
                    stable_load_frontier: Vec::new(),
                    unstable_load_frontier: Vec::new(),
                    unversioned_load_frontier: Vec::new(),
                })
            })
            .collect()
    });
    SelectorSinkCaseSummaryFact {
        alternatives,
        reachable_alternatives,
        minimum_instructions: minimum.map_or(0, |recipe| recipe.recipe.instructions.len()),
        maximum_instructions: maximum.map_or(0, |recipe| recipe.recipe.instructions.len()),
        mean_instructions: instruction_sum
            .checked_div(reachable_alternatives)
            .unwrap_or(0),
        minimum_cost: recipes.iter().map(|recipe| recipe.cost).min().unwrap_or(0),
        maximum_cost: recipes.iter().map(|recipe| recipe.cost).max().unwrap_or(0),
        mean_cost: cost_sum.checked_div(reachable_alternatives).unwrap_or(0),
        maximum_blocks: recipes
            .iter()
            .map(|recipe| recipe.recipe.blocks.len())
            .max()
            .unwrap_or(0),
        maximum_load_frontier: recipes
            .iter()
            .map(|recipe| recipe.recipe.load_frontier.len())
            .max()
            .unwrap_or(0),
        maximum_dominating_ssa_frontier: recipes
            .iter()
            .map(|recipe| recipe.recipe.dominating_ssa_frontier.len())
            .max()
            .unwrap_or(0),
        maximum_external_frontier: recipes
            .iter()
            .map(|recipe| recipe.recipe.external_frontier.len())
            .max()
            .unwrap_or(0),
        maximum_control_merges: recipes
            .iter()
            .map(|recipe| recipe.recipe.control_merges.len())
            .max()
            .unwrap_or(0),
        maximum_non_dominating_control_merges: recipes
            .iter()
            .map(|recipe| recipe.recipe.non_dominating_control_merges.len())
            .max()
            .unwrap_or(0),
        maximum_loop_cutoffs: recipes
            .iter()
            .map(|recipe| recipe.recipe.loop_cutoffs.len())
            .max()
            .unwrap_or(0),
        all_load_frontier: union(|recipe| &recipe.load_frontier),
        all_dominating_ssa_frontier: union(|recipe| &recipe.dominating_ssa_frontier),
        all_external_frontier: union(|recipe| &recipe.external_frontier),
        all_control_merges: union(|recipe| &recipe.control_merges),
        all_non_dominating_control_merges: union(|recipe| &recipe.non_dominating_control_merges),
        non_dominating_control_merge_cases: recipes
            .iter()
            .filter(|case| !case.recipe.non_dominating_control_merges.is_empty())
            .map(|case| {
                (
                    case.label.clone(),
                    case.recipe
                        .non_dominating_control_merges
                        .iter()
                        .copied()
                        .collect(),
                )
            })
            .collect(),
        path_local_placements,
        all_loop_cutoffs: union(|recipe| &recipe.loop_cutoffs),
        stable_load_frontier: Vec::new(),
        unstable_load_frontier: Vec::new(),
        unversioned_load_frontier: Vec::new(),
        maximum_unstable_loads_per_case: 0,
        maximum_unversioned_loads_per_case: 0,
        case_load_frontiers: recipes
            .iter()
            .map(|recipe| recipe.recipe.load_frontier.iter().copied().collect())
            .collect(),
        maximum_instruction_case: maximum
            .map_or_else(|| "unreachable".to_owned(), |recipe| recipe.label.clone()),
        maximum_instruction_recipe: maximum
            .map(|recipe| recipe.recipe.instructions.iter().copied().collect())
            .unwrap_or_default(),
        maximum_instruction_load_frontier: maximum
            .map(|recipe| recipe.recipe.load_frontier.iter().copied().collect())
            .unwrap_or_default(),
        maximum_instruction_dominating_ssa_frontier: maximum
            .map(|recipe| {
                recipe
                    .recipe
                    .dominating_ssa_frontier
                    .iter()
                    .copied()
                    .collect()
            })
            .unwrap_or_default(),
        maximum_instruction_external_frontier: maximum
            .map(|recipe| recipe.recipe.external_frontier.iter().copied().collect())
            .unwrap_or_default(),
        maximum_instruction_control_merges: maximum
            .map(|recipe| recipe.recipe.control_merges.iter().copied().collect())
            .unwrap_or_default(),
        maximum_instruction_loop_cutoffs: maximum
            .map(|recipe| recipe.recipe.loop_cutoffs.iter().copied().collect())
            .unwrap_or_default(),
        maximum_cost_case: maximum_cost_recipe
            .map_or_else(|| "unreachable".to_owned(), |recipe| recipe.label.clone()),
        maximum_cost_instructions: maximum_cost_recipe
            .map_or(0, |recipe| recipe.recipe.instructions.len()),
        maximum_cost_recipe: maximum_cost_recipe
            .map(|recipe| recipe.recipe.instructions.iter().copied().collect())
            .unwrap_or_default(),
        maximum_cost_load_frontier: maximum_cost_recipe
            .map(|recipe| recipe.recipe.load_frontier.iter().copied().collect())
            .unwrap_or_default(),
        maximum_cost_dominating_ssa_frontier: maximum_cost_recipe
            .map(|recipe| {
                recipe
                    .recipe
                    .dominating_ssa_frontier
                    .iter()
                    .copied()
                    .collect()
            })
            .unwrap_or_default(),
        maximum_cost_external_frontier: maximum_cost_recipe
            .map(|recipe| recipe.recipe.external_frontier.iter().copied().collect())
            .unwrap_or_default(),
        maximum_cost_control_merges: maximum_cost_recipe
            .map(|recipe| recipe.recipe.control_merges.iter().copied().collect())
            .unwrap_or_default(),
        maximum_cost_loop_cutoffs: maximum_cost_recipe
            .map(|recipe| recipe.recipe.loop_cutoffs.iter().copied().collect())
            .unwrap_or_default(),
    }
}

#[allow(clippy::too_many_arguments)]
fn analyze_case_sink_recipe(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    context: &SelectorCaseContext,
    sink: (BlockId, usize),
    selector_closure: &CrossBlockClosure,
    definitions: &HashMap<RegisterId, DefinitionSite>,
    parameter_blocks: &HashMap<RegisterId, BlockId>,
    incoming_values: &HashMap<RegisterId, Vec<IncomingValue>>,
    predecessors: &HashMap<BlockId, Vec<BlockId>>,
    cfg: Option<&SirCfg>,
) -> Option<(SinkRecipe, usize)> {
    let instruction = eu.blocks.get(&sink.0)?.instructions.get(sink.1)?;
    let SIRInstruction::Store(_, _, _, source, _, _) = instruction else {
        return None;
    };
    if !context.reachable.contains(&sink.0) {
        return None;
    }

    let mut can_reach_sink = BTreeSet::new();
    let mut block_work = vec![sink.0];
    while let Some(block) = block_work.pop() {
        if !can_reach_sink.insert(block) {
            continue;
        }
        block_work.extend(
            predecessors
                .get(&block)
                .into_iter()
                .flatten()
                .filter(|predecessor| {
                    context.reachable.contains(predecessor)
                        && specialized_successors(
                            &eu.blocks[predecessor].terminator,
                            &context.known,
                        )
                        .contains(&block)
                })
                .copied(),
        );
    }

    let mut recipe = SinkRecipe::default();
    recipe.blocks.insert(sink.0);
    let mut active = BTreeSet::<RegisterId>::new();
    let mut complete = BTreeSet::<RegisterId>::new();
    let mut pending_clone = BTreeMap::<RegisterId, (BlockId, usize)>::new();
    let mut work = vec![RecipeWalk::Enter(*source)];
    while let Some(item) = work.pop() {
        let register = match item {
            RecipeWalk::Exit(register) => {
                active.remove(&register);
                complete.insert(register);
                if let Some(site) = pending_clone.remove(&register) {
                    recipe.clone_order.push(site);
                }
                continue;
            }
            RecipeWalk::Enter(register) => register,
        };
        if complete.contains(&register) {
            continue;
        }
        if !active.insert(register) {
            recipe.loop_cutoffs.insert(register);
            continue;
        }
        work.push(RecipeWalk::Exit(register));

        if context.known.contains_key(&register) {
            recipe.constant_frontier.insert(register);
            recipe
                .known_values
                .insert(register, context.known[&register]);
            continue;
        }

        if let Some(incoming) = incoming_values.get(&register) {
            let feasible = incoming
                .iter()
                .filter(|incoming| {
                    context.reachable.contains(&incoming.predecessor)
                        && context.reachable.contains(&incoming.target)
                        && can_reach_sink.contains(&incoming.predecessor)
                        && can_reach_sink.contains(&incoming.target)
                        && specialized_successors(
                            &eu.blocks[&incoming.predecessor].terminator,
                            &context.known,
                        )
                        .contains(&incoming.target)
                })
                .collect::<Vec<_>>();
            if feasible.is_empty() {
                classify_ssa_frontier(
                    &mut recipe,
                    register,
                    parameter_blocks.get(&register).copied(),
                    sink.0,
                    cfg,
                );
            } else {
                if feasible.len() > 1 {
                    if parameter_blocks
                        .get(&register)
                        .zip(cfg)
                        .is_some_and(|(definition, cfg)| cfg.dominates(*definition, sink.0))
                    {
                        recipe.control_merges.insert(register);
                    } else {
                        recipe.non_dominating_control_merges.insert(register);
                    }
                } else {
                    recipe.aliases.insert(register, feasible[0].value);
                    work.push(RecipeWalk::Enter(feasible[0].value));
                }
            }
            continue;
        }

        let Some(site) = definitions.get(&register).copied() else {
            classify_ssa_frontier(
                &mut recipe,
                register,
                parameter_blocks.get(&register).copied(),
                sink.0,
                cfg,
            );
            continue;
        };
        let definition = &eu.blocks[&site.block].instructions[site.index];
        match definition {
            SIRInstruction::Imm(..) => {
                recipe.constant_frontier.insert(register);
            }
            SIRInstruction::Load(..) => {
                recipe.load_frontier.insert(register);
                pending_clone.insert(register, (site.block, site.index));
                for operand in instruction_uses(definition).into_iter().rev() {
                    work.push(RecipeWalk::Enter(operand));
                }
            }
            _ if is_recipe_pure(definition)
                && selector_closure.blocks.contains(&site.block)
                && context.reachable.contains(&site.block) =>
            {
                let operands = specialized_uses(definition, &context.known);
                let selected_mux = matches!(
                    definition,
                    SIRInstruction::Mux(_, condition, _, _)
                        if context.known.contains_key(condition)
                );
                if !selected_mux {
                    recipe.instructions.insert((site.block, site.index));
                    recipe.blocks.insert(site.block);
                    pending_clone.insert(register, (site.block, site.index));
                } else if let Some(&operand) = operands.first() {
                    recipe.aliases.insert(register, operand);
                }
                for operand in operands.into_iter().rev() {
                    work.push(RecipeWalk::Enter(operand));
                }
            }
            _ => {
                classify_ssa_frontier(&mut recipe, register, Some(site.block), sink.0, cfg);
            }
        }
    }

    recipe.selector_control_blocks.extend(
        selector_closure
            .branch_conditions
            .intersection(&can_reach_sink)
            .filter(|block| context.reachable.contains(block))
            .copied(),
    );
    recipe
        .blocks
        .extend(recipe.selector_control_blocks.iter().copied());

    let instruction_cost = recipe
        .instructions
        .iter()
        .map(|&(block, index)| {
            estimate_clif_cost(
                &eu.blocks[&block].instructions[index],
                &eu.register_map,
                false,
            )
        })
        .sum::<usize>();
    let load_cost = recipe
        .load_frontier
        .iter()
        .filter_map(|register| definitions.get(register))
        .map(|site| {
            estimate_clif_cost(
                &eu.blocks[&site.block].instructions[site.index],
                &eu.register_map,
                false,
            )
        })
        .sum::<usize>();
    Some((recipe, instruction_cost.saturating_add(load_cost)))
}

fn classify_ssa_frontier(
    recipe: &mut SinkRecipe,
    register: RegisterId,
    definition_block: Option<BlockId>,
    insertion_block: BlockId,
    cfg: Option<&SirCfg>,
) {
    if definition_block
        .zip(cfg)
        .is_some_and(|(definition, cfg)| cfg.dominates(definition, insertion_block))
    {
        recipe.dominating_ssa_frontier.insert(register);
    } else {
        recipe.external_frontier.insert(register);
    }
}

fn value_available_at_block_entry(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    register: RegisterId,
    insertion_block: BlockId,
    definitions: &HashMap<RegisterId, DefinitionSite>,
    parameter_blocks: &HashMap<RegisterId, BlockId>,
    cfg: &SirCfg,
) -> bool {
    if let Some(&block) = parameter_blocks.get(&register) {
        return block == insertion_block
            || (block != insertion_block && cfg.dominates(block, insertion_block));
    }
    let Some(site) = definitions.get(&register) else {
        return false;
    };
    if matches!(
        eu.blocks[&site.block].instructions[site.index],
        SIRInstruction::Imm(..)
    ) {
        return true;
    }
    site.block != insertion_block && cfg.dominates(site.block, insertion_block)
}

fn build_selector_case_contexts(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    origin: BlockId,
    selector: RegisterId,
    cases: &BTreeSet<BigUint>,
    constants: &HashMap<RegisterId, SIRValue>,
    sinks: &BTreeSet<(BlockId, usize)>,
) -> Vec<SelectorCaseContext> {
    cases
        .iter()
        .map(Some)
        .chain(std::iter::once(None))
        .map(|selected| {
            let known =
                propagate_selector_facts(eu, &eu.blocks[&origin], selector, selected, constants);
            let reachable = selector_reachable_blocks_until_sinks(eu, origin, &known, sinks);
            SelectorCaseContext {
                #[cfg(test)]
                label: selected.map_or_else(|| "default".to_owned(), ToString::to_string),
                selected_case: selected.cloned(),
                known,
                reachable,
            }
        })
        .collect()
}

fn selector_reachable_blocks_until_sinks(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    origin: BlockId,
    known: &HashMap<RegisterId, bool>,
    sinks: &BTreeSet<(BlockId, usize)>,
) -> BTreeSet<BlockId> {
    let sink_blocks = sinks
        .iter()
        .map(|(block, _)| *block)
        .collect::<BTreeSet<_>>();
    let mut reachable = BTreeSet::new();
    let mut work = vec![origin];
    while let Some(block) = work.pop() {
        if !reachable.insert(block) {
            continue;
        }
        if sink_blocks.contains(&block) {
            continue;
        }
        work.extend(specialized_successors(&eu.blocks[&block].terminator, known));
    }
    reachable
}

fn specialized_successors(
    terminator: &SIRTerminator,
    known: &HashMap<RegisterId, bool>,
) -> Vec<BlockId> {
    match terminator {
        SIRTerminator::Branch {
            cond,
            true_block,
            false_block,
        } => match known.get(cond).copied() {
            Some(true) => vec![true_block.0],
            Some(false) => vec![false_block.0],
            None => vec![true_block.0, false_block.0],
        },
        _ => terminator_successors(terminator),
    }
}

#[cfg(test)]
fn compare_sink_recipes(
    recipes: &[SelectorSinkRecipeFact],
    cfg: Option<&SirCfg>,
) -> Vec<SelectorSinkRecipePairFact> {
    let mut result = Vec::new();
    for (left_index, left) in recipes.iter().enumerate() {
        for right in &recipes[left_index + 1..] {
            let left_instructions = left
                .recipe_instructions
                .iter()
                .copied()
                .collect::<BTreeSet<_>>();
            let right_instructions = right
                .recipe_instructions
                .iter()
                .copied()
                .collect::<BTreeSet<_>>();
            let common_instructions = left_instructions.intersection(&right_instructions).count();
            let left_blocks = left.recipe_blocks.iter().copied().collect::<BTreeSet<_>>();
            let right_blocks = right.recipe_blocks.iter().copied().collect::<BTreeSet<_>>();
            let left_frontier = sink_recipe_frontier(left);
            let right_frontier = sink_recipe_frontier(right);
            result.push(SelectorSinkRecipePairFact {
                left_sink: left.sink,
                right_sink: right.sink,
                same_publication: left.publication == right.publication,
                same_continuation: left.continuations == right.continuations,
                common_dominator: cfg
                    .and_then(|cfg| cfg.common_dominator(left.sink.0, right.sink.0)),
                common_postdominator: cfg
                    .and_then(|cfg| cfg.common_postdominator(left.sink.0, right.sink.0)),
                common_instructions,
                left_only_instructions: left_instructions.len() - common_instructions,
                right_only_instructions: right_instructions.len() - common_instructions,
                common_blocks: left_blocks.intersection(&right_blocks).count(),
                common_frontier_values: left_frontier.intersection(&right_frontier).count(),
            });
        }
    }
    result
}

#[cfg(test)]
fn sink_recipe_frontier(recipe: &SelectorSinkRecipeFact) -> BTreeSet<RegisterId> {
    recipe
        .constant_frontier
        .iter()
        .chain(&recipe.load_frontier)
        .chain(&recipe.shared_ssa_frontier)
        .chain(&recipe.external_frontier)
        .copied()
        .collect()
}

fn is_recipe_pure(instruction: &SIRInstruction<RegionedAbsoluteAddr>) -> bool {
    matches!(
        instruction,
        SIRInstruction::Imm(..)
            | SIRInstruction::Binary(..)
            | SIRInstruction::Unary(..)
            | SIRInstruction::Load(..)
            | SIRInstruction::Concat(..)
            | SIRInstruction::Slice(..)
            | SIRInstruction::Mux(..)
    )
}

fn specialize_block(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block_id: BlockId,
    selector: RegisterId,
    selected_case: Option<&BigUint>,
    constants: &HashMap<RegisterId, SIRValue>,
    uses: &HashMap<RegisterId, Vec<UseSite>>,
) -> SpecializedBlock {
    let block = &eu.blocks[&block_id];
    let local_defs = block
        .instructions
        .iter()
        .enumerate()
        .filter_map(|(index, instruction)| def_reg(instruction).map(|register| (register, index)))
        .collect::<HashMap<_, _>>();
    let known = propagate_selector_facts(eu, block, selector, selected_case, constants);

    let mut needed = vec![false; block.instructions.len()];
    let mut work = VecDeque::<RegisterId>::new();
    let mut constant_outputs = BTreeSet::<RegisterId>::new();
    for instruction in &block.instructions {
        let Some(register) = def_reg(instruction) else {
            continue;
        };
        if uses
            .get(&register)
            .into_iter()
            .flatten()
            .any(|site| site.block() != block_id)
        {
            if known.contains_key(&register) {
                constant_outputs.insert(register);
            } else {
                work.push_back(register);
            }
        }
    }
    work.extend(terminator_uses(&block.terminator));

    for (index, instruction) in block.instructions.iter().enumerate() {
        if def_reg(instruction).is_none() {
            needed[index] = true;
            work.extend(specialized_uses(instruction, &known));
        }
    }
    while let Some(register) = work.pop_front() {
        if known.contains_key(&register) {
            continue;
        }
        let Some(&index) = local_defs.get(&register) else {
            continue;
        };
        if std::mem::replace(&mut needed[index], true) {
            continue;
        }
        work.extend(specialized_uses(&block.instructions[index], &known));
    }

    let instruction_cost = block
        .instructions
        .iter()
        .enumerate()
        .filter(|(index, _)| needed[*index])
        .map(|(_, instruction)| estimate_clif_cost(instruction, &eu.register_map, false))
        .sum::<usize>();
    SpecializedBlock {
        // A known live-out still needs one case-local edge materialization.
        cost: instruction_cost.saturating_add(constant_outputs.len()),
        needed_instructions: needed.iter().filter(|needed| **needed).count()
            + constant_outputs.len(),
    }
}

fn propagate_selector_facts(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block: &crate::ir::BasicBlock<RegionedAbsoluteAddr>,
    selector: RegisterId,
    selected_case: Option<&BigUint>,
    constants: &HashMap<RegisterId, SIRValue>,
) -> HashMap<RegisterId, bool> {
    let mut known = HashMap::<RegisterId, bool>::default();
    for instruction in &block.instructions {
        let Some(destination) = def_reg(instruction) else {
            continue;
        };
        let value = match instruction {
            SIRInstruction::Imm(_, value)
                if eu.register_map.get(&destination).map(|ty| ty.width()) == Some(1)
                    && value.mask.is_zero() =>
            {
                Some(!value.payload.is_zero())
            }
            SIRInstruction::Binary(_, lhs, BinaryOp::Eq | BinaryOp::EqWildcard, rhs) => {
                exact_selector_comparison(*lhs, *rhs, selector, selected_case, constants)
            }
            SIRInstruction::Binary(_, lhs, operation, rhs)
                if eu.register_map.get(&destination).map(|ty| ty.width()) == Some(1) =>
            {
                let lhs = known.get(lhs).copied();
                let rhs = known.get(rhs).copied();
                match operation {
                    BinaryOp::And | BinaryOp::LogicAnd => match (lhs, rhs) {
                        (Some(false), _) | (_, Some(false)) => Some(false),
                        (Some(true), Some(true)) => Some(true),
                        _ => None,
                    },
                    BinaryOp::Or | BinaryOp::LogicOr => match (lhs, rhs) {
                        (Some(true), _) | (_, Some(true)) => Some(true),
                        (Some(false), Some(false)) => Some(false),
                        _ => None,
                    },
                    BinaryOp::Eq => lhs.zip(rhs).map(|(lhs, rhs)| lhs == rhs),
                    BinaryOp::Ne => lhs.zip(rhs).map(|(lhs, rhs)| lhs != rhs),
                    _ => None,
                }
            }
            SIRInstruction::Unary(
                _,
                UnaryOp::Ident | UnaryOp::Or | UnaryOp::ToTwoState,
                source,
            ) => known.get(source).copied(),
            SIRInstruction::Unary(_, UnaryOp::LogicNot | UnaryOp::BitNot, source)
                if eu.register_map.get(&destination).map(|ty| ty.width()) == Some(1) =>
            {
                known.get(source).map(|value| !value)
            }
            SIRInstruction::Mux(_, condition, true_value, false_value) => {
                match known.get(condition).copied() {
                    Some(true) => known.get(true_value).copied(),
                    Some(false) => known.get(false_value).copied(),
                    None => known
                        .get(true_value)
                        .zip(known.get(false_value))
                        .filter(|(lhs, rhs)| lhs == rhs)
                        .map(|(value, _)| *value),
                }
            }
            _ => None,
        };
        if let Some(value) = value {
            known.insert(destination, value);
        }
    }
    known
}

fn exact_selector_comparison(
    lhs: RegisterId,
    rhs: RegisterId,
    selector: RegisterId,
    selected_case: Option<&BigUint>,
    constants: &HashMap<RegisterId, SIRValue>,
) -> Option<bool> {
    let constant = if lhs == selector {
        constants.get(&rhs)?
    } else if rhs == selector {
        constants.get(&lhs)?
    } else {
        return None;
    };
    constant
        .mask
        .is_zero()
        .then(|| selected_case.is_some_and(|selected| selected == &constant.payload))
}

fn specialized_uses(
    instruction: &SIRInstruction<RegionedAbsoluteAddr>,
    known: &HashMap<RegisterId, bool>,
) -> Vec<RegisterId> {
    match instruction {
        SIRInstruction::Mux(_, condition, true_value, false_value) => {
            match known.get(condition).copied() {
                Some(true) => vec![*true_value],
                Some(false) => vec![*false_value],
                None => vec![*condition, *true_value, *false_value],
            }
        }
        _ => instruction_uses(instruction),
    }
}

fn terminator_uses(terminator: &SIRTerminator) -> Vec<RegisterId> {
    match terminator {
        SIRTerminator::Jump(_, arguments) => arguments.clone(),
        SIRTerminator::Branch {
            cond,
            true_block,
            false_block,
        } => std::iter::once(*cond)
            .chain(true_block.1.iter().copied())
            .chain(false_block.1.iter().copied())
            .collect(),
        SIRTerminator::Switch { selector, .. } => vec![*selector],
        SIRTerminator::Return | SIRTerminator::Error(_) => Vec::new(),
    }
}

fn terminator_successors(terminator: &SIRTerminator) -> Vec<BlockId> {
    match terminator {
        SIRTerminator::Jump(target, _) => vec![*target],
        SIRTerminator::Branch {
            true_block,
            false_block,
            ..
        } => vec![true_block.0, false_block.0],
        SIRTerminator::Switch { cases, default, .. } => cases
            .iter()
            .map(|case| case.target)
            .chain(std::iter::once(*default))
            .collect(),
        SIRTerminator::Return | SIRTerminator::Error(_) => Vec::new(),
    }
}

pub(crate) fn plan_best_effect_case_dispatch(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
) -> Option<EffectCaseRewritePlan> {
    let constants = collect_exact_constants(eu);
    let uses = collect_uses(eu);
    let edge_param_uses = collect_edge_param_uses(eu);
    let definitions = collect_definition_sites(eu);
    let parameter_blocks = collect_parameter_blocks(eu);
    let incoming_values = collect_incoming_values(eu);
    let predecessors = collect_predecessors(eu);
    let cfg = SirCfg::analyze_structure(eu).ok()?;

    let mut candidates = Vec::new();
    for (&block, body) in &eu.blocks {
        let baseline = body
            .instructions
            .iter()
            .map(|instruction| estimate_clif_cost(instruction, &eu.register_map, false))
            .sum::<usize>();
        for group in selector_groups(body, &constants) {
            let Some(selector_width) = eu.register_map.get(&group.selector).map(|ty| ty.width())
            else {
                continue;
            };
            if group.cases.len() < 2 || !(1..=8).contains(&selector_width) {
                continue;
            }
            let specializations = group
                .cases
                .iter()
                .map(Some)
                .chain(std::iter::once(None))
                .map(|selected| {
                    specialize_block(eu, block, group.selector, selected, &constants, &uses)
                })
                .collect::<Vec<_>>();
            let Some(worst) = specializations
                .iter()
                .map(|specialization| specialization.cost)
                .max()
            else {
                continue;
            };
            let Some(minimum_skipped) = specializations
                .iter()
                .map(|specialization| {
                    body.instructions
                        .len()
                        .saturating_sub(specialization.needed_instructions)
                })
                .min()
            else {
                continue;
            };
            let saving = baseline.saturating_sub(worst);
            // A dispatch must remove a substantial dynamic region, not merely
            // win by a few static instructions. This also keeps expensive
            // cross-block closure construction sparse.
            if saving >= 64 && minimum_skipped > group.cases.len() {
                candidates.push((saving, minimum_skipped, block, group));
            }
        }
    }
    candidates.sort_unstable_by(|left, right| {
        (right.0, right.1, right.3.cases.len(), right.2).cmp(&(
            left.0,
            left.1,
            left.3.cases.len(),
            left.2,
        ))
    });

    candidates
        .into_iter()
        .find_map(|(saving, _, origin, group)| {
            build_effect_case_rewrite_plan(
                eu,
                origin,
                group,
                saving,
                &constants,
                &uses,
                &edge_param_uses,
                &definitions,
                &parameter_blocks,
                &incoming_values,
                &predecessors,
                &cfg,
            )
        })
}

#[allow(clippy::too_many_arguments)]
fn build_effect_case_rewrite_plan(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    origin: BlockId,
    group: SelectorGroup,
    estimated_saving: usize,
    constants: &HashMap<RegisterId, SIRValue>,
    uses: &HashMap<RegisterId, Vec<UseSite>>,
    edge_param_uses: &HashMap<RegisterId, Vec<RegisterId>>,
    definitions: &HashMap<RegisterId, DefinitionSite>,
    parameter_blocks: &HashMap<RegisterId, BlockId>,
    incoming_values: &HashMap<RegisterId, Vec<IncomingValue>>,
    predecessors: &HashMap<BlockId, Vec<BlockId>>,
    cfg: &SirCfg,
) -> Option<EffectCaseRewritePlan> {
    let cross_block = cross_block_selector_closure(
        eu,
        origin,
        group.selector,
        &group.cases,
        constants,
        uses,
        edge_param_uses,
    );
    if cross_block.effect_sinks.is_empty() {
        return None;
    }
    let contexts = build_selector_case_contexts(
        eu,
        origin,
        group.selector,
        &group.cases,
        constants,
        &cross_block.effect_sinks,
    );
    let mut sinks = Vec::new();
    let mut path_local_exits = Vec::new();
    let mut publication = None;
    let mut shared_continuation = None;
    for &sink in &cross_block.effect_sinks {
        let block = eu.blocks.get(&sink.0)?;
        if sink.1 + 1 != block.instructions.len()
            || block.instructions[..sink.1]
                .iter()
                .any(|instruction| def_reg(instruction).is_none())
        {
            return None;
        }
        let SIRInstruction::Store(address, offset, width, source, triggers, capture_sites) =
            &block.instructions[sink.1]
        else {
            return None;
        };
        if !matches!(offset, SIROffset::Static(_))
            || !triggers.is_empty()
            || !capture_sites.is_empty()
        {
            return None;
        }
        let SIRTerminator::Jump(continuation, arguments) = &block.terminator else {
            return None;
        };
        if !arguments.is_empty() {
            return None;
        }
        let identity = format!(
            "addr={address},offset={offset},bits={width},triggers={triggers:?},\
             comb_capture_sites={capture_sites:?}"
        );
        if publication.as_ref().is_some_and(|old| old != &identity) {
            return None;
        }
        publication = Some(identity);
        if shared_continuation.is_some_and(|old| old != *continuation) {
            return None;
        }
        shared_continuation = Some(*continuation);

        let recipes = contexts
            .iter()
            .filter_map(|context| {
                analyze_case_sink_recipe(
                    eu,
                    context,
                    sink,
                    &cross_block,
                    definitions,
                    parameter_blocks,
                    incoming_values,
                    predecessors,
                    Some(cfg),
                )
                .map(|(recipe, _)| (context, recipe))
            })
            .collect::<Vec<_>>();
        if recipes.len() != contexts.len() {
            return None;
        }
        let mut sink_cases = Vec::new();
        for (context, recipe) in recipes {
            if !recipe.external_frontier.is_empty()
                || !recipe.loop_cutoffs.is_empty()
                || recipe.clone_order.len()
                    != recipe.instructions.len() + recipe.load_frontier.len()
            {
                return None;
            }
            let executable = ExecutableCaseRecipe {
                selected_case: context.selected_case.clone(),
                source: *source,
                clone_order: recipe.clone_order.clone(),
                aliases: recipe.aliases.clone(),
                known_values: recipe.known_values.clone(),
            };
            if recipe.non_dominating_control_merges.is_empty() {
                if !loads_are_stable_at(eu, cfg, definitions, sink.0, &recipe.load_frontier) {
                    return None;
                }
                sink_cases.push(executable);
                continue;
            }
            if context.selected_case.is_none() || recipe.non_dominating_control_merges.len() != 1 {
                return None;
            }
            let merge = *recipe.non_dominating_control_merges.first()?;
            let insertion_block = *parameter_blocks.get(&merge)?;
            if !recipe_is_available_at_entry(
                eu,
                &recipe,
                merge,
                insertion_block,
                definitions,
                parameter_blocks,
                cfg,
            ) || !loads_are_stable_at(
                eu,
                cfg,
                definitions,
                insertion_block,
                &recipe.load_frontier,
            ) {
                return None;
            }
            let guard = exact_case_guard(
                eu,
                origin,
                group.selector,
                context.selected_case.as_ref()?,
                constants,
            )?;
            if !value_available_at_block_entry(
                eu,
                guard,
                insertion_block,
                definitions,
                parameter_blocks,
                cfg,
            ) {
                return None;
            }
            path_local_exits.push(PathLocalEffectExitPlan {
                sink,
                continuation: *continuation,
                insertion_block,
                guard,
                recipe: executable,
            });
        }
        if !sink_cases.iter().any(|case| case.selected_case.is_none()) {
            return None;
        }
        sinks.push(EffectSinkDispatchPlan {
            sink,
            continuation: *continuation,
            cases: sink_cases,
        });
    }
    path_local_exits.sort_unstable_by_key(|exit| {
        (
            exit.insertion_block,
            exit.recipe.selected_case.clone(),
            exit.sink,
        )
    });
    if path_local_exits
        .windows(2)
        .any(|pair| pair[0].insertion_block == pair[1].insertion_block)
    {
        return None;
    }
    Some(EffectCaseRewritePlan {
        origin,
        selector: group.selector,
        explicit_cases: group.cases.into_iter().collect(),
        sinks,
        path_local_exits,
        estimated_saving,
    })
}

#[allow(clippy::too_many_arguments)]
fn recipe_is_available_at_entry(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    recipe: &SinkRecipe,
    merge: RegisterId,
    insertion_block: BlockId,
    definitions: &HashMap<RegisterId, DefinitionSite>,
    parameter_blocks: &HashMap<RegisterId, BlockId>,
    cfg: &SirCfg,
) -> bool {
    parameter_blocks.get(&merge) == Some(&insertion_block)
        && recipe
            .non_dominating_control_merges
            .iter()
            .all(|candidate| *candidate == merge)
        && recipe
            .dominating_ssa_frontier
            .iter()
            .chain(&recipe.control_merges)
            .chain(&recipe.external_frontier)
            .all(|&register| {
                value_available_at_block_entry(
                    eu,
                    register,
                    insertion_block,
                    definitions,
                    parameter_blocks,
                    cfg,
                )
            })
        && recipe.load_frontier.iter().all(|load| {
            let Some(site) = definitions.get(load) else {
                return false;
            };
            instruction_uses(&eu.blocks[&site.block].instructions[site.index])
                .into_iter()
                .all(|register| {
                    value_available_at_block_entry(
                        eu,
                        register,
                        insertion_block,
                        definitions,
                        parameter_blocks,
                        cfg,
                    )
                })
        })
}

fn loads_are_stable_at(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &SirCfg,
    definitions: &HashMap<RegisterId, DefinitionSite>,
    target: BlockId,
    loads: &BTreeSet<RegisterId>,
) -> bool {
    let mut by_region = BTreeMap::<u32, HashSet<RegisterId>>::new();
    for &register in loads {
        let Some(site) = definitions.get(&register) else {
            return false;
        };
        let SIRInstruction::Load(_, address, ..) = &eu.blocks[&site.block].instructions[site.index]
        else {
            return false;
        };
        by_region
            .entry(address.region)
            .or_default()
            .insert(register);
    }
    let states = by_region
        .iter()
        .map(|(&region, selected)| {
            StateSsa::analyze_selected_loads_two_state(eu, cfg, region, selected)
                .ok()
                .map(|state| (region, state))
        })
        .collect::<Option<BTreeMap<_, _>>>();
    let Some(states) = states else {
        return false;
    };
    loads.iter().all(|&register| {
        load_frontier_version(eu, definitions, &states, register, target)
            == LoadFrontierVersion::Stable
    })
}

fn exact_case_guard(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    origin: BlockId,
    selector: RegisterId,
    selected: &BigUint,
    constants: &HashMap<RegisterId, SIRValue>,
) -> Option<RegisterId> {
    let block = eu.blocks.get(&origin)?;
    let (mut current, mut index) =
        block
            .instructions
            .iter()
            .enumerate()
            .find_map(|(index, instruction)| {
                let SIRInstruction::Binary(
                    destination,
                    lhs,
                    BinaryOp::Eq | BinaryOp::EqWildcard,
                    rhs,
                ) = instruction
                else {
                    return None;
                };
                let constant = if *lhs == selector {
                    constants.get(rhs)
                } else if *rhs == selector {
                    constants.get(lhs)
                } else {
                    None
                }?;
                (constant.mask.is_zero() && &constant.payload == selected)
                    .then_some((*destination, index + 1))
            })?;
    if eu.register_map.get(&current).map(|ty| ty.width()) != Some(1) {
        return None;
    }
    while let Some(SIRInstruction::Unary(destination, operation, source)) =
        block.instructions.get(index)
    {
        if *source != current
            || !matches!(
                operation,
                UnaryOp::Ident | UnaryOp::Or | UnaryOp::ToTwoState
            )
            || eu.register_map.get(destination).map(|ty| ty.width()) != Some(1)
        {
            break;
        }
        current = *destination;
        index += 1;
        if *operation == UnaryOp::ToTwoState {
            return Some(*destination);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{AbsoluteAddr, BasicBlock, InstanceId, RegisterType, SIROffset, STABLE_REGION};
    use veryl_analyzer::ir::VarId;

    fn bit(width: usize) -> RegisterType {
        RegisterType::Bit {
            width,
            signed: false,
        }
    }

    fn logic(width: usize) -> RegisterType {
        RegisterType::Logic { width }
    }

    fn address(variable: u32) -> RegionedAbsoluteAddr {
        RegionedAbsoluteAddr::from_absolute_addr(
            STABLE_REGION,
            AbsoluteAddr {
                instance_id: InstanceId(0),
                var_id: VarId::from_raw(variable),
            },
        )
    }

    #[test]
    fn exact_case_guard_requires_the_adjacent_one_bit_normalization_chain() {
        let selector = RegisterId(0);
        let selected = RegisterId(1);
        let comparison = RegisterId(2);
        let reduction = RegisterId(3);
        let normalized = RegisterId(4);
        let unrelated = RegisterId(5);
        let block = BasicBlock {
            id: BlockId(0),
            params: vec![selector],
            instructions: vec![
                SIRInstruction::Imm(selected, SIRValue::new(3u8)),
                SIRInstruction::Binary(comparison, selector, BinaryOp::EqWildcard, selected),
                SIRInstruction::Unary(reduction, UnaryOp::Or, comparison),
                SIRInstruction::Unary(normalized, UnaryOp::ToTwoState, reduction),
            ],
            terminator: SIRTerminator::Return,
        };
        let mut eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [(BlockId(0), block)].into_iter().collect(),
            register_map: [
                (selector, logic(7)),
                (selected, logic(7)),
                (comparison, logic(1)),
                (reduction, logic(1)),
                (normalized, bit(1)),
                (unrelated, logic(1)),
            ]
            .into_iter()
            .collect(),
        };
        let constants = collect_exact_constants(&eu);

        assert_eq!(
            exact_case_guard(&eu, BlockId(0), selector, &BigUint::from(3u8), &constants,),
            Some(normalized)
        );

        eu.blocks
            .get_mut(&BlockId(0))
            .unwrap()
            .instructions
            .insert(2, SIRInstruction::Imm(unrelated, SIRValue::new(0u8)));
        assert_eq!(
            exact_case_guard(&eu, BlockId(0), selector, &BigUint::from(3u8), &constants,),
            None
        );

        eu.blocks
            .get_mut(&BlockId(0))
            .unwrap()
            .instructions
            .remove(2);
        eu.register_map.insert(comparison, logic(7));
        assert_eq!(
            exact_case_guard(&eu, BlockId(0), selector, &BigUint::from(3u8), &constants,),
            None
        );
    }

    #[test]
    fn exact_selector_specialization_skips_every_untaken_payload() {
        let selector = RegisterId(0);
        let zero = RegisterId(1);
        let one = RegisterId(2);
        let guard_zero = RegisterId(3);
        let guard_one = RegisterId(4);
        let payload_zero = RegisterId(5);
        let payload_one = RegisterId(6);
        let selected_zero = RegisterId(7);
        let result = RegisterId(8);
        let block = BasicBlock {
            id: BlockId(0),
            params: vec![selector],
            instructions: vec![
                SIRInstruction::Imm(zero, SIRValue::new(0u8)),
                SIRInstruction::Imm(one, SIRValue::new(1u8)),
                SIRInstruction::Binary(guard_zero, selector, BinaryOp::Eq, zero),
                SIRInstruction::Binary(guard_one, selector, BinaryOp::Eq, one),
                SIRInstruction::Binary(payload_zero, selector, BinaryOp::Mul, selector),
                SIRInstruction::Binary(payload_one, payload_zero, BinaryOp::Add, one),
                SIRInstruction::Mux(selected_zero, guard_zero, payload_zero, zero),
                SIRInstruction::Mux(result, guard_one, payload_one, selected_zero),
            ],
            terminator: SIRTerminator::Jump(BlockId(1), vec![result]),
        };
        let exit = BasicBlock {
            id: BlockId(1),
            params: vec![RegisterId(9)],
            instructions: vec![SIRInstruction::Imm(RegisterId(10), SIRValue::new(3u8))],
            terminator: SIRTerminator::Return,
        };
        let eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [(BlockId(0), block), (BlockId(1), exit)]
                .into_iter()
                .collect(),
            register_map: [
                (selector, bit(2)),
                (zero, bit(2)),
                (one, bit(2)),
                (guard_zero, bit(1)),
                (guard_one, bit(1)),
                (payload_zero, bit(2)),
                (payload_one, bit(2)),
                (selected_zero, bit(2)),
                (result, bit(2)),
                (RegisterId(9), bit(2)),
                (RegisterId(10), bit(2)),
            ]
            .into_iter()
            .collect(),
        };
        let report = analyze(&eu, &[(BlockId(0), 10), (BlockId(1), 10)]);
        assert_eq!(report.profitable_regions, 1);
        let fact = &report.facts[0];
        assert_eq!(fact.explicit_cases, 2);
        assert!(fact.minimum_skipped_instructions >= 2);
        assert!(fact.worst_case_cost < fact.baseline_cost);
        assert!(
            report.profile_weighted_selected_cost > (fact.baseline_cost as u128).saturating_mul(10)
        );
    }

    #[test]
    fn selector_closure_crosses_block_parameters_and_names_exact_effect_sinks() {
        let selector = RegisterId(0);
        let zero = RegisterId(1);
        let one = RegisterId(2);
        let guard_zero = RegisterId(3);
        let guard_one = RegisterId(4);
        let first_value = RegisterId(5);
        let selected = RegisterId(6);
        let parameter = RegisterId(7);
        let result = RegisterId(8);
        let origin = BasicBlock {
            id: BlockId(0),
            params: vec![selector, first_value],
            instructions: vec![
                SIRInstruction::Imm(zero, SIRValue::new(0u8)),
                SIRInstruction::Imm(one, SIRValue::new(1u8)),
                SIRInstruction::Binary(guard_zero, selector, BinaryOp::Eq, zero),
                SIRInstruction::Binary(guard_one, selector, BinaryOp::Eq, one),
                SIRInstruction::Mux(selected, guard_zero, first_value, zero),
            ],
            terminator: SIRTerminator::Jump(BlockId(1), vec![selected]),
        };
        let sink = BasicBlock {
            id: BlockId(1),
            params: vec![parameter],
            instructions: vec![
                SIRInstruction::Mux(result, guard_one, one, parameter),
                SIRInstruction::Store(
                    address(0),
                    SIROffset::Static(0),
                    2,
                    result,
                    Vec::new(),
                    Vec::new(),
                ),
            ],
            terminator: SIRTerminator::Return,
        };
        let eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [(BlockId(0), origin), (BlockId(1), sink)]
                .into_iter()
                .collect(),
            register_map: [
                (selector, bit(2)),
                (zero, bit(2)),
                (one, bit(2)),
                (guard_zero, bit(1)),
                (guard_one, bit(1)),
                (first_value, bit(2)),
                (selected, bit(2)),
                (parameter, bit(2)),
                (result, bit(2)),
            ]
            .into_iter()
            .collect(),
        };

        let report = analyze(&eu, &[(BlockId(0), 10)]);
        let fact = &report.facts[0];
        assert_eq!(fact.cross_block_affected_blocks, 2);
        assert_eq!(fact.cross_block_effect_sinks, 1);
        assert_eq!(fact.cross_block_effect_sites, vec![(BlockId(1), 1)]);
        assert_eq!(fact.sink_recipes.len(), 1);
        let recipe = &fact.sink_recipes[0];
        assert_eq!(recipe.sink, (BlockId(1), 1));
        assert_eq!(recipe.source, result);
        assert_eq!(recipe.recipe_instructions.len(), 4);
        assert_eq!(recipe.recipe_blocks, vec![BlockId(0), BlockId(1)]);
        assert_eq!(recipe.constant_frontier.len(), 2);
        assert_eq!(recipe.external_frontier.len(), 2);
        assert_eq!(recipe.control_merges, vec![parameter]);
        assert!(recipe.loop_cutoffs.is_empty());
        assert_eq!(recipe.case_summary.alternatives, 3);
        assert_eq!(recipe.case_summary.reachable_alternatives, 3);
        assert_eq!(recipe.case_summary.maximum_instructions, 0);
        assert_eq!(recipe.case_summary.maximum_dominating_ssa_frontier, 1);
        assert_eq!(recipe.case_summary.maximum_external_frontier, 0);
    }

    #[test]
    fn sink_recipe_stops_at_executable_load_and_shared_ssa_frontiers() {
        let selector = RegisterId(0);
        let zero = RegisterId(1);
        let one = RegisterId(2);
        let guard_zero = RegisterId(3);
        let guard_one = RegisterId(4);
        let external = RegisterId(5);
        let shared = RegisterId(6);
        let loaded = RegisterId(7);
        let selected = RegisterId(8);
        let parameter = RegisterId(9);
        let result = RegisterId(10);
        let preheader = BasicBlock {
            id: BlockId(2),
            params: vec![selector, external],
            instructions: Vec::new(),
            terminator: SIRTerminator::Jump(BlockId(0), Vec::new()),
        };
        let origin = BasicBlock {
            id: BlockId(0),
            params: Vec::new(),
            instructions: vec![
                SIRInstruction::Imm(zero, SIRValue::new(0u8)),
                SIRInstruction::Imm(one, SIRValue::new(1u8)),
                SIRInstruction::Binary(guard_zero, selector, BinaryOp::Eq, zero),
                SIRInstruction::Binary(guard_one, selector, BinaryOp::Eq, one),
                SIRInstruction::Binary(shared, external, BinaryOp::Add, one),
                SIRInstruction::Load(loaded, address(1), SIROffset::Static(0), 2),
                SIRInstruction::Mux(selected, guard_zero, loaded, shared),
                SIRInstruction::Store(
                    address(2),
                    SIROffset::Static(0),
                    2,
                    shared,
                    Vec::new(),
                    Vec::new(),
                ),
            ],
            terminator: SIRTerminator::Jump(BlockId(1), vec![selected]),
        };
        let sink = BasicBlock {
            id: BlockId(1),
            params: vec![parameter],
            instructions: vec![
                SIRInstruction::Mux(result, guard_one, one, parameter),
                SIRInstruction::Store(
                    address(0),
                    SIROffset::Static(0),
                    2,
                    result,
                    Vec::new(),
                    Vec::new(),
                ),
            ],
            terminator: SIRTerminator::Jump(BlockId(3), Vec::new()),
        };
        let continuation = BasicBlock {
            id: BlockId(3),
            params: Vec::new(),
            instructions: Vec::new(),
            terminator: SIRTerminator::Return,
        };
        let eu = ExecutionUnit {
            entry_block_id: BlockId(2),
            blocks: [
                (BlockId(0), origin),
                (BlockId(1), sink),
                (BlockId(2), preheader),
                (BlockId(3), continuation),
            ]
            .into_iter()
            .collect(),
            register_map: [
                (selector, bit(2)),
                (zero, bit(2)),
                (one, bit(2)),
                (guard_zero, bit(1)),
                (guard_one, bit(1)),
                (external, bit(2)),
                (shared, bit(2)),
                (loaded, bit(2)),
                (selected, bit(2)),
                (parameter, bit(2)),
                (result, bit(2)),
            ]
            .into_iter()
            .collect(),
        };

        let report = analyze(&eu, &[(BlockId(0), 10)]);
        let recipe = &report.facts[0].sink_recipes[0];
        assert_eq!(recipe.load_frontier, vec![loaded]);
        assert_eq!(recipe.shared_ssa_frontier, vec![shared]);
        assert_eq!(recipe.entering_edges, vec![(BlockId(2), BlockId(0))]);
        assert_eq!(recipe.continuations, vec![BlockId(3)]);
        assert!(recipe.effect.contains("src_reg = 10"));
        assert_eq!(recipe.case_summary.alternatives, 3);
        assert_eq!(recipe.case_summary.maximum_instructions, 1);
        assert_eq!(recipe.case_summary.maximum_load_frontier, 1);
        assert_eq!(recipe.case_summary.maximum_dominating_ssa_frontier, 1);
        assert_eq!(recipe.case_summary.maximum_external_frontier, 0);
        assert_eq!(recipe.case_summary.stable_load_frontier, vec![loaded]);
        assert!(recipe.case_summary.unstable_load_frontier.is_empty());
        assert_eq!(recipe.case_summary.maximum_unstable_loads_per_case, 0);
    }

    #[test]
    fn case_recipe_preserves_unknown_outer_control_as_a_merge_leaf() {
        let selector = RegisterId(0);
        let outer = RegisterId(1);
        let true_value = RegisterId(2);
        let false_value = RegisterId(3);
        let zero = RegisterId(4);
        let one = RegisterId(5);
        let guard_zero = RegisterId(6);
        let guard_one = RegisterId(7);
        let parameter = RegisterId(8);
        let result = RegisterId(9);
        let origin = BasicBlock {
            id: BlockId(0),
            params: vec![selector, outer, true_value, false_value],
            instructions: vec![
                SIRInstruction::Imm(zero, SIRValue::new(0u8)),
                SIRInstruction::Imm(one, SIRValue::new(1u8)),
                SIRInstruction::Binary(guard_zero, selector, BinaryOp::Eq, zero),
                SIRInstruction::Binary(guard_one, selector, BinaryOp::Eq, one),
            ],
            terminator: SIRTerminator::Branch {
                cond: outer,
                true_block: (BlockId(1), Vec::new()),
                false_block: (BlockId(2), Vec::new()),
            },
        };
        let true_path = BasicBlock {
            id: BlockId(1),
            params: Vec::new(),
            instructions: Vec::new(),
            terminator: SIRTerminator::Jump(BlockId(3), vec![true_value]),
        };
        let false_path = BasicBlock {
            id: BlockId(2),
            params: Vec::new(),
            instructions: Vec::new(),
            terminator: SIRTerminator::Jump(BlockId(3), vec![false_value]),
        };
        let sink = BasicBlock {
            id: BlockId(3),
            params: vec![parameter],
            instructions: vec![
                SIRInstruction::Mux(result, guard_zero, parameter, zero),
                SIRInstruction::Store(
                    address(0),
                    SIROffset::Static(0),
                    2,
                    result,
                    Vec::new(),
                    Vec::new(),
                ),
            ],
            terminator: SIRTerminator::Return,
        };
        let eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [
                (BlockId(0), origin),
                (BlockId(1), true_path),
                (BlockId(2), false_path),
                (BlockId(3), sink),
            ]
            .into_iter()
            .collect(),
            register_map: [
                (selector, bit(2)),
                (outer, bit(1)),
                (true_value, bit(2)),
                (false_value, bit(2)),
                (zero, bit(2)),
                (one, bit(2)),
                (guard_zero, bit(1)),
                (guard_one, bit(1)),
                (parameter, bit(2)),
                (result, bit(2)),
            ]
            .into_iter()
            .collect(),
        };

        let report = analyze(&eu, &[(BlockId(0), 10)]);
        let summary = &report.facts[0].sink_recipes[0].case_summary;
        assert_eq!(summary.all_control_merges, vec![parameter]);
        assert!(summary.all_external_frontier.is_empty());
    }

    #[test]
    fn case_recipe_does_not_treat_a_path_local_merge_as_a_sink_leaf() {
        let selector = RegisterId(0);
        let outer = RegisterId(1);
        let true_value = RegisterId(2);
        let false_value = RegisterId(3);
        let fallback = RegisterId(4);
        let zero = RegisterId(5);
        let one = RegisterId(6);
        let guard_zero = RegisterId(7);
        let guard_one = RegisterId(8);
        let path_local = RegisterId(9);
        let joined = RegisterId(10);
        let combined = RegisterId(11);
        let result = RegisterId(12);
        let loaded = RegisterId(13);
        let origin = BasicBlock {
            id: BlockId(0),
            params: vec![selector, outer, true_value, false_value, fallback],
            instructions: vec![
                SIRInstruction::Imm(zero, SIRValue::new(0u8)),
                SIRInstruction::Imm(one, SIRValue::new(1u8)),
                SIRInstruction::Binary(guard_zero, selector, BinaryOp::Eq, zero),
                SIRInstruction::Binary(guard_one, selector, BinaryOp::Eq, one),
                SIRInstruction::Load(loaded, address(1), SIROffset::Static(0), 2),
            ],
            terminator: SIRTerminator::Branch {
                cond: guard_zero,
                true_block: (BlockId(1), Vec::new()),
                false_block: (BlockId(2), Vec::new()),
            },
        };
        let selected_path = BasicBlock {
            id: BlockId(1),
            params: Vec::new(),
            instructions: Vec::new(),
            terminator: SIRTerminator::Branch {
                cond: outer,
                true_block: (BlockId(3), Vec::new()),
                false_block: (BlockId(4), Vec::new()),
            },
        };
        let default_path = BasicBlock {
            id: BlockId(2),
            params: Vec::new(),
            instructions: Vec::new(),
            terminator: SIRTerminator::Jump(BlockId(6), vec![fallback]),
        };
        let true_path = BasicBlock {
            id: BlockId(3),
            params: Vec::new(),
            instructions: Vec::new(),
            terminator: SIRTerminator::Jump(BlockId(5), vec![true_value]),
        };
        let false_path = BasicBlock {
            id: BlockId(4),
            params: Vec::new(),
            instructions: Vec::new(),
            terminator: SIRTerminator::Jump(BlockId(5), vec![false_value]),
        };
        let path_merge = BasicBlock {
            id: BlockId(5),
            params: vec![path_local],
            instructions: Vec::new(),
            terminator: SIRTerminator::Jump(BlockId(6), vec![path_local]),
        };
        let sink = BasicBlock {
            id: BlockId(6),
            params: vec![joined],
            instructions: vec![
                SIRInstruction::Binary(combined, joined, BinaryOp::Add, loaded),
                SIRInstruction::Mux(result, guard_one, one, combined),
                SIRInstruction::Store(
                    address(0),
                    SIROffset::Static(0),
                    2,
                    result,
                    Vec::new(),
                    Vec::new(),
                ),
            ],
            terminator: SIRTerminator::Return,
        };
        let eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [
                (BlockId(0), origin),
                (BlockId(1), selected_path),
                (BlockId(2), default_path),
                (BlockId(3), true_path),
                (BlockId(4), false_path),
                (BlockId(5), path_merge),
                (BlockId(6), sink),
            ]
            .into_iter()
            .collect(),
            register_map: [
                (selector, bit(2)),
                (outer, bit(1)),
                (true_value, bit(2)),
                (false_value, bit(2)),
                (fallback, bit(2)),
                (zero, bit(2)),
                (one, bit(2)),
                (guard_zero, bit(1)),
                (guard_one, bit(1)),
                (path_local, bit(2)),
                (joined, bit(2)),
                (combined, bit(2)),
                (result, bit(2)),
                (loaded, bit(2)),
            ]
            .into_iter()
            .collect(),
        };

        let report = analyze(&eu, &[(BlockId(0), 10)]);
        let summary = &report.facts[0].sink_recipes[0].case_summary;
        assert_eq!(summary.all_non_dominating_control_merges, vec![path_local]);
        assert_eq!(
            summary.non_dominating_control_merge_cases,
            vec![("0".to_owned(), vec![path_local])]
        );
        assert_eq!(
            summary.path_local_placements,
            vec![PathLocalPlacementFact {
                case: "0".to_owned(),
                merge: path_local,
                insertion_block: BlockId(5),
                load_frontier: vec![loaded],
                unavailable_ssa_frontier: Vec::new(),
                stable_load_frontier: vec![loaded],
                unstable_load_frontier: Vec::new(),
                unversioned_load_frontier: Vec::new(),
            }]
        );
        assert!(summary.all_control_merges.is_empty());
    }

    #[test]
    fn case_recipe_rejects_reloading_a_changed_state_version() {
        let selector = RegisterId(0);
        let replacement = RegisterId(1);
        let zero = RegisterId(2);
        let one = RegisterId(3);
        let guard_zero = RegisterId(4);
        let guard_one = RegisterId(5);
        let loaded = RegisterId(6);
        let selected = RegisterId(7);
        let parameter = RegisterId(8);
        let result = RegisterId(9);
        let origin = BasicBlock {
            id: BlockId(0),
            params: vec![selector, replacement],
            instructions: vec![
                SIRInstruction::Imm(zero, SIRValue::new(0u8)),
                SIRInstruction::Imm(one, SIRValue::new(1u8)),
                SIRInstruction::Binary(guard_zero, selector, BinaryOp::Eq, zero),
                SIRInstruction::Binary(guard_one, selector, BinaryOp::Eq, one),
                SIRInstruction::Load(loaded, address(1), SIROffset::Static(0), 2),
                SIRInstruction::Mux(selected, guard_zero, loaded, zero),
                SIRInstruction::Store(
                    address(1),
                    SIROffset::Static(0),
                    2,
                    replacement,
                    Vec::new(),
                    Vec::new(),
                ),
            ],
            terminator: SIRTerminator::Jump(BlockId(1), vec![selected]),
        };
        let sink = BasicBlock {
            id: BlockId(1),
            params: vec![parameter],
            instructions: vec![
                SIRInstruction::Mux(result, guard_one, one, parameter),
                SIRInstruction::Store(
                    address(0),
                    SIROffset::Static(0),
                    2,
                    result,
                    Vec::new(),
                    Vec::new(),
                ),
            ],
            terminator: SIRTerminator::Return,
        };
        let eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [(BlockId(0), origin), (BlockId(1), sink)]
                .into_iter()
                .collect(),
            register_map: [
                (selector, bit(2)),
                (replacement, bit(2)),
                (zero, bit(2)),
                (one, bit(2)),
                (guard_zero, bit(1)),
                (guard_one, bit(1)),
                (loaded, bit(2)),
                (selected, bit(2)),
                (parameter, bit(2)),
                (result, bit(2)),
            ]
            .into_iter()
            .collect(),
        };

        let report = analyze(&eu, &[(BlockId(0), 10)]);
        let summary = &report.facts[0].sink_recipes[0].case_summary;
        assert!(summary.stable_load_frontier.is_empty());
        assert_eq!(summary.unstable_load_frontier, vec![loaded]);
        assert_eq!(summary.maximum_unstable_loads_per_case, 1);
        assert!(summary.unversioned_load_frontier.is_empty());
    }

    #[test]
    fn sink_recipe_pair_separates_shared_and_path_local_work() {
        let recipe = |sink: (BlockId, usize),
                      source: RegisterId,
                      instructions: Vec<(BlockId, usize)>,
                      frontier: RegisterId| SelectorSinkRecipeFact {
            sink,
            effect: format!("store from r{}", source.0),
            publication: "same exact range".to_string(),
            source,
            recipe_instructions: instructions,
            recipe_blocks: vec![BlockId(0), sink.0],
            selector_control_blocks: vec![BlockId(0)],
            entering_edges: Vec::new(),
            continuations: vec![BlockId(3)],
            constant_frontier: vec![frontier],
            load_frontier: Vec::new(),
            shared_ssa_frontier: Vec::new(),
            external_frontier: Vec::new(),
            control_merges: Vec::new(),
            loop_cutoffs: Vec::new(),
            case_summary: SelectorSinkCaseSummaryFact::default(),
        };
        let left = recipe(
            (BlockId(1), 0),
            RegisterId(10),
            vec![(BlockId(0), 0), (BlockId(1), 0)],
            RegisterId(20),
        );
        let mut right = recipe(
            (BlockId(2), 0),
            RegisterId(11),
            vec![(BlockId(0), 0), (BlockId(2), 0), (BlockId(2), 1)],
            RegisterId(20),
        );
        right.recipe_blocks = vec![BlockId(0), BlockId(2)];

        let pairs = compare_sink_recipes(&[left, right], None);

        assert_eq!(
            pairs,
            vec![SelectorSinkRecipePairFact {
                left_sink: (BlockId(1), 0),
                right_sink: (BlockId(2), 0),
                same_publication: true,
                same_continuation: true,
                common_dominator: None,
                common_postdominator: None,
                common_instructions: 1,
                left_only_instructions: 1,
                right_only_instructions: 2,
                common_blocks: 1,
                common_frontier_values: 1,
            }]
        );
    }
}
