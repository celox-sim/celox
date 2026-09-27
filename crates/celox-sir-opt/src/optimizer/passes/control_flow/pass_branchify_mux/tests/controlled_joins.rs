use super::*;

#[test]
fn removes_mux_at_cfg_controlled_join() {
    let mut register_map = HashMap::default();
    for reg in 0..8 {
        register_map.insert(
            RegisterId(reg),
            RegisterType::Bit {
                width: if reg == 0 { 1 } else { 64 },
                signed: false,
            },
        );
    }
    let mut blocks = HashMap::default();
    blocks.insert(
        BlockId(0),
        BasicBlock {
            id: BlockId(0),
            params: vec![RegisterId(0)],
            instructions: vec![imm(1, 3), imm(2, 5)],
            terminator: SIRTerminator::Branch {
                cond: RegisterId(0),
                true_block: (BlockId(1), Vec::new()),
                false_block: (BlockId(2), Vec::new()),
            },
        },
    );
    blocks.insert(
        BlockId(1),
        BasicBlock {
            id: BlockId(1),
            params: Vec::new(),
            instructions: vec![SIRInstruction::Binary(
                RegisterId(3),
                RegisterId(1),
                crate::ir::BinaryOp::Mul,
                RegisterId(1),
            )],
            terminator: SIRTerminator::Jump(BlockId(3), Vec::new()),
        },
    );
    blocks.insert(
        BlockId(2),
        BasicBlock {
            id: BlockId(2),
            params: Vec::new(),
            instructions: vec![SIRInstruction::Binary(
                RegisterId(4),
                RegisterId(2),
                crate::ir::BinaryOp::Mul,
                RegisterId(2),
            )],
            terminator: SIRTerminator::Jump(BlockId(3), Vec::new()),
        },
    );
    blocks.insert(
        BlockId(3),
        BasicBlock {
            id: BlockId(3),
            params: Vec::new(),
            instructions: vec![
                SIRInstruction::Mux(RegisterId(5), RegisterId(0), RegisterId(3), RegisterId(4)),
                SIRInstruction::Unary(RegisterId(6), crate::ir::UnaryOp::BitNot, RegisterId(5)),
            ],
            terminator: SIRTerminator::Return,
        },
    );
    let mut eu = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks,
        register_map,
    };

    BranchifyMuxPass.run(&mut eu, &PassOptions::default());

    assert_eq!(eu.verify_result(), Ok(()));
    assert_eq!(eu.blocks[&BlockId(3)].params, vec![RegisterId(5)]);
    assert!(
        !eu.blocks[&BlockId(3)]
            .instructions
            .iter()
            .any(|inst| { matches!(inst, SIRInstruction::Mux(RegisterId(5), ..)) })
    );
    assert!(matches!(
        &eu.blocks[&BlockId(1)].terminator,
        SIRTerminator::Jump(BlockId(3), args) if args == &vec![RegisterId(3)]
    ));
    assert!(matches!(
        &eu.blocks[&BlockId(2)].terminator,
        SIRTerminator::Jump(BlockId(3), args) if args == &vec![RegisterId(4)]
    ));
}

#[test]
fn controlled_join_sinks_multiple_join_loads_to_the_selected_predecessor() {
    let element = |index| SIROffset::Element {
        index: RegisterId(index),
        element_width: 64,
        bit_offset: 0,
        dynamic_bit_offset: None,
    };
    let blocks = vec![
        BasicBlock {
            id: BlockId(0),
            params: vec![RegisterId(0), RegisterId(1), RegisterId(2), RegisterId(3)],
            instructions: Vec::new(),
            terminator: SIRTerminator::Branch {
                cond: RegisterId(0),
                true_block: (BlockId(1), Vec::new()),
                false_block: (BlockId(2), Vec::new()),
            },
        },
        BasicBlock {
            id: BlockId(1),
            params: Vec::new(),
            instructions: Vec::new(),
            terminator: SIRTerminator::Jump(BlockId(3), Vec::new()),
        },
        BasicBlock {
            id: BlockId(2),
            params: Vec::new(),
            instructions: Vec::new(),
            terminator: SIRTerminator::Jump(BlockId(3), Vec::new()),
        },
        BasicBlock {
            id: BlockId(3),
            params: Vec::new(),
            instructions: vec![
                SIRInstruction::Load(RegisterId(4), addr(0), element(1), 64),
                SIRInstruction::Load(RegisterId(6), addr(1), element(1), 64),
                SIRInstruction::Mux(RegisterId(5), RegisterId(0), RegisterId(4), RegisterId(2)),
                SIRInstruction::Mux(RegisterId(7), RegisterId(0), RegisterId(6), RegisterId(3)),
                SIRInstruction::Concat(RegisterId(8), vec![RegisterId(5), RegisterId(7)]),
            ],
            terminator: SIRTerminator::Return,
        },
    ];
    let mut eu = cfg_unit(9, &[0], blocks);
    eu.register_map.insert(
        RegisterId(8),
        RegisterType::Bit {
            width: 128,
            signed: false,
        },
    );

    eliminate_controlled_join_muxes(&mut eu, None);

    assert_eq!(eu.verify_result(), Ok(()));
    assert_eq!(
        eu.blocks[&BlockId(3)].params,
        vec![RegisterId(5), RegisterId(7)]
    );
    assert!(
        eu.blocks[&BlockId(3)]
            .instructions
            .iter()
            .all(|instruction| !matches!(
                instruction,
                SIRInstruction::Load(..) | SIRInstruction::Mux(..)
            ))
    );
    assert!(matches!(
        &eu.blocks[&BlockId(1)].instructions[..],
        [
            SIRInstruction::Load(RegisterId(4), ..),
            SIRInstruction::Load(RegisterId(6), ..)
        ]
    ));
    assert!(eu.blocks[&BlockId(2)].instructions.is_empty());
    assert!(matches!(
        &eu.blocks[&BlockId(1)].terminator,
        SIRTerminator::Jump(BlockId(3), args)
            if args == &vec![RegisterId(4), RegisterId(6)]
    ));
    assert!(matches!(
        &eu.blocks[&BlockId(2)].terminator,
        SIRTerminator::Jump(BlockId(3), args)
            if args == &vec![RegisterId(2), RegisterId(3)]
    ));
}

