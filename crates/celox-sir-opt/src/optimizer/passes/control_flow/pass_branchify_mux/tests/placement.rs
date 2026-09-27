use super::*;

#[test]
fn atomic_priority_does_not_charge_preexisting_suffix_live_ranges() {
    let mut register_map = HashMap::default();
    for reg in 0..60 {
        register_map.insert(
            RegisterId(reg),
            RegisterType::Bit {
                width: 64,
                signed: false,
            },
        );
    }
    let mut instructions = vec![imm(7, 3)];
    append_mul_chain(&mut instructions, 6, 7, &[8, 9, 10, 11, 12, 13, 14, 15]);
    instructions.extend([
        SIRInstruction::Mux(RegisterId(16), RegisterId(2), RegisterId(5), RegisterId(15)),
        SIRInstruction::Mux(RegisterId(17), RegisterId(1), RegisterId(4), RegisterId(16)),
        SIRInstruction::Mux(RegisterId(18), RegisterId(0), RegisterId(3), RegisterId(17)),
        SIRInstruction::Unary(RegisterId(19), crate::ir::UnaryOp::BitNot, RegisterId(18)),
    ]);
    // These values are live from entry to the suffix both before and
    // after priority-region formation.  They must not be treated as
    // newly introduced live-through pressure.
    instructions.extend((20..60).map(|register| {
        SIRInstruction::Store(
            addr(register),
            SIROffset::Static(0),
            64,
            RegisterId(register),
            Vec::new(),
            Vec::new(),
        )
    }));
    let mut eu = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [(
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: (0..=6).chain(20..60).map(RegisterId).collect(),
                instructions,
                terminator: SIRTerminator::Return,
            },
        )]
        .into_iter()
        .collect(),
        register_map,
    };

    let placement = PlacementAnalysis::analyze(&eu).unwrap();
    let plan = find_atomic_priority_placement(&eu, &placement).unwrap();
    assert_eq!(plan.regions.len(), 1);
    assert_eq!(plan.regions[0].muxes.len(), 3);
    let mut next_block_id = 1;
    let mut reg_counter = 19;
    assert_eq!(
        apply_atomic_priority_placement(&mut eu, plan, &mut next_block_id, &mut reg_counter,),
        1
    );

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
        3
    );
    assert!(
        !eu.blocks[&BlockId(0)]
            .instructions
            .iter()
            .any(|instruction| {
                matches!(
                    instruction,
                    SIRInstruction::Binary(_, _, crate::ir::BinaryOp::Mul, _)
                )
            })
    );
}

#[test]
fn atomic_priority_moves_cross_block_occurrences_with_valid_state_versions() {
    let mut register_map = HashMap::default();
    for reg in 0..24 {
        register_map.insert(
            RegisterId(reg),
            RegisterType::Bit {
                width: if matches!(reg, 4 | 6) { 1 } else { 64 },
                signed: false,
            },
        );
    }
    let mut source = vec![
        imm(3, 1),
        SIRInstruction::Binary(
            RegisterId(4),
            RegisterId(0),
            crate::ir::BinaryOp::Eq,
            RegisterId(3),
        ),
        imm(5, 2),
        SIRInstruction::Binary(
            RegisterId(6),
            RegisterId(0),
            crate::ir::BinaryOp::Eq,
            RegisterId(5),
        ),
        SIRInstruction::Load(RegisterId(7), addr(0), SIROffset::Static(0), 64),
        imm(8, 3),
    ];
    append_mul_chain(
        &mut source,
        7,
        8,
        &[9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20],
    );
    let mut eu = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [
            (
                BlockId(0),
                BasicBlock {
                    id: BlockId(0),
                    params: vec![RegisterId(0), RegisterId(1), RegisterId(2)],
                    instructions: source,
                    terminator: SIRTerminator::Jump(BlockId(1), vec![]),
                },
            ),
            (
                BlockId(1),
                BasicBlock {
                    id: BlockId(1),
                    params: vec![],
                    instructions: vec![
                        SIRInstruction::Mux(
                            RegisterId(21),
                            RegisterId(4),
                            RegisterId(20),
                            RegisterId(1),
                        ),
                        SIRInstruction::Mux(
                            RegisterId(22),
                            RegisterId(6),
                            RegisterId(2),
                            RegisterId(21),
                        ),
                        SIRInstruction::Unary(
                            RegisterId(23),
                            crate::ir::UnaryOp::BitNot,
                            RegisterId(22),
                        ),
                    ],
                    terminator: SIRTerminator::Return,
                },
            ),
        ]
        .into_iter()
        .collect(),
        register_map,
    };

    let placement = PlacementAnalysis::analyze(&eu).unwrap();
    let plan = find_atomic_priority_placement(&eu, &placement).unwrap();
    assert_eq!(plan.regions.len(), 1);
    let moved_load = plan.regions[0]
        .placed
        .iter()
        .find(|placed| def_reg(&placed.instruction) == Some(RegisterId(7)))
        .unwrap();
    assert_eq!(moved_load.block, BlockId(0));
    assert_eq!(moved_load.site, PriorityPlacementSite::Leaf(1));
    let moved_inner_condition = plan.regions[0]
        .placed
        .iter()
        .find(|placed| def_reg(&placed.instruction) == Some(RegisterId(4)))
        .unwrap();
    assert_eq!(
        moved_inner_condition.site,
        PriorityPlacementSite::Decision(0)
    );

    let mut next_block_id = 2;
    let mut reg_counter = 23;
    assert_eq!(
        apply_atomic_priority_placement(&mut eu, plan, &mut next_block_id, &mut reg_counter),
        1
    );
    assert_eq!(eu.verify_result(), Ok(()));
    assert!(
        !eu.blocks[&BlockId(0)]
            .instructions
            .iter()
            .any(|instruction| {
                matches!(instruction, SIRInstruction::Load(RegisterId(7), ..))
                    || def_reg(instruction) == Some(RegisterId(4))
            })
    );
    let load_block =
        eu.blocks
            .values()
            .find(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(instruction, SIRInstruction::Load(RegisterId(7), ..))
                })
            })
            .unwrap();
    let cfg = SirCfg::analyze(&eu).unwrap();
    assert!(!cfg.controllers[cfg.block_index(load_block.id).unwrap()].is_empty());
    let condition_block = eu
        .blocks
        .values()
        .find(|block| {
            block
                .instructions
                .iter()
                .any(|instruction| def_reg(instruction) == Some(RegisterId(4)))
        })
        .unwrap();
    assert!(matches!(
        condition_block.terminator,
        SIRTerminator::Branch {
            cond: RegisterId(4),
            ..
        }
    ));
}

