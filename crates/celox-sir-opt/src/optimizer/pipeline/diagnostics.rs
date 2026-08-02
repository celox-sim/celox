//! Optional optimizer diagnostics kept out of the production pipeline logic.

use super::*;
use crate::optimizer::passes::analysis::shared;

pub(super) fn dump_mux_chain_stats(units: &[ExecutionUnit<RegionedAbsoluteAddr>]) {
    let mut rows = Vec::new();

    for (eu_idx, eu) in units.iter().enumerate() {
        for block in eu.blocks.values() {
            let mut defs: crate::HashMap<RegisterId, usize> = crate::HashMap::default();
            for (idx, inst) in block.instructions.iter().enumerate() {
                if let Some(dst) = shared::def_reg(inst) {
                    defs.insert(dst, idx);
                }
            }

            let mut mux_else_children = crate::HashSet::default();
            for inst in &block.instructions {
                if let SIRInstruction::Mux(_, _, _, else_val) = inst
                    && matches!(
                        defs.get(else_val).map(|&i| &block.instructions[i]),
                        Some(SIRInstruction::Mux(..))
                    )
                {
                    mux_else_children.insert(*else_val);
                }
            }

            for inst in &block.instructions {
                let SIRInstruction::Mux(dst, ..) = inst else {
                    continue;
                };
                if mux_else_children.contains(dst) {
                    continue;
                }

                let mut len = 0usize;
                let mut direct_case = 0usize;
                let mut acc_guarded_priority = 0usize;
                let mut cursor = Some(*dst);
                while let Some(reg) = cursor {
                    let Some(&idx) = defs.get(&reg) else {
                        break;
                    };
                    let SIRInstruction::Mux(_, cond, _, else_val) = &block.instructions[idx] else {
                        break;
                    };
                    len += 1;
                    if is_direct_case_eq(*cond, &defs, &block.instructions) {
                        direct_case += 1;
                    }
                    if is_acc_guarded_priority_cond(*cond, *else_val, &defs, &block.instructions) {
                        acc_guarded_priority += 1;
                    }
                    cursor = match defs.get(else_val).map(|&i| &block.instructions[i]) {
                        Some(SIRInstruction::Mux(..)) => Some(*else_val),
                        _ => None,
                    };
                }

                if len >= 4 {
                    rows.push((
                        len,
                        direct_case,
                        acc_guarded_priority,
                        eu_idx,
                        block.id,
                        *dst,
                    ));
                }
            }
        }
    }

    rows.sort_by(|a, b| b.cmp(a));
    for (rank, (len, direct_case, acc_guarded_priority, eu_idx, block_id, root)) in
        rows.into_iter().take(20).enumerate()
    {
        eprintln!(
            "[mux-chain-stats] rank={} eu={} block={} root=r{} len={} direct_case={} acc_guarded_priority={}",
            rank + 1,
            eu_idx,
            block_id.0,
            root.0,
            len,
            direct_case,
            acc_guarded_priority
        );
    }
}

fn is_direct_case_eq(
    cond: RegisterId,
    defs: &crate::HashMap<RegisterId, usize>,
    instructions: &[SIRInstruction<RegionedAbsoluteAddr>],
) -> bool {
    let Some(&idx) = defs.get(&cond) else {
        return false;
    };
    match &instructions[idx] {
        SIRInstruction::Binary(_, lhs, BinaryOp::Eq | BinaryOp::EqWildcard, rhs) => {
            is_zero_mask_imm(*lhs, defs, instructions) || is_zero_mask_imm(*rhs, defs, instructions)
        }
        _ => false,
    }
}

fn is_acc_guarded_priority_cond(
    cond: RegisterId,
    prev_acc: RegisterId,
    defs: &crate::HashMap<RegisterId, usize>,
    instructions: &[SIRInstruction<RegionedAbsoluteAddr>],
) -> bool {
    let Some(&idx) = defs.get(&cond) else {
        return false;
    };
    match &instructions[idx] {
        SIRInstruction::Binary(_, lhs, BinaryOp::LogicAnd, rhs) => {
            is_acc_eq_imm(*lhs, prev_acc, defs, instructions)
                || is_acc_eq_imm(*rhs, prev_acc, defs, instructions)
        }
        _ => false,
    }
}

fn is_acc_eq_imm(
    reg: RegisterId,
    prev_acc: RegisterId,
    defs: &crate::HashMap<RegisterId, usize>,
    instructions: &[SIRInstruction<RegionedAbsoluteAddr>],
) -> bool {
    let Some(&idx) = defs.get(&reg) else {
        return false;
    };
    match &instructions[idx] {
        SIRInstruction::Binary(_, lhs, BinaryOp::Eq, rhs) => {
            (*lhs == prev_acc && is_zero_mask_imm(*rhs, defs, instructions))
                || (*rhs == prev_acc && is_zero_mask_imm(*lhs, defs, instructions))
        }
        _ => false,
    }
}

fn is_zero_mask_imm(
    reg: RegisterId,
    defs: &crate::HashMap<RegisterId, usize>,
    instructions: &[SIRInstruction<RegionedAbsoluteAddr>],
) -> bool {
    defs.get(&reg).is_some_and(|&idx| {
        matches!(
            &instructions[idx],
            SIRInstruction::Imm(_, value) if value.mask == num_bigint::BigUint::ZERO
        )
    })
}