#[test]
fn controlled_join_does_not_move_a_load_before_a_join_write() {
    let blocks = vec![
        BasicBlock {
            id: BlockId(0),
            params: vec![RegisterId(0), RegisterId(1), RegisterId(2)],
            instructions: Vec::new(),
            terminator: SIRTerminator::Branch {
                cond: RegisterId(0),
                true_block: (BlockId(1), Vec::new()),
                false_block: (BlockId(2), Vec::new()),
            },
        },
        BasicBlock {
            id: BlockId(1),
            params: Vec::new(),
            instructions: Vec::new(),
            terminator: SIRTerminator::Jump(BlockId(3), Vec::new()),
        },
        BasicBlock {
            id: BlockId(2),
            params: Vec::new(),
            instructions: Vec::new(),
            terminator: SIRTerminator::Jump(BlockId(3), Vec::new()),
        },
        BasicBlock {
            id: BlockId(3),
            params: Vec::new(),
            instructions: vec![
                store(0, 1),
                SIRInstruction::Load(RegisterId(3), addr(0), SIROffset::Static(0), 64),
                SIRInstruction::Mux(RegisterId(4), RegisterId(0), RegisterId(3), RegisterId(2)),
                store(1, 4),
            ],
            terminator: SIRTerminator::Return,
        },
    ];
    let mut eu = cfg_unit(5, &[0], blocks);

    eliminate_controlled_join_muxes(&mut eu, None);

    assert_eq!(eu.verify_result(), Ok(()));
    assert!(eu.blocks[&BlockId(1)].instructions.is_empty());
    assert!(
        eu.blocks[&BlockId(3)]
            .instructions
            .iter()
            .any(|instruction| {
                matches!(
                    instruction,
                    SIRInstruction::Mux(RegisterId(4), RegisterId(0), RegisterId(3), RegisterId(2))
                )
            })
    );
}

#[test]
fn uses_per_edge_path_facts_for_reconvergent_mux() {
    let mut register_map = HashMap::default();
    for reg in 0..6 {
        register_map.insert(
            RegisterId(reg),
            RegisterType::Bit {
                width: if reg < 2 { 1 } else { 64 },
                signed: false,
            },
        );
    }
    let mut blocks = HashMap::default();
    blocks.insert(
        BlockId(0),
        BasicBlock {
            id: BlockId(0),
            params: vec![RegisterId(0), RegisterId(1)],
            instructions: vec![imm(2, 3), imm(3, 5)],
            terminator: SIRTerminator::Branch {
                cond: RegisterId(0),
                true_block: (BlockId(1), Vec::new()),
                false_block: (BlockId(2), Vec::new()),
            },
        },
    );
    blocks.insert(
        BlockId(1),
        BasicBlock {
            id: BlockId(1),
            params: Vec::new(),
            instructions: Vec::new(),
            terminator: SIRTerminator::Branch {
                cond: RegisterId(1),
                true_block: (BlockId(3), Vec::new()),
                false_block: (BlockId(4), Vec::new()),
            },
        },
    );
    blocks.insert(
        BlockId(2),
        BasicBlock {
            id: BlockId(2),
            params: Vec::new(),
            instructions: Vec::new(),
            terminator: SIRTerminator::Branch {
                cond: RegisterId(1),
                true_block: (BlockId(3), Vec::new()),
                false_block: (BlockId(5), Vec::new()),
            },
        },
    );
    blocks.insert(
        BlockId(3),
        BasicBlock {
            id: BlockId(3),
            params: Vec::new(),
            instructions: vec![SIRInstruction::Mux(
                RegisterId(4),
                RegisterId(1),
                RegisterId(2),
                RegisterId(3),
            )],
            terminator: SIRTerminator::Return,
        },
    );
    for block_id in [BlockId(4), BlockId(5)] {
        blocks.insert(
            block_id,
            BasicBlock {
                id: block_id,
                params: Vec::new(),
                instructions: Vec::new(),
                terminator: SIRTerminator::Return,
            },
        );
    }
    let mut eu = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks,
        register_map,
    };

    BranchifyMuxPass.run(&mut eu, &PassOptions::default());

    assert_eq!(eu.verify_result(), Ok(()));
    assert_eq!(eu.blocks[&BlockId(3)].params, vec![RegisterId(4)]);
    assert!(
        !eu.blocks[&BlockId(3)]
            .instructions
            .iter()
            .any(|inst| { matches!(inst, SIRInstruction::Mux(RegisterId(4), ..)) })
    );
    assert!(matches!(
        &eu.blocks[&BlockId(1)].terminator,
        SIRTerminator::Branch { true_block: (_, args), .. } if args == &vec![RegisterId(2)]
    ));
    assert!(matches!(
        &eu.blocks[&BlockId(2)].terminator,
        SIRTerminator::Branch { true_block: (_, args), .. } if args == &vec![RegisterId(2)]
    ));
}

