use super::*;

#[cfg(debug_assertions)]
#[test]
fn coupled_rewrites_verify_once_across_many_source_blocks() {
    const COUNT: usize = 16;
    for priority_chain in [false, true] {
        let blocks = (0..COUNT)
            .map(|lane| {
                let base = 6 + lane * 4;
                BasicBlock {
                    id: BlockId(lane),
                    params: if lane == 0 {
                        (0..6).map(RegisterId).collect()
                    } else {
                        Vec::new()
                    },
                    instructions: vec![
                        SIRInstruction::Mux(
                            RegisterId(base),
                            RegisterId(0),
                            RegisterId(4),
                            RegisterId(2),
                        ),
                        SIRInstruction::Mux(
                            RegisterId(base + 1),
                            RegisterId(0),
                            RegisterId(5),
                            RegisterId(3),
                        ),
                        SIRInstruction::Mux(
                            RegisterId(base + 2),
                            RegisterId(1),
                            RegisterId(4),
                            RegisterId(base),
                        ),
                        SIRInstruction::Mux(
                            RegisterId(base + 3),
                            RegisterId(1),
                            RegisterId(5),
                            RegisterId(base + 1),
                        ),
                        store(lane * 2, base + 2),
                        store(lane * 2 + 1, base + 3),
                    ],
                    terminator: if lane + 1 == COUNT {
                        SIRTerminator::Return
                    } else {
                        SIRTerminator::Jump(BlockId(lane + 1), Vec::new())
                    },
                }
            })
            .collect();
        let mut eu = cfg_unit(6 + COUNT * 4, &[0, 1], blocks);
        eu.verify_result().unwrap();
        let counts = count_uses(&eu);
        let mut next_block = COUNT;
        let mut next_register = 5 + COUNT * 4;
        COUPLED_BATCH_VERIFICATIONS.with(|count| count.set(0));
        let applied = if priority_chain {
            super::super::coupled::branchify_coupled_priority_chains(
                &mut eu,
                &counts,
                &mut next_block,
                &mut next_register,
            )
        } else {
            super::super::coupled::branchify_coupled_state_updates(
                &mut eu,
                &counts,
                &mut next_block,
                &mut next_register,
            )
        };
        assert_eq!(applied, COUNT * if priority_chain { 1 } else { 2 });
        assert_eq!(COUPLED_BATCH_VERIFICATIONS.with(|count| count.get()), 1);
        eu.verify_result().unwrap();
        assert!(!eu.blocks.values().any(|block| {
            block
                .instructions
                .iter()
                .any(|inst| matches!(inst, SIRInstruction::Mux(..)))
        }));
        assert_eq!(
            eu.blocks
                .values()
                .flat_map(|block| &block.instructions)
                .filter(|inst| matches!(inst, SIRInstruction::Store(..)))
                .count(),
            COUNT * 2
        );
    }
}

