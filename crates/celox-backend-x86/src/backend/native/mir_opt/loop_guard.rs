//! Defer an expensive, private boolean operand until its guard passes.

use super::*;

pub(super) fn movable(inst: &MInst) -> bool {
    matches!(
        inst,
        MInst::LoadImm { .. }
            | MInst::Mov { .. }
            | MInst::Mov32 { .. }
            | MInst::Load {
                base: BaseReg::SimState,
                ..
            }
            | MInst::LoadIndexed {
                base: BaseReg::SimState,
                ..
            }
            | MInst::Add { .. }
            | MInst::Add32 { .. }
            | MInst::AddImm { .. }
            | MInst::Sub { .. }
            | MInst::Sub32 { .. }
            | MInst::SubImm { .. }
            | MInst::Mul { .. }
            | MInst::Mul32 { .. }
            | MInst::And { .. }
            | MInst::And32 { .. }
            | MInst::AndImm { .. }
            | MInst::AndImm32 { .. }
            | MInst::Or { .. }
            | MInst::Or32 { .. }
            | MInst::OrImm { .. }
            | MInst::Xor { .. }
            | MInst::Xor32 { .. }
            | MInst::BitNot { .. }
            | MInst::Neg { .. }
            | MInst::Shl { .. }
            | MInst::ShlImm { .. }
            | MInst::Shr { .. }
            | MInst::ShrImm { .. }
            | MInst::Sar { .. }
            | MInst::SarImm { .. }
            | MInst::Cmp { .. }
            | MInst::CmpImm { .. }
            | MInst::Select { .. }
            | MInst::CmpSelect { .. }
            | MInst::CmpImmSelect { .. }
    )
}

pub(super) fn private_operand(block: &MBlock, operand: VReg, uses: &[usize]) -> Vec<usize> {
    let mut needed = HashMap::<VReg, usize>::default();
    needed.insert(operand, 1);
    let mut selected = Vec::new();
    for (position, inst) in block.insts.iter().enumerate().rev() {
        let Some(dst) = inst.def() else { continue };
        if needed.get(&dst).copied() != Some(uses[dst.0 as usize]) || !movable(inst) {
            continue;
        }
        selected.push(position);
        for source in inst.uses() {
            *needed.entry(source).or_default() += 1;
        }
    }
    selected.reverse();
    selected
}

