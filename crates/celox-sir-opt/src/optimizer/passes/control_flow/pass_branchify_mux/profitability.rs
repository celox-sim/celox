//! Branch probability and runtime-work cost estimates.

use super::*;

pub(super) fn branch_is_profitable(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block: &BasicBlock<RegionedAbsoluteAddr>,
    plan: &BranchifyPlan,
    def_blocks: &HashMap<RegisterId, BlockId>,
    def_pos: &HashMap<RegisterId, usize>,
    live_through_chunks: Option<u128>,
) -> bool {
    // Restoring definitions to the head can only reduce skipped arm work.
    // Reject an unprofitable upper bound before walking/copying the block.
    // Without a cached suffix cost, zero remains a valid lower bound on
    // introduced work; the full check below still calculates the exact cost.
    let arm_cost = |defs: &[usize]| {
        defs.iter()
            .map(|&idx| branchified_instruction_cost(&block.instructions[idx], &eu.register_map))
            .sum()
    };
    let result_chunks = if plan.preserve_result {
        eu.register_map
            .get(&plan.dst)
            .map(|ty| ty.width().div_ceil(64).max(1))
            .unwrap_or(1) as u128
    } else {
        0
    };
    let upper_bound = BranchProfitability {
        true_arm_cost: arm_cost(&plan.true_defs),
        false_arm_cost: arm_cost(&plan.false_defs),
        removed_mux_cost: branchified_instruction_cost(
            &block.instructions[plan.mux_idx],
            &eu.register_map,
        ),
        probability: static_true_probability(block, def_pos, plan.cond),
        control_cost: BRANCH_CONTROL_COST,
        phi_copy_cost: result_chunks.saturating_mul(PHI_COPY_COST_PER_CHUNK),
        live_through_cost: live_through_chunks
            .unwrap_or(0)
            .saturating_mul(LIVE_THROUGH_COST_PER_CHUNK),
    };
    upper_bound.proves_expected_benefit()
        && branch_profitability(eu, block, plan, def_blocks, def_pos, live_through_chunks)
            .proves_expected_benefit()
}

pub(super) fn branch_profitability(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block: &BasicBlock<RegionedAbsoluteAddr>,
    plan: &BranchifyPlan,
    def_blocks: &HashMap<RegisterId, BlockId>,
    def_pos: &HashMap<RegisterId, usize>,
    cached_live_through_chunks: Option<u128>,
) -> BranchProfitability {
    let remove_defs = removable_defs_after_head_restore(block, plan, def_blocks);
    let arm_cost = |defs: &[usize]| {
        defs.iter()
            .filter(|idx| remove_defs.contains(idx))
            .map(|&idx| branchified_instruction_cost(&block.instructions[idx], &eu.register_map))
            .sum::<u128>()
    };
    let chunks_for = |value: RegisterId| {
        eu.register_map
            .get(&value)
            .map(|register| register.width().div_ceil(64).max(1))
            .unwrap_or(1) as u128
    };
    let result_chunks = if plan.preserve_result {
        chunks_for(plan.dst)
    } else {
        0
    };
    let live_through_chunks = cached_live_through_chunks.unwrap_or_else(|| {
        let suffix = block
            .instructions
            .iter()
            .enumerate()
            .skip(plan.mux_idx + 1)
            .filter(|(idx, _)| !remove_defs.contains(idx))
            .map(|(_, inst)| inst.clone())
            .collect::<Vec<_>>();
        let mut live_through = block_live_ins(&suffix, &terminator_uses(&block.terminator));
        live_through.retain(|value| *value != plan.dst);
        live_through.sort_unstable();
        live_through.dedup();

        live_through.into_iter().map(chunks_for).sum::<u128>()
    });

    BranchProfitability {
        true_arm_cost: arm_cost(&plan.true_defs),
        false_arm_cost: arm_cost(&plan.false_defs),
        removed_mux_cost: branchified_instruction_cost(
            &block.instructions[plan.mux_idx],
            &eu.register_map,
        ),
        probability: static_true_probability(block, def_pos, plan.cond),
        control_cost: BRANCH_CONTROL_COST,
        phi_copy_cost: result_chunks.saturating_mul(PHI_COPY_COST_PER_CHUNK),
        live_through_cost: live_through_chunks.saturating_mul(LIVE_THROUGH_COST_PER_CHUNK),
    }
}

pub(super) fn static_true_probability(
    block: &BasicBlock<RegionedAbsoluteAddr>,
    def_pos: &HashMap<RegisterId, usize>,
    cond: RegisterId,
) -> StaticBranchProbability {
    let mut current = cond;
    let mut inverted = false;
    let mut seen = HashSet::default();

    while seen.insert(current) {
        let Some(&idx) = def_pos.get(&current) else {
            break;
        };
        match &block.instructions[idx] {
            SIRInstruction::Unary(_, crate::ir::UnaryOp::LogicNot, inner) => {
                inverted = !inverted;
                current = *inner;
            }
            SIRInstruction::Unary(_, crate::ir::UnaryOp::Ident, inner) => {
                current = *inner;
            }
            SIRInstruction::Binary(
                _,
                lhs,
                op @ (crate::ir::BinaryOp::Eq
                | crate::ir::BinaryOp::Ne
                | crate::ir::BinaryOp::EqWildcard
                | crate::ir::BinaryOp::NeWildcard),
                rhs,
            ) if register_is_immediate(block, def_pos, *lhs)
                || register_is_immediate(block, def_pos, *rhs) =>
            {
                let equality = matches!(
                    op,
                    crate::ir::BinaryOp::Eq | crate::ir::BinaryOp::EqWildcard
                );
                let probability = if equality != inverted {
                    StaticBranchProbability::EQUALITY_TO_CONSTANT
                } else {
                    StaticBranchProbability::EQUALITY_TO_CONSTANT.inverted()
                };
                return probability;
            }
            _ => break,
        }
    }

    if inverted {
        StaticBranchProbability::EVEN.inverted()
    } else {
        StaticBranchProbability::EVEN
    }
}

