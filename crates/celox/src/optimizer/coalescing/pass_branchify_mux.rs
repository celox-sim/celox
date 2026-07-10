use super::pass_manager::ExecutionUnitPass;
use super::shared::def_reg;
use crate::ir::{
    BasicBlock, BlockId, ExecutionUnit, RegionedAbsoluteAddr, RegisterId, SIRInstruction,
    SIROffset, SIRTerminator,
};
use crate::optimizer::PassOptions;
use crate::{HashMap, HashSet};
use std::collections::VecDeque;

pub(super) struct BranchifyMuxPass;

#[derive(Clone)]
struct BranchifyPlan {
    block_id: BlockId,
    mux_idx: usize,
    dst: RegisterId,
    cond: RegisterId,
    true_val: RegisterId,
    false_val: RegisterId,
    true_defs: Vec<usize>,
    false_defs: Vec<usize>,
    distributed_store: Option<DistributedStore>,
    preserve_result: bool,
}

#[derive(Clone)]
struct DistributedStore {
    idx: usize,
    true_inst: SIRInstruction<RegionedAbsoluteAddr>,
    false_inst: SIRInstruction<RegionedAbsoluteAddr>,
}

impl ExecutionUnitPass for BranchifyMuxPass {
    fn name(&self) -> &'static str {
        "branchify_mux"
    }

    fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, _options: &PassOptions) {
        let stats = std::env::var_os("CELOX_BRANCHIFY_STATS").is_some();
        let stats_start = stats.then(crate::timing::now);
        let trace_reg = std::env::var("CELOX_BRANCHIFY_TRACE_REG")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .map(RegisterId);
        let mut use_counts = count_uses(eu);
        let mut block_ids = eu.blocks.keys().copied().collect::<Vec<_>>();
        block_ids.sort_by_key(|id| id.0);
        let mut worklist = VecDeque::from(block_ids);
        let mut queued = HashSet::default();
        queued.extend(worklist.iter().copied());
        let mut repair_roots = Vec::new();
        let mut applied = 0usize;

        while let Some(block_id) = worklist.pop_front() {
            queued.remove(&block_id);
            if !eu.blocks.contains_key(&block_id) {
                continue;
            }
            while let Some(plan) = find_branchify_mux_in_block(eu, block_id, &use_counts) {
                let new_blocks = apply_branchify_mux(eu, plan, &mut use_counts, trace_reg);
                applied += 1;
                if stats && applied % 1000 == 0 {
                    let insts = eu
                        .blocks
                        .values()
                        .map(|block| block.instructions.len())
                        .sum::<usize>();
                    eprintln!(
                        "[branchify-stats] applied={applied} blocks={} insts={} worklist={} elapsed={:?}",
                        eu.blocks.len(),
                        insts,
                        worklist.len(),
                        stats_start.unwrap().elapsed()
                    );
                }
                repair_roots.extend(new_blocks);
                for new_block in new_blocks {
                    if queued.insert(new_block) {
                        worklist.push_back(new_block);
                    }
                }
            }
        }
        if stats {
            eprintln!(
                "[branchify-stats] before_pre_repair_inline applied={applied} blocks={} elapsed={:?}",
                eu.blocks.len(),
                stats_start.unwrap().elapsed()
            );
        }
        inline_param_only_jump_blocks(eu);
        if stats {
            let mut unique_repair_roots = repair_roots.clone();
            unique_repair_roots.sort();
            unique_repair_roots.dedup();
            eprintln!(
                "[branchify-stats] before_repair applied={applied} blocks={} repair_roots={} unique_repair_roots={} elapsed={:?}",
                eu.blocks.len(),
                repair_roots.len(),
                unique_repair_roots.len(),
                stats_start.unwrap().elapsed()
            );
        }
        repair_param_origin_live_ins(eu, repair_roots);
        if stats {
            eprintln!(
                "[branchify-stats] before_post_repair_inline applied={applied} blocks={} elapsed={:?}",
                eu.blocks.len(),
                stats_start.unwrap().elapsed()
            );
        }
        inline_param_only_jump_blocks(eu);
        if stats {
            let insts = eu
                .blocks
                .values()
                .map(|block| block.instructions.len())
                .sum::<usize>();
            eprintln!(
                "[branchify-stats] done applied={applied} blocks={} insts={} elapsed={:?}",
                eu.blocks.len(),
                insts,
                stats_start.unwrap().elapsed()
            );
        }
        if std::env::var_os("CELOX_BRANCHIFY_VERIFY").is_some() {
            verify_all_uses_have_defs(eu);
        }
    }
}

fn find_branchify_mux_in_block(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block_id: BlockId,
    use_counts: &HashMap<RegisterId, usize>,
) -> Option<BranchifyPlan> {
    let block = eu.blocks.get(&block_id)?;
    let mut def_pos = HashMap::default();
    for (idx, inst) in block.instructions.iter().enumerate() {
        if let Some(def) = def_reg(inst) {
            def_pos.insert(def, idx);
        }
    }

    for (mux_idx, inst) in block.instructions.iter().enumerate() {
        let SIRInstruction::Mux(dst, cond, true_val, false_val) = inst else {
            continue;
        };

        if use_counts.get(dst).copied().unwrap_or(0) > block_use_count(block, *dst) {
            continue;
        }

        let immediate_store = find_distributed_store(block, mux_idx, *dst, *true_val, *false_val);
        let preserve_result =
            immediate_store.is_none() || use_counts.get(dst).copied().unwrap_or(0) > 1;
        let memory_barrier_idx = if preserve_result {
            mux_idx
        } else {
            immediate_store
                .as_ref()
                .expect("single-use store mux should have a store")
                .idx
                + 1
        };

        let mut true_defs = HashSet::default();
        let mut false_defs = HashSet::default();
        collect_sinkable_defs(
            block,
            &def_pos,
            use_counts,
            mux_idx,
            memory_barrier_idx,
            *true_val,
            &mut true_defs,
        );
        collect_sinkable_defs(
            block,
            &def_pos,
            use_counts,
            mux_idx,
            memory_barrier_idx,
            *false_val,
            &mut false_defs,
        );
        if !true_defs.is_disjoint(&false_defs) {
            continue;
        }
        if !terminator_uses(&block.terminator).contains(dst)
            && true_defs
                .iter()
                .chain(false_defs.iter())
                .all(|idx| is_trivial_select_input(&block.instructions[*idx]))
        {
            continue;
        }

        let mut true_defs = true_defs.into_iter().collect::<Vec<_>>();
        let mut false_defs = false_defs.into_iter().collect::<Vec<_>>();
        true_defs.sort_unstable();
        false_defs.sort_unstable();
        return Some(BranchifyPlan {
            block_id,
            mux_idx,
            dst: *dst,
            cond: *cond,
            true_val: *true_val,
            false_val: *false_val,
            true_defs,
            false_defs,
            distributed_store: if preserve_result {
                None
            } else {
                immediate_store
            },
            preserve_result,
        });
    }

    None
}

fn find_distributed_store(
    block: &BasicBlock<RegionedAbsoluteAddr>,
    mux_idx: usize,
    dst: RegisterId,
    true_val: RegisterId,
    false_val: RegisterId,
) -> Option<DistributedStore> {
    let store_idx = mux_idx + 1;
    let store = block.instructions.get(store_idx)?;
    match store {
        SIRInstruction::Store(addr, offset, width, src, triggers, sites) if *src == dst => {
            Some(DistributedStore {
                idx: store_idx,
                true_inst: SIRInstruction::Store(
                    *addr,
                    offset.clone(),
                    *width,
                    true_val,
                    triggers.clone(),
                    sites.clone(),
                ),
                false_inst: SIRInstruction::Store(
                    *addr,
                    offset.clone(),
                    *width,
                    false_val,
                    triggers.clone(),
                    sites.clone(),
                ),
            })
        }
        _ => None,
    }
}

