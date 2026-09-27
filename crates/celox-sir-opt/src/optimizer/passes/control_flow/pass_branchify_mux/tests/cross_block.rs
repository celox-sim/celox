use super::*;

#[test]
fn does_not_sever_cross_block_condition_prefix_from_local_user() {
    let definitions = vec![
        LocatedInstruction {
            block: BlockId(0),
            index: 0,
            instruction: imm(0, 1),
        },
        LocatedInstruction {
            block: BlockId(1),
            index: 0,
            instruction: SIRInstruction::Unary(
                RegisterId(1),
                crate::ir::UnaryOp::Ident,
                RegisterId(0),
            ),
        },
    ];

    assert!(
        closed_cross_block_condition_slice(definitions, BlockId(1)).is_empty(),
        "a producer cannot move below the local condition node which still uses it"
    );
}

#[test]
fn cross_arm_collection_preserves_long_chain_order_and_root_rules() {
    let outputs = (2..258).collect::<Vec<_>>();
    let mut instructions = Vec::new();
    append_mul_chain(&mut instructions, 1, 0, &outputs);
    let eu = cfg_unit(
        259,
        &[],
        vec![
            BasicBlock {
                id: BlockId(0),
                params: vec![RegisterId(0), RegisterId(1)],
                instructions: instructions.clone(),
                terminator: SIRTerminator::Jump(BlockId(1), vec![]),
            },
            BasicBlock {
                id: BlockId(1),
                params: vec![],
                instructions: vec![SIRInstruction::Binary(
                    RegisterId(258),
                    RegisterId(257),
                    crate::ir::BinaryOp::Mul,
                    RegisterId(0),
                )],
                terminator: SIRTerminator::Return,
            },
        ],
    );
    let cfg = SirDominance::analyze(&eu).unwrap();
    let mut counts = count_uses(&eu);
    let locations = instruction_def_locations(&eu);
    let mut seen = HashSet::default();
    let result = collect_cross_arm_defs(
        &eu,
        &cfg,
        &counts,
        &locations,
        BlockId(1),
        0,
        RegisterId(257),
        true,
        &mut seen,
    )
    .unwrap();
    assert_eq!(result.len(), instructions.len());
    for (index, actual) in result.iter().enumerate() {
        assert_eq!((actual.block, actual.index), (BlockId(0), index));
        assert_eq!(actual.instruction, instructions[index]);
    }
    assert!(
        collect_cross_arm_defs(
            &eu,
            &cfg,
            &counts,
            &locations,
            BlockId(1),
            0,
            RegisterId(257),
            true,
            &mut seen
        )
        .unwrap()
        .is_empty()
    );
    counts.insert(RegisterId(257), 2);
    for root in [RegisterId(257), RegisterId(1)] {
        assert!(
            collect_cross_arm_defs(
                &eu,
                &cfg,
                &counts,
                &locations,
                BlockId(1),
                0,
                root,
                true,
                &mut HashSet::default()
            )
            .unwrap()
            .is_empty()
        );
        assert!(
            collect_cross_arm_defs(
                &eu,
                &cfg,
                &counts,
                &locations,
                BlockId(1),
                0,
                root,
                false,
                &mut HashSet::default()
            )
            .is_none()
        );
    }
}

