use super::*;
use crate::native::{emit, jit_mem::JitCode, regalloc};

fn bank(offsets: &[i32; 8]) -> MFunction {
    let mut f = MFunction::new(VRegAllocator::new(), vec![]);
    let mut reg = || alloc_transient_vreg(&mut f.vregs, &mut f.spill_descs);
    let key = reg();
    let base = reg();
    let target = reg();
    let mut root = MBlock::new(BlockId(0));
    root.push(MInst::Load {
        dst: key,
        base: BaseReg::SimState,
        offset: 0,
        size: OpSize::S8,
    });
    root.push(MInst::Scratch { dst: base });
    root.push(MInst::Scratch { dst: target });
    // Nonzero first key, a repeated register, and equal constants in distinct blocks.
    let targets = [9, 9, 1, 2, 3, 4, 5, 6, 7, 8, 1, 10, 10, 10, 10, 10].map(BlockId);
    root.push(MInst::JumpTable {
        index: key,
        table_base: base,
        target,
        targets: Box::new(targets),
    });
    let join = BlockId(11);
    let mut sources = vec![];
    f.blocks.push(root);
    for id in 1..=10 {
        let dst = reg();
        let mut arm = MBlock::new(BlockId(id));
        arm.push(if id <= 8 {
            MInst::Load {
                dst,
                base: BaseReg::SimState,
                offset: offsets[id as usize - 1],
                size: OpSize::S16,
            }
        } else {
            MInst::LoadImm { dst, value: 0xa55a }
        });
        arm.push(MInst::Jump { target: join });
        sources.push((arm.id, dst));
        f.blocks.push(arm);
    }
    let dst = reg();
    let mut tail = MBlock::new(join);
    tail.phis.push(PhiNode { dst, sources });
    tail.push(MInst::Store {
        src: dst,
        base: BaseReg::SimState,
        offset: 4,
        size: OpSize::S16,
    });
    tail.push(MInst::Return);
    f.blocks.push(tail);
    f
}

#[test]
fn bank_offsets_select_one_load_and_preserve_constant_arms() {
    for (stride, entry_size) in [(3, OpSize::S8), (100, OpSize::S16), (10000, OpSize::S32)] {
        let offsets = [7, 2, 4, 1, 0, 6, 5, 3].map(|i| 31 + i * stride);
        let mut f = bank(&offsets);
        run(&mut f);
        f.verify_result().unwrap();
        assert_eq!(f.blocks.len(), 4);
        assert!(
            f.blocks
                .iter()
                .flat_map(|b| &b.insts)
                .any(|i| matches!(i,MInst::LoadPtrIndexed{size,..} if *size==entry_size))
        );
        let alloc = regalloc::run_regalloc(&mut f).unwrap();
        let emitted = emit::emit(&f, &alloc.assignment, alloc.spill_frame_size).unwrap();
        let code = JitCode::new(&emitted.code).unwrap();
        let mut state = vec![0u8; emitted.required_state_size as usize];
        for (i, &offset) in offsets.iter().enumerate() {
            state[offset as usize..offset as usize + 2]
                .copy_from_slice(&(0x8000 + i as u16).to_le_bytes());
        }
        for key in 0..16 {
            state[0] = key;
            assert_eq!(unsafe { code.call(&mut state) }, 0);
            let expected = match key {
                2..=9 => 0x8000 + u16::from(key - 2),
                10 => 0x8000,
                _ => 0xa55a,
            };
            assert_eq!(u16::from_le_bytes([state[4], state[5]]), expected);
        }
    }
}

#[test]
fn regularly_spaced_homes_use_one_scaled_load_with_the_original_bounds() {
    for stride in [1, 2, 4, 8] {
        let offsets = std::array::from_fn(|i| 32 + i as i32 * stride);
        let mut f = bank(&offsets);
        // Replace the repeated first home after the interval with the fallback.
        let Some(MInst::JumpTable { targets, .. }) = f.blocks[0].insts.last_mut() else {
            unreachable!();
        };
        targets[10] = BlockId(10);
        run(&mut f);
        f.verify_result().unwrap();
        assert!(f.constant_tables().is_empty());
        assert!(f.blocks.iter().flat_map(|b| &b.insts).any(|inst| matches!(
            inst, MInst::LoadIndexed { offset: 32, scale, alias_range, .. }
                if i32::from(*scale) == stride
                    && *alias_range == MemoryAliasRange::new(32, (7 * stride + 2) as usize)
        )));

        let alloc = regalloc::run_regalloc(&mut f).unwrap();
        let emitted = emit::emit(&f, &alloc.assignment, alloc.spill_frame_size).unwrap();
        let code = JitCode::new(&emitted.code).unwrap();
        let mut state: Vec<u8> = (0..emitted.required_state_size).map(|i| i as u8).collect();
        for key in 0u8..16 {
            state[0] = key;
            let expected = if (2..10).contains(&key) {
                let pos = offsets[(key - 2) as usize] as usize;
                [state[pos], state[pos + 1]]
            } else {
                0xa55au16.to_le_bytes()
            };
            assert_eq!(unsafe { code.call(&mut state) }, 0);
            assert_eq!(state[4..6], expected);
        }
    }
}

#[test]
fn ineligible_arms_keep_the_jump_table() {
    for variant in 0..4 {
        let mut f = bank(&[21, 34, 43, 55, 62, 74, 88, 93]);
        match variant {
            0 => {
                let Some(MInst::JumpTable { targets, .. }) = f.blocks[0].insts.last_mut() else {
                    unreachable!()
                };
                targets.swap(0, 6);
            }
            1 => {
                let MInst::LoadImm { value, .. } = &mut f.blocks[10].insts[0] else {
                    unreachable!()
                };
                *value = 7;
            }
            2 => {
                // A second entry into a load arm must retain its original fixed address.
                let mut entry = MBlock::new(BlockId(12));
                let condition = alloc_transient_vreg(&mut f.vregs, &mut f.spill_descs);
                entry.push(MInst::Load {
                    dst: condition,
                    base: BaseReg::SimState,
                    offset: 1,
                    size: OpSize::S8,
                });
                entry.push(MInst::Branch {
                    cond: condition,
                    true_bb: BlockId(2),
                    false_bb: BlockId(0),
                });
                f.blocks.insert(0, entry);
            }
            _ => f.blocks[2].insts.insert(
                0,
                MInst::StoreImm {
                    base: BaseReg::SimState,
                    offset: 12,
                    size: OpSize::S8,
                    imm: 1,
                },
            ),
        }
        run(&mut f);
        assert!(
            f.blocks
                .iter()
                .any(|b| matches!(b.terminator(), Some(MInst::JumpTable { .. })))
        );
        assert!(f.constant_tables().is_empty());
    }
}

#[test]
fn a_full_bank_needs_no_range_branch() {
    let mut f = bank(&[21, 34, 43, 55, 62, 74, 88, 93]);
    let Some(MInst::JumpTable { targets, .. }) = f.blocks[0].insts.last_mut() else {
        unreachable!()
    };
    for (i, target) in targets.iter_mut().enumerate() {
        *target = BlockId((i % 8 + 1) as u32);
    }
    f.blocks
        .retain(|block| !matches!(block.id, BlockId(9 | 10)));
    f.blocks.last_mut().unwrap().phis[0]
        .sources
        .retain(|(pred, _)| !matches!(pred, BlockId(9 | 10)));
    run(&mut f);
    f.verify_result().unwrap();
    assert_eq!(f.blocks.len(), 3);
    assert!(matches!(f.blocks[0].terminator(), Some(MInst::Jump { .. })));
}
