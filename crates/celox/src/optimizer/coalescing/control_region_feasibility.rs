//! Analysis-only reverse-if-conversion gate for profile-selected SIR blocks.
//!
//! HDL case statements can reach fused SIR as one large superblock: every
//! selector arm is evaluated, then Muxes select the observable values. This
//! probe specializes such a block for each exact selector value and computes
//! the closed backward slice which would remain. It does not rewrite SIR.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;

use num_bigint::BigUint;
use num_traits::Zero;

use super::cost_model::estimate_clif_cost;
use super::shared::def_reg;
use super::sir_analysis::{UseSite, collect_uses, instruction_uses};
use crate::HashMap;
use crate::ir::cfg::SirCfg;
use crate::ir::{
    BinaryOp, BlockId, ExecutionUnit, RegionedAbsoluteAddr, RegisterId, SIRInstruction,
    SIRTerminator, SIRValue, UnaryOp,
};

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
}

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

impl SelectorRegionFact {
    fn worst_saving(&self) -> usize {
        self.baseline_cost.saturating_sub(self.worst_case_cost)
    }

    fn profile_weighted_worst_saving(&self) -> u128 {
        (self.worst_saving() as u128).saturating_mul(self.samples as u128)
    }
}

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

impl ControlRegionFeasibilityReport {
    pub(crate) fn detail_lines(&self) -> impl Iterator<Item = String> + '_ {
        self.facts.iter().map(|fact| {
            let sink_recipes = fact
                .sink_recipes
                .iter()
                .map(SelectorSinkRecipeFact::summary)
                .collect::<Vec<_>>()
                .join(" | ");
            let sink_recipe_pairs = fact
                .sink_recipe_pairs
                .iter()
                .map(SelectorSinkRecipePairFact::summary)
                .collect::<Vec<_>>()
                .join(" | ");
            format!(
                "block=b{} samples={} selector=r{} cases={} instructions={} \
                 baseline_cost={} worst_case_cost={} mean_case_cost={} \
                 minimum_skipped_instructions={} maximum_skipped_instructions={} \
                 live_outputs={} effects={} cross_block_affected_instructions={} \
                 cross_block_affected_blocks={} cross_block_effect_sinks={} \
                 cross_block_branch_conditions={} cross_block_effect_sites={:?} \
                 sink_recipes=[{}] \
                 sink_recipe_pairs=[{}] \
                 weighted_worst_saving={}",
                fact.block.0,
                fact.samples,
                fact.selector.0,
                fact.explicit_cases,
                fact.block_instructions,
                fact.baseline_cost,
                fact.worst_case_cost,
                fact.mean_case_cost,
                fact.minimum_skipped_instructions,
                fact.maximum_skipped_instructions,
                fact.live_outputs,
                fact.effects,
                fact.cross_block_affected_instructions,
                fact.cross_block_affected_blocks,
                fact.cross_block_effect_sinks,
                fact.cross_block_branch_conditions,
                fact.cross_block_effect_sites,
                sink_recipes,
                sink_recipe_pairs,
                fact.profile_weighted_worst_saving(),
            )
        })
    }

    pub(crate) fn recipe_detail_lines(&self) -> impl Iterator<Item = String> + '_ {
        self.facts.iter().flat_map(|fact| {
            fact.sink_recipes.iter().map(move |recipe| {
                format!(
                    "origin=b{} selector=r{} recipe={recipe:?}",
                    fact.block.0, fact.selector.0
                )
            })
        })
    }
}

impl SelectorSinkRecipeFact {
    fn summary(&self) -> String {
        format!(
            "sink=b{}:{} source=r{} effect={:?} publication={:?} recipe_instructions={} \
             recipe_blocks={} selector_control_blocks={} entering_edges={} \
             continuations={:?} frontier_constants={} frontier_loads={} \
             frontier_shared_ssa={} frontier_external={} control_merges={} \
             loop_cutoffs={}",
            self.sink.0.0,
            self.sink.1,
            self.source.0,
            self.effect,
            self.publication,
            self.recipe_instructions.len(),
            self.recipe_blocks.len(),
            self.selector_control_blocks.len(),
            self.entering_edges.len(),
            self.continuations,
            self.constant_frontier.len(),
            self.load_frontier.len(),
            self.shared_ssa_frontier.len(),
            self.external_frontier.len(),
            self.control_merges.len(),
            self.loop_cutoffs.len(),
        )
    }
}

impl SelectorSinkRecipePairFact {
    fn summary(&self) -> String {
        format!(
            "sinks=b{}:{}/b{}:{} same_publication={} same_continuation={} \
             common_instructions={} left_only_instructions={} \
             right_only_instructions={} common_blocks={} \
             common_frontier_values={} common_dominator={:?} \
             common_postdominator={:?}",
            self.left_sink.0.0,
            self.left_sink.1,
            self.right_sink.0.0,
            self.right_sink.1,
            self.same_publication,
            self.same_continuation,
            self.common_instructions,
            self.left_only_instructions,
            self.right_only_instructions,
            self.common_blocks,
            self.common_frontier_values,
            self.common_dominator,
            self.common_postdominator,
        )
    }
}

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
    entering_edges: BTreeSet<(BlockId, BlockId)>,
    continuations: BTreeSet<BlockId>,
    constant_frontier: BTreeSet<RegisterId>,
    load_frontier: BTreeSet<RegisterId>,
    shared_ssa_frontier: BTreeSet<RegisterId>,
    external_frontier: BTreeSet<RegisterId>,
    control_merges: BTreeSet<RegisterId>,
    loop_cutoffs: BTreeSet<RegisterId>,
}

pub(crate) fn analyze(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    profile_blocks: &[(BlockId, u64)],
) -> ControlRegionFeasibilityReport {
    let constants = collect_exact_constants(eu);
    let uses = collect_uses(eu);
    let edge_param_uses = collect_edge_param_uses(eu);
    let definitions = collect_definition_sites(eu);
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
    report
}

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
    let sink_recipes = cross_block
        .effect_sinks
        .iter()
        .filter_map(|&sink| {
            analyze_sink_recipe(
                eu,
                sink,
                &cross_block,
                uses,
                definitions,
                incoming_values,
                predecessors,
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
fn analyze_sink_recipe(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    sink: (BlockId, usize),
    selector_closure: &CrossBlockClosure,
    uses: &HashMap<RegisterId, Vec<UseSite>>,
    definitions: &HashMap<RegisterId, DefinitionSite>,
    incoming_values: &HashMap<RegisterId, Vec<IncomingValue>>,
    predecessors: &HashMap<BlockId, Vec<BlockId>>,
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
    })
}

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