fn register_is_immediate(
    block: &BasicBlock<RegionedAbsoluteAddr>,
    def_pos: &HashMap<RegisterId, usize>,
    register: RegisterId,
) -> bool {
    let mut current = register;
    let mut seen = HashSet::default();
    while seen.insert(current) {
        let Some(&idx) = def_pos.get(&current) else {
            return false;
        };
        match &block.instructions[idx] {
            SIRInstruction::Imm(..) => return true,
            SIRInstruction::Unary(_, crate::ir::UnaryOp::Ident, inner) => current = *inner,
            _ => return false,
        }
    }
    false
}

/// Estimated dynamic target work for an instruction that can be moved into a
/// branch arm.  This deliberately follows the same width/chunk model as
/// cost-directed SLT mux lowering instead of the CLIF-size estimator: the
/// decision is about runtime work skipped, not compiler IR expansion.
pub(super) fn branchified_instruction_cost(
    inst: &SIRInstruction<RegionedAbsoluteAddr>,
    register_map: &HashMap<RegisterId, crate::ir::RegisterType>,
) -> u128 {
    let register_width = |register: RegisterId| {
        register_map
            .get(&register)
            .map(crate::ir::RegisterType::width)
            .unwrap_or(64)
    };
    let chunks = |width: usize| width.div_ceil(64).max(1) as u128;

    match inst {
        SIRInstruction::Imm(dst, _) => chunks(register_width(*dst)),
        SIRInstruction::Binary(dst, lhs, op, rhs) => {
            let operand_chunks = chunks(
                register_width(*dst)
                    .max(register_width(*lhs))
                    .max(register_width(*rhs)),
            );
            match op {
                crate::ir::BinaryOp::And
                | crate::ir::BinaryOp::Or
                | crate::ir::BinaryOp::Xor
                | crate::ir::BinaryOp::LogicAnd
                | crate::ir::BinaryOp::LogicOr => operand_chunks,
                crate::ir::BinaryOp::Add | crate::ir::BinaryOp::Sub => 3 * operand_chunks,
                crate::ir::BinaryOp::Mul => 5 * operand_chunks.saturating_mul(operand_chunks),
                crate::ir::BinaryOp::DivU
                | crate::ir::BinaryOp::DivS
                | crate::ir::BinaryOp::RemU
                | crate::ir::BinaryOp::RemS => 12 * operand_chunks.saturating_mul(operand_chunks),
                crate::ir::BinaryOp::Shl | crate::ir::BinaryOp::Shr | crate::ir::BinaryOp::Sar => {
                    4 * operand_chunks
                }
                crate::ir::BinaryOp::Eq
                | crate::ir::BinaryOp::Ne
                | crate::ir::BinaryOp::EqCase
                | crate::ir::BinaryOp::NeCase
                | crate::ir::BinaryOp::EqWildcard
                | crate::ir::BinaryOp::NeWildcard
                | crate::ir::BinaryOp::LtU
                | crate::ir::BinaryOp::LtS
                | crate::ir::BinaryOp::LeU
                | crate::ir::BinaryOp::LeS
                | crate::ir::BinaryOp::GtU
                | crate::ir::BinaryOp::GtS
                | crate::ir::BinaryOp::GeU
                | crate::ir::BinaryOp::GeS => 3 * operand_chunks,
            }
        }
        SIRInstruction::Unary(dst, op, src) => {
            let operand_chunks = chunks(register_width(*dst).max(register_width(*src)));
            match op {
                crate::ir::UnaryOp::PopCount => 2 * operand_chunks + 1,
                crate::ir::UnaryOp::CountLeadingZeros | crate::ir::UnaryOp::CountTrailingZeros => {
                    3 * operand_chunks + 1
                }
                _ => 2 * operand_chunks,
            }
        }
        SIRInstruction::Load(_, _, offset, width) => {
            3 * chunks(*width) + 3 * u128::from(offset.is_dynamic())
        }
        SIRInstruction::Concat(dst, args) => chunks(register_width(*dst)) + args.len() as u128,
        SIRInstruction::Slice(dst, _, _, _) => 2 * chunks(register_width(*dst)),
        SIRInstruction::Mux(dst, _, true_value, false_value) => chunks(
            register_width(*dst)
                .max(register_width(*true_value))
                .max(register_width(*false_value)),
        ),
        SIRInstruction::Store(..)
        | SIRInstruction::Commit(..)
        | SIRInstruction::RuntimeEvent { .. }
        | SIRInstruction::CombCaptureEvent { .. }
        | SIRInstruction::CombCaptureEnableIfChanged { .. } => 0,
    }
}
