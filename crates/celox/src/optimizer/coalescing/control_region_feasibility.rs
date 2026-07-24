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
            format!(
                "block=b{} samples={} selector=r{} cases={} instructions={} \
                 baseline_cost={} worst_case_cost={} mean_case_cost={} \
                 minimum_skipped_instructions={} maximum_skipped_instructions={} \
                 live_outputs={} effects={} weighted_worst_saving={}",
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
                fact.profile_weighted_worst_saving(),
            )
        })
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

#[derive(Debug)]
struct SpecializedBlock {
    cost: usize,
    needed_instructions: usize,
}

pub(crate) fn analyze(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    profile_blocks: &[(BlockId, u64)],
) -> ControlRegionFeasibilityReport {
    let constants = collect_exact_constants(eu);
    let uses = collect_uses(eu);
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
    baseline_cost: usize,
    live_outputs: usize,
    effects: usize,
) -> Option<SelectorRegionFact> {
    // A binary condition belongs to the ordinary branch recovery path.
    if group.cases.len() < 2 {
        return None;
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{BasicBlock, RegisterType};

    fn bit(width: usize) -> RegisterType {
        RegisterType::Bit {
            width,
            signed: false,
        }
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
}
