//! Local mux planning, memory-conflict checks, and CFG rewriting.

use super::*;

pub(super) fn find_branchify_mux_in_block(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block_id: BlockId,
    use_counts: &HashMap<RegisterId, usize>,
    def_blocks: &HashMap<RegisterId, BlockId>,
) -> Option<BranchifyPlan> {
    let block = eu.blocks.get(&block_id)?;
    let mut def_pos = HashMap::default();
    for (idx, inst) in block.instructions.iter().enumerate() {
        if let Some(def) = def_reg(inst) {
            def_pos.insert(def, idx);
        }
    }

    let mut local_uses = HashMap::default();
    add_block_uses(&mut local_uses, block);
    let mut suffix_chunks = None;
    let mut store_suffix_chunks = None;
    for (mux_idx, inst) in block.instructions.iter().enumerate() {
        let SIRInstruction::Mux(dst, cond, true_val, false_val) = inst else {
            continue;
        };

        if use_counts.get(dst).copied().unwrap_or(0) > local_uses.get(dst).copied().unwrap_or(0) {
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
        let plan = BranchifyPlan {
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
        };
        let live_through_chunks = Some(if plan.preserve_result {
            suffix_chunks.get_or_insert_with(|| mux_live_through_chunks(block, &eu.register_map))
                [mux_idx]
        } else {
            store_suffix_chunks
                .get_or_insert_with(|| mux_store_live_through_chunks(block, &eu.register_map))
                [mux_idx]
        });
        if !branch_is_profitable(eu, block, &plan, def_blocks, &def_pos, live_through_chunks) {
            continue;
        }
        return Some(plan);
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

pub(super) fn memory_write(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> Option<MemAccess<'_>> {
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
        SIROffset::Dynamic(_)
        | SIROffset::Element { .. }
        | SIROffset::ElementRun { .. }
        | SIROffset::PackedElements { .. } => None,
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

pub(super) fn is_memory_barrier(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> bool {
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

pub(super) fn apply_branchify_mux(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    plan: BranchifyPlan,
    use_counts: &mut HashMap<RegisterId, usize>,
    def_blocks: &mut HashMap<RegisterId, BlockId>,
    next_block_id: &mut usize,
    reg_counter: &mut usize,
    trace_reg: Option<RegisterId>,
) -> [BlockId; 3] {
    let true_id = BlockId(*next_block_id);
    let false_id = BlockId(*next_block_id + 1);
    let merge_id = BlockId(*next_block_id + 2);
    *next_block_id += 3;

    let original = eu
        .blocks
        .remove(&plan.block_id)
        .expect("branchify target block must exist");
    if let Some(reg) = trace_reg {
        trace_reg_in_original(&original, &plan, reg);
    }
    remove_block_uses(use_counts, &original);
    let remove_defs = removable_defs_after_head_restore(&original, &plan, def_blocks);
    if let Some(reg) = trace_reg {
        trace_reg_branchify_plan(&original, &plan, &remove_defs, reg);
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
    let (mut head_insts, suffix) =
        partition_instructions(original.instructions, plan.mux_idx, &remove_defs);
    let branch_cond = normalize_branch_condition(
        &mut eu.register_map,
        &mut head_insts,
        plan.cond,
        reg_counter,
    );
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
    let merge_params = if plan.preserve_result {
        vec![plan.dst]
    } else {
        Vec::new()
    };

    let merge_terminator = original.terminator;

    let head = BasicBlock {
        id: plan.block_id,
        params: original.params,
        instructions: head_insts,
        terminator: SIRTerminator::Branch {
            cond: branch_cond,
            true_block: (true_id, Vec::new()),
            false_block: (false_id, Vec::new()),
        },
    };
    let true_block = BasicBlock {
        id: true_id,
        params: Vec::new(),
        instructions: true_insts,
        terminator: SIRTerminator::Jump(merge_id, true_args),
    };
    let false_block = BasicBlock {
        id: false_id,
        params: Vec::new(),
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

    for block_id in [plan.block_id, true_id, false_id, merge_id] {
        for inst in &eu.blocks[&block_id].instructions {
            if let Some(def) = def_reg(inst) {
                def_blocks.insert(def, block_id);
            }
        }
    }

    if let Some(reg) = trace_reg {
        for block_id in [plan.block_id, true_id, false_id, merge_id] {
            if let Some(block) = eu.blocks.get(&block_id) {
                trace_reg_in_new_block(block, reg);
            }
        }
    }

    [true_id, false_id, merge_id]
}

// Reuse the original allocation for the larger half. A local rewrite often
// peels a tiny head from a very long suffix; cloning and growing that suffix
// for every accepted Mux doubles live instructions and repeatedly allocates
// large buffers. Only arm definitions shared by the new branches need clones.
fn partition_instructions(
    mut instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
    mux_idx: usize,
    remove_defs: &HashSet<usize>,
) -> (
    Vec<SIRInstruction<RegionedAbsoluteAddr>>,
    Vec<SIRInstruction<RegionedAbsoluteAddr>>,
) {
    let (mut head, mut suffix) = if mux_idx < instructions.len() / 2 {
        let head = instructions.drain(..=mux_idx).collect::<Vec<_>>();
        (head, instructions)
    } else {
        let suffix = instructions.split_off(mux_idx + 1);
        (instructions, suffix)
    };
    head.truncate(mux_idx);
    for (values, offset) in [(&mut head, 0), (&mut suffix, mux_idx + 1)] {
        let mut index = offset;
        values.retain(|_| {
            let keep = !remove_defs.contains(&index);
            index += 1;
            keep
        });
        // Release a buffer only after its retained contents have halved,
        // instead of reallocating it after each small suffix rewrite.
        if values.len() < values.capacity() / 2 {
            values.shrink_to_fit();
        }
    }
    (head, suffix)
}

#[cfg(test)]
#[test]
fn partitions_reuse_the_larger_side_and_preserve_instruction_order() {
    use crate::ir::SIRValue;
    for mux_idx in [0, 8, 32, 56, 63] {
        let mut instructions = Vec::with_capacity(64);
        for id in 0..64 {
            instructions.push(SIRInstruction::Imm(
                RegisterId(id),
                SIRValue::new(id as u64),
            ));
        }
        let allocation = instructions.as_ptr();
        let removed = [2, mux_idx, mux_idx + 1].into_iter().collect();
        let (head, suffix) = partition_instructions(instructions, mux_idx, &removed);
        let ids = |values: &[SIRInstruction<RegionedAbsoluteAddr>]| {
            values
                .iter()
                .map(|inst| def_reg(inst).unwrap().0)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            ids(&head),
            (0..mux_idx)
                .filter(|id| !removed.contains(id))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            ids(&suffix),
            (mux_idx + 1..64)
                .filter(|id| !removed.contains(id))
                .collect::<Vec<_>>()
        );
        if matches!(mux_idx, 8 | 56) {
            assert_eq!(
                allocation,
                if mux_idx < 32 {
                    suffix.as_ptr()
                } else {
                    head.as_ptr()
                }
            );
        }
    }
}

pub(super) fn removable_defs_after_head_restore(
    original: &BasicBlock<RegionedAbsoluteAddr>,
    plan: &BranchifyPlan,
    def_blocks: &HashMap<RegisterId, BlockId>,
) -> HashSet<usize> {
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
    let restore_defs = head_restore_defs(original, plan, &remove_defs, def_blocks);
    for idx in restore_defs {
        remove_defs.remove(&idx);
    }
    remove_defs
}
