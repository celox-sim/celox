use super::*;

#[test]
fn mux_suffix_costs_match_independent_forward_live_in_scans() {
    let register_map = (0..96)
        .map(|value| {
            (
                RegisterId(value),
                RegisterType::Bit {
                    width: [0, 1, 64, 65, 129][value % 5],
                    signed: false,
                },
            )
        })
        .collect::<HashMap<_, _>>();
    for seed in 0..32 {
        let block = BasicBlock {
            id: BlockId(0),
            params: Vec::new(),
            instructions: (0..128)
                .map(|index| {
                    if index % 3 == 0 {
                        SIRInstruction::Mux(
                            RegisterId(index % 80),
                            RegisterId((index * 7 + seed) % 100),
                            RegisterId((index * 13 + seed) % 100),
                            RegisterId((index * 17 + seed) % 100),
                        )
                    } else {
                        SIRInstruction::Unary(
                            RegisterId(index % 80),
                            crate::ir::UnaryOp::Ident,
                            RegisterId((index * 19 + seed) % 100),
                        )
                    }
                })
                .collect(),
            terminator: SIRTerminator::Jump(
                BlockId(1),
                vec![RegisterId(seed), RegisterId(seed), RegisterId(99)],
            ),
        };
        let costs = mux_live_through_chunks(&block, &register_map);
        for (index, instruction) in block.instructions.iter().enumerate() {
            let SIRInstruction::Mux(destination, ..) = instruction else {
                continue;
            };
            let expected = block_live_ins(
                &block.instructions[index + 1..],
                &terminator_uses(&block.terminator),
            )
            .into_iter()
            .filter(|value| value != destination)
            .map(|value| {
                register_map
                    .get(&value)
                    .map(|register| register.width().div_ceil(64).max(1))
                    .unwrap_or(1) as u128
            })
            .sum::<u128>();
            assert_eq!(costs[index], expected, "seed={seed} instruction={index}");
        }
    }
}