#[test]
fn repeated_predicate_sinks_the_join_value_to_the_actual_selected_edge() {
    // `b0` and `b2` branch on the same SSA predicate.  Structurally, b2
    // dominates b3, so an ancestor-only arm classification would label
    // b3 as b0's false arm.  But b3 is reached on b2's true edge, where
    // the Mux must select r6.  The join-local definition of r6 must move
    // to b3, never to the infeasible ancestor classification.
    let mut register_map = HashMap::default();
    for reg in 0..8 {
        register_map.insert(
            RegisterId(reg),
            RegisterType::Bit {
                width: if reg == 0 { 1 } else { 64 },
                signed: false,
            },
        );
    }
    let mut blocks = HashMap::default();
    blocks.insert(
        BlockId(0),
        BasicBlock {
            id: BlockId(0),
            params: vec![RegisterId(0), RegisterId(1)],
            instructions: Vec::new(),
            terminator: SIRTerminator::Branch {
                cond: RegisterId(0),
                true_block: (BlockId(1), Vec::new()),
                false_block: (BlockId(2), Vec::new()),
            },
        },
    );
    blocks.insert(
        BlockId(1),
        BasicBlock {
            id: BlockId(1),
            params: Vec::new(),
            instructions: Vec::new(),
            terminator: SIRTerminator::Jump(BlockId(2), Vec::new()),
        },
    );
    blocks.insert(
        BlockId(2),
        BasicBlock {
            id: BlockId(2),
            params: Vec::new(),
            instructions: Vec::new(),
            terminator: SIRTerminator::Branch {
                cond: RegisterId(0),
                true_block: (BlockId(3), Vec::new()),
                false_block: (BlockId(4), Vec::new()),
            },
        },
    );
    for block_id in [BlockId(3), BlockId(4)] {
        blocks.insert(
            block_id,
            BasicBlock {
                id: block_id,
                params: Vec::new(),
                instructions: Vec::new(),
                terminator: SIRTerminator::Jump(BlockId(5), Vec::new()),
            },
        );
    }
    blocks.insert(
        BlockId(5),
        BasicBlock {
            id: BlockId(5),
            params: Vec::new(),
            instructions: vec![
                SIRInstruction::Unary(RegisterId(6), crate::ir::UnaryOp::BitNot, RegisterId(1)),
                SIRInstruction::Mux(RegisterId(7), RegisterId(0), RegisterId(6), RegisterId(1)),
                SIRInstruction::Store(
                    addr(0),
                    SIROffset::Static(0),
                    64,
                    RegisterId(7),
                    Vec::new(),
                    Vec::new(),
                ),
            ],
            terminator: SIRTerminator::Return,
        },
    );
    let mut eu = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks,
        register_map,
    };

    eliminate_controlled_join_muxes(&mut eu, None);

    assert_eq!(eu.verify_result(), Ok(()));
    assert_eq!(eu.blocks[&BlockId(5)].params, vec![RegisterId(7)]);
    assert!(
        !eu.blocks[&BlockId(5)]
            .instructions
            .iter()
            .any(|instruction| { matches!(instruction, SIRInstruction::Mux(RegisterId(7), ..)) })
    );
    assert!(
        eu.blocks[&BlockId(3)]
            .instructions
            .iter()
            .any(|instruction| {
                matches!(
                    instruction,
                    SIRInstruction::Unary(RegisterId(6), crate::ir::UnaryOp::BitNot, RegisterId(1))
                )
            })
    );
    assert!(eu.blocks[&BlockId(4)].instructions.is_empty());
    assert!(matches!(
        &eu.blocks[&BlockId(3)].terminator,
        SIRTerminator::Jump(BlockId(5), args) if args == &vec![RegisterId(6)]
    ));
    assert!(matches!(
        &eu.blocks[&BlockId(4)].terminator,
        SIRTerminator::Jump(BlockId(5), args) if args == &vec![RegisterId(1)]
    ));
}