#[test]
fn batched_cross_block_plans_match_serial_rewrites_with_shared_sources() {
    fn fixture(priority: bool, reverse_sources: bool) -> ExecutionUnit<RegionedAbsoluteAddr> {
        let mut register = 2;
        let mut booleans = vec![1];
        let mut source = BasicBlock {
            id: BlockId(0),
            params: vec![RegisterId(0), RegisterId(1)],
            instructions: Vec::new(),
            terminator: SIRTerminator::Jump(BlockId(1), Vec::new()),
        };
        let mut blocks = Vec::new();
        for index in 1..=8 {
            let target = if reverse_sources { 9 - index } else { index };
            let mut arms = Vec::new();
            for _ in 0..if priority { 3 } else { 2 } {
                let outputs = (register..register + 16).collect::<Vec<_>>();
                register += outputs.len();
                append_mul_chain(&mut source.instructions, 0, 0, &outputs);
                arms.push(RegisterId(*outputs.last().unwrap()));
            }
            let mut instructions = Vec::new();
            let mut result = arms[0];
            for &arm in &arms[1..] {
                let condition = RegisterId(register);
                booleans.push(register);
                register += 1;
                source.instructions.push(SIRInstruction::Unary(
                    condition,
                    crate::ir::UnaryOp::Ident,
                    RegisterId(1),
                ));
                let next = RegisterId(register);
                register += 1;
                instructions.push(SIRInstruction::Mux(next, condition, arm, result));
                result = next;
            }
            instructions.push(store(target, result.0));
            blocks.push(BasicBlock {
                id: BlockId(target),
                params: Vec::new(),
                instructions,
                terminator: if target == 8 {
                    SIRTerminator::Return
                } else {
                    SIRTerminator::Jump(BlockId(target + 1), Vec::new())
                },
            });
        }
        blocks.push(source);
        cfg_unit(register, &booleans, blocks)
    }
    fn rewrite(
        mut eu: ExecutionUnit<RegionedAbsoluteAddr>,
        priority: bool,
        multiple: bool,
    ) -> (ExecutionUnit<RegionedAbsoluteAddr>, usize) {
        let mut next_block = 9;
        let mut next_register = eu.register_map.len();
        let mut largest_batch = 0;
        loop {
            let counts = count_uses(&eu);
            let mut offsets = CrossBlockOffsets::default();
            if priority {
                let Some(plans) = find_cross_block_priority_chain_plans(&eu, &counts, multiple)
                else {
                    break;
                };
                largest_batch = largest_batch.max(plans.len());
                for mut plan in plans {
                    offsets.remap(
                        &eu,
                        plan.block_id,
                        plan.condition_defs
                            .iter_mut()
                            .flatten()
                            .chain(plan.arm_defs.iter_mut().flatten()),
                    );
                    apply_cross_block_priority_chain(
                        &mut eu,
                        plan,
                        &mut next_block,
                        &mut next_register,
                    );
                    assert_eq!(eu.verify_result(), Ok(()));
                }
            } else {
                let Some(plans) = find_cross_block_branchify_plans(&eu, &counts, multiple) else {
                    break;
                };
                largest_batch = largest_batch.max(plans.len());
                for mut plan in plans {
                    offsets.remap(
                        &eu,
                        plan.block_id,
                        plan.condition_defs
                            .iter_mut()
                            .chain(&mut plan.true_defs)
                            .chain(&mut plan.false_defs),
                    );
                    apply_cross_block_branchify(&mut eu, plan, &mut next_block, &mut next_register);
                    assert_eq!(eu.verify_result(), Ok(()));
                }
            }
        }
        (eu, largest_batch)
    }
    for priority in [false, true] {
        for reverse_sources in [false, true] {
            let original = fixture(priority, reverse_sources);
            assert_eq!(original.verify_result(), Ok(()));
            let (serial, _) = rewrite(original.clone(), priority, false);
            let (batched, largest_batch) = rewrite(original, priority, true);
            assert_eq!(largest_batch, 8);
            assert_eq!(batched.blocks, serial.blocks);
            assert_eq!(batched.register_map, serial.register_map);
        }
    }
    let definition = |block, index| LocatedInstruction {
        block: BlockId(block),
        index,
        instruction: imm(index, 1),
    };
    let mut selection = CrossBlockBatch::default();
    assert!(selection.reserve(BlockId(1), [&definition(0, 0)].into_iter()));
    assert!(!selection.reserve(BlockId(2), [&definition(0, 0)].into_iter()));
    assert!(!selection.reserve(BlockId(0), [&definition(3, 0)].into_iter()));
    assert!(!selection.reserve(BlockId(3), [&definition(1, 0)].into_iter()));
    assert!(selection.reserve(BlockId(2), [&definition(0, 1)].into_iter()));
}

