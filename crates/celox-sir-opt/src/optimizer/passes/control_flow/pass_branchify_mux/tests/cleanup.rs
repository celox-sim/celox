use super::*;

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
fn inlines_chained_param_only_blocks_without_dangling_targets() {
    let mut register_map = HashMap::default();
    for reg in 0..4 {
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
            instructions: vec![imm(0, 3)],
            terminator: SIRTerminator::Jump(BlockId(1), vec![RegisterId(0)]),
        },
    );
    blocks.insert(
        BlockId(1),
        BasicBlock {
            id: BlockId(1),
            params: vec![RegisterId(1)],
            instructions: Vec::new(),
            terminator: SIRTerminator::Jump(BlockId(2), vec![RegisterId(1)]),
        },
    );
    blocks.insert(
        BlockId(2),
        BasicBlock {
            id: BlockId(2),
            params: vec![RegisterId(2)],
            instructions: Vec::new(),
            terminator: SIRTerminator::Jump(BlockId(3), vec![RegisterId(2)]),
        },
    );
    blocks.insert(
        BlockId(3),
        BasicBlock {
            id: BlockId(3),
            params: vec![RegisterId(3)],
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
    assert!(!eu.blocks.contains_key(&BlockId(2)));
    assert!(matches!(
        &eu.blocks[&BlockId(0)].terminator,
        SIRTerminator::Jump(target, args)
            if *target == BlockId(3) && args == &vec![RegisterId(0)]
    ));
    eu.verify();
}

#[test]
fn keeps_param_only_branch_when_descendant_uses_parameter_directly() {
    let mut register_map = HashMap::default();
    for reg in 0..6 {
        register_map.insert(
            RegisterId(reg),
            RegisterType::Bit {
                width: 64,
                signed: false,
            },
        );
    }
    register_map.insert(
        RegisterId(5),
        RegisterType::Bit {
            width: 1,
            signed: false,
        },
    );
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
            instructions: vec![SIRInstruction::Imm(RegisterId(5), SIRValue::new(1u8))],
            terminator: SIRTerminator::Branch {
                cond: RegisterId(5),
                true_block: (BlockId(2), Vec::new()),
                false_block: (BlockId(3), Vec::new()),
            },
        },
    );
    blocks.insert(
        BlockId(2),
        BasicBlock {
            id: BlockId(2),
            params: Vec::new(),
            instructions: vec![SIRInstruction::Unary(
                RegisterId(4),
                crate::ir::UnaryOp::BitNot,
                RegisterId(2),
            )],
            terminator: SIRTerminator::Return,
        },
    );
    blocks.insert(
        BlockId(3),
        BasicBlock {
            id: BlockId(3),
            params: Vec::new(),
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

    assert!(eu.blocks.contains_key(&BlockId(1)));
    eu.verify();
}