#[test]
fn whole_priority_places_a_shared_descendant_once_at_its_lca() {
    let mut register_map = HashMap::default();
    for reg in 0..48 {
        register_map.insert(
            RegisterId(reg),
            RegisterType::Bit {
                width: if reg <= 2 { 1 } else { 64 },
                signed: false,
            },
        );
    }
    let mut instructions = vec![imm(3, 3), imm(4, 5), imm(5, 7), imm(6, 11)];
    let shared_outputs = (7..=38).collect::<Vec<_>>();
    append_mul_chain(&mut instructions, 3, 4, &shared_outputs);
    instructions.extend([
        SIRInstruction::Binary(
            RegisterId(39),
            RegisterId(38),
            crate::ir::BinaryOp::Add,
            RegisterId(5),
        ),
        SIRInstruction::Binary(
            RegisterId(40),
            RegisterId(38),
            crate::ir::BinaryOp::Sub,
            RegisterId(6),
        ),
        SIRInstruction::Mux(RegisterId(41), RegisterId(2), RegisterId(39), RegisterId(3)),
        SIRInstruction::Mux(
            RegisterId(42),
            RegisterId(1),
            RegisterId(40),
            RegisterId(41),
        ),
        SIRInstruction::Mux(RegisterId(43), RegisterId(0), RegisterId(4), RegisterId(42)),
        SIRInstruction::Unary(RegisterId(44), crate::ir::UnaryOp::BitNot, RegisterId(43)),
    ]);
    let mut eu = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [(
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: vec![RegisterId(0), RegisterId(1), RegisterId(2)],
                instructions,
                terminator: SIRTerminator::Return,
            },
        )]
        .into_iter()
        .collect(),
        register_map,
    };

    let placement = PlacementAnalysis::analyze(&eu).unwrap();
    let plan = find_atomic_priority_placement(&eu, &placement).unwrap();
    let shared = plan.regions[0]
        .placed
        .iter()
        .find(|placed| def_reg(&placed.instruction) == Some(RegisterId(38)))
        .unwrap();
    assert_eq!(shared.site, PriorityPlacementSite::Decision(1));

    let mut next_block_id = 1;
    let mut reg_counter = 47;
    apply_atomic_priority_placement(&mut eu, plan, &mut next_block_id, &mut reg_counter);
    assert_eq!(eu.verify_result(), Ok(()));
    let definition_blocks = eu
        .blocks
        .values()
        .filter(|block| {
            block
                .instructions
                .iter()
                .any(|instruction| def_reg(instruction) == Some(RegisterId(38)))
        })
        .collect::<Vec<_>>();
    assert_eq!(definition_blocks.len(), 1);
    assert!(matches!(
        definition_blocks[0].terminator,
        SIRTerminator::Branch {
            cond: RegisterId(1),
            ..
        }
    ));
}