#[test]
fn moved_condition_insertion_matches_operand_by_operand_search() {
    fn reference(
        head: &[SIRInstruction<RegionedAbsoluteAddr>],
        moved: &[LocatedInstruction],
    ) -> Option<usize> {
        if moved.is_empty() {
            return Some(head.len());
        }
        let registers = moved
            .iter()
            .filter_map(|definition| def_reg(&definition.instruction))
            .collect::<HashSet<_>>();
        if registers.len() != moved.len() {
            return None;
        }
        let first_use = head
            .iter()
            .position(|instruction| {
                inst_uses(instruction)
                    .iter()
                    .any(|register| registers.contains(register))
            })
            .unwrap_or(head.len());
        let insertion = moved
            .iter()
            .flat_map(|definition| inst_uses(&definition.instruction))
            .filter(|operand| !registers.contains(operand))
            .filter_map(|operand| {
                head.iter()
                    .position(|instruction| def_reg(instruction) == Some(operand))
                    .map(|index| index + 1)
            })
            .max()
            .unwrap_or(0);
        (insertion <= first_use).then_some(insertion)
    }
    for seed in 0..32 {
        let head = (0..80)
            .map(|index| {
                SIRInstruction::Unary(
                    RegisterId(index % 64),
                    crate::ir::UnaryOp::Ident,
                    RegisterId((index * 19 + seed) % 160),
                )
            })
            .collect::<Vec<_>>();
        let mut moved = (0..32)
            .map(|index| LocatedInstruction {
                block: BlockId(1),
                index,
                instruction: SIRInstruction::Unary(
                    RegisterId(128 + index),
                    crate::ir::UnaryOp::Ident,
                    RegisterId((index * 17 + seed) % 160),
                ),
            })
            .collect::<Vec<_>>();
        for head_len in [0, 1, 16, 80] {
            for moved_len in [0, 1, 8, 32] {
                assert_eq!(
                    moved_defs_insertion_index(&head[..head_len], &moved[..moved_len]),
                    reference(&head[..head_len], &moved[..moved_len]),
                    "seed={seed} head={head_len} moved={moved_len}"
                );
            }
        }
        moved.push(moved[0].clone());
        assert_eq!(moved_defs_insertion_index(&head, &moved), None);
        assert_eq!(reference(&head, &moved), None);
    }
}

#[test]
fn cross_block_rejection_filters_match_full_definition_walks() {
    const PER_BLOCK: usize = 64;
    let eu = cfg_unit(
        4 * PER_BLOCK,
        &[],
        (0..4)
            .map(|block| BasicBlock {
                id: BlockId(block),
                params: Vec::new(),
                instructions: (0..PER_BLOCK)
                    .map(|index| {
                        let root = block * PER_BLOCK + index;
                        if root == 0 {
                            imm(root, 1)
                        } else if index > 0 && index % 11 == 0 {
                            SIRInstruction::Load(
                                RegisterId(root),
                                addr(block),
                                SIROffset::Static(0),
                                64,
                            )
                        } else if index % 7 == 0 {
                            SIRInstruction::Binary(
                                RegisterId(root),
                                RegisterId(root - 1),
                                crate::ir::BinaryOp::Add,
                                RegisterId(0),
                            )
                        } else {
                            SIRInstruction::Unary(
                                RegisterId(root),
                                crate::ir::UnaryOp::Ident,
                                RegisterId(root - 1),
                            )
                        }
                    })
                    .collect(),
                terminator: if block < 3 {
                    SIRTerminator::Jump(BlockId(block + 1), Vec::new())
                } else {
                    SIRTerminator::Return
                },
            })
            .collect(),
    );
    let cfg = SirDominance::analyze(&eu).unwrap();
    let counts = count_uses(&eu);
    let locations = instruction_def_locations(&eu);
    let cross_inputs = movable_cross_block_inputs(&eu, &counts, &locations);
    for block in 0..4 {
        for position in [0, PER_BLOCK / 2, PER_BLOCK] {
            for root in (0..=4 * PER_BLOCK).map(RegisterId) {
                let full = collect_cross_arm_defs(
                    &eu,
                    &cfg,
                    &counts,
                    &locations,
                    BlockId(block),
                    position,
                    root,
                    true,
                    &mut HashSet::default(),
                );
                if locations
                    .get(&root)
                    .is_some_and(|&(owner, _)| owner == BlockId(block))
                    && full.as_ref().is_some_and(|defs| {
                        defs.iter()
                            .any(|definition| definition.block != BlockId(block))
                    })
                {
                    assert!(
                        cross_inputs.contains(&root),
                        "missing cross-block root {root:?}"
                    );
                }
                let expected = full
                    .map(|defs| closed_cross_block_condition_slice(defs, BlockId(block)))
                    .map(|defs| defs.iter().map(located_instruction_key).collect::<Vec<_>>());
                let actual = collect_cross_condition_defs(
                    &eu,
                    &cfg,
                    &counts,
                    &locations,
                    BlockId(block),
                    position,
                    root,
                )
                .map(|defs| defs.iter().map(located_instruction_key).collect::<Vec<_>>());
                assert_eq!(
                    actual, expected,
                    "block={block} position={position} root={root:?}"
                );
            }
        }
    }
}

