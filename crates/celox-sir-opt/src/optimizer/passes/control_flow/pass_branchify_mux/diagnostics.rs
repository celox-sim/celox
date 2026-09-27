//! Register tracing for branchification plans and rewritten blocks.

use super::*;

pub(super) fn trace_reg_in_original(
    block: &BasicBlock<RegionedAbsoluteAddr>,
    plan: &BranchifyPlan,
    reg: RegisterId,
) {
    let defines = block
        .instructions
        .iter()
        .enumerate()
        .filter_map(|(idx, inst)| (def_reg(inst) == Some(reg)).then_some((idx, inst)))
        .collect::<Vec<_>>();
    let inst_uses = block
        .instructions
        .iter()
        .enumerate()
        .filter(|(_, inst)| inst_uses(inst).contains(&reg))
        .collect::<Vec<_>>();
    let term_uses = terminator_uses(&block.terminator).contains(&reg);
    if defines.is_empty() && inst_uses.is_empty() && !term_uses && !block.params.contains(&reg) {
        return;
    }
    tracing::debug!(
        "[branchify-trace] original block=b{} mux_idx={} dst=r{} cond=r{} true=r{} false=r{} params={} term_uses={} true_defs={:?} false_defs={:?}",
        block.id.0,
        plan.mux_idx,
        plan.dst.0,
        plan.cond.0,
        plan.true_val.0,
        plan.false_val.0,
        block.params.contains(&reg),
        term_uses,
        plan.true_defs,
        plan.false_defs
    );
    for (idx, inst) in defines {
        tracing::debug!(
            "[branchify-trace] original defines r{} at inst {idx}: {inst}",
            reg.0
        );
    }
    for (idx, inst) in inst_uses {
        tracing::debug!(
            "[branchify-trace] original uses r{} at inst {idx}: {inst}",
            reg.0
        );
    }
    if term_uses {
        tracing::debug!(
            "[branchify-trace] original terminator uses r{}: {}",
            reg.0,
            block.terminator
        );
    }
}

pub(super) fn trace_reg_branchify_plan(
    block: &BasicBlock<RegionedAbsoluteAddr>,
    plan: &BranchifyPlan,
    remove_defs: &HashSet<usize>,
    reg: RegisterId,
) {
    for (idx, inst) in block.instructions.iter().enumerate() {
        if def_reg(inst) == Some(reg) {
            tracing::debug!(
                "[branchify-trace] after restore decision block=b{} r{} def_idx={idx} removed={} inst={inst}",
                block.id.0,
                reg.0,
                remove_defs.contains(&idx)
            );
        }
    }
    if plan.cond == reg || plan.true_val == reg || plan.false_val == reg || plan.dst == reg {
        tracing::debug!(
            "[branchify-trace] plan directly references r{} block=b{} mux_idx={} dst=r{} cond=r{} true=r{} false=r{}",
            reg.0,
            block.id.0,
            plan.mux_idx,
            plan.dst.0,
            plan.cond.0,
            plan.true_val.0,
            plan.false_val.0
        );
    }
}

pub(super) fn trace_reg_in_new_block(block: &BasicBlock<RegionedAbsoluteAddr>, reg: RegisterId) {
    let term_uses = terminator_uses(&block.terminator).contains(&reg);
    let inst_uses = block
        .instructions
        .iter()
        .enumerate()
        .filter(|(_, inst)| inst_uses(inst).contains(&reg))
        .collect::<Vec<_>>();
    let defines = block
        .instructions
        .iter()
        .enumerate()
        .filter_map(|(idx, inst)| (def_reg(inst) == Some(reg)).then_some((idx, inst)))
        .collect::<Vec<_>>();
    if !block.params.contains(&reg) && !term_uses && inst_uses.is_empty() && defines.is_empty() {
        return;
    }
    tracing::debug!(
        "[branchify-trace] new block=b{} params={} term_uses={} insts={} defs={}",
        block.id.0,
        block.params.contains(&reg),
        term_uses,
        inst_uses.len(),
        defines.len()
    );
    for (idx, inst) in defines {
        tracing::debug!(
            "[branchify-trace] new defines r{} at inst {idx}: {inst}",
            reg.0
        );
    }
    for (idx, inst) in inst_uses {
        tracing::debug!(
            "[branchify-trace] new uses r{} at inst {idx}: {inst}",
            reg.0
        );
    }
    if term_uses {
        tracing::debug!(
            "[branchify-trace] new terminator uses r{}: {}",
            reg.0,
            block.terminator
        );
    }
}