fn collect_sinkable_defs(
    block: &BasicBlock<RegionedAbsoluteAddr>,
    def_pos: &HashMap<RegisterId, usize>,
    use_counts: &HashMap<RegisterId, usize>,
    user_idx: usize,
    memory_barrier_idx: usize,
    root: RegisterId,
    defs: &mut HashSet<usize>,
) {
    if use_counts.get(&root).copied().unwrap_or(0) != 1 {
        return;
    }
    let Some(&idx) = def_pos.get(&root) else {
        return;
    };
    if idx >= user_idx || defs.contains(&idx) {
        return;
    }
    let inst = &block.instructions[idx];
    if !is_sinkable_input(inst) {
        return;
    }
    if let Some(load) = memory_read(inst)
        && has_intervening_memory_conflict(block, idx + 1, memory_barrier_idx, load)
    {
        return;
    }

    defs.insert(idx);
    for use_reg in inst_uses(inst) {
        collect_sinkable_defs(
            block,
            def_pos,
            use_counts,
            idx,
            memory_barrier_idx,
            use_reg,
            defs,
        );
    }
}

fn is_sinkable_input(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> bool {
    matches!(
        inst,
        SIRInstruction::Imm(..)
            | SIRInstruction::Binary(..)
            | SIRInstruction::Unary(..)
            | SIRInstruction::Load(..)
            | SIRInstruction::Concat(..)
            | SIRInstruction::Slice(..)
            | SIRInstruction::Mux(..)
    )
}

fn is_trivial_select_input(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> bool {
    matches!(inst, SIRInstruction::Imm(..))
}

#[derive(Clone, Copy)]
struct MemAccess<'a> {
    addr: &'a RegionedAbsoluteAddr,
    offset: Option<usize>,
    width: usize,
}

fn memory_read(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> Option<MemAccess<'_>> {
    match inst {
        SIRInstruction::Load(_, addr, offset, width) => Some(MemAccess {
            addr,
            offset: offset_static(offset),
            width: *width,
        }),
        _ => None,
    }
}

fn memory_write(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> Option<MemAccess<'_>> {
    match inst {
        SIRInstruction::Store(addr, offset, width, _, _, _) => Some(MemAccess {
            addr,
            offset: offset_static(offset),
            width: *width,
        }),
        SIRInstruction::Commit(_, dst, offset, width, _) => Some(MemAccess {
            addr: dst,
            offset: offset_static(offset),
            width: *width,
        }),
        _ => None,
    }
}

fn offset_static(offset: &SIROffset) -> Option<usize> {
    match offset {
        SIROffset::Static(offset) => Some(*offset),
        SIROffset::Dynamic(_) => None,
    }
}

fn has_intervening_memory_conflict(
    block: &BasicBlock<RegionedAbsoluteAddr>,
    start: usize,
    end: usize,
    read: MemAccess<'_>,
) -> bool {
    block.instructions[start..end].iter().any(|inst| {
        is_memory_barrier(inst)
            || memory_write(inst).is_some_and(|write| mem_may_alias(read, write))
    })
}

fn is_memory_barrier(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> bool {
    matches!(
        inst,
        SIRInstruction::RuntimeEvent { .. }
            | SIRInstruction::CombCaptureEvent { .. }
            | SIRInstruction::CombCaptureEnableIfChanged { .. }
    )
}

fn mem_may_alias(a: MemAccess<'_>, b: MemAccess<'_>) -> bool {
    if a.addr != b.addr {
        return false;
    }
    match (a.offset, b.offset) {
        (Some(a_off), Some(b_off)) => a_off < b_off + b.width && b_off < a_off + a.width,
        _ => true,
    }
}

fn apply_branchify_mux(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    plan: BranchifyPlan,
    use_counts: &mut HashMap<RegisterId, usize>,
    trace_reg: Option<RegisterId>,
) -> [BlockId; 3] {
    let def_blocks = instruction_def_blocks(eu);
    let next_id = eu.blocks.keys().map(|id| id.0).max().unwrap_or(0) + 1;
    let true_id = BlockId(next_id);
    let false_id = BlockId(next_id + 1);
    let merge_id = BlockId(next_id + 2);

    let original = eu
        .blocks
        .remove(&plan.block_id)
        .expect("branchify target block must exist");
    if let Some(reg) = trace_reg {
        trace_reg_in_original(&original, &plan, reg);
    }
    remove_block_uses(use_counts, &original);
    let mut remove_defs = plan
        .true_defs
        .iter()
        .chain(plan.false_defs.iter())
        .copied()
        .collect::<HashSet<_>>();
    remove_defs.insert(plan.mux_idx);
    if let Some(store) = &plan.distributed_store {
        remove_defs.insert(store.idx);
    }
    let restore_defs = head_restore_defs(&original, &plan, &remove_defs, &def_blocks);
    for idx in restore_defs {
        remove_defs.remove(&idx);
    }
    if let Some(reg) = trace_reg {
        trace_reg_branchify_plan(&original, &plan, &remove_defs, reg);
    }

    let mut head_insts = Vec::new();
    for (idx, inst) in original.instructions.iter().enumerate().take(plan.mux_idx) {
        if !remove_defs.contains(&idx) {
            head_insts.push(inst.clone());
        }
    }
    let head_defs = instruction_defs_in(&head_insts);

    let mut suffix = Vec::new();
    for (idx, inst) in original
        .instructions
        .iter()
        .enumerate()
        .skip(plan.mux_idx + 1)
    {
        if !remove_defs.contains(&idx) {
            suffix.push(inst.clone());
        }
    }

    let mut true_insts = plan
        .true_defs
        .iter()
        .filter(|idx| remove_defs.contains(idx))
        .map(|&idx| original.instructions[idx].clone())
        .collect::<Vec<_>>();
    let mut false_insts = plan
        .false_defs
        .iter()
        .filter(|idx| remove_defs.contains(idx))
        .map(|&idx| original.instructions[idx].clone())
        .collect::<Vec<_>>();
    if let Some(store) = &plan.distributed_store {
        true_insts.push(store.true_inst.clone());
        false_insts.push(store.false_inst.clone());
    }
    let mut next_reg_id = eu.register_map.keys().map(|reg| reg.0).max().unwrap_or(0) + 1;

    let true_args = if plan.preserve_result {
        vec![plan.true_val]
    } else {
        Vec::new()
    };
    let false_args = if plan.preserve_result {
        vec![plan.false_val]
    } else {
        Vec::new()
    };
    let mut merge_params = if plan.preserve_result {
        vec![plan.dst]
    } else {
        Vec::new()
    };

    let mut merge_terminator = original.terminator;
    let merge_terminator_uses = terminator_uses(&merge_terminator);
    let mut merge_live_ins = block_live_ins(&suffix, &merge_terminator_uses);
    if plan.preserve_result {
        merge_live_ins.retain(|reg| *reg != plan.dst);
    }
    merge_live_ins.retain(|reg| {
        !head_defs.contains(reg)
            && def_blocks
                .get(reg)
                .is_none_or(|def_block| *def_block >= plan.block_id)
    });
    let merge_param_map = create_live_in_params(eu, &merge_live_ins, &mut next_reg_id);
    replace_inst_uses(&mut suffix, &merge_param_map);
    replace_terminator_uses(&mut merge_terminator, &merge_param_map);
    merge_params.extend(merge_live_ins.iter().map(|reg| merge_param_map[reg]));

    let mut true_args = true_args;
    true_args.extend(merge_live_ins.iter().copied());
    let mut false_args = false_args;
    false_args.extend(merge_live_ins.iter().copied());

    let true_live_ins = block_live_ins(&true_insts, &true_args);
    let false_live_ins = block_live_ins(&false_insts, &false_args);
    let true_params = create_live_in_params(eu, &true_live_ins, &mut next_reg_id);
    let false_params = create_live_in_params(eu, &false_live_ins, &mut next_reg_id);
    replace_inst_uses(&mut true_insts, &true_params);
    replace_inst_uses(&mut false_insts, &false_params);
    let true_args = replace_regs_in_args(&true_args, &true_params);
    let false_args = replace_regs_in_args(&false_args, &false_params);
    let true_param_regs = true_live_ins
        .iter()
        .map(|reg| true_params[reg])
        .collect::<Vec<_>>();
    let false_param_regs = false_live_ins
        .iter()
        .map(|reg| false_params[reg])
        .collect::<Vec<_>>();

    let head = BasicBlock {
        id: plan.block_id,
        params: original.params,
        instructions: head_insts,
        terminator: SIRTerminator::Branch {
            cond: plan.cond,
            true_block: (true_id, true_live_ins.clone()),
            false_block: (false_id, false_live_ins.clone()),
        },
    };
    let true_block = BasicBlock {
        id: true_id,
        params: true_param_regs,
        instructions: true_insts,
        terminator: SIRTerminator::Jump(merge_id, true_args),
    };
    let false_block = BasicBlock {
        id: false_id,
        params: false_param_regs,
        instructions: false_insts,
        terminator: SIRTerminator::Jump(merge_id, false_args),
    };
    let merge_block = BasicBlock {
        id: merge_id,
        params: merge_params,
        instructions: suffix,
        terminator: merge_terminator,
    };

    add_block_uses(use_counts, &head);
    add_block_uses(use_counts, &true_block);
    add_block_uses(use_counts, &false_block);
    add_block_uses(use_counts, &merge_block);

    eu.blocks.insert(plan.block_id, head);
    eu.blocks.insert(true_id, true_block);
    eu.blocks.insert(false_id, false_block);
    eu.blocks.insert(merge_id, merge_block);

    if let Some(reg) = trace_reg {
        for block_id in [plan.block_id, true_id, false_id, merge_id] {
            if let Some(block) = eu.blocks.get(&block_id) {
                trace_reg_in_new_block(block, reg);
            }
        }
    }

    [true_id, false_id, merge_id]
}

fn trace_reg_in_original(
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
    eprintln!(
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
        eprintln!(
            "[branchify-trace] original defines r{} at inst {idx}: {inst}",
            reg.0
        );
    }
    for (idx, inst) in inst_uses {
        eprintln!(
            "[branchify-trace] original uses r{} at inst {idx}: {inst}",
            reg.0
        );
    }
    if term_uses {
        eprintln!(
            "[branchify-trace] original terminator uses r{}: {}",
            reg.0, block.terminator
        );
    }
}

fn trace_reg_branchify_plan(
    block: &BasicBlock<RegionedAbsoluteAddr>,
    plan: &BranchifyPlan,
    remove_defs: &HashSet<usize>,
    reg: RegisterId,
) {
    for (idx, inst) in block.instructions.iter().enumerate() {
        if def_reg(inst) == Some(reg) {
            eprintln!(
                "[branchify-trace] after restore decision block=b{} r{} def_idx={idx} removed={} inst={inst}",
                block.id.0,
                reg.0,
                remove_defs.contains(&idx)
            );
        }
    }
    if plan.cond == reg || plan.true_val == reg || plan.false_val == reg || plan.dst == reg {
        eprintln!(
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

fn trace_reg_in_new_block(block: &BasicBlock<RegionedAbsoluteAddr>, reg: RegisterId) {
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
    eprintln!(
        "[branchify-trace] new block=b{} params={} term_uses={} insts={} defs={}",
        block.id.0,
        block.params.contains(&reg),
        term_uses,
        inst_uses.len(),
        defines.len()
    );
    for (idx, inst) in defines {
        eprintln!(
            "[branchify-trace] new defines r{} at inst {idx}: {inst}",
            reg.0
        );
    }
    for (idx, inst) in inst_uses {
        eprintln!(
            "[branchify-trace] new uses r{} at inst {idx}: {inst}",
            reg.0
        );
    }
    if term_uses {
        eprintln!(
            "[branchify-trace] new terminator uses r{}: {}",
            reg.0, block.terminator
        );
    }
}

fn instruction_defs_in(head_insts: &[SIRInstruction<RegionedAbsoluteAddr>]) -> HashSet<RegisterId> {
    let mut defs = HashSet::default();
    for inst in head_insts {
        if let Some(def) = def_reg(inst) {
            defs.insert(def);
        }
    }
    defs
}

fn instruction_def_blocks(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
) -> HashMap<RegisterId, BlockId> {
    let mut defs = HashMap::default();
    for block in eu.blocks.values() {
        for inst in &block.instructions {
            if let Some(def) = def_reg(inst) {
                defs.insert(def, block.id);
            }
        }
    }
    defs
}

fn head_restore_defs(
    original: &BasicBlock<RegionedAbsoluteAddr>,
    plan: &BranchifyPlan,
    remove_defs: &HashSet<usize>,
    def_blocks: &HashMap<RegisterId, BlockId>,
) -> HashSet<usize> {
    let mut head_insts = Vec::new();
    for (idx, inst) in original.instructions.iter().enumerate().take(plan.mux_idx) {
        if !remove_defs.contains(&idx) {
            head_insts.push(inst.clone());
        }
    }
    let head_defs = instruction_defs_in(&head_insts);

    let mut suffix = Vec::new();
    for (idx, inst) in original
        .instructions
        .iter()
        .enumerate()
        .skip(plan.mux_idx + 1)
    {
        if !remove_defs.contains(&idx) {
            suffix.push(inst.clone());
        }
    }

    let mut merge_live_ins = block_live_ins(&suffix, &terminator_uses(&original.terminator));
    if plan.preserve_result {
        merge_live_ins.retain(|reg| *reg != plan.dst);
    }
    merge_live_ins.retain(|reg| {
        !head_defs.contains(reg)
            && def_blocks
                .get(reg)
                .is_none_or(|def_block| *def_block >= plan.block_id)
    });

    let mut true_args = if plan.preserve_result {
        vec![plan.true_val]
    } else {
        Vec::new()
    };
    true_args.extend(merge_live_ins.iter().copied());
    let mut false_args = if plan.preserve_result {
        vec![plan.false_val]
    } else {
        Vec::new()
    };
    false_args.extend(merge_live_ins.iter().copied());

    let true_insts = plan
        .true_defs
        .iter()
        .filter(|idx| remove_defs.contains(idx))
        .map(|&idx| original.instructions[idx].clone())
        .collect::<Vec<_>>();
    let false_insts = plan
        .false_defs
        .iter()
        .filter(|idx| remove_defs.contains(idx))
        .map(|&idx| original.instructions[idx].clone())
        .collect::<Vec<_>>();
    let true_live_ins = block_live_ins(&true_insts, &true_args);
    let false_live_ins = block_live_ins(&false_insts, &false_args);

    let mut needed = HashSet::default();
    needed.insert(plan.cond);
    needed.extend(true_live_ins);
    needed.extend(false_live_ins);
    collect_removed_defs_needed_by_head(original, remove_defs, needed)
}

fn collect_removed_defs_needed_by_head(
    original: &BasicBlock<RegionedAbsoluteAddr>,
    remove_defs: &HashSet<usize>,
    needed: HashSet<RegisterId>,
) -> HashSet<usize> {
    let mut removed_def_pos = HashMap::default();
    for &idx in remove_defs {
        if let Some(def) = def_reg(&original.instructions[idx]) {
            removed_def_pos.insert(def, idx);
        }
    }

    let mut restore = HashSet::default();
    let mut queue = VecDeque::from_iter(needed);
    let mut seen = HashSet::default();
    while let Some(reg) = queue.pop_front() {
        if !seen.insert(reg) {
            continue;
        }
        let Some(&idx) = removed_def_pos.get(&reg) else {
            continue;
        };
        if restore.insert(idx) {
            for use_reg in inst_uses(&original.instructions[idx]) {
                queue.push_back(use_reg);
            }
        }
    }
    restore
}

fn block_live_ins(
    instructions: &[SIRInstruction<RegionedAbsoluteAddr>],
    terminator_args: &[RegisterId],
) -> Vec<RegisterId> {
    let mut defs = HashSet::default();
    let mut live_ins = Vec::new();
    let mut seen = HashSet::default();

    for inst in instructions {
        for reg in inst_uses(inst) {
            if !defs.contains(&reg) && seen.insert(reg) {
                live_ins.push(reg);
            }
        }
        if let Some(def) = def_reg(inst) {
            defs.insert(def);
        }
    }
    for &reg in terminator_args {
        if !defs.contains(&reg) && seen.insert(reg) {
            live_ins.push(reg);
        }
    }

    live_ins
}

fn create_live_in_params(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    live_ins: &[RegisterId],
    next_reg_id: &mut usize,
) -> HashMap<RegisterId, RegisterId> {
    let mut params = HashMap::default();
    for &live_in in live_ins {
        let param = RegisterId(*next_reg_id);
        *next_reg_id += 1;
        let ty = eu
            .register_map
            .get(&live_in)
            .unwrap_or_else(|| panic!("missing register type for live-in r{}", live_in.0))
            .clone();
        eu.register_map.insert(param, ty);
        params.insert(live_in, param);
    }
    params
}

fn replace_inst_uses(
    instructions: &mut [SIRInstruction<RegionedAbsoluteAddr>],
    replacements: &HashMap<RegisterId, RegisterId>,
) {
    for inst in instructions {
        replace_inst_use(inst, replacements);
    }
}

fn repair_param_origin_live_ins(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, roots: Vec<BlockId>) {
    let stats = std::env::var_os("CELOX_BRANCHIFY_STATS").is_some();
    let stats_start = stats.then(crate::timing::now);
    let inst_defs = eu
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter().filter_map(def_reg))
        .collect::<HashSet<_>>();
    let mut next_reg_id = eu.register_map.keys().map(|reg| reg.0).max().unwrap_or(0) + 1;
    let mut roots = roots.into_iter().collect::<Vec<_>>();
    roots.sort();
    roots.dedup();
    let mut queue = VecDeque::from(roots);
    let mut queued = queue.iter().copied().collect::<HashSet<_>>();
    let pred_map = predecessor_blocks(eu);
    let mut processed = 0usize;
    let mut repaired = 0usize;
    let mut added_params = 0usize;
    let mut repaired_live_ins = HashSet::default();

    while let Some(block_id) = queue.pop_front() {
        processed += 1;
        if stats && processed % 10000 == 0 {
            eprintln!(
                "[branchify-stats] repair processed={processed} repaired={repaired} added_params={added_params} queue={} elapsed={:?}",
                queue.len(),
                stats_start.unwrap().elapsed()
            );
        }
        queued.remove(&block_id);
        if !eu.blocks.contains_key(&block_id) {
            continue;
        }
        let missing = {
            let block = &eu.blocks[&block_id];
            missing_param_origin_live_ins(block, &inst_defs)
        };
        if missing.is_empty() {
            continue;
        }
        repaired += 1;

        let mut replacements = HashMap::default();
        for live_in in missing {
            if !repaired_live_ins.insert((block_id, live_in)) {
                continue;
            }
            added_params += 1;
            let param = RegisterId(next_reg_id);
            next_reg_id += 1;
            let ty = eu
                .register_map
                .get(&live_in)
                .unwrap_or_else(|| panic!("missing register type for live-in r{}", live_in.0))
                .clone();
            eu.register_map.insert(param, ty);
            replacements.insert(live_in, param);
            for pred in append_arg_to_predecessors(eu, &pred_map, block_id, live_in) {
                if queued.insert(pred) {
                    queue.push_back(pred);
                }
            }
        }

        let block = eu.blocks.get_mut(&block_id).unwrap();
        block.params.extend(replacements.values().copied());
        replace_inst_uses(&mut block.instructions, &replacements);
        replace_terminator_uses(&mut block.terminator, &replacements);
    }
}

fn missing_param_origin_live_ins(
    block: &BasicBlock<RegionedAbsoluteAddr>,
    inst_defs: &HashSet<RegisterId>,
) -> Vec<RegisterId> {
    let mut local_defs = block.params.iter().copied().collect::<HashSet<_>>();
    let mut missing = Vec::new();
    let mut seen = HashSet::default();
    for inst in &block.instructions {
        for reg in inst_uses(inst) {
            if !local_defs.contains(&reg) && !inst_defs.contains(&reg) && seen.insert(reg) {
                missing.push(reg);
            }
        }
        if let Some(def) = def_reg(inst) {
            local_defs.insert(def);
        }
    }
    missing
}

fn append_arg_to_predecessors(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    pred_map: &HashMap<BlockId, Vec<BlockId>>,
    target: BlockId,
    arg: RegisterId,
) -> Vec<BlockId> {
    let mut preds = Vec::new();
    let Some(pred_ids) = pred_map.get(&target) else {
        return preds;
    };
    for &pred_id in pred_ids {
        let Some(block) = eu.blocks.get_mut(&pred_id) else {
            continue;
        };
        match &mut block.terminator {
            SIRTerminator::Jump(dst, args) if *dst == target => {
                args.push(arg);
                preds.push(block.id);
            }
            SIRTerminator::Branch {
                true_block,
                false_block,
                ..
            } => {
                if true_block.0 == target {
                    true_block.1.push(arg);
                    preds.push(block.id);
                }
                if false_block.0 == target {
                    false_block.1.push(arg);
                    preds.push(block.id);
                }
            }
            _ => {}
        }
    }
    preds
}

fn predecessor_blocks(eu: &ExecutionUnit<RegionedAbsoluteAddr>) -> HashMap<BlockId, Vec<BlockId>> {
    let mut preds: HashMap<BlockId, Vec<BlockId>> = HashMap::default();
    for block in eu.blocks.values() {
        match &block.terminator {
            SIRTerminator::Jump(target, _) => {
                preds.entry(*target).or_default().push(block.id);
            }
            SIRTerminator::Branch {
                true_block,
                false_block,
                ..
            } => {
                preds.entry(true_block.0).or_default().push(block.id);
                if false_block.0 != true_block.0 {
                    preds.entry(false_block.0).or_default().push(block.id);
                }
            }
            SIRTerminator::Return | SIRTerminator::Error(_) => {}
        }
    }
    for pred_list in preds.values_mut() {
        pred_list.sort();
    }
    preds
}

fn replace_terminator_uses(
    term: &mut SIRTerminator,
    replacements: &HashMap<RegisterId, RegisterId>,
) {
    let replace = |reg: &mut RegisterId| {
        if let Some(&replacement) = replacements.get(reg) {
            *reg = replacement;
        }
    };
    match term {
        SIRTerminator::Jump(_, args) => {
            for arg in args {
                replace(arg);
            }
        }
        SIRTerminator::Branch {
            cond,
            true_block,
            false_block,
        } => {
            replace(cond);
            for arg in &mut true_block.1 {
                replace(arg);
            }
            for arg in &mut false_block.1 {
                replace(arg);
            }
        }
        SIRTerminator::Return | SIRTerminator::Error(_) => {}
    }
}

fn inline_param_only_jump_blocks(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) {
    loop {
        let (pred_counts, jump_preds) = predecessor_info(eu);
        let mut eligible = eu
            .blocks
            .keys()
            .copied()
            .filter(|&block_id| block_id != eu.entry_block_id)
            .filter(|block_id| param_only_replacement(eu, *block_id).is_some())
            .filter(|block_id| {
                let jump_count = jump_preds.get(block_id).map_or(0, Vec::len);
                jump_count > 0 && pred_counts.get(block_id).copied().unwrap_or(0) == jump_count
            })
            .collect::<Vec<_>>();
        eligible.sort();

        if eligible.is_empty() {
            break;
        }

        for block_id in eligible {
            if !eu.blocks.contains_key(&block_id) {
                continue;
            }
            let Some(replacement) = param_only_replacement(eu, block_id) else {
                continue;
            };
            let Some(preds) = jump_preds.get(&block_id) else {
                continue;
            };
            let params = eu.blocks[&block_id].params.clone();
            for &pred_id in preds {
                if !eu.blocks.contains_key(&pred_id) {
                    continue;
                }
                let pred_args = match &eu.blocks[&pred_id].terminator {
                    SIRTerminator::Jump(target, args) if *target == block_id => args.clone(),
                    _ => continue,
                };
                let map = params
                    .iter()
                    .copied()
                    .zip(pred_args)
                    .collect::<HashMap<_, _>>();
                eu.blocks.get_mut(&pred_id).unwrap().terminator =
                    substitute_terminator(&replacement, &map);
            }
            eu.blocks.remove(&block_id);
        }
    }
}

fn param_only_replacement(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block_id: BlockId,
) -> Option<SIRTerminator> {
    let block = eu.blocks.get(&block_id)?;
    if !block.instructions.is_empty() || block.params.is_empty() {
        return None;
    }
    match &block.terminator {
        SIRTerminator::Jump(_, _) | SIRTerminator::Branch { .. } => Some(block.terminator.clone()),
        SIRTerminator::Return | SIRTerminator::Error(_) => None,
    }
}

fn predecessor_info(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
) -> (HashMap<BlockId, usize>, HashMap<BlockId, Vec<BlockId>>) {
    let mut pred_counts = HashMap::default();
    let mut jump_preds: HashMap<BlockId, Vec<BlockId>> = HashMap::default();
    for block in eu.blocks.values() {
        match &block.terminator {
            SIRTerminator::Jump(dst, _) => {
                *pred_counts.entry(*dst).or_default() += 1;
                jump_preds.entry(*dst).or_default().push(block.id);
            }
            SIRTerminator::Branch {
                true_block,
                false_block,
                ..
            } => {
                *pred_counts.entry(true_block.0).or_default() += 1;
                *pred_counts.entry(false_block.0).or_default() += 1;
            }
            SIRTerminator::Return | SIRTerminator::Error(_) => {}
        }
    }
    for preds in jump_preds.values_mut() {
        preds.sort();
    }
    (pred_counts, jump_preds)
}

fn substitute_terminator(
    term: &SIRTerminator,
    map: &HashMap<RegisterId, RegisterId>,
) -> SIRTerminator {
    let replace = |reg: RegisterId| map.get(&reg).copied().unwrap_or(reg);
    match term {
        SIRTerminator::Jump(target, args) => {
            SIRTerminator::Jump(*target, args.iter().copied().map(replace).collect())
        }
        SIRTerminator::Branch {
            cond,
            true_block,
            false_block,
        } => SIRTerminator::Branch {
            cond: replace(*cond),
            true_block: (
                true_block.0,
                true_block.1.iter().copied().map(replace).collect(),
            ),
            false_block: (
                false_block.0,
                false_block.1.iter().copied().map(replace).collect(),
            ),
        },
        SIRTerminator::Return => SIRTerminator::Return,
        SIRTerminator::Error(code) => SIRTerminator::Error(*code),
    }
}

fn replace_inst_use(
    inst: &mut SIRInstruction<RegionedAbsoluteAddr>,
    replacements: &HashMap<RegisterId, RegisterId>,
) {
    let replace = |reg: &mut RegisterId| {
        if let Some(&replacement) = replacements.get(reg) {
            *reg = replacement;
        }
    };
    match inst {
        SIRInstruction::Imm(_, _) => {}
        SIRInstruction::Binary(_, lhs, _, rhs) => {
            replace(lhs);
            replace(rhs);
        }
        SIRInstruction::Unary(_, _, src) => replace(src),
        SIRInstruction::Load(_, _, SIROffset::Dynamic(offset), _) => replace(offset),
        SIRInstruction::Load(_, _, SIROffset::Static(_), _) => {}
        SIRInstruction::Store(_, SIROffset::Dynamic(offset), _, src, _, _) => {
            replace(offset);
            replace(src);
        }
        SIRInstruction::Store(_, SIROffset::Static(_), _, src, _, _) => replace(src),
        SIRInstruction::Commit(_, _, SIROffset::Dynamic(offset), _, _) => replace(offset),
        SIRInstruction::Commit(_, _, SIROffset::Static(_), _, _) => {}
        SIRInstruction::Concat(_, args) => {
            for arg in args {
                replace(arg);
            }
        }
        SIRInstruction::Slice(_, src, _, _) => replace(src),
        SIRInstruction::Mux(_, cond, true_val, false_val) => {
            replace(cond);
            replace(true_val);
            replace(false_val);
        }
        SIRInstruction::RuntimeEvent { args, .. }
        | SIRInstruction::CombCaptureEvent { args, .. } => {
            for arg in args {
                replace(arg);
            }
        }
        SIRInstruction::CombCaptureEnableIfChanged { old, new, .. } => {
            replace(old);
            replace(new);
        }
    }
}

fn replace_regs_in_args(
    args: &[RegisterId],
    replacements: &HashMap<RegisterId, RegisterId>,
) -> Vec<RegisterId> {
    args.iter()
        .map(|reg| replacements.get(reg).copied().unwrap_or(*reg))
        .collect()
}

fn verify_all_uses_have_defs(eu: &ExecutionUnit<RegionedAbsoluteAddr>) {
    let mut defs = HashSet::default();
    for block in eu.blocks.values() {
        defs.extend(block.params.iter().copied());
        for inst in &block.instructions {
            if let Some(def) = def_reg(inst) {
                defs.insert(def);
            }
        }
    }

    for block in eu.blocks.values() {
        for (idx, inst) in block.instructions.iter().enumerate() {
            for reg in inst_uses(inst) {
                assert!(
                    defs.contains(&reg),
                    "branchify verify: r{} used without def/param in b{} inst {}: {}",
                    reg.0,
                    block.id.0,
                    idx,
                    inst
                );
            }
        }
        for reg in terminator_uses(&block.terminator) {
            assert!(
                defs.contains(&reg),
                "branchify verify: r{} used without def/param in b{} terminator: {}",
                reg.0,
                block.id.0,
                block.terminator
            );
        }
    }
}

fn count_uses(eu: &ExecutionUnit<RegionedAbsoluteAddr>) -> HashMap<RegisterId, usize> {
    let mut counts = HashMap::default();
    for block in eu.blocks.values() {
        add_block_uses(&mut counts, block);
    }
    counts
}

fn block_use_count(block: &BasicBlock<RegionedAbsoluteAddr>, reg: RegisterId) -> usize {
    let inst_uses = block
        .instructions
        .iter()
        .map(|inst| {
            inst_uses(inst)
                .into_iter()
                .filter(|use_reg| *use_reg == reg)
                .count()
        })
        .sum::<usize>();
    let term_uses = terminator_uses(&block.terminator)
        .into_iter()
        .filter(|use_reg| *use_reg == reg)
        .count();
    inst_uses + term_uses
}

fn add_block_uses(
    counts: &mut HashMap<RegisterId, usize>,
    block: &BasicBlock<RegionedAbsoluteAddr>,
) {
    for inst in &block.instructions {
        for reg in inst_uses(inst) {
            *counts.entry(reg).or_default() += 1;
        }
    }
    for reg in terminator_uses(&block.terminator) {
        *counts.entry(reg).or_default() += 1;
    }
}

fn remove_block_uses(
    counts: &mut HashMap<RegisterId, usize>,
    block: &BasicBlock<RegionedAbsoluteAddr>,
) {
    for inst in &block.instructions {
        for reg in inst_uses(inst) {
            decrement_use(counts, reg);
        }
    }
    for reg in terminator_uses(&block.terminator) {
        decrement_use(counts, reg);
    }
}

fn decrement_use(counts: &mut HashMap<RegisterId, usize>, reg: RegisterId) {
    let Some(count) = counts.get_mut(&reg) else {
        return;
    };
    *count -= 1;
    if *count == 0 {
        counts.remove(&reg);
    }
}

fn inst_uses(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> Vec<RegisterId> {
    match inst {
        SIRInstruction::Imm(_, _) => Vec::new(),
        SIRInstruction::Binary(_, lhs, _, rhs) => vec![*lhs, *rhs],
        SIRInstruction::Unary(_, _, src) => vec![*src],
        SIRInstruction::Load(_, _, SIROffset::Dynamic(off), _) => vec![*off],
        SIRInstruction::Load(_, _, SIROffset::Static(_), _) => Vec::new(),
        SIRInstruction::Store(_, SIROffset::Dynamic(off), _, src, _, _) => vec![*off, *src],
        SIRInstruction::Store(_, SIROffset::Static(_), _, src, _, _) => vec![*src],
        SIRInstruction::Commit(_, _, SIROffset::Dynamic(off), _, _) => vec![*off],
        SIRInstruction::Commit(_, _, SIROffset::Static(_), _, _) => Vec::new(),
        SIRInstruction::Concat(_, args) => args.clone(),
        SIRInstruction::Slice(_, src, _, _) => vec![*src],
        SIRInstruction::Mux(_, cond, true_val, false_val) => vec![*cond, *true_val, *false_val],
        SIRInstruction::RuntimeEvent { args, .. }
        | SIRInstruction::CombCaptureEvent { args, .. } => args.clone(),
        SIRInstruction::CombCaptureEnableIfChanged { old, new, .. } => vec![*old, *new],
    }
}

fn terminator_uses(term: &SIRTerminator) -> Vec<RegisterId> {
    match term {
        SIRTerminator::Jump(_, args) => args.clone(),
        SIRTerminator::Branch {
            cond,
            true_block,
            false_block,
        } => {
            let mut uses = Vec::with_capacity(1 + true_block.1.len() + false_block.1.len());
            uses.push(*cond);
            uses.extend(true_block.1.iter().copied());
            uses.extend(false_block.1.iter().copied());
            uses
        }
        SIRTerminator::Return | SIRTerminator::Error(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{InstanceId, RegisterType, SIRValue};
    use num_bigint::BigUint;
    use veryl_analyzer::ir::VarId;

    fn addr(id: usize) -> RegionedAbsoluteAddr {
        RegionedAbsoluteAddr {
            region: 0,
            instance_id: InstanceId(id),
            var_id: VarId::default(),
        }
    }

    fn unit(
        instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
    ) -> ExecutionUnit<RegionedAbsoluteAddr> {
        let mut register_map = HashMap::default();
        for reg in 0..16 {
            register_map.insert(
                RegisterId(reg),
                RegisterType::Bit {
                    width: 64,
                    signed: false,
                },
            );
        }
        let mut blocks = HashMap::default();
        blocks.insert(
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: Vec::new(),
                instructions,
                terminator: SIRTerminator::Return,
            },
        );
        ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        }
    }

    fn imm(dst: usize, value: u64) -> SIRInstruction<RegionedAbsoluteAddr> {
        SIRInstruction::Imm(RegisterId(dst), SIRValue::new(BigUint::from(value)))
    }

    #[test]
    fn branchifies_single_use_mux_arm_work() {
        let mut eu = unit(vec![
            imm(1, 3),
            imm(4, 5),
            SIRInstruction::Binary(
                RegisterId(2),
                RegisterId(1),
                crate::ir::BinaryOp::Mul,
                RegisterId(1),
            ),
            SIRInstruction::Mux(RegisterId(3), RegisterId(0), RegisterId(2), RegisterId(4)),
            SIRInstruction::Store(
                addr(0),
                SIROffset::Static(0),
                64,
                RegisterId(3),
                Vec::new(),
                Vec::new(),
            ),
        ]);

        BranchifyMuxPass.run(&mut eu, &PassOptions::default());

        let head = &eu.blocks[&BlockId(0)];
        assert!(matches!(head.terminator, SIRTerminator::Branch { .. }));
        assert!(eu.blocks.values().any(|block| {
            block.params.is_empty() && matches!(block.terminator, SIRTerminator::Return)
        }));
        assert!(eu.blocks.values().any(|block| {
            block
                .instructions
                .iter()
                .any(|inst| matches!(inst, SIRInstruction::Store(_, _, 64, RegisterId(2), _, _)))
        }));
        let SIRTerminator::Branch { false_block, .. } = &head.terminator else {
            panic!("expected mux to become branch");
        };
        assert!(false_block.1.is_empty());
        let false_block = &eu.blocks[&false_block.0];
        assert!(
            false_block.instructions.iter().any(|inst| {
                matches!(inst, SIRInstruction::Store(_, _, 64, RegisterId(4), _, _))
            })
        );
        assert!(eu.blocks.values().any(|block| {
            block.instructions.iter().any(|inst| {
                matches!(
                    inst,
                    SIRInstruction::Binary(RegisterId(2), _, crate::ir::BinaryOp::Mul, _)
                )
            })
        }));
        assert!(!head.instructions.iter().any(|inst| {
            matches!(
                inst,
                SIRInstruction::Binary(RegisterId(2), _, crate::ir::BinaryOp::Mul, _)
            )
        }));
    }

    #[test]
    fn keeps_shared_mux_input_hoisted() {
        let mut eu = unit(vec![
            imm(1, 3),
            SIRInstruction::Binary(
                RegisterId(2),
                RegisterId(1),
                crate::ir::BinaryOp::Mul,
                RegisterId(1),
            ),
            SIRInstruction::Mux(RegisterId(3), RegisterId(0), RegisterId(2), RegisterId(2)),
        ]);

        BranchifyMuxPass.run(&mut eu, &PassOptions::default());

        assert_eq!(eu.blocks.len(), 1);
    }

    #[test]
    fn branchifies_non_store_mux_with_arm_work() {
        let mut eu = unit(vec![
            imm(1, 3),
            SIRInstruction::Binary(
                RegisterId(2),
                RegisterId(1),
                crate::ir::BinaryOp::Mul,
                RegisterId(1),
            ),
            SIRInstruction::Binary(
                RegisterId(4),
                RegisterId(1),
                crate::ir::BinaryOp::Add,
                RegisterId(1),
            ),
            SIRInstruction::Mux(RegisterId(3), RegisterId(0), RegisterId(2), RegisterId(4)),
            SIRInstruction::Unary(RegisterId(5), crate::ir::UnaryOp::BitNot, RegisterId(3)),
        ]);

        BranchifyMuxPass.run(&mut eu, &PassOptions::default());

        assert_eq!(eu.blocks.len(), 4);
        assert!(!eu.blocks.values().any(|block| {
            block
                .instructions
                .iter()
                .any(|inst| matches!(inst, SIRInstruction::Mux(RegisterId(3), _, _, _)))
        }));
        assert!(
            eu.blocks
                .values()
                .any(|block| block.params == vec![RegisterId(3)])
        );
        assert!(eu.blocks.values().any(|block| {
            block.instructions.iter().any(|inst| {
                matches!(
                    inst,
                    SIRInstruction::Binary(RegisterId(2), _, crate::ir::BinaryOp::Mul, _)
                )
            })
        }));
        assert!(eu.blocks.values().any(|block| {
            block.instructions.iter().any(|inst| {
                matches!(
                    inst,
                    SIRInstruction::Binary(RegisterId(4), _, crate::ir::BinaryOp::Add, _)
                )
            })
        }));
    }

    #[test]
    fn does_not_branchify_mux_with_external_uses() {
        let mut eu = unit(vec![
            imm(1, 3),
            SIRInstruction::Binary(
                RegisterId(2),
                RegisterId(1),
                crate::ir::BinaryOp::Mul,
                RegisterId(1),
            ),
            SIRInstruction::Mux(RegisterId(3), RegisterId(0), RegisterId(2), RegisterId(4)),
        ]);
        eu.blocks.get_mut(&BlockId(0)).unwrap().terminator =
            SIRTerminator::Jump(BlockId(1), Vec::new());
        eu.blocks.insert(
            BlockId(1),
            BasicBlock {
                id: BlockId(1),
                params: Vec::new(),
                instructions: vec![SIRInstruction::Unary(
                    RegisterId(5),
                    crate::ir::UnaryOp::BitNot,
                    RegisterId(3),
                )],
                terminator: SIRTerminator::Return,
            },
        );

        BranchifyMuxPass.run(&mut eu, &PassOptions::default());

        assert_eq!(eu.blocks.len(), 2);
        assert!(eu.blocks[&BlockId(0)].instructions.iter().any(|inst| {
            matches!(
                inst,
                SIRInstruction::Mux(RegisterId(3), RegisterId(0), RegisterId(2), RegisterId(4))
            )
        }));
    }

    #[test]
    fn does_not_sink_load_across_aliasing_store() {
        let mut eu = unit(vec![
            SIRInstruction::Load(RegisterId(1), addr(0), SIROffset::Static(0), 64),
            SIRInstruction::Store(
                addr(0),
                SIROffset::Static(0),
                64,
                RegisterId(4),
                Vec::new(),
                Vec::new(),
            ),
            SIRInstruction::Binary(
                RegisterId(2),
                RegisterId(1),
                crate::ir::BinaryOp::Mul,
                RegisterId(1),
            ),
            SIRInstruction::Mux(RegisterId(3), RegisterId(0), RegisterId(2), RegisterId(5)),
            SIRInstruction::Store(
                addr(1),
                SIROffset::Static(0),
                64,
                RegisterId(3),
                Vec::new(),
                Vec::new(),
            ),
        ]);

        BranchifyMuxPass.run(&mut eu, &PassOptions::default());

        let head = &eu.blocks[&BlockId(0)];
        assert!(matches!(head.terminator, SIRTerminator::Branch { .. }));
        assert!(
            head.instructions
                .iter()
                .any(|inst| { matches!(inst, SIRInstruction::Load(RegisterId(1), _, _, _)) })
        );
        assert!(eu.blocks.values().any(|block| {
            block.instructions.iter().any(|inst| {
                matches!(
                    inst,
                    SIRInstruction::Binary(RegisterId(2), _, crate::ir::BinaryOp::Mul, _)
                )
            })
        }));
    }

    #[test]
    fn passes_live_ins_to_sunk_arm_blocks() {
        let mut eu = unit(vec![
            imm(1, 3),
            imm(4, 5),
            SIRInstruction::Binary(
                RegisterId(2),
                RegisterId(7),
                crate::ir::BinaryOp::And,
                RegisterId(1),
            ),
            SIRInstruction::Mux(RegisterId(3), RegisterId(0), RegisterId(2), RegisterId(4)),
            SIRInstruction::Store(
                addr(0),
                SIROffset::Static(0),
                64,
                RegisterId(3),
                Vec::new(),
                Vec::new(),
            ),
        ]);
        eu.blocks.get_mut(&BlockId(0)).unwrap().params = vec![RegisterId(7)];

        BranchifyMuxPass.run(&mut eu, &PassOptions::default());

        let head = &eu.blocks[&BlockId(0)];
        let SIRTerminator::Branch {
            true_block: true_edge,
            false_block: false_edge,
            ..
        } = &head.terminator
        else {
            panic!("expected mux to become branch");
        };
        let true_block = &eu.blocks[&true_edge.0];
        let false_block = &eu.blocks[&false_edge.0];
        assert_eq!(true_edge.1, vec![RegisterId(7)]);
        assert!(false_edge.1.is_empty());
        assert_eq!(true_block.params.len(), 1);
        assert!(false_block.params.is_empty());
        assert_ne!(true_block.params[0], RegisterId(7));
        assert!(true_block.instructions.iter().any(|inst| {
            matches!(
                inst,
                SIRInstruction::Binary(dst, lhs, crate::ir::BinaryOp::And, _)
                    if *dst == RegisterId(2) && *lhs == true_block.params[0]
            )
        }));
    }

    #[test]
    fn branchifies_when_suffix_uses_dominating_live_in() {
        let mut eu = unit(vec![
            imm(1, 3),
            imm(6, 11),
            SIRInstruction::Binary(
                RegisterId(2),
                RegisterId(1),
                crate::ir::BinaryOp::Mul,
                RegisterId(1),
            ),
            SIRInstruction::Mux(RegisterId(3), RegisterId(0), RegisterId(2), RegisterId(4)),
            SIRInstruction::Binary(
                RegisterId(5),
                RegisterId(6),
                crate::ir::BinaryOp::Add,
                RegisterId(3),
            ),
        ]);

        BranchifyMuxPass.run(&mut eu, &PassOptions::default());

        assert_eq!(eu.blocks.len(), 4);
        assert!(eu.blocks.values().any(|block| {
            block.instructions.iter().any(|inst| {
                matches!(
                    inst,
                    SIRInstruction::Binary(
                        RegisterId(5),
                        RegisterId(6),
                        crate::ir::BinaryOp::Add,
                        RegisterId(3)
                    )
                )
            })
        }));
    }

    #[test]
    fn passes_param_origin_suffix_live_ins_through_merge() {
        let mut eu = unit(vec![
            imm(1, 3),
            SIRInstruction::Binary(
                RegisterId(2),
                RegisterId(1),
                crate::ir::BinaryOp::Mul,
                RegisterId(1),
            ),
            SIRInstruction::Mux(RegisterId(3), RegisterId(0), RegisterId(2), RegisterId(4)),
            SIRInstruction::Binary(
                RegisterId(5),
                RegisterId(7),
                crate::ir::BinaryOp::Add,
                RegisterId(3),
            ),
        ]);
        eu.blocks.get_mut(&BlockId(0)).unwrap().params = vec![RegisterId(7)];

        BranchifyMuxPass.run(&mut eu, &PassOptions::default());

        let merge = eu
            .blocks
            .values()
            .find(|block| {
                block
                    .params
                    .first()
                    .is_some_and(|param| *param == RegisterId(3))
            })
            .expect("expected merge block with mux result param");
        assert_eq!(merge.params.len(), 2);
        assert_ne!(merge.params[1], RegisterId(7));
        assert!(merge.instructions.iter().any(|inst| {
            matches!(
                inst,
                SIRInstruction::Binary(RegisterId(5), lhs, crate::ir::BinaryOp::Add, RegisterId(3))
                    if *lhs == merge.params[1]
            )
        }));
        assert!(eu.blocks.values().any(|block| {
            matches!(
                &block.terminator,
                SIRTerminator::Jump(target, args)
                    if *target == merge.id && args.len() == 2 && args[1] != RegisterId(7)
            )
        }));
    }

    #[test]
    fn inlines_param_only_branch_blocks_from_jump_predecessors() {
        let mut register_map = HashMap::default();
        for reg in 0..8 {
            register_map.insert(
                RegisterId(reg),
                RegisterType::Bit {
                    width: 64,
                    signed: false,
                },
            );
        }
        let mut blocks = HashMap::default();
        blocks.insert(
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: Vec::new(),
                instructions: vec![imm(1, 3)],
                terminator: SIRTerminator::Jump(BlockId(1), vec![RegisterId(1)]),
            },
        );
        blocks.insert(
            BlockId(1),
            BasicBlock {
                id: BlockId(1),
                params: vec![RegisterId(2)],
                instructions: Vec::new(),
                terminator: SIRTerminator::Branch {
                    cond: RegisterId(0),
                    true_block: (BlockId(2), vec![RegisterId(2)]),
                    false_block: (BlockId(3), vec![RegisterId(2)]),
                },
            },
        );
        blocks.insert(
            BlockId(2),
            BasicBlock {
                id: BlockId(2),
                params: vec![RegisterId(4)],
                instructions: Vec::new(),
                terminator: SIRTerminator::Return,
            },
        );
        blocks.insert(
            BlockId(3),
            BasicBlock {
                id: BlockId(3),
                params: vec![RegisterId(5)],
                instructions: Vec::new(),
                terminator: SIRTerminator::Return,
            },
        );
        let mut eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        };

        inline_param_only_jump_blocks(&mut eu);

        assert!(!eu.blocks.contains_key(&BlockId(1)));
        assert!(matches!(
            &eu.blocks[&BlockId(0)].terminator,
            SIRTerminator::Branch {
                true_block,
                false_block,
                ..
            } if true_block.1 == vec![RegisterId(1)] && false_block.1 == vec![RegisterId(1)]
        ));
    }

    #[test]
    fn branchifies_mux_feeding_jump_args_for_param_only_inlining() {
        let mut register_map = HashMap::default();
        for reg in 0..8 {
            register_map.insert(
                RegisterId(reg),
                RegisterType::Bit {
                    width: 64,
                    signed: false,
                },
            );
        }
        let mut blocks = HashMap::default();
        blocks.insert(
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: Vec::new(),
                instructions: vec![imm(1, 1), imm(2, 2), imm(3, 3)],
                terminator: SIRTerminator::Jump(
                    BlockId(1),
                    vec![RegisterId(1), RegisterId(2), RegisterId(3)],
                ),
            },
        );
        blocks.insert(
            BlockId(1),
            BasicBlock {
                id: BlockId(1),
                params: vec![RegisterId(4), RegisterId(5), RegisterId(6)],
                instructions: vec![SIRInstruction::Mux(
                    RegisterId(7),
                    RegisterId(4),
                    RegisterId(5),
                    RegisterId(6),
                )],
                terminator: SIRTerminator::Jump(BlockId(2), vec![RegisterId(7)]),
            },
        );
        blocks.insert(
            BlockId(2),
            BasicBlock {
                id: BlockId(2),
                params: vec![RegisterId(7)],
                instructions: Vec::new(),
                terminator: SIRTerminator::Return,
            },
        );
        let mut eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        };

        BranchifyMuxPass.run(&mut eu, &PassOptions::default());

        assert!(!eu.blocks.values().any(|block| {
            block
                .instructions
                .iter()
                .any(|inst| matches!(inst, SIRInstruction::Mux(RegisterId(7), _, _, _)))
        }));
    }

    #[test]
    fn preserves_mux_result_through_merge_when_used_after_store() {
        let mut eu = unit(vec![
            imm(1, 3),
            SIRInstruction::Binary(
                RegisterId(2),
                RegisterId(1),
                crate::ir::BinaryOp::Mul,
                RegisterId(1),
            ),
            SIRInstruction::Mux(RegisterId(3), RegisterId(0), RegisterId(2), RegisterId(4)),
            SIRInstruction::Store(
                addr(0),
                SIROffset::Static(0),
                64,
                RegisterId(3),
                Vec::new(),
                Vec::new(),
            ),
            SIRInstruction::Store(
                addr(1),
                SIROffset::Static(0),
                64,
                RegisterId(3),
                Vec::new(),
                Vec::new(),
            ),
        ]);

        BranchifyMuxPass.run(&mut eu, &PassOptions::default());

        assert_eq!(eu.blocks.len(), 4);
        assert!(!eu.blocks.values().any(|block| {
            block
                .instructions
                .iter()
                .any(|inst| matches!(inst, SIRInstruction::Mux(RegisterId(3), _, _, _)))
        }));
        assert!(
            eu.blocks
                .values()
                .any(|block| block.params == vec![RegisterId(3)])
        );
        assert!(eu.blocks.values().any(|block| {
            block
                .instructions
                .iter()
                .any(|inst| matches!(inst, SIRInstruction::Store(_, _, 64, RegisterId(3), _, _)))
        }));
        assert!(!eu.blocks.values().any(|block| {
            block
                .instructions
                .iter()
                .any(|inst| matches!(inst, SIRInstruction::Store(_, _, 64, RegisterId(2), _, _)))
        }));
        assert!(!eu.blocks.values().any(|block| {
            block
                .instructions
                .iter()
                .any(|inst| matches!(inst, SIRInstruction::Store(_, _, 64, RegisterId(4), _, _)))
        }));
    }
}