#[test]
fn branchifies_coupled_state_updates_with_interleaved_conditions() {
    let mut eu = cfg_unit(
        23,
        &[2, 4, 6, 8, 9, 11, 12, 14, 15],
        vec![BasicBlock {
            id: BlockId(0),
            params: (0..=7).map(RegisterId).collect(),
            instructions: vec![
                SIRInstruction::Binary(
                    RegisterId(8),
                    RegisterId(3),
                    crate::ir::BinaryOp::GtU,
                    RegisterId(0),
                ),
                SIRInstruction::Binary(
                    RegisterId(9),
                    RegisterId(2),
                    crate::ir::BinaryOp::LogicAnd,
                    RegisterId(8),
                ),
                SIRInstruction::Mux(RegisterId(10), RegisterId(9), RegisterId(3), RegisterId(0)),
                SIRInstruction::Binary(
                    RegisterId(11),
                    RegisterId(5),
                    crate::ir::BinaryOp::GtU,
                    RegisterId(10),
                ),
                SIRInstruction::Binary(
                    RegisterId(12),
                    RegisterId(4),
                    crate::ir::BinaryOp::LogicAnd,
                    RegisterId(11),
                ),
                SIRInstruction::Mux(
                    RegisterId(13),
                    RegisterId(12),
                    RegisterId(5),
                    RegisterId(10),
                ),
                SIRInstruction::Binary(
                    RegisterId(14),
                    RegisterId(7),
                    crate::ir::BinaryOp::GtU,
                    RegisterId(13),
                ),
                SIRInstruction::Binary(
                    RegisterId(15),
                    RegisterId(6),
                    crate::ir::BinaryOp::LogicAnd,
                    RegisterId(14),
                ),
                SIRInstruction::Mux(
                    RegisterId(16),
                    RegisterId(15),
                    RegisterId(7),
                    RegisterId(13),
                ),
                imm(17, 1),
                imm(18, 2),
                imm(19, 3),
                SIRInstruction::Mux(RegisterId(20), RegisterId(9), RegisterId(17), RegisterId(1)),
                SIRInstruction::Mux(
                    RegisterId(21),
                    RegisterId(12),
                    RegisterId(18),
                    RegisterId(20),
                ),
                SIRInstruction::Mux(
                    RegisterId(22),
                    RegisterId(15),
                    RegisterId(19),
                    RegisterId(21),
                ),
                store(0, 16),
                store(1, 22),
            ],
            terminator: SIRTerminator::Return,
        }],
    );

    BranchifyMuxPass.run(&mut eu, &PassOptions::default());

    assert_eq!(eu.verify_result(), Ok(()));
    assert!(!eu.blocks.values().any(|block| {
        block
            .instructions
            .iter()
            .any(|instruction| matches!(instruction, SIRInstruction::Mux(..)))
    }));
    assert_eq!(
        eu.blocks
            .values()
            .filter(|block| matches!(block.terminator, SIRTerminator::Branch { .. }))
            .count(),
        6
    );
    for (guard, delayed) in [(2, 8), (4, 11), (6, 14)] {
        let guard_block = eu
            .blocks
            .values()
            .find(|block| {
                matches!(
                    block.terminator,
                    SIRTerminator::Branch {
                        cond,
                        ..
                    } if cond == RegisterId(guard)
                )
            })
            .expect("eligibility guard must become the first branch");
        let delayed_block_id = match &guard_block.terminator {
            SIRTerminator::Branch { true_block, .. } => true_block.0,
            _ => unreachable!(),
        };
        let delayed_block = &eu.blocks[&delayed_block_id];
        assert!(delayed_block.instructions.iter().any(|instruction| {
            matches!(
                instruction,
                SIRInstruction::Binary(dst, _, crate::ir::BinaryOp::GtU, _)
                    if *dst == RegisterId(delayed)
            )
        }));
        assert!(matches!(
            delayed_block.terminator,
            SIRTerminator::Branch {
                cond,
                ..
            } if cond == RegisterId(delayed)
        ));
    }
    for outputs in [
        [RegisterId(10), RegisterId(20)],
        [RegisterId(13), RegisterId(21)],
        [RegisterId(16), RegisterId(22)],
    ] {
        assert!(eu.blocks.values().any(|block| block.params == outputs));
    }
}

#[test]
fn branchifies_a_coupled_priority_chain_outermost_first() {
    let mut eu = cfg_unit(
        10,
        &[0, 1],
        vec![BasicBlock {
            id: BlockId(0),
            params: (0..=5).map(RegisterId).collect(),
            instructions: vec![
                SIRInstruction::Mux(RegisterId(6), RegisterId(0), RegisterId(4), RegisterId(2)),
                SIRInstruction::Mux(RegisterId(7), RegisterId(1), RegisterId(4), RegisterId(6)),
                store(0, 7),
                SIRInstruction::Mux(RegisterId(8), RegisterId(0), RegisterId(5), RegisterId(3)),
                SIRInstruction::Mux(RegisterId(9), RegisterId(1), RegisterId(5), RegisterId(8)),
                store(1, 9),
            ],
            terminator: SIRTerminator::Return,
        }],
    );

    BranchifyMuxPass.run(&mut eu, &PassOptions::default());

    assert_eq!(eu.verify_result(), Ok(()));
    assert!(matches!(
        eu.blocks[&BlockId(0)].terminator,
        SIRTerminator::Branch {
            cond: RegisterId(1),
            ..
        }
    ));
    assert!(!eu.blocks.values().any(|block| {
        block
            .instructions
            .iter()
            .any(|instruction| matches!(instruction, SIRInstruction::Mux(..)))
    }));
    assert!(
        eu.blocks
            .values()
            .any(|block| block.params == [RegisterId(7), RegisterId(9)])
    );
}
