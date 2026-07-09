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
        let mut use_counts = count_uses(eu);
        let mut block_ids = eu.blocks.keys().copied().collect::<Vec<_>>();
        block_ids.sort_by_key(|id| id.0);
        let mut worklist = VecDeque::from(block_ids);
        let mut queued = HashSet::default();
        queued.extend(worklist.iter().copied());

        while let Some(block_id) = worklist.pop_front() {
            queued.remove(&block_id);
            if !eu.blocks.contains_key(&block_id) {
                continue;
            }
            while let Some(plan) = find_branchify_mux_in_block(eu, block_id, &use_counts) {
                let new_blocks = apply_branchify_mux(eu, plan, &mut use_counts);
                for new_block in new_blocks {
                    if queued.insert(new_block) {
                        worklist.push_back(new_block);
                    }
                }
            }
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
        if true_defs
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
) -> [BlockId; 3] {
    let next_id = eu.blocks.keys().map(|id| id.0).max().unwrap_or(0) + 1;
    let true_id = BlockId(next_id);
    let false_id = BlockId(next_id + 1);
    let merge_id = BlockId(next_id + 2);

    let original = eu
        .blocks
        .remove(&plan.block_id)
        .expect("branchify target block must exist");
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

    let mut head_insts = Vec::new();
    for (idx, inst) in original.instructions.iter().enumerate().take(plan.mux_idx) {
        if !remove_defs.contains(&idx) {
            head_insts.push(inst.clone());
        }
    }

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
        .map(|&idx| original.instructions[idx].clone())
        .collect::<Vec<_>>();
    let mut false_insts = plan
        .false_defs
        .iter()
        .map(|&idx| original.instructions[idx].clone())
        .collect::<Vec<_>>();
    if let Some(store) = &plan.distributed_store {
        true_insts.push(store.true_inst.clone());
        false_insts.push(store.false_inst.clone());
    }

    let head = BasicBlock {
        id: plan.block_id,
        params: original.params,
        instructions: head_insts,
        terminator: SIRTerminator::Branch {
            cond: plan.cond,
            true_block: (true_id, Vec::new()),
            false_block: (false_id, Vec::new()),
        },
    };
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
        terminator: original.terminator,
    };

    add_block_uses(use_counts, &head);
    add_block_uses(use_counts, &true_block);
    add_block_uses(use_counts, &false_block);
    add_block_uses(use_counts, &merge_block);

    eu.blocks.insert(plan.block_id, head);
    eu.blocks.insert(true_id, true_block);
    eu.blocks.insert(false_id, false_block);
    eu.blocks.insert(merge_id, merge_block);

    [true_id, false_id, merge_id]
}

fn count_uses(eu: &ExecutionUnit<RegionedAbsoluteAddr>) -> HashMap<RegisterId, usize> {
    let mut counts = HashMap::default();
    for block in eu.blocks.values() {
        add_block_uses(&mut counts, block);
    }
    counts
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
        assert!(eu.blocks.values().any(|block| {
            block
                .instructions
                .iter()
                .any(|inst| matches!(inst, SIRInstruction::Store(_, _, 64, RegisterId(4), _, _)))
        }));
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
    fn preserves_mux_result_through_merge_when_used_after_store() {
        let mut eu = unit(vec![
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