pub(super) fn run(func: &mut MFunction) {
    let facts = known_bits::known_zeros(func);
    let positions = func
        .blocks
        .iter()
        .enumerate()
        .map(|(index, block)| (block.id, index))
        .collect::<HashMap<_, _>>();
    let successors = func
        .blocks
        .iter()
        .map(|block| {
            block
                .successors()
                .into_iter()
                .map(|target| positions[&target])
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let Ok(cfg) = celox_analysis::cfg::ForwardControlFlowGraph::analyze(successors, 0) else {
        return;
    };
    let mut headers = HashSet::default();
    for (predecessor, block) in func.blocks.iter().enumerate() {
        for target in block.successors() {
            if cfg.dominators.dominates(positions[&target], predecessor) {
                headers.insert(target);
            }
        }
    }
    let mut uses = vec![0usize; func.vregs.count() as usize];
    for block in &func.blocks {
        for phi in &block.phis {
            for &(_, source) in &phi.sources {
                uses[source.0 as usize] += 1;
            }
        }
        for inst in &block.insts {
            for source in inst.uses() {
                uses[source.0 as usize] += 1;
            }
        }
    }
    let Some(mut next_id) = func
        .blocks
        .iter()
        .map(|block| block.id.0)
        .max()
        .and_then(|id| id.checked_add(1))
    else {
        return;
    };
    let mut index = 0;
    while index < func.blocks.len() {
        let block = &func.blocks[index];
        index += 1;
        if !headers.contains(&block.id) {
            continue;
        }
        let Some(&MInst::Branch {
            cond,
            true_bb,
            false_bb,
        }) = block.terminator()
        else {
            continue;
        };
        if true_bb == false_bb || uses[cond.0 as usize] != 1 {
            continue;
        }
        let Some((condition_position, condition)) = block
            .insts
            .iter()
            .enumerate()
            .find(|(_, inst)| inst.def() == Some(cond))
        else {
            continue;
        };
        let (lhs, rhs, is_and) = match condition {
            MInst::And { lhs, rhs, .. } | MInst::And32 { lhs, rhs, .. } => (*lhs, *rhs, true),
            MInst::Or { lhs, rhs, .. } | MInst::Or32 { lhs, rhs, .. } => (*lhs, *rhs, false),
            _ => continue,
        };
        if lhs == rhs
            || (!is_and
                && [lhs, rhs]
                    .iter()
                    .any(|source| !facts[source.0 as usize] & !1 != 0))
        {
            continue;
        }
        let mut best = None;
        for (guard, delayed) in [(lhs, rhs), (rhs, lhs)] {
            let selected = private_operand(block, delayed, &uses);
            let cost = selected
                .iter()
                .filter(|&&position| {
                    !matches!(
                        block.insts[position],
                        MInst::LoadImm { .. } | MInst::Mov { .. } | MInst::Mov32 { .. }
                    )
                })
                .count();
            if cost < 6 {
                continue;
            }
            let first = selected[0];
            // Loads must not pass a write, call, or other observable operation.
            if block.insts[first..block.insts.len() - 1]
                .iter()
                .any(|inst| !movable(inst))
            {
                continue;
            }
            if best
                .as_ref()
                .is_none_or(|(_, _, _, previous)| cost > *previous)
            {
                best = Some((guard, delayed, selected, cost));
            }
        }
        let Some((guard, delayed, selected, _)) = best else {
            continue;
        };
        let Some(following_id) = next_id.checked_add(1) else {
            return;
        };
        let new_id = BlockId(next_id);
        next_id = following_id;
        let old_id = block.id;
        let deferred_condition = block.insts[condition_position].clone();
        let bypass = if is_and { false_bb } else { true_bb };
        let block = &mut func.blocks[index - 1];
        let original = std::mem::take(&mut block.insts);
        let mut deferred = MBlock::new(new_id);
        let mut positions = selected.into_iter().peekable();
        let last = original.len() - 1;
        for (position, inst) in original.into_iter().enumerate() {
            if positions.peek() == Some(&position) {
                positions.next();
                deferred.push(inst);
            } else if position != condition_position && position != last {
                block.push(inst);
            }
        }
        block.push(MInst::Branch {
            cond: guard,
            true_bb: if is_and { new_id } else { true_bb },
            false_bb: if is_and { false_bb } else { new_id },
        });
        if is_and {
            deferred.push(deferred_condition);
        }
        deferred.push(MInst::Branch {
            cond: if is_and { cond } else { delayed },
            true_bb,
            false_bb,
        });
        for successor in &mut func.blocks {
            if successor.id != true_bb && successor.id != false_bb {
                continue;
            }
            for phi in &mut successor.phis {
                if let Some(row) = phi.sources.iter_mut().find(|(pred, _)| *pred == old_id) {
                    if successor.id == bypass {
                        let source = row.1;
                        phi.sources.push((new_id, source));
                    } else {
                        row.0 = new_id;
                    }
                }
            }
        }
        func.push_block(deferred);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::{emit, jit_mem::JitCode, mir_legalize, regalloc};

    fn fixture(is_and: bool, word32: bool, barrier: u8) -> MFunction {
        let mut vregs = VRegAllocator::new();
        for _ in 0..32 {
            vregs.alloc();
        }
        let mut function = MFunction::new(vregs, vec![SpillDesc::transient(); 32]);
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: VReg(0),
            value: 0,
        });
        entry.push(MInst::Jump { target: BlockId(1) });
        let mut header = MBlock::new(BlockId(1));
        header.phis.push(PhiNode {
            dst: VReg(1),
            sources: vec![(BlockId(0), VReg(0)), (BlockId(4), VReg(25))],
        });
        header.phis.push(PhiNode {
            dst: VReg(2),
            sources: vec![(BlockId(0), VReg(0)), (BlockId(4), VReg(24))],
        });
        header.push(MInst::Load {
            dst: VReg(3),
            base: BaseReg::SimState,
            offset: 0,
            size: OpSize::S64,
        });
        let guard = if is_and {
            VReg(3)
        } else {
            header.push(MInst::AndImm {
                dst: VReg(4),
                src: VReg(3),
                imm: 1,
            });
            VReg(4)
        };
        header.push(MInst::Load {
            dst: VReg(5),
            base: BaseReg::SimState,
            offset: 8,
            size: OpSize::S64,
        });
        for next in 6..20 {
            header.push(MInst::AddImm {
                dst: VReg(next),
                src: VReg(next - 1),
                imm: next as i32,
            });
        }
        header.push(MInst::AndImm {
            dst: VReg(20),
            src: VReg(19),
            imm: if is_and { 3 } else { 1 },
        });
        if barrier == 1 {
            header.push(MInst::MemFill {
                dst_offset: 8,
                byte_len: 8,
                value: 0xa5,
            });
        }
        header.push(match (is_and, word32) {
            (true, false) => MInst::And {
                dst: VReg(21),
                lhs: guard,
                rhs: VReg(20),
            },
            (true, true) => MInst::And32 {
                dst: VReg(21),
                lhs: guard,
                rhs: VReg(20),
            },
            (false, false) => MInst::Or {
                dst: VReg(21),
                lhs: guard,
                rhs: VReg(20),
            },
            (false, true) => MInst::Or32 {
                dst: VReg(21),
                lhs: guard,
                rhs: VReg(20),
            },
        });
        header.push(MInst::Branch {
            cond: VReg(21),
            true_bb: BlockId(2),
            false_bb: BlockId(3),
        });
        let mut hit = MBlock::new(BlockId(2));
        hit.push(MInst::AddImm {
            dst: VReg(23),
            src: VReg(2),
            imm: 1,
        });
        if barrier == 2 {
            hit.push(MInst::Store {
                base: BaseReg::SimState,
                offset: 24,
                src: VReg(20),
                size: OpSize::S64,
            });
        }
        hit.push(MInst::Jump { target: BlockId(4) });
        let mut miss = MBlock::new(BlockId(3));
        miss.push(MInst::Jump { target: BlockId(4) });
        let mut latch = MBlock::new(BlockId(4));
        latch.phis.push(PhiNode {
            dst: VReg(24),
            sources: vec![(BlockId(2), VReg(23)), (BlockId(3), VReg(2))],
        });
        latch.push(MInst::AddImm {
            dst: VReg(25),
            src: VReg(1),
            imm: 1,
        });
        latch.push(MInst::CmpImm {
            dst: VReg(26),
            lhs: VReg(25),
            imm: 4,
            kind: CmpKind::LtU,
        });
        latch.push(MInst::Branch {
            cond: VReg(26),
            true_bb: BlockId(1),
            false_bb: BlockId(5),
        });
        let mut exit = MBlock::new(BlockId(5));
        exit.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 16,
            src: VReg(24),
            size: OpSize::S64,
        });
        exit.push(MInst::Return);
        function.blocks = vec![entry, header, hit, miss, latch, exit];
        function
    }

    fn compile(mut function: MFunction) -> (JitCode, usize) {
        mir_legalize::legalize(&mut function);
        let allocation = regalloc::run_regalloc(&mut function).unwrap();
        let code = emit::emit(
            &function,
            &allocation.assignment,
            allocation.spill_frame_size,
        )
        .unwrap();
        (
            JitCode::new(&code.code).unwrap(),
            code.required_state_size.max(32) as usize,
        )
    }

    #[test]
    fn guarded_loops_preserve_bitwise_conditions_phis_and_memory_order() {
        for is_and in [false, true] {
            for word32 in [false, true] {
                for barrier in 0..3 {
                    let original = fixture(is_and, word32, barrier);
                    original.verify();
                    let mut optimized = original.clone();
                    run(&mut optimized);
                    optimized.verify();
                    assert_eq!(optimized.blocks.len(), if barrier == 0 { 7 } else { 6 });
                    let (before, before_size) = compile(original);
                    let (after, after_size) = compile(optimized);
                    for guard in [0u64, 1, 2, 3, 1 << 32, 1 << 63, u64::MAX] {
                        for seed in [0u64, 1, 2, 3, 0xdead_beef, u64::MAX] {
                            let mut left = vec![0u8; before_size.max(after_size)];
                            left[..8].copy_from_slice(&guard.to_le_bytes());
                            left[8..16].copy_from_slice(&seed.to_le_bytes());
                            let mut right = left.clone();
                            assert_eq!(unsafe { before.call(&mut left) }, 0);
                            assert_eq!(unsafe { after.call(&mut right) }, 0);
                            assert_eq!(
                                &left[..32],
                                &right[..32],
                                "and={is_and}, word32={word32}, barrier={barrier}, guard={guard:#x}, seed={seed:#x}"
                            );
                        }
                    }
                }
            }
        }
    }
}