#[test]
fn branchifies_single_use_mux_arm_work_when_expected_savings_pay_cost() {
    let mut eu = unit(vec![
        imm(1, 3),
        imm(4, 5),
        SIRInstruction::Binary(
            RegisterId(5),
            RegisterId(1),
            crate::ir::BinaryOp::Mul,
            RegisterId(1),
        ),
        SIRInstruction::Binary(
            RegisterId(6),
            RegisterId(5),
            crate::ir::BinaryOp::Mul,
            RegisterId(1),
        ),
        SIRInstruction::Binary(
            RegisterId(7),
            RegisterId(6),
            crate::ir::BinaryOp::Mul,
            RegisterId(1),
        ),
        SIRInstruction::Binary(
            RegisterId(2),
            RegisterId(7),
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
        false_block
            .instructions
            .iter()
            .any(|inst| { matches!(inst, SIRInstruction::Store(_, _, 64, RegisterId(4), _, _)) })
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
fn keeps_a_single_cheap_mul_arm_as_a_mux() {
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

    assert_eq!(eu.blocks.len(), 1);
    assert!(eu.blocks[&BlockId(0)].instructions.iter().any(|inst| {
        matches!(
            inst,
            SIRInstruction::Mux(RegisterId(3), RegisterId(0), RegisterId(2), RegisterId(4))
        )
    }));
}

#[test]
fn branchifies_a_decoder_biased_arm_with_expected_benefit() {
    let mut instructions = vec![
        imm(1, 3),
        imm(4, 5),
        imm(13, 7),
        SIRInstruction::Binary(
            RegisterId(14),
            RegisterId(0),
            crate::ir::BinaryOp::Eq,
            RegisterId(13),
        ),
    ];
    append_mul_chain(&mut instructions, 1, 1, &[5, 2]);
    instructions.extend([
        SIRInstruction::Mux(RegisterId(3), RegisterId(14), RegisterId(2), RegisterId(4)),
        SIRInstruction::Store(
            addr(0),
            SIROffset::Static(0),
            64,
            RegisterId(3),
            Vec::new(),
            Vec::new(),
        ),
    ]);
    let mut eu = unit(instructions);
    eu.register_map.insert(
        RegisterId(14),
        RegisterType::Bit {
            width: 1,
            signed: false,
        },
    );

    BranchifyMuxPass.run(&mut eu, &PassOptions::default());

    assert!(matches!(
        eu.blocks[&BlockId(0)].terminator,
        SIRTerminator::Branch {
            cond: RegisterId(14),
            ..
        }
    ));
}

#[test]
fn keeps_muxes_in_four_state_mode() {
    let mut instructions = vec![imm(1, 3), imm(4, 5)];
    append_mul_chain(&mut instructions, 1, 1, &[5, 6, 7, 2]);
    instructions.extend([
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
    let mut eu = unit(instructions);
    let options = PassOptions {
        four_state: true,
        ..Default::default()
    };

    BranchifyMuxPass.run(&mut eu, &options);

    assert_eq!(eu.blocks.len(), 1);
    assert!(
        eu.blocks[&BlockId(0)]
            .instructions
            .iter()
            .any(|inst| matches!(inst, SIRInstruction::Mux(RegisterId(3), _, _, _)))
    );
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
fn keeps_cheap_select_as_mux() {
    let mut eu = unit(vec![
        imm(1, 3),
        SIRInstruction::Unary(RegisterId(2), crate::ir::UnaryOp::BitNot, RegisterId(1)),
        SIRInstruction::Mux(RegisterId(3), RegisterId(0), RegisterId(2), RegisterId(4)),
    ]);

    BranchifyMuxPass.run(&mut eu, &PassOptions::default());

    assert_eq!(eu.blocks.len(), 1);
    assert!(eu.blocks[&BlockId(0)].instructions.iter().any(|inst| {
        matches!(
            inst,
            SIRInstruction::Mux(RegisterId(3), RegisterId(0), RegisterId(2), RegisterId(4))
        )
    }));
}

#[test]
fn branchifies_non_store_mux_with_arm_work() {
    let mut instructions = vec![imm(1, 3)];
    append_mul_chain(&mut instructions, 1, 1, &[8, 10, 2]);
    append_mul_chain(&mut instructions, 1, 1, &[9, 4]);
    instructions.extend([
        SIRInstruction::Mux(RegisterId(3), RegisterId(0), RegisterId(2), RegisterId(4)),
        SIRInstruction::Unary(RegisterId(5), crate::ir::UnaryOp::BitNot, RegisterId(3)),
    ]);
    let mut eu = unit(instructions);

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
                SIRInstruction::Binary(RegisterId(4), _, crate::ir::BinaryOp::Mul, _)
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
    let mut instructions = vec![
        SIRInstruction::Load(RegisterId(1), addr(0), SIROffset::Static(0), 64),
        imm(9, 3),
        SIRInstruction::Store(
            addr(0),
            SIROffset::Static(0),
            64,
            RegisterId(4),
            Vec::new(),
            Vec::new(),
        ),
    ];
    append_mul_chain(&mut instructions, 1, 9, &[6, 7, 8, 10, 2]);
    instructions.extend([
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
    let mut eu = unit(instructions);

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
fn sunk_arm_uses_dominating_live_in_directly() {
    let mut instructions = vec![imm(1, 3), imm(4, 5)];
    append_mul_chain(&mut instructions, 7, 1, &[5, 6, 8, 2]);
    instructions.extend([
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
    let mut eu = unit(instructions);
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
    assert!(true_edge.1.is_empty());
    assert!(false_edge.1.is_empty());
    assert!(true_block.params.is_empty());
    assert!(false_block.params.is_empty());
    assert!(true_block.instructions.iter().any(|inst| {
        matches!(
            inst,
            SIRInstruction::Binary(dst, lhs, crate::ir::BinaryOp::Mul, _)
                if *dst == RegisterId(5) && *lhs == RegisterId(7)
        )
    }));
}

#[test]
fn branchifies_when_suffix_uses_dominating_live_in() {
    let mut instructions = vec![imm(1, 3), imm(6, 11)];
    append_mul_chain(&mut instructions, 1, 1, &[7, 8, 9, 10, 11, 2]);
    instructions.extend([
        SIRInstruction::Mux(RegisterId(3), RegisterId(0), RegisterId(2), RegisterId(4)),
        SIRInstruction::Binary(
            RegisterId(5),
            RegisterId(6),
            crate::ir::BinaryOp::Add,
            RegisterId(3),
        ),
    ]);
    let mut eu = unit(instructions);

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
fn merge_uses_dominating_param_directly() {
    let mut instructions = vec![imm(1, 3)];
    append_mul_chain(&mut instructions, 1, 1, &[8, 9, 10, 11, 12, 2]);
    instructions.extend([
        SIRInstruction::Mux(RegisterId(3), RegisterId(0), RegisterId(2), RegisterId(4)),
        SIRInstruction::Binary(
            RegisterId(5),
            RegisterId(7),
            crate::ir::BinaryOp::Add,
            RegisterId(3),
        ),
    ]);
    let mut eu = unit(instructions);
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
    assert_eq!(merge.params, vec![RegisterId(3)]);
    assert!(merge.instructions.iter().any(|inst| {
        matches!(
            inst,
            SIRInstruction::Binary(RegisterId(5), lhs, crate::ir::BinaryOp::Add, RegisterId(3))
                if *lhs == RegisterId(7)
        )
    }));
    assert!(eu.blocks.values().any(|block| {
        matches!(
            &block.terminator,
            SIRTerminator::Jump(target, args)
                if *target == merge.id && args.len() == 1
        )
    }));
}

#[test]
fn keeps_cheap_mux_feeding_jump_args() {
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

    assert!(eu.blocks.values().any(|block| {
        block
            .instructions
            .iter()
            .any(|inst| matches!(inst, SIRInstruction::Mux(RegisterId(7), _, _, _)))
    }));
}

#[test]
fn preserves_mux_result_through_merge_when_used_after_store() {
    let mut instructions = vec![imm(1, 3)];
    append_mul_chain(&mut instructions, 1, 1, &[5, 6, 7, 8, 2]);
    instructions.extend([
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
    let mut eu = unit(instructions);

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