#[test]
fn whole_priority_pins_a_definition_with_an_external_use() {
    let mut register_map = HashMap::default();
    for reg in 0..40 {
        register_map.insert(
            RegisterId(reg),
            RegisterType::Bit {
                width: if reg <= 2 { 1 } else { 64 },
                signed: false,
            },
        );
    }
    let mut instructions = vec![imm(3, 3), imm(4, 5), imm(5, 7), imm(6, 11)];
    append_mul_chain(&mut instructions, 3, 4, &(7..=18).collect::<Vec<_>>());
    append_mul_chain(&mut instructions, 5, 6, &(19..=30).collect::<Vec<_>>());
    instructions.extend([
        SIRInstruction::Mux(
            RegisterId(31),
            RegisterId(2),
            RegisterId(30),
            RegisterId(18),
        ),
        SIRInstruction::Mux(RegisterId(32), RegisterId(1), RegisterId(4), RegisterId(31)),
        SIRInstruction::Mux(RegisterId(33), RegisterId(0), RegisterId(3), RegisterId(32)),
        SIRInstruction::Binary(
            RegisterId(34),
            RegisterId(33),
            crate::ir::BinaryOp::Add,
            RegisterId(18),
        ),
    ]);
    let mut eu = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [(
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: vec![RegisterId(0), RegisterId(1), RegisterId(2)],
                instructions,
                terminator: SIRTerminator::Return,
            },
        )]
        .into_iter()
        .collect(),
        register_map,
    };

    let placement = PlacementAnalysis::analyze(&eu).unwrap();
    let plan = find_atomic_priority_placement(&eu, &placement).unwrap();
    assert!(
        !plan.regions[0]
            .placed
            .iter()
            .any(|placed| def_reg(&placed.instruction) == Some(RegisterId(18)))
    );

    let mut next_block_id = 1;
    let mut reg_counter = 39;
    apply_atomic_priority_placement(&mut eu, plan, &mut next_block_id, &mut reg_counter);
    assert_eq!(eu.verify_result(), Ok(()));
    assert!(
        eu.blocks[&BlockId(0)]
            .instructions
            .iter()
            .any(|instruction| def_reg(instruction) == Some(RegisterId(18)))
    );
}

#[test]
fn whole_priority_delays_an_inner_condition_dag_until_fallthrough() {
    let mut register_map = HashMap::default();
    for reg in 0..30 {
        register_map.insert(
            RegisterId(reg),
            RegisterType::Bit {
                width: if reg <= 1 || reg == 22 { 1 } else { 64 },
                signed: false,
            },
        );
    }
    let mut instructions = vec![imm(2, 3), imm(3, 5), imm(4, 7), imm(5, 11)];
    append_mul_chain(&mut instructions, 2, 3, &(6..=21).collect::<Vec<_>>());
    instructions.extend([
        SIRInstruction::Binary(
            RegisterId(22),
            RegisterId(21),
            crate::ir::BinaryOp::Eq,
            RegisterId(4),
        ),
        SIRInstruction::Mux(RegisterId(23), RegisterId(22), RegisterId(4), RegisterId(5)),
        SIRInstruction::Mux(RegisterId(24), RegisterId(1), RegisterId(2), RegisterId(23)),
        SIRInstruction::Mux(RegisterId(25), RegisterId(0), RegisterId(3), RegisterId(24)),
        SIRInstruction::Unary(RegisterId(26), crate::ir::UnaryOp::BitNot, RegisterId(25)),
    ]);
    let mut eu = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [(
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: vec![RegisterId(0), RegisterId(1)],
                instructions,
                terminator: SIRTerminator::Return,
            },
        )]
        .into_iter()
        .collect(),
        register_map,
    };

    let placement = PlacementAnalysis::analyze(&eu).unwrap();
    let plan = find_atomic_priority_placement(&eu, &placement).unwrap();
    let condition = plan.regions[0]
        .placed
        .iter()
        .find(|placed| def_reg(&placed.instruction) == Some(RegisterId(22)))
        .unwrap();
    assert_eq!(condition.site, PriorityPlacementSite::Decision(0));

    let mut next_block_id = 1;
    let mut reg_counter = 29;
    apply_atomic_priority_placement(&mut eu, plan, &mut next_block_id, &mut reg_counter);
    assert_eq!(eu.verify_result(), Ok(()));
    let condition_block = eu
        .blocks
        .values()
        .find(|block| {
            block
                .instructions
                .iter()
                .any(|instruction| def_reg(instruction) == Some(RegisterId(22)))
        })
        .unwrap();
    assert_ne!(condition_block.id, BlockId(0));
    assert!(matches!(
        condition_block.terminator,
        SIRTerminator::Branch {
            cond: RegisterId(22),
            ..
        }
    ));
    let middle_block = eu
        .blocks
        .values()
        .find(|block| {
            matches!(
                block.terminator,
                SIRTerminator::Branch {
                    cond: RegisterId(1),
                    ..
                }
            )
        })
        .unwrap();
    let SIRTerminator::Branch {
        false_block: middle_fallthrough,
        ..
    } = &middle_block.terminator
    else {
        unreachable!()
    };
    assert_eq!(middle_fallthrough.0, condition_block.id);
    let SIRTerminator::Branch {
        cond: RegisterId(0),
        false_block: outer_fallthrough,
        ..
    } = &eu.blocks[&BlockId(0)].terminator
    else {
        panic!("expected the outer priority decision")
    };
    assert_eq!(outer_fallthrough.0, middle_block.id);
}