#[test]
fn short_circuits_a_cross_block_priority_chain() {
    let mut register_map = HashMap::default();
    for reg in 0..21 {
        register_map.insert(
            RegisterId(reg),
            RegisterType::Bit {
                width: if matches!(reg, 6 | 8 | 10) { 1 } else { 64 },
                signed: false,
            },
        );
    }
    let mut blocks = HashMap::default();
    blocks.insert(
        BlockId(0),
        BasicBlock {
            id: BlockId(0),
            params: vec![
                RegisterId(0),
                RegisterId(1),
                RegisterId(2),
                RegisterId(3),
                RegisterId(12),
            ],
            instructions: vec![
                imm(5, 1),
                SIRInstruction::Binary(
                    RegisterId(6),
                    RegisterId(0),
                    crate::ir::BinaryOp::Eq,
                    RegisterId(5),
                ),
                imm(7, 2),
                SIRInstruction::Binary(
                    RegisterId(8),
                    RegisterId(0),
                    crate::ir::BinaryOp::Eq,
                    RegisterId(7),
                ),
                imm(9, 3),
                SIRInstruction::Binary(
                    RegisterId(10),
                    RegisterId(0),
                    crate::ir::BinaryOp::Eq,
                    RegisterId(9),
                ),
            ],
            terminator: SIRTerminator::Jump(BlockId(1), Vec::new()),
        },
    );
    blocks.insert(
        BlockId(1),
        BasicBlock {
            id: BlockId(1),
            params: Vec::new(),
            instructions: vec![
                SIRInstruction::Unary(RegisterId(17), crate::ir::UnaryOp::BitNot, RegisterId(12)),
                SIRInstruction::Unary(RegisterId(18), crate::ir::UnaryOp::BitNot, RegisterId(1)),
                SIRInstruction::Unary(RegisterId(19), crate::ir::UnaryOp::BitNot, RegisterId(2)),
                SIRInstruction::Unary(RegisterId(20), crate::ir::UnaryOp::BitNot, RegisterId(3)),
                SIRInstruction::Mux(
                    RegisterId(13),
                    RegisterId(6),
                    RegisterId(18),
                    RegisterId(17),
                ),
                SIRInstruction::Mux(
                    RegisterId(14),
                    RegisterId(8),
                    RegisterId(19),
                    RegisterId(13),
                ),
                SIRInstruction::Mux(
                    RegisterId(15),
                    RegisterId(10),
                    RegisterId(20),
                    RegisterId(14),
                ),
                SIRInstruction::Unary(RegisterId(16), crate::ir::UnaryOp::BitNot, RegisterId(15)),
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
    assert!(!eu.blocks.values().any(|block| {
        block
            .instructions
            .iter()
            .any(|inst| matches!(inst, SIRInstruction::Mux(..)))
    }));
    assert!(eu.blocks.values().any(|block| {
        matches!(block.terminator, SIRTerminator::Branch { .. })
            && block
                .instructions
                .iter()
                .any(|inst| matches!(inst, SIRInstruction::Binary(RegisterId(10), ..)))
    }));
    assert!(!eu.blocks[&BlockId(0)].instructions.iter().any(|inst| {
        matches!(
            inst,
            SIRInstruction::Binary(RegisterId(6) | RegisterId(8) | RegisterId(10), ..)
        )
    }));
    let cfg = SirCfg::analyze(&eu).unwrap();
    for payload in 17..=20 {
        let payload_block = eu
            .blocks
            .values()
            .find(|block| {
                block
                    .instructions
                    .iter()
                    .any(|instruction| def_reg(instruction) == Some(RegisterId(payload)))
            })
            .expect("each selected payload must retain one definition");
        assert!(matches!(payload_block.terminator, SIRTerminator::Jump(..)));
        assert!(
            !cfg.controllers[cfg.block_index(payload_block.id).unwrap()].is_empty(),
            "payload r{payload} must execute only below its selector edge"
        );
    }
}

#[test]
fn moves_pure_arm_dags_from_dominating_blocks() {
    let mut register_map = HashMap::default();
    for reg in 0..26 {
        register_map.insert(
            RegisterId(reg),
            RegisterType::Bit {
                width: if reg == 0 { 1 } else { 64 },
                signed: false,
            },
        );
    }
    let mut blocks = HashMap::default();
    let mut preheader_insts = vec![imm(1, 3), imm(2, 5)];
    append_mul_chain(
        &mut preheader_insts,
        1,
        1,
        &[3, 4, 5, 6, 7, 8, 9, 10, 11, 12],
    );
    append_mul_chain(
        &mut preheader_insts,
        2,
        2,
        &[13, 14, 15, 16, 17, 18, 19, 20, 21, 22],
    );
    blocks.insert(
        BlockId(0),
        BasicBlock {
            id: BlockId(0),
            params: vec![RegisterId(0)],
            instructions: preheader_insts,
            terminator: SIRTerminator::Jump(BlockId(1), Vec::new()),
        },
    );
    blocks.insert(
        BlockId(1),
        BasicBlock {
            id: BlockId(1),
            params: Vec::new(),
            instructions: vec![
                SIRInstruction::Mux(
                    RegisterId(23),
                    RegisterId(0),
                    RegisterId(12),
                    RegisterId(22),
                ),
                SIRInstruction::Unary(RegisterId(24), crate::ir::UnaryOp::BitNot, RegisterId(23)),
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
    assert!(!eu.blocks.values().any(|block| {
        block
            .instructions
            .iter()
            .any(|inst| matches!(inst, SIRInstruction::Mux(RegisterId(23), ..)))
    }));
    assert!(eu.blocks.values().any(|block| {
        block.instructions.iter().any(|inst| {
            matches!(
                inst,
                SIRInstruction::Binary(RegisterId(12), _, crate::ir::BinaryOp::Mul, _)
            )
        })
    }));
    assert!(eu.blocks.values().any(|block| {
        block.instructions.iter().any(|inst| {
            matches!(
                inst,
                SIRInstruction::Binary(RegisterId(22), _, crate::ir::BinaryOp::Mul, _)
            )
        })
    }));
    assert!(
        eu.blocks
            .values()
            .any(|block| { block.params == vec![RegisterId(23)] })
    );
    assert!(!eu.blocks[&BlockId(0)].instructions.iter().any(|inst| {
        matches!(
            inst,
            SIRInstruction::Binary(
                RegisterId(12) | RegisterId(22),
                _,
                crate::ir::BinaryOp::Mul,
                _
            )
        )
    }));
}

#[test]
fn branches_once_for_multiple_muxes_sharing_an_arm_dag() {
    let mut register_map = HashMap::default();
    for reg in 0..26 {
        register_map.insert(
            RegisterId(reg),
            RegisterType::Bit {
                width: if reg == 0 { 1 } else { 64 },
                signed: false,
            },
        );
    }
    let mut blocks = HashMap::default();
    let mut preheader_insts = vec![imm(1, 3), imm(2, 5)];
    append_mul_chain(
        &mut preheader_insts,
        1,
        1,
        &[3, 4, 5, 6, 7, 8, 9, 10, 11, 12],
    );
    append_mul_chain(
        &mut preheader_insts,
        2,
        2,
        &[13, 14, 15, 16, 17, 18, 19, 20, 21, 22],
    );
    blocks.insert(
        BlockId(0),
        BasicBlock {
            id: BlockId(0),
            params: vec![RegisterId(0)],
            instructions: preheader_insts,
            terminator: SIRTerminator::Jump(BlockId(1), Vec::new()),
        },
    );
    blocks.insert(
        BlockId(1),
        BasicBlock {
            id: BlockId(1),
            params: Vec::new(),
            instructions: vec![
                SIRInstruction::Mux(
                    RegisterId(23),
                    RegisterId(0),
                    RegisterId(12),
                    RegisterId(22),
                ),
                SIRInstruction::Mux(
                    RegisterId(24),
                    RegisterId(0),
                    RegisterId(12),
                    RegisterId(22),
                ),
                SIRInstruction::Binary(
                    RegisterId(25),
                    RegisterId(23),
                    crate::ir::BinaryOp::Add,
                    RegisterId(24),
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
    assert!(!eu.blocks.values().any(|block| {
        block.instructions.iter().any(|inst| {
            matches!(
                inst,
                SIRInstruction::Mux(RegisterId(23) | RegisterId(24), ..)
            )
        })
    }));
    assert!(
        eu.blocks
            .values()
            .any(|block| { block.params == vec![RegisterId(23), RegisterId(24)] })
    );
    assert!(eu.blocks.values().any(|block| {
        block.instructions.iter().any(|inst| {
            matches!(
                inst,
                SIRInstruction::Binary(RegisterId(12), _, crate::ir::BinaryOp::Mul, _)
            )
        })
    }));
    assert!(eu.blocks.values().any(|block| {
        block.instructions.iter().any(|inst| {
            matches!(
                inst,
                SIRInstruction::Binary(RegisterId(22), _, crate::ir::BinaryOp::Mul, _)
            )
        })
    }));
    assert!(!eu.blocks[&BlockId(0)].instructions.iter().any(|inst| {
        matches!(
            inst,
            SIRInstruction::Binary(
                RegisterId(12) | RegisterId(22),
                _,
                crate::ir::BinaryOp::Mul,
                _
            )
        )
    }));
}