#[test]
fn existing_cfg_moves_a_state_read_only_with_memoryssa_proof() {
    let mut register_map = HashMap::default();
    for reg in 0..17 {
        register_map.insert(
            RegisterId(reg),
            RegisterType::Bit {
                width: if reg == 0 { 1 } else { 64 },
                signed: false,
            },
        );
    }
    let mut source = vec![
        SIRInstruction::Load(RegisterId(1), addr(0), SIROffset::Static(0), 64),
        imm(2, 3),
        imm(13, 5),
    ];
    append_mul_chain(&mut source, 1, 2, &[3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);
    let mut blocks = HashMap::default();
    blocks.insert(
        BlockId(0),
        BasicBlock {
            id: BlockId(0),
            params: vec![RegisterId(0)],
            instructions: source,
            terminator: SIRTerminator::Jump(BlockId(1), vec![]),
        },
    );
    blocks.insert(
        BlockId(1),
        BasicBlock {
            id: BlockId(1),
            params: vec![],
            instructions: vec![
                SIRInstruction::Mux(
                    RegisterId(14),
                    RegisterId(0),
                    RegisterId(12),
                    RegisterId(13),
                ),
                SIRInstruction::Mux(
                    RegisterId(15),
                    RegisterId(0),
                    RegisterId(12),
                    RegisterId(13),
                ),
                SIRInstruction::Binary(
                    RegisterId(16),
                    RegisterId(14),
                    crate::ir::BinaryOp::Add,
                    RegisterId(15),
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

    BranchifyMuxPass.run(&mut eu, &PassOptions::default());

    assert_eq!(eu.verify_result(), Ok(()));
    let load_blocks =
        eu.blocks
            .values()
            .filter(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(instruction, SIRInstruction::Load(RegisterId(1), ..))
                })
            })
            .map(|block| block.id)
            .collect::<Vec<_>>();
    assert_eq!(load_blocks.len(), 1);
    let cfg = SirCfg::analyze(&eu).unwrap();
    let load_block = cfg.block_index(load_blocks[0]).unwrap();
    assert!(
        !cfg.controllers[load_block].is_empty(),
        "an unchanged versioned state read should execute only in its selected arm"
    );
}

#[test]
fn atomic_placement_keeps_a_load_before_a_reaching_write() {
    let mut register_map = HashMap::default();
    for reg in 0..17 {
        register_map.insert(
            RegisterId(reg),
            RegisterType::Bit {
                width: if reg == 0 { 1 } else { 64 },
                signed: false,
            },
        );
    }
    let mut source = vec![
        SIRInstruction::Load(RegisterId(1), addr(0), SIROffset::Static(0), 64),
        imm(2, 3),
        imm(13, 5),
        SIRInstruction::Store(
            addr(0),
            SIROffset::Static(0),
            64,
            RegisterId(13),
            vec![],
            vec![],
        ),
    ];
    append_mul_chain(&mut source, 1, 2, &[3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);
    let mut blocks = HashMap::default();
    blocks.insert(
        BlockId(0),
        BasicBlock {
            id: BlockId(0),
            params: vec![RegisterId(0)],
            instructions: source,
            terminator: SIRTerminator::Jump(BlockId(1), vec![]),
        },
    );
    blocks.insert(
        BlockId(1),
        BasicBlock {
            id: BlockId(1),
            params: vec![],
            instructions: vec![
                SIRInstruction::Mux(
                    RegisterId(14),
                    RegisterId(0),
                    RegisterId(12),
                    RegisterId(13),
                ),
                SIRInstruction::Mux(
                    RegisterId(15),
                    RegisterId(0),
                    RegisterId(12),
                    RegisterId(13),
                ),
                SIRInstruction::Binary(
                    RegisterId(16),
                    RegisterId(14),
                    crate::ir::BinaryOp::Add,
                    RegisterId(15),
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

    let placement = PlacementAnalysis::analyze(&eu).unwrap();
    if let Some(plan) = find_atomic_priority_placement(&eu, &placement) {
        assert!(
            !plan
                .regions
                .iter()
                .flat_map(|region| &region.placed)
                .any(|placed| matches!(
                    placed.instruction,
                    SIRInstruction::Load(RegisterId(1), ..)
                ))
        );
    }

    BranchifyMuxPass.run(&mut eu, &PassOptions::default());

    assert_eq!(eu.verify_result(), Ok(()));
    assert!(
        eu.blocks[&BlockId(0)]
            .instructions
            .iter()
            .any(|instruction| { matches!(instruction, SIRInstruction::Load(RegisterId(1), ..)) })
    );
}

#[test]
fn one_atomic_plan_selects_disjoint_regions_bottom_up() {
    let mut register_map = HashMap::default();
    register_map.insert(
        RegisterId(0),
        RegisterType::Bit {
            width: 1,
            signed: false,
        },
    );
    register_map.insert(
        RegisterId(1),
        RegisterType::Bit {
            width: 1,
            signed: false,
        },
    );
    for reg in 2..32 {
        register_map.insert(
            RegisterId(reg),
            RegisterType::Bit {
                width: 64,
                signed: false,
            },
        );
    }
    let mut true_instructions = vec![imm(2, 3), imm(3, 5), imm(12, 7)];
    append_mul_chain(&mut true_instructions, 2, 3, &[4, 5, 6, 7, 8, 9, 10, 11]);
    true_instructions.extend([
        SIRInstruction::Mux(
            RegisterId(13),
            RegisterId(1),
            RegisterId(11),
            RegisterId(12),
        ),
        SIRInstruction::Mux(
            RegisterId(14),
            RegisterId(1),
            RegisterId(12),
            RegisterId(13),
        ),
        SIRInstruction::Binary(
            RegisterId(15),
            RegisterId(12),
            crate::ir::BinaryOp::Add,
            RegisterId(14),
        ),
    ]);
    let mut false_instructions = vec![imm(16, 11), imm(17, 13), imm(26, 17)];
    append_mul_chain(
        &mut false_instructions,
        16,
        17,
        &[18, 19, 20, 21, 22, 23, 24, 25],
    );
    false_instructions.extend([
        SIRInstruction::Mux(
            RegisterId(27),
            RegisterId(1),
            RegisterId(25),
            RegisterId(26),
        ),
        SIRInstruction::Mux(
            RegisterId(28),
            RegisterId(1),
            RegisterId(26),
            RegisterId(27),
        ),
        SIRInstruction::Binary(
            RegisterId(29),
            RegisterId(26),
            crate::ir::BinaryOp::Add,
            RegisterId(28),
        ),
    ]);
    let eu = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [
            (
                BlockId(0),
                BasicBlock {
                    id: BlockId(0),
                    params: vec![RegisterId(0), RegisterId(1)],
                    instructions: vec![],
                    terminator: SIRTerminator::Branch {
                        cond: RegisterId(0),
                        true_block: (BlockId(1), vec![]),
                        false_block: (BlockId(2), vec![]),
                    },
                },
            ),
            (
                BlockId(1),
                BasicBlock {
                    id: BlockId(1),
                    params: vec![],
                    instructions: true_instructions,
                    terminator: SIRTerminator::Return,
                },
            ),
            (
                BlockId(2),
                BasicBlock {
                    id: BlockId(2),
                    params: vec![],
                    instructions: false_instructions,
                    terminator: SIRTerminator::Return,
                },
            ),
        ]
        .into_iter()
        .collect(),
        register_map,
    };
    let placement = PlacementAnalysis::analyze(&eu).unwrap();
    let plan = find_atomic_priority_placement(&eu, &placement).unwrap();
    let mut heads = plan
        .regions
        .iter()
        .map(|region| region.block_id)
        .collect::<Vec<_>>();
    heads.sort_unstable();

    assert_eq!(heads, vec![BlockId(1), BlockId(2)]);
}

#[test]
fn atomic_apply_handles_disjoint_regions_sharing_a_definition_block() {
    let mut register_map = HashMap::default();
    for reg in 0..30 {
        register_map.insert(
            RegisterId(reg),
            RegisterType::Bit {
                width: if reg <= 2 { 1 } else { 64 },
                signed: false,
            },
        );
    }
    let mut source = vec![imm(4, 3), imm(5, 5)];
    append_mul_chain(&mut source, 4, 5, &[6, 7, 8, 9, 10, 11, 12, 13]);
    source.extend([imm(14, 7), imm(15, 11)]);
    append_mul_chain(&mut source, 14, 15, &[16, 17, 18, 19, 20, 21, 22, 23]);
    let priority_block = |id, inner, outer, result, value| BasicBlock {
        id: BlockId(id),
        params: vec![],
        instructions: vec![
            SIRInstruction::Mux(
                RegisterId(inner),
                RegisterId(2),
                RegisterId(value),
                RegisterId(3),
            ),
            SIRInstruction::Mux(
                RegisterId(outer),
                RegisterId(1),
                RegisterId(3),
                RegisterId(inner),
            ),
            SIRInstruction::Unary(
                RegisterId(result),
                crate::ir::UnaryOp::BitNot,
                RegisterId(outer),
            ),
        ],
        terminator: SIRTerminator::Return,
    };
    let mut eu = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [
            (
                BlockId(0),
                BasicBlock {
                    id: BlockId(0),
                    params: vec![RegisterId(0), RegisterId(1), RegisterId(2), RegisterId(3)],
                    instructions: source,
                    terminator: SIRTerminator::Branch {
                        cond: RegisterId(0),
                        true_block: (BlockId(1), vec![]),
                        false_block: (BlockId(2), vec![]),
                    },
                },
            ),
            (BlockId(1), priority_block(1, 24, 25, 26, 13)),
            (BlockId(2), priority_block(2, 27, 28, 29, 23)),
        ]
        .into_iter()
        .collect(),
        register_map,
    };

    let placement = PlacementAnalysis::analyze(&eu).unwrap();
    let plan = find_atomic_priority_placement(&eu, &placement).unwrap();
    assert_eq!(plan.regions.len(), 2);
    let mut next_block_id = 3;
    let mut reg_counter = 29;
    assert_eq!(
        apply_atomic_priority_placement(&mut eu, plan, &mut next_block_id, &mut reg_counter),
        2
    );

    assert_eq!(eu.verify_result(), Ok(()));
    assert!(!eu.blocks.values().any(|block| {
        block
            .instructions
            .iter()
            .any(|instruction| matches!(instruction, SIRInstruction::Mux(..)))
    }));
    assert!(
        !eu.blocks[&BlockId(0)]
            .instructions
            .iter()
            .any(|instruction| {
                def_reg(instruction).is_some_and(|register| (4..=23).contains(&register.0))
            })
    );
    for register in [RegisterId(13), RegisterId(23)] {
        assert_eq!(
            eu.blocks
                .values()
                .flat_map(|block| &block.instructions)
                .filter(|instruction| def_reg(instruction) == Some(register))
                .count(),
            1
        );
    }
}

#[test]
fn existing_cfg_places_complete_dags_in_their_used_arms() {
    let mut eu = cfg_unit(
        10,
        &[0],
        vec![
            BasicBlock {
                id: BlockId(0),
                params: vec![RegisterId(0), RegisterId(1), RegisterId(2)],
                instructions: vec![
                    SIRInstruction::Binary(
                        RegisterId(3),
                        RegisterId(1),
                        crate::ir::BinaryOp::Mul,
                        RegisterId(2),
                    ),
                    SIRInstruction::Binary(
                        RegisterId(4),
                        RegisterId(3),
                        crate::ir::BinaryOp::Mul,
                        RegisterId(2),
                    ),
                    SIRInstruction::Binary(
                        RegisterId(5),
                        RegisterId(1),
                        crate::ir::BinaryOp::Mul,
                        RegisterId(2),
                    ),
                    SIRInstruction::Binary(
                        RegisterId(6),
                        RegisterId(5),
                        crate::ir::BinaryOp::Mul,
                        RegisterId(2),
                    ),
                    SIRInstruction::Binary(
                        RegisterId(7),
                        RegisterId(1),
                        crate::ir::BinaryOp::Add,
                        RegisterId(2),
                    ),
                ],
                terminator: SIRTerminator::Branch {
                    cond: RegisterId(0),
                    true_block: (BlockId(1), vec![]),
                    false_block: (BlockId(2), vec![]),
                },
            },
            BasicBlock {
                id: BlockId(1),
                params: vec![],
                instructions: vec![store(10, 4), store(11, 7)],
                terminator: SIRTerminator::Return,
            },
            BasicBlock {
                id: BlockId(2),
                params: vec![],
                instructions: vec![store(12, 6), store(13, 7)],
                terminator: SIRTerminator::Return,
            },
        ],
    );
    assert_eq!(eu.verify_result(), Ok(()));

    let placement = PlacementAnalysis::analyze(&eu).unwrap();
    let plan = find_existing_cfg_placement(&eu, &placement).unwrap();
    assert_eq!(apply_existing_cfg_placement(&mut eu, plan), 4);

    let definitions = |block: BlockId| {
        eu.blocks[&block]
            .instructions
            .iter()
            .filter_map(def_reg)
            .collect::<Vec<_>>()
    };
    assert_eq!(definitions(BlockId(0)), vec![RegisterId(7)]);
    assert_eq!(definitions(BlockId(1)), vec![RegisterId(3), RegisterId(4)]);
    assert_eq!(definitions(BlockId(2)), vec![RegisterId(5), RegisterId(6)]);
    assert_eq!(eu.verify_result(), Ok(()));
}

#[test]
fn existing_cfg_schedules_pure_dags_into_a_postdominating_use_block() {
    let mut eu = cfg_unit(
        6,
        &[1],
        vec![
            BasicBlock {
                id: BlockId(0),
                params: vec![RegisterId(0), RegisterId(1)],
                instructions: vec![
                    SIRInstruction::Unary(RegisterId(2), crate::ir::UnaryOp::BitNot, RegisterId(0)),
                    SIRInstruction::Binary(
                        RegisterId(3),
                        RegisterId(2),
                        crate::ir::BinaryOp::Add,
                        RegisterId(0),
                    ),
                ],
                terminator: SIRTerminator::Branch {
                    cond: RegisterId(1),
                    true_block: (BlockId(1), vec![]),
                    false_block: (BlockId(2), vec![]),
                },
            },
            BasicBlock {
                id: BlockId(1),
                params: vec![],
                instructions: vec![],
                terminator: SIRTerminator::Jump(BlockId(3), vec![]),
            },
            BasicBlock {
                id: BlockId(2),
                params: vec![],
                instructions: vec![],
                terminator: SIRTerminator::Jump(BlockId(3), vec![]),
            },
            BasicBlock {
                id: BlockId(3),
                params: vec![],
                instructions: vec![store(0, 3)],
                terminator: SIRTerminator::Return,
            },
        ],
    );
    let placement = PlacementAnalysis::analyze(&eu).unwrap();
    let plan = find_existing_cfg_placement(&eu, &placement).unwrap();

    assert_eq!(apply_existing_cfg_placement(&mut eu, plan), 2);
    assert!(eu.blocks[&BlockId(0)].instructions.is_empty());
    assert_eq!(
        eu.blocks[&BlockId(3)]
            .instructions
            .iter()
            .filter_map(def_reg)
            .collect::<Vec<_>>(),
        vec![RegisterId(2), RegisterId(3)]
    );
    assert_eq!(eu.verify_result(), Ok(()));
}

#[test]
fn existing_cfg_sinks_a_dynamic_load_within_the_same_loop_iteration() {
    let mut eu = cfg_unit(
        5,
        &[1, 2],
        vec![
            BasicBlock {
                id: BlockId(0),
                params: vec![],
                instructions: vec![imm(0, 0), imm(1, 1), imm(2, 0)],
                terminator: SIRTerminator::Jump(BlockId(1), vec![]),
            },
            BasicBlock {
                id: BlockId(1),
                params: vec![],
                instructions: vec![SIRInstruction::Load(
                    RegisterId(3),
                    addr(0),
                    SIROffset::Dynamic(RegisterId(0)),
                    64,
                )],
                terminator: SIRTerminator::Branch {
                    cond: RegisterId(1),
                    true_block: (BlockId(2), vec![]),
                    false_block: (BlockId(3), vec![]),
                },
            },
            BasicBlock {
                id: BlockId(2),
                params: vec![],
                instructions: vec![SIRInstruction::Unary(
                    RegisterId(4),
                    crate::ir::UnaryOp::Ident,
                    RegisterId(3),
                )],
                terminator: SIRTerminator::Jump(BlockId(3), vec![]),
            },
            BasicBlock {
                id: BlockId(3),
                params: vec![],
                instructions: vec![],
                terminator: SIRTerminator::Branch {
                    cond: RegisterId(2),
                    true_block: (BlockId(1), vec![]),
                    false_block: (BlockId(4), vec![]),
                },
            },
            BasicBlock {
                id: BlockId(4),
                params: vec![],
                instructions: vec![],
                terminator: SIRTerminator::Return,
            },
        ],
    );
    let placement = PlacementAnalysis::analyze(&eu).unwrap();
    let plan = find_existing_cfg_placement(&eu, &placement).unwrap();

    assert_eq!(apply_existing_cfg_placement(&mut eu, plan), 1);
    assert!(eu.blocks[&BlockId(1)].instructions.is_empty());
    assert!(matches!(
        eu.blocks[&BlockId(2)].instructions.first(),
        Some(SIRInstruction::Load(RegisterId(3), _, _, 64))
    ));
    assert_eq!(eu.verify_result(), Ok(()));
}

#[test]
fn existing_cfg_uses_edge_arguments_and_preserves_dependency_order() {
    let mut eu = cfg_unit(
        5,
        &[0],
        vec![
            BasicBlock {
                id: BlockId(0),
                params: vec![RegisterId(0), RegisterId(1)],
                instructions: vec![
                    SIRInstruction::Binary(
                        RegisterId(2),
                        RegisterId(1),
                        crate::ir::BinaryOp::Mul,
                        RegisterId(1),
                    ),
                    SIRInstruction::Binary(
                        RegisterId(3),
                        RegisterId(2),
                        crate::ir::BinaryOp::Mul,
                        RegisterId(2),
                    ),
                ],
                terminator: SIRTerminator::Branch {
                    cond: RegisterId(0),
                    true_block: (BlockId(1), vec![]),
                    false_block: (BlockId(2), vec![]),
                },
            },
            BasicBlock {
                id: BlockId(1),
                params: vec![],
                instructions: vec![],
                terminator: SIRTerminator::Jump(BlockId(3), vec![RegisterId(3)]),
            },
            BasicBlock {
                id: BlockId(2),
                params: vec![],
                instructions: vec![],
                terminator: SIRTerminator::Return,
            },
            BasicBlock {
                id: BlockId(3),
                params: vec![RegisterId(4)],
                instructions: vec![store(0, 4)],
                terminator: SIRTerminator::Return,
            },
        ],
    );
    let placement = PlacementAnalysis::analyze(&eu).unwrap();
    let plan = find_existing_cfg_placement(&eu, &placement).unwrap();

    assert_eq!(apply_existing_cfg_placement(&mut eu, plan), 2);
    assert_eq!(
        eu.blocks[&BlockId(1)]
            .instructions
            .iter()
            .filter_map(def_reg)
            .collect::<Vec<_>>(),
        vec![RegisterId(2), RegisterId(3)]
    );
    assert_eq!(eu.verify_result(), Ok(()));
}

#[test]
fn existing_cfg_sinks_only_loads_with_the_same_state_version() {
    let make_unit = |intervening_write: bool| {
        let mut instructions = vec![
            SIRInstruction::Load(RegisterId(3), addr(0), SIROffset::Static(0), 64),
            SIRInstruction::Binary(
                RegisterId(4),
                RegisterId(3),
                crate::ir::BinaryOp::Mul,
                RegisterId(1),
            ),
        ];
        if intervening_write {
            instructions.push(store(0, 2));
        }
        cfg_unit(
            5,
            &[0],
            vec![
                BasicBlock {
                    id: BlockId(0),
                    params: vec![RegisterId(0), RegisterId(1), RegisterId(2)],
                    instructions,
                    terminator: SIRTerminator::Branch {
                        cond: RegisterId(0),
                        true_block: (BlockId(1), vec![]),
                        false_block: (BlockId(2), vec![]),
                    },
                },
                BasicBlock {
                    id: BlockId(1),
                    params: vec![],
                    instructions: vec![],
                    terminator: SIRTerminator::Return,
                },
                BasicBlock {
                    id: BlockId(2),
                    params: vec![],
                    instructions: vec![store(1, 4)],
                    terminator: SIRTerminator::Return,
                },
            ],
        )
    };

    let mut unchanged = make_unit(false);
    let placement = PlacementAnalysis::analyze(&unchanged).unwrap();
    let plan = find_existing_cfg_placement(&unchanged, &placement).unwrap();
    assert_eq!(apply_existing_cfg_placement(&mut unchanged, plan), 2);
    assert!(matches!(
        unchanged.blocks[&BlockId(2)].instructions.first(),
        Some(SIRInstruction::Load(RegisterId(3), _, _, _))
    ));
    assert_eq!(unchanged.verify_result(), Ok(()));

    let mut changed = make_unit(true);
    let placement = PlacementAnalysis::analyze(&changed).unwrap();
    let plan = find_existing_cfg_placement(&changed, &placement).unwrap();
    assert_eq!(apply_existing_cfg_placement(&mut changed, plan), 1);
    assert!(
        changed.blocks[&BlockId(0)]
            .instructions
            .iter()
            .any(|instruction| matches!(instruction, SIRInstruction::Load(RegisterId(3), _, _, _)))
    );
    assert!(
        changed.blocks[&BlockId(0)]
            .instructions
            .iter()
            .any(|instruction| matches!(instruction, SIRInstruction::Store(_, _, _, _, _, _)))
    );
    assert_eq!(changed.verify_result(), Ok(()));
}

#[test]
fn existing_cfg_rejects_cyclic_targets_and_unprofitable_fan_in() {
    let loop_unit = cfg_unit(
        6,
        &[0, 5],
        vec![
            BasicBlock {
                id: BlockId(0),
                params: vec![RegisterId(0), RegisterId(1), RegisterId(2), RegisterId(5)],
                instructions: vec![SIRInstruction::Binary(
                    RegisterId(3),
                    RegisterId(1),
                    crate::ir::BinaryOp::Mul,
                    RegisterId(2),
                )],
                terminator: SIRTerminator::Branch {
                    cond: RegisterId(0),
                    true_block: (BlockId(1), vec![]),
                    false_block: (BlockId(3), vec![]),
                },
            },
            BasicBlock {
                id: BlockId(1),
                params: vec![],
                instructions: vec![store(0, 3)],
                terminator: SIRTerminator::Branch {
                    cond: RegisterId(5),
                    true_block: (BlockId(1), vec![]),
                    false_block: (BlockId(2), vec![]),
                },
            },
            BasicBlock {
                id: BlockId(2),
                params: vec![],
                instructions: vec![],
                terminator: SIRTerminator::Return,
            },
            BasicBlock {
                id: BlockId(3),
                params: vec![],
                instructions: vec![],
                terminator: SIRTerminator::Return,
            },
        ],
    );
    let placement = PlacementAnalysis::analyze(&loop_unit).unwrap();
    assert!(find_existing_cfg_placement(&loop_unit, &placement).is_none());

    let cheap = cfg_unit(
        4,
        &[0],
        vec![
            BasicBlock {
                id: BlockId(0),
                params: vec![RegisterId(0), RegisterId(1), RegisterId(2)],
                instructions: vec![SIRInstruction::Binary(
                    RegisterId(3),
                    RegisterId(1),
                    crate::ir::BinaryOp::And,
                    RegisterId(2),
                )],
                terminator: SIRTerminator::Branch {
                    cond: RegisterId(0),
                    true_block: (BlockId(1), vec![]),
                    false_block: (BlockId(2), vec![]),
                },
            },
            BasicBlock {
                id: BlockId(1),
                params: vec![],
                instructions: vec![store(0, 3)],
                terminator: SIRTerminator::Return,
            },
            BasicBlock {
                id: BlockId(2),
                params: vec![],
                instructions: vec![],
                terminator: SIRTerminator::Return,
            },
        ],
    );
    let placement = PlacementAnalysis::analyze(&cheap).unwrap();
    assert!(find_existing_cfg_placement(&cheap, &placement).is_none());
}
