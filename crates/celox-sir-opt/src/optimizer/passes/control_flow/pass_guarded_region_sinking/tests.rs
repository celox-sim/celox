use super::*;
use crate::ir::{DomainKind, InstanceId, SIRValue, TriggerIdWithKind};
use celox_design::StateObjectId as VarId;

fn address(id: usize) -> RegionedAbsoluteAddr {
    RegionedAbsoluteAddr {
        region: 0,
        instance_id: InstanceId(id),
        var_id: VarId::default(),
    }
}

fn bit(width: usize) -> RegisterType {
    RegisterType::Bit {
        width,
        signed: false,
    }
}

fn insert_block(
    blocks: &mut HashMap<BlockId, BasicBlock<RegionedAbsoluteAddr>>,
    id: usize,
    params: Vec<RegisterId>,
    instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
    terminator: SIRTerminator,
) {
    blocks.insert(
        BlockId(id),
        BasicBlock {
            id: BlockId(id),
            params,
            instructions,
            terminator,
        },
    );
}

#[test]
fn single_predecessor_merge_reads_arguments_after_earlier_merges() {
    // b0 -> b1(r1) passes r0; b2 -> b3(r2) passes b1's parameter r1.
    // Merging b1 first replaces r1 with r0, so the b2 -> b3 merge must
    // bind r2 to r0, not to the eliminated r1.
    let mut register_map = HashMap::default();
    for reg in 0..=3 {
        register_map.insert(RegisterId(reg), bit(1));
    }
    let mut blocks = HashMap::default();
    insert_block(
        &mut blocks,
        0,
        Vec::new(),
        vec![SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8))],
        SIRTerminator::Jump(BlockId(1), vec![RegisterId(0)]),
    );
    insert_block(
        &mut blocks,
        1,
        vec![RegisterId(1)],
        Vec::new(),
        SIRTerminator::Branch {
            cond: RegisterId(1),
            true_block: (BlockId(2), Vec::new()),
            false_block: (BlockId(4), Vec::new()),
        },
    );
    insert_block(
        &mut blocks,
        2,
        Vec::new(),
        Vec::new(),
        SIRTerminator::Jump(BlockId(3), vec![RegisterId(1)]),
    );
    insert_block(
        &mut blocks,
        3,
        vec![RegisterId(2)],
        vec![SIRInstruction::Store(
            address(10),
            SIROffset::Static(0),
            1,
            RegisterId(2),
            Vec::new(),
            Vec::new(),
        )],
        SIRTerminator::Return,
    );
    insert_block(
        &mut blocks,
        4,
        Vec::new(),
        Vec::new(),
        SIRTerminator::Return,
    );
    let mut eu = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks,
        register_map,
    };
    eu.verify_result().unwrap();
    merge_single_predecessor_jump_blocks(&mut eu);
    eu.verify_result().unwrap();
    let stored = eu
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
        .find_map(|instruction| match instruction {
            SIRInstruction::Store(_, _, _, source, _, _) => Some(*source),
            _ => None,
        });
    assert_eq!(stored, Some(RegisterId(0)));
}

fn shared_dag_unit() -> ExecutionUnit<RegionedAbsoluteAddr> {
    let mut register_map = HashMap::default();
    register_map.insert(RegisterId(0), bit(1));
    for reg in 1..=8 {
        register_map.insert(RegisterId(reg), bit(8));
    }
    let mut blocks = HashMap::default();
    insert_block(
        &mut blocks,
        0,
        vec![RegisterId(0), RegisterId(1)],
        vec![
            SIRInstruction::Imm(RegisterId(2), SIRValue::new(0u8)),
            SIRInstruction::Binary(RegisterId(3), RegisterId(1), BinaryOp::Add, RegisterId(1)),
            SIRInstruction::Binary(RegisterId(4), RegisterId(3), BinaryOp::Mul, RegisterId(1)),
            SIRInstruction::Binary(RegisterId(5), RegisterId(3), BinaryOp::Or, RegisterId(4)),
            SIRInstruction::Mux(RegisterId(6), RegisterId(0), RegisterId(5), RegisterId(2)),
            SIRInstruction::Store(
                address(10),
                SIROffset::Static(0),
                8,
                RegisterId(6),
                Vec::new(),
                Vec::new(),
            ),
        ],
        SIRTerminator::Branch {
            cond: RegisterId(0),
            true_block: (BlockId(1), vec![RegisterId(5)]),
            false_block: (BlockId(2), vec![RegisterId(2)]),
        },
    );
    insert_block(
        &mut blocks,
        1,
        vec![RegisterId(7)],
        vec![SIRInstruction::Store(
            address(11),
            SIROffset::Static(0),
            8,
            RegisterId(7),
            Vec::new(),
            Vec::new(),
        )],
        SIRTerminator::Jump(BlockId(3), Vec::new()),
    );
    insert_block(
        &mut blocks,
        2,
        vec![RegisterId(8)],
        vec![SIRInstruction::Store(
            address(12),
            SIROffset::Static(0),
            8,
            RegisterId(8),
            Vec::new(),
            Vec::new(),
        )],
        SIRTerminator::Jump(BlockId(3), Vec::new()),
    );
    insert_block(
        &mut blocks,
        3,
        Vec::new(),
        Vec::new(),
        SIRTerminator::Return,
    );
    ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks,
        register_map,
    }
}

fn repeated_predicate_unit() -> ExecutionUnit<RegionedAbsoluteAddr> {
    let mut register_map = HashMap::default();
    register_map.insert(RegisterId(0), bit(1));
    for reg in 1..=11 {
        register_map.insert(RegisterId(reg), bit(8));
    }
    let mut blocks = HashMap::default();
    insert_block(
        &mut blocks,
        0,
        vec![RegisterId(0), RegisterId(1), RegisterId(2)],
        vec![
            // Two closed arm DAGs. The selected values are postprocessed
            // and reconverge before their one external Store.
            SIRInstruction::Binary(RegisterId(3), RegisterId(1), BinaryOp::Mul, RegisterId(1)),
            SIRInstruction::Binary(RegisterId(4), RegisterId(2), BinaryOp::Add, RegisterId(2)),
            SIRInstruction::Mux(RegisterId(5), RegisterId(0), RegisterId(3), RegisterId(4)),
            SIRInstruction::Binary(RegisterId(6), RegisterId(5), BinaryOp::And, RegisterId(1)),
            SIRInstruction::Binary(RegisterId(7), RegisterId(3), BinaryOp::Mul, RegisterId(1)),
            SIRInstruction::Binary(RegisterId(8), RegisterId(4), BinaryOp::Add, RegisterId(2)),
            SIRInstruction::Mux(RegisterId(9), RegisterId(0), RegisterId(7), RegisterId(8)),
            SIRInstruction::Binary(RegisterId(10), RegisterId(9), BinaryOp::Or, RegisterId(2)),
            SIRInstruction::Binary(RegisterId(11), RegisterId(6), BinaryOp::Or, RegisterId(10)),
            SIRInstruction::Store(
                address(40),
                SIROffset::Static(0),
                8,
                RegisterId(11),
                Vec::new(),
                Vec::new(),
            ),
        ],
        SIRTerminator::Return,
    );
    ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks,
        register_map,
    }
}

fn repeated_gated_and_unit() -> ExecutionUnit<RegionedAbsoluteAddr> {
    let mut register_map = HashMap::default();
    register_map.insert(RegisterId(0), bit(1));
    register_map.insert(RegisterId(1), bit(8));
    register_map.insert(RegisterId(2), bit(8));
    for reg in 3..=12 {
        register_map.insert(RegisterId(reg), bit(8));
    }
    for reg in 13..=17 {
        register_map.insert(RegisterId(reg), bit(1));
    }
    let mut blocks = HashMap::default();
    insert_block(
        &mut blocks,
        0,
        vec![RegisterId(0), RegisterId(1), RegisterId(2)],
        vec![
            SIRInstruction::Binary(RegisterId(3), RegisterId(1), BinaryOp::Mul, RegisterId(1)),
            SIRInstruction::Binary(RegisterId(4), RegisterId(3), BinaryOp::Mul, RegisterId(1)),
            SIRInstruction::Binary(RegisterId(5), RegisterId(4), BinaryOp::Mul, RegisterId(1)),
            SIRInstruction::Binary(RegisterId(6), RegisterId(5), BinaryOp::Mul, RegisterId(1)),
            SIRInstruction::Binary(RegisterId(7), RegisterId(6), BinaryOp::Mul, RegisterId(1)),
            SIRInstruction::Binary(RegisterId(13), RegisterId(7), BinaryOp::Eq, RegisterId(2)),
            SIRInstruction::Binary(
                RegisterId(14),
                RegisterId(0),
                BinaryOp::LogicAnd,
                RegisterId(13),
            ),
            SIRInstruction::Binary(RegisterId(8), RegisterId(2), BinaryOp::Mul, RegisterId(2)),
            SIRInstruction::Binary(RegisterId(9), RegisterId(8), BinaryOp::Mul, RegisterId(2)),
            SIRInstruction::Binary(RegisterId(10), RegisterId(9), BinaryOp::Mul, RegisterId(2)),
            SIRInstruction::Binary(RegisterId(11), RegisterId(10), BinaryOp::Mul, RegisterId(2)),
            SIRInstruction::Binary(RegisterId(12), RegisterId(11), BinaryOp::Mul, RegisterId(2)),
            SIRInstruction::Binary(RegisterId(15), RegisterId(12), BinaryOp::Eq, RegisterId(1)),
            SIRInstruction::Binary(RegisterId(16), RegisterId(15), BinaryOp::And, RegisterId(0)),
            SIRInstruction::Binary(
                RegisterId(17),
                RegisterId(14),
                BinaryOp::LogicOr,
                RegisterId(16),
            ),
            SIRInstruction::Store(
                address(41),
                SIROffset::Static(0),
                1,
                RegisterId(17),
                Vec::new(),
                Vec::new(),
            ),
        ],
        SIRTerminator::Return,
    );
    ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks,
        register_map,
    }
}

#[derive(Debug, PartialEq, Eq)]
struct ExecutionTrace {
    stores: Vec<(RegionedAbsoluteAddr, usize, usize, u64)>,
}

fn execute(eu: &ExecutionUnit<RegionedAbsoluteAddr>, condition: u64, input: u64) -> ExecutionTrace {
    execute_with_inputs(eu, condition, input, input)
}

fn execute_with_inputs(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    condition: u64,
    input: u64,
    second_input: u64,
) -> ExecutionTrace {
    let mut registers = HashMap::default();
    registers.insert(RegisterId(0), condition);
    registers.insert(RegisterId(1), input);
    registers.insert(RegisterId(2), second_input);
    registers.insert(RegisterId(3), input);
    let mut memory = HashMap::<(RegionedAbsoluteAddr, usize), u64>::default();
    let mut stores = Vec::new();
    let mut current = eu.entry_block_id;
    let mut entered = HashSet::default();
    loop {
        assert!(
            entered.insert(current),
            "test fixture unexpectedly contains a loop"
        );
        let block = &eu.blocks[&current];
        for inst in &block.instructions {
            match inst {
                SIRInstruction::Imm(dst, value) => {
                    let digits = value.payload.to_u64_digits();
                    registers.insert(*dst, digits.first().copied().unwrap_or(0));
                }
                SIRInstruction::Binary(dst, lhs, op, rhs) => {
                    let lhs = registers[lhs];
                    let rhs = registers[rhs];
                    let value = match op {
                        BinaryOp::Add => lhs.wrapping_add(rhs),
                        BinaryOp::Mul => lhs.wrapping_mul(rhs),
                        BinaryOp::Eq => u64::from(lhs == rhs),
                        BinaryOp::Or | BinaryOp::LogicOr => lhs | rhs,
                        BinaryOp::And | BinaryOp::LogicAnd => lhs & rhs,
                        other => panic!("unsupported test binary op {other:?}"),
                    };
                    let width = eu.register_map[dst].width();
                    let mask = if width >= 64 {
                        u64::MAX
                    } else {
                        (1u64 << width) - 1
                    };
                    registers.insert(*dst, value & mask);
                }
                SIRInstruction::Unary(dst, op, source) => {
                    let width = eu.register_map[dst].width();
                    let mask = if width >= 64 {
                        u64::MAX
                    } else {
                        (1u64 << width) - 1
                    };
                    let value = match op {
                        UnaryOp::Ident => registers[source],
                        UnaryOp::BitNot => !registers[source],
                        UnaryOp::Minus => registers[source].wrapping_neg(),
                        other => panic!("unsupported test unary op {other:?}"),
                    };
                    registers.insert(*dst, value & mask);
                }
                SIRInstruction::Load(dst, addr, offset, _) => {
                    let offset = match offset {
                        SIROffset::Static(offset)
                        | SIROffset::PackedElements {
                            bit_offset: offset, ..
                        } => *offset,
                        SIROffset::Dynamic(offset) => registers[offset] as usize,
                        SIROffset::Element {
                            index,
                            element_width,
                            bit_offset,
                            dynamic_bit_offset,
                        } => {
                            registers[index] as usize * element_width
                                + bit_offset
                                + dynamic_bit_offset
                                    .map(|register| registers[&register] as usize)
                                    .unwrap_or(0)
                        }
                    };
                    registers.insert(*dst, memory.get(&(*addr, offset)).copied().unwrap_or(0));
                }
                SIRInstruction::Mux(dst, cond, true_value, false_value) => {
                    let selected = if registers[cond] != 0 {
                        registers[true_value]
                    } else {
                        registers[false_value]
                    };
                    registers.insert(*dst, selected);
                }
                SIRInstruction::Store(addr, SIROffset::Static(offset), width, source, _, _) => {
                    let value = registers[source];
                    memory.insert((*addr, *offset), value);
                    stores.push((*addr, *offset, *width, value));
                }
                other => panic!("unsupported test instruction {other:?}"),
            }
        }
        let (target, arguments) = match &block.terminator {
            SIRTerminator::Jump(target, args) => (*target, args.clone()),
            SIRTerminator::Branch {
                cond,
                true_block,
                false_block,
            } => {
                if registers[cond] != 0 {
                    (true_block.0, true_block.1.clone())
                } else {
                    (false_block.0, false_block.1.clone())
                }
            }
            SIRTerminator::Switch {
                selector,
                cases,
                default,
            } => {
                let selected = registers[selector];
                let target = cases
                    .iter()
                    .find(|case| {
                        case.value.to_u64_digits().first().copied().unwrap_or(0) == selected
                    })
                    .map_or(*default, |case| case.target);
                (target, Vec::new())
            }
            SIRTerminator::Return => return ExecutionTrace { stores },
            SIRTerminator::Error(code) => panic!("unexpected test error {code}"),
        };
        let values = arguments
            .iter()
            .map(|argument| registers[argument])
            .collect::<Vec<_>>();
        for (&param, value) in eu.blocks[&target].params.iter().zip(values) {
            registers.insert(param, value);
        }
        current = target;
    }
}

fn assert_unchanged(
    before: &ExecutionUnit<RegionedAbsoluteAddr>,
    after: &ExecutionUnit<RegionedAbsoluteAddr>,
) {
    assert_eq!(after.entry_block_id, before.entry_block_id);
    assert_eq!(after.register_map, before.register_map);
    assert_eq!(after.blocks, before.blocks);
}

#[cfg(debug_assertions)]
#[test]
fn coupled_store_batches_verify_once_and_preserve_all_stores() {
    const COUNT: usize = 16;
    let mut registers = HashMap::from_iter([
        (RegisterId(0), bit(1)),
        (RegisterId(1), bit(8)),
        (RegisterId(2), bit(8)),
    ]);
    let mut blocks = HashMap::default();
    for lane in 0..COUNT {
        let base = 3 + lane * 11;
        let reg = |offset| RegisterId(base + offset);
        for offset in 0..11 {
            registers.insert(reg(offset), bit(8));
        }
        let mut instructions = vec![SIRInstruction::Binary(
            reg(0),
            RegisterId(1),
            BinaryOp::Mul,
            RegisterId(1),
        )];
        for offset in 1..7 {
            instructions.push(SIRInstruction::Binary(
                reg(offset),
                reg(offset - 1),
                BinaryOp::Mul,
                RegisterId(1),
            ));
        }
        instructions.extend([
            SIRInstruction::Imm(reg(7), SIRValue::new(85u8)),
            SIRInstruction::Binary(reg(8), reg(6), BinaryOp::Add, RegisterId(2)),
            SIRInstruction::Mux(reg(9), RegisterId(0), reg(6), reg(7)),
            SIRInstruction::Mux(reg(10), RegisterId(0), reg(8), reg(7)),
            SIRInstruction::Store(
                address(100 + lane * 2),
                SIROffset::Static(0),
                8,
                reg(9),
                vec![],
                vec![],
            ),
            SIRInstruction::Store(
                address(101 + lane * 2),
                SIROffset::Static(0),
                8,
                reg(10),
                vec![],
                vec![],
            ),
        ]);
        insert_block(
            &mut blocks,
            lane,
            if lane == 0 {
                vec![RegisterId(0), RegisterId(1), RegisterId(2)]
            } else {
                vec![]
            },
            instructions,
            if lane + 1 == COUNT {
                SIRTerminator::Return
            } else {
                SIRTerminator::Jump(BlockId(lane + 1), vec![])
            },
        );
    }
    let mut eu = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks,
        register_map: registers,
    };
    eu.verify_result().unwrap();
    let before = eu.clone();
    let mut verifications = 0;
    form_coupled_store_regions_verified_by(&mut eu, |eu| {
        verifications += 1;
        verify_coupled_store_batch(eu);
    });
    assert_eq!(verifications, 1);
    assert_eq!(
        eu.blocks.len(),
        COUNT * 4,
        "every independent store group should be transformed"
    );
    eu.verify_result().unwrap();
    for condition in [0, 1] {
        for input in [0, 1] {
            let expected = execute_with_inputs(&before, condition, input, 7);
            let actual = execute_with_inputs(&eu, condition, input, 7);
            assert_eq!(actual, expected);
        }
    }
}

#[test]
fn coupled_stores_recover_the_complete_shared_priority_spine() {
    let mut register_map = HashMap::default();
    register_map.insert(RegisterId(0), bit(1));
    register_map.insert(RegisterId(1), bit(1));
    register_map.insert(RegisterId(2), bit(1));
    for register in 3..=22 {
        register_map.insert(RegisterId(register), bit(8));
    }
    register_map.insert(RegisterId(21), bit(1));
    register_map.insert(RegisterId(22), bit(1));
    let mut instructions = Vec::new();
    for register in 4..=10 {
        instructions.push(SIRInstruction::Binary(
            RegisterId(register),
            RegisterId(register - 1),
            BinaryOp::Mul,
            RegisterId(3),
        ));
    }
    instructions.extend([
        SIRInstruction::Imm(RegisterId(11), SIRValue::new(0x55u8)),
        SIRInstruction::Imm(RegisterId(12), SIRValue::new(0xaau8)),
        SIRInstruction::Binary(RegisterId(21), RegisterId(10), BinaryOp::Eq, RegisterId(11)),
        SIRInstruction::Binary(
            RegisterId(22),
            RegisterId(2),
            BinaryOp::LogicAnd,
            RegisterId(21),
        ),
        SIRInstruction::Binary(RegisterId(13), RegisterId(10), BinaryOp::Add, RegisterId(3)),
        SIRInstruction::Mux(
            RegisterId(14),
            RegisterId(22),
            RegisterId(11),
            RegisterId(13),
        ),
        SIRInstruction::Mux(
            RegisterId(15),
            RegisterId(1),
            RegisterId(11),
            RegisterId(14),
        ),
        SIRInstruction::Mux(
            RegisterId(16),
            RegisterId(0),
            RegisterId(12),
            RegisterId(15),
        ),
        SIRInstruction::Store(
            address(60),
            SIROffset::Static(0),
            8,
            RegisterId(16),
            Vec::new(),
            Vec::new(),
        ),
        SIRInstruction::Mux(
            RegisterId(18),
            RegisterId(22),
            RegisterId(12),
            RegisterId(10),
        ),
        SIRInstruction::Mux(
            RegisterId(19),
            RegisterId(1),
            RegisterId(12),
            RegisterId(18),
        ),
        SIRInstruction::Mux(
            RegisterId(20),
            RegisterId(0),
            RegisterId(11),
            RegisterId(19),
        ),
        SIRInstruction::Store(
            address(61),
            SIROffset::Static(0),
            8,
            RegisterId(20),
            Vec::new(),
            Vec::new(),
        ),
    ]);
    let mut blocks = HashMap::default();
    insert_block(
        &mut blocks,
        0,
        vec![RegisterId(0), RegisterId(1), RegisterId(2), RegisterId(3)],
        instructions,
        SIRTerminator::Return,
    );
    let mut eu = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks,
        register_map,
    };
    eu.verify_result().unwrap();
    let before = eu.clone();

    form_coupled_store_regions(&mut eu);

    eu.verify_result().unwrap();
    for outer in [0, 1] {
        for inner in [0, 1] {
            for final_condition in [0, 1] {
                assert_eq!(
                    execute_with_inputs(&before, outer, inner, final_condition),
                    execute_with_inputs(&eu, outer, inner, final_condition)
                );
            }
        }
    }
    assert_eq!(
        eu.blocks
            .values()
            .flat_map(|block| &block.instructions)
            .filter(|instruction| matches!(instruction, SIRInstruction::Mux(..)))
            .count(),
        0
    );
    assert_eq!(
        eu.blocks
            .values()
            .filter(|block| block.instructions.iter().any(|instruction| {
                matches!(instruction, SIRInstruction::Binary(_, _, BinaryOp::Mul, _))
            }))
            .count(),
        1
    );
    assert!(
        !eu.blocks[&eu.entry_block_id]
            .instructions
            .iter()
            .any(|instruction| matches!(
                instruction,
                SIRInstruction::Binary(_, _, BinaryOp::Mul, _)
            ))
    );
    let multiplication_block = eu
        .blocks
        .values()
        .find(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(instruction, SIRInstruction::Binary(_, _, BinaryOp::Mul, _))
            })
        })
        .unwrap();
    assert!(matches!(
        multiplication_block.terminator,
        SIRTerminator::Branch { .. }
    ));

    // Store templates are cloned into every priority leaf.  A dynamic
    // offset which is also part of a late leaf's value cone must therefore
    // stay above the region instead of following that value cone.
    let mut dynamic_offset_eu = before;
    dynamic_offset_eu
        .register_map
        .insert(RegisterId(22), RegisterType::Logic { width: 1 });
    for instruction in &mut dynamic_offset_eu
        .blocks
        .get_mut(&BlockId(0))
        .unwrap()
        .instructions
    {
        if let SIRInstruction::Store(_, offset, _, _, _, _) = instruction {
            *offset = SIROffset::Dynamic(RegisterId(10));
        }
    }
    dynamic_offset_eu.verify_result().unwrap();
    form_coupled_store_regions(&mut dynamic_offset_eu);
    dynamic_offset_eu.verify_result().unwrap();
    assert!(
        dynamic_offset_eu.blocks[&dynamic_offset_eu.entry_block_id]
            .instructions
            .iter()
            .any(|instruction| matches!(instruction, SIRInstruction::Binary(RegisterId(10), ..)))
    );
}

#[test]
fn sinks_a_shared_true_edge_dag_and_preserves_both_guard_outcomes() {
    let mut eu = shared_dag_unit();
    eu.verify_result().unwrap();
    let before = eu.clone();

    GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

    eu.verify_result().unwrap();
    assert_eq!(execute(&before, 0, 7), execute(&eu, 0, 7));
    assert_eq!(execute(&before, 1, 7), execute(&eu, 1, 7));
    let SIRTerminator::Branch {
        true_block,
        false_block,
        ..
    } = &eu.blocks[&BlockId(0)].terminator
    else {
        panic!("guard must remain a branch");
    };
    let head = &eu.blocks[&BlockId(0)];
    assert!(!head.instructions.iter().any(|inst| {
        matches!(
            def_reg(inst),
            Some(RegisterId(3) | RegisterId(4) | RegisterId(5))
        )
    }));
    let true_shim = &eu.blocks[&true_block.0];
    assert!(
        true_shim
            .instructions
            .iter()
            .any(|inst| { matches!(inst, SIRInstruction::Binary(RegisterId(3), ..)) })
    );
    assert!(
        true_shim
            .instructions
            .iter()
            .any(|inst| { matches!(inst, SIRInstruction::Binary(RegisterId(4), ..)) })
    );
    assert!(
        true_shim
            .instructions
            .iter()
            .any(|inst| { matches!(inst, SIRInstruction::Binary(RegisterId(5), ..)) })
    );
    assert!(
        true_shim
            .instructions
            .iter()
            .any(|inst| { matches!(inst, SIRInstruction::Store(_, _, 8, RegisterId(5), _, _)) })
    );
    let false_shim = &eu.blocks[&false_block.0];
    assert!(
        false_shim
            .instructions
            .iter()
            .any(|inst| { matches!(inst, SIRInstruction::Store(_, _, 8, RegisterId(2), _, _)) })
    );
}

#[test]
fn sinks_the_more_expensive_false_edge_dag() {
    let mut eu = shared_dag_unit();
    let head = eu.blocks.get_mut(&BlockId(0)).unwrap();
    let SIRInstruction::Mux(_, _, true_value, false_value) = &mut head.instructions[4] else {
        panic!("fixture must select the shared DAG");
    };
    std::mem::swap(true_value, false_value);
    let SIRTerminator::Branch {
        true_block,
        false_block,
        ..
    } = &mut head.terminator
    else {
        panic!("fixture must branch on the same predicate");
    };
    std::mem::swap(&mut true_block.1, &mut false_block.1);
    eu.verify_result().unwrap();
    let before = eu.clone();

    GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

    eu.verify_result().unwrap();
    assert_eq!(execute(&before, 0, 7), execute(&eu, 0, 7));
    assert_eq!(execute(&before, 1, 7), execute(&eu, 1, 7));
    assert!(!eu.blocks[&BlockId(0)].instructions.iter().any(|inst| {
        matches!(
            def_reg(inst),
            Some(RegisterId(3) | RegisterId(4) | RegisterId(5))
        )
    }));
    let SIRTerminator::Branch { false_block, .. } = &eu.blocks[&BlockId(0)].terminator else {
        panic!("guard must remain a branch");
    };
    let false_shim = &eu.blocks[&false_block.0];
    assert!(
        false_shim
            .instructions
            .iter()
            .any(|inst| { matches!(inst, SIRInstruction::Binary(RegisterId(5), ..)) })
    );
    assert!(
        false_shim
            .instructions
            .iter()
            .any(|inst| { matches!(inst, SIRInstruction::Store(_, _, 8, RegisterId(5), _, _)) })
    );
}

#[test]
fn phi_store_distribution_recovers_the_remaining_priority_chain() {
    let mut register_map = HashMap::default();
    register_map.insert(RegisterId(0), bit(1));
    register_map.insert(RegisterId(1), bit(1));
    for register in 2..=20 {
        register_map.insert(RegisterId(register), bit(8));
    }

    let mut blocks = HashMap::default();
    insert_block(
        &mut blocks,
        0,
        vec![RegisterId(0), RegisterId(1), RegisterId(2)],
        vec![
            SIRInstruction::Binary(RegisterId(4), RegisterId(2), BinaryOp::Mul, RegisterId(2)),
            SIRInstruction::Binary(RegisterId(5), RegisterId(4), BinaryOp::Mul, RegisterId(2)),
            SIRInstruction::Binary(RegisterId(15), RegisterId(5), BinaryOp::Mul, RegisterId(2)),
            SIRInstruction::Binary(RegisterId(16), RegisterId(15), BinaryOp::Mul, RegisterId(2)),
            SIRInstruction::Binary(RegisterId(17), RegisterId(16), BinaryOp::Mul, RegisterId(2)),
            SIRInstruction::Binary(RegisterId(18), RegisterId(17), BinaryOp::Mul, RegisterId(2)),
            SIRInstruction::Binary(RegisterId(19), RegisterId(18), BinaryOp::Mul, RegisterId(2)),
            SIRInstruction::Binary(RegisterId(20), RegisterId(19), BinaryOp::Mul, RegisterId(2)),
            SIRInstruction::Imm(RegisterId(7), SIRValue::new(0x70u8)),
            SIRInstruction::Imm(RegisterId(8), SIRValue::new(0x80u8)),
            SIRInstruction::Imm(RegisterId(11), SIRValue::new(0x11u8)),
            SIRInstruction::Imm(RegisterId(13), SIRValue::new(0x13u8)),
            SIRInstruction::Mux(RegisterId(14), RegisterId(1), RegisterId(8), RegisterId(20)),
            SIRInstruction::Mux(RegisterId(6), RegisterId(0), RegisterId(7), RegisterId(14)),
            SIRInstruction::Store(
                address(70),
                SIROffset::Static(0),
                8,
                RegisterId(6),
                Vec::new(),
                Vec::new(),
            ),
        ],
        SIRTerminator::Branch {
            cond: RegisterId(0),
            true_block: (BlockId(1), Vec::new()),
            false_block: (BlockId(2), Vec::new()),
        },
    );
    insert_block(
        &mut blocks,
        1,
        Vec::new(),
        Vec::new(),
        SIRTerminator::Jump(BlockId(3), vec![RegisterId(13)]),
    );
    insert_block(
        &mut blocks,
        2,
        Vec::new(),
        vec![SIRInstruction::Mux(
            RegisterId(12),
            RegisterId(1),
            RegisterId(8),
            RegisterId(11),
        )],
        SIRTerminator::Jump(BlockId(3), vec![RegisterId(12)]),
    );
    insert_block(
        &mut blocks,
        3,
        vec![RegisterId(9)],
        vec![SIRInstruction::Store(
            address(71),
            SIROffset::Static(0),
            8,
            RegisterId(9),
            Vec::new(),
            Vec::new(),
        )],
        SIRTerminator::Return,
    );
    let mut eu = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks,
        register_map,
    };
    eu.verify_result().unwrap();
    let before = eu.clone();

    GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

    eu.verify_result().unwrap();
    for outer in [0, 1] {
        for inner in [0, 1] {
            assert_eq!(execute(&before, outer, inner), execute(&eu, outer, inner));
        }
    }
    assert!(eu.blocks[&BlockId(3)].instructions.is_empty());
    assert!(
        !eu.blocks[&BlockId(0)]
            .instructions
            .iter()
            .any(|instruction| {
                matches!(instruction, SIRInstruction::Binary(_, _, BinaryOp::Mul, _))
            })
    );
    let multiplication_block = eu
        .blocks
        .values()
        .find(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(instruction, SIRInstruction::Binary(_, _, BinaryOp::Mul, _))
            })
        })
        .expect("normal leaf must retain the expensive DAG");
    let cfg = SirCfg::analyze(&eu).unwrap();
    assert!(cfg.dominates(BlockId(0), multiplication_block.id));
    assert!(
        eu.blocks.values().any(|block| {
            matches!(
                block.terminator,
                SIRTerminator::Branch {
                    cond: RegisterId(1),
                    ..
                }
            ) && cfg.dominates(block.id, multiplication_block.id)
        }),
        "{eu:#?}"
    );
}

#[test]
fn one_branch_specializes_a_repeated_predicate_and_merges_only_the_live_out() {
    let mut eu = repeated_predicate_unit();
    eu.verify_result().unwrap();
    let before = eu.clone();

    GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

    eu.verify_result().unwrap();
    for condition in [0, 1] {
        for input in [3, 5, 7, 11] {
            assert_eq!(
                execute(&before, condition, input),
                execute(&eu, condition, input),
            );
        }
    }
    let head = &eu.blocks[&BlockId(0)];
    let SIRTerminator::Branch {
        true_block,
        false_block,
        ..
    } = &head.terminator
    else {
        panic!("the repeated predicate must form one shared branch");
    };
    assert!(
        eu.blocks[&true_block.0]
            .instructions
            .iter()
            .all(|inst| !matches!(inst, SIRInstruction::Mux(_, RegisterId(0), ..)))
    );
    assert!(
        eu.blocks[&false_block.0]
            .instructions
            .iter()
            .all(|inst| !matches!(inst, SIRInstruction::Mux(_, RegisterId(0), ..)))
    );
    let merge = match &eu.blocks[&true_block.0].terminator {
        SIRTerminator::Jump(merge, args) => {
            assert_eq!(
                args.len(),
                1,
                "only the reconverged result crosses the edge"
            );
            *merge
        }
        other => panic!("unexpected true-edge terminator: {other:?}"),
    };
    assert_eq!(eu.blocks[&merge].params, vec![RegisterId(11)]);
    assert!(matches!(
        eu.blocks[&merge].instructions.as_slice(),
        [SIRInstruction::Store(_, _, 8, RegisterId(11), _, _)]
    ));
    for dead in 3..=10 {
        assert!(
            !eu.register_map.contains_key(&RegisterId(dead)),
            "replaced original r{dead} must not allocate a native VReg"
        );
    }
    assert!(
        eu.register_map.contains_key(&RegisterId(11)),
        "the merge parameter keeps its original register type"
    );
}

#[test]
fn one_branch_skips_repeated_gated_and_payloads_and_merges_one_bit() {
    let mut eu = repeated_gated_and_unit();
    eu.verify_result().unwrap();
    let before = eu.clone();

    GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

    eu.verify_result().unwrap();
    for condition in [0, 1] {
        for input in [0, 1, 3, 7, 11, 255] {
            assert_eq!(
                execute(&before, condition, input),
                execute(&eu, condition, input),
            );
        }
    }
    let SIRTerminator::Branch {
        true_block,
        false_block,
        ..
    } = &eu.blocks[&BlockId(0)].terminator
    else {
        panic!("the repeated gate must form one shared branch");
    };
    assert!(eu.blocks[&BlockId(0)].instructions.is_empty());
    assert!(
        eu.blocks[&true_block.0]
            .instructions
            .iter()
            .any(|inst| matches!(inst, SIRInstruction::Binary(_, _, BinaryOp::Mul, _)))
    );
    assert!(
        eu.blocks[&true_block.0]
            .instructions
            .iter()
            .all(|inst| { !matches!(inst, SIRInstruction::Binary(_, _, BinaryOp::LogicAnd, _)) })
    );
    assert!(eu.blocks[&false_block.0].instructions.iter().all(|inst| {
        matches!(
            inst,
            SIRInstruction::Imm(_, _) | SIRInstruction::Binary(_, _, BinaryOp::LogicOr, _)
        )
    }));
    let merge = match &eu.blocks[&true_block.0].terminator {
        SIRTerminator::Jump(merge, args) => {
            assert_eq!(args.len(), 1);
            *merge
        }
        other => panic!("unexpected true-edge terminator: {other:?}"),
    };
    assert_eq!(eu.blocks[&merge].params, vec![RegisterId(17)]);
    assert!(matches!(
        eu.blocks[&merge].instructions.as_slice(),
        [SIRInstruction::Store(_, _, 1, RegisterId(17), _, _)]
    ));
}

#[test]
fn same_predicate_region_moves_static_and_dynamic_loads_with_their_arms() {
    let mut eu = repeated_predicate_unit();
    eu.register_map.insert(RegisterId(12), bit(8));
    eu.register_map.insert(RegisterId(13), bit(8));
    let block = eu.blocks.get_mut(&BlockId(0)).unwrap();
    block.instructions.insert(
        0,
        SIRInstruction::Load(RegisterId(12), address(50), SIROffset::Static(0), 8),
    );
    block.instructions.insert(
        1,
        SIRInstruction::Load(
            RegisterId(13),
            address(51),
            SIROffset::Dynamic(RegisterId(2)),
            8,
        ),
    );
    let SIRInstruction::Binary(_, true_lhs, _, _) = &mut block.instructions[2] else {
        panic!("fixture true arm must start with a binary operation");
    };
    *true_lhs = RegisterId(12);
    let SIRInstruction::Binary(_, false_lhs, _, _) = &mut block.instructions[3] else {
        panic!("fixture false arm must start with a binary operation");
    };
    *false_lhs = RegisterId(13);
    eu.verify_result().unwrap();
    let before = eu.clone();

    GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

    eu.verify_result().unwrap();
    for condition in [0, 1] {
        assert_eq!(execute(&before, condition, 7), execute(&eu, condition, 7));
    }
    let SIRTerminator::Branch {
        true_block,
        false_block,
        ..
    } = &eu.blocks[&BlockId(0)].terminator
    else {
        panic!("load-bearing repeated predicate must branch");
    };
    assert!(
        eu.blocks[&true_block.0]
            .instructions
            .iter()
            .any(|inst| matches!(inst, SIRInstruction::Load(_, _, SIROffset::Static(0), 8)))
    );
    assert!(
        eu.blocks[&false_block.0]
            .instructions
            .iter()
            .any(|inst| { matches!(inst, SIRInstruction::Load(_, _, SIROffset::Dynamic(_), 8)) })
    );
}

#[test]
fn nested_same_predicate_mux_drops_the_unselected_cofactor() {
    let mut eu = repeated_predicate_unit();
    for reg in 12..=14 {
        eu.register_map.insert(RegisterId(reg), bit(8));
    }
    let block = eu.blocks.get_mut(&BlockId(0)).unwrap();
    let store = block.instructions.pop().unwrap();
    block.instructions.extend([
        SIRInstruction::Unary(RegisterId(12), UnaryOp::BitNot, RegisterId(11)),
        SIRInstruction::Unary(RegisterId(13), UnaryOp::Minus, RegisterId(11)),
        SIRInstruction::Mux(
            RegisterId(14),
            RegisterId(0),
            RegisterId(12),
            RegisterId(13),
        ),
    ]);
    let SIRInstruction::Store(address, offset, width, _, triggers, sites) = store else {
        panic!("fixture must end in a Store");
    };
    block.instructions.push(SIRInstruction::Store(
        address,
        offset,
        width,
        RegisterId(14),
        triggers,
        sites,
    ));
    eu.verify_result().unwrap();
    let before = eu.clone();

    GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

    eu.verify_result().unwrap();
    for condition in [0, 1] {
        assert_eq!(execute(&before, condition, 9), execute(&eu, condition, 9));
    }
    let SIRTerminator::Branch {
        true_block,
        false_block,
        ..
    } = &eu.blocks[&BlockId(0)].terminator
    else {
        panic!("nested repeated predicate must branch");
    };
    assert!(
        eu.blocks[&true_block.0]
            .instructions
            .iter()
            .any(|inst| { matches!(inst, SIRInstruction::Unary(_, UnaryOp::BitNot, _)) })
    );
    assert!(
        !eu.blocks[&true_block.0]
            .instructions
            .iter()
            .any(|inst| { matches!(inst, SIRInstruction::Unary(_, UnaryOp::Minus, _)) })
    );
    assert!(
        eu.blocks[&false_block.0]
            .instructions
            .iter()
            .any(|inst| { matches!(inst, SIRInstruction::Unary(_, UnaryOp::Minus, _)) })
    );
    assert!(
        !eu.blocks[&false_block.0]
            .instructions
            .iter()
            .any(|inst| { matches!(inst, SIRInstruction::Unary(_, UnaryOp::BitNot, _)) })
    );
}

#[test]
fn keeps_a_locally_defined_predicate_before_the_recovered_branch() {
    let mut register_map = HashMap::default();
    for reg in 0..=14 {
        register_map.insert(RegisterId(reg), bit(1));
    }
    let mut blocks = HashMap::default();
    insert_block(
        &mut blocks,
        0,
        vec![RegisterId(0), RegisterId(1), RegisterId(2)],
        vec![
            SIRInstruction::Unary(RegisterId(3), UnaryOp::Ident, RegisterId(0)),
            SIRInstruction::Binary(RegisterId(4), RegisterId(1), BinaryOp::Mul, RegisterId(1)),
            SIRInstruction::Binary(RegisterId(5), RegisterId(4), BinaryOp::Mul, RegisterId(1)),
            SIRInstruction::Binary(RegisterId(6), RegisterId(5), BinaryOp::Mul, RegisterId(1)),
            SIRInstruction::Binary(RegisterId(7), RegisterId(6), BinaryOp::Mul, RegisterId(1)),
            SIRInstruction::Binary(RegisterId(8), RegisterId(2), BinaryOp::Mul, RegisterId(2)),
            SIRInstruction::Binary(RegisterId(9), RegisterId(8), BinaryOp::Mul, RegisterId(2)),
            SIRInstruction::Binary(RegisterId(10), RegisterId(9), BinaryOp::Mul, RegisterId(2)),
            SIRInstruction::Binary(RegisterId(11), RegisterId(10), BinaryOp::Mul, RegisterId(2)),
            // The predicate is also selected as data. Moving r3 into the
            // true arm would leave the newly introduced Branch undefined.
            SIRInstruction::Mux(RegisterId(12), RegisterId(3), RegisterId(3), RegisterId(7)),
            SIRInstruction::Mux(RegisterId(13), RegisterId(3), RegisterId(3), RegisterId(11)),
            SIRInstruction::Binary(RegisterId(14), RegisterId(12), BinaryOp::Or, RegisterId(13)),
            SIRInstruction::Store(
                address(42),
                SIROffset::Static(0),
                1,
                RegisterId(14),
                Vec::new(),
                Vec::new(),
            ),
        ],
        SIRTerminator::Return,
    );
    let mut eu = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks,
        register_map,
    };
    eu.verify_result().unwrap();
    let before = eu.clone();

    GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

    eu.verify_result().unwrap();
    for condition in [0, 1] {
        for input in [0, 1] {
            assert_eq!(
                execute(&before, condition, input),
                execute(&eu, condition, input),
            );
        }
    }
    let head = &eu.blocks[&BlockId(0)];
    assert!(head.instructions.iter().any(|inst| {
        matches!(
            inst,
            SIRInstruction::Unary(RegisterId(3), UnaryOp::Ident, RegisterId(0))
        )
    }));
    assert!(matches!(
        head.terminator,
        SIRTerminator::Branch {
            cond: RegisterId(3),
            ..
        }
    ));
}

#[test]
fn rejects_a_value_used_from_the_false_region() {
    let mut eu = shared_dag_unit();
    eu.blocks
        .get_mut(&BlockId(2))
        .unwrap()
        .instructions
        .push(SIRInstruction::Store(
            address(13),
            SIROffset::Static(0),
            8,
            RegisterId(5),
            Vec::new(),
            Vec::new(),
        ));
    eu.verify_result().unwrap();
    let before = eu.clone();

    GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

    assert_unchanged(&before, &eu);
}

#[test]
fn an_aliasing_write_keeps_the_load_before_the_guard() {
    let mut eu = shared_dag_unit();
    let head = eu.blocks.get_mut(&BlockId(0)).unwrap();
    head.instructions = vec![
        SIRInstruction::Imm(RegisterId(2), SIRValue::new(0u8)),
        SIRInstruction::Load(RegisterId(5), address(20), SIROffset::Static(0), 8),
        SIRInstruction::Store(
            address(20),
            SIROffset::Static(0),
            8,
            RegisterId(1),
            Vec::new(),
            Vec::new(),
        ),
        SIRInstruction::Mux(RegisterId(6), RegisterId(0), RegisterId(5), RegisterId(2)),
        SIRInstruction::Store(
            address(10),
            SIROffset::Static(0),
            8,
            RegisterId(6),
            Vec::new(),
            Vec::new(),
        ),
    ];
    if let SIRTerminator::Branch { true_block, .. } = &mut head.terminator {
        true_block.1 = vec![RegisterId(5)];
    }
    eu.verify_result().unwrap();
    let before = eu.clone();

    GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

    assert_unchanged(&before, &eu);
}

#[test]
fn rejects_a_load_after_the_first_distributed_store() {
    let mut eu = shared_dag_unit();
    eu.register_map.insert(RegisterId(9), bit(8));
    eu.blocks
        .get_mut(&BlockId(0))
        .unwrap()
        .instructions
        .push(SIRInstruction::Load(
            RegisterId(9),
            address(30),
            SIROffset::Static(0),
            8,
        ));
    eu.verify_result().unwrap();
    let before = eu.clone();

    GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

    assert_unchanged(&before, &eu);
}

#[test]
fn rejects_an_effect_after_the_first_distributed_store() {
    let mut eu = shared_dag_unit();
    eu.blocks
        .get_mut(&BlockId(0))
        .unwrap()
        .instructions
        .push(SIRInstruction::RuntimeEvent {
            site_id: 7,
            args: vec![RegisterId(1)],
        });
    eu.verify_result().unwrap();
    let before = eu.clone();

    GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

    assert_unchanged(&before, &eu);
}

#[test]
fn preserves_multiple_distributed_store_order() {
    let mut eu = shared_dag_unit();
    eu.register_map.insert(RegisterId(9), bit(8));
    let head = eu.blocks.get_mut(&BlockId(0)).unwrap();
    head.instructions.push(SIRInstruction::Mux(
        RegisterId(9),
        RegisterId(0),
        RegisterId(4),
        RegisterId(2),
    ));
    head.instructions.push(SIRInstruction::Store(
        address(14),
        SIROffset::Static(0),
        8,
        RegisterId(9),
        Vec::new(),
        Vec::new(),
    ));
    eu.verify_result().unwrap();
    let before = eu.clone();

    GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

    eu.verify_result().unwrap();
    assert_eq!(execute(&before, 0, 3), execute(&eu, 0, 3));
    assert_eq!(execute(&before, 1, 3), execute(&eu, 1, 3));
    let SIRTerminator::Branch {
        true_block,
        false_block,
        ..
    } = &eu.blocks[&BlockId(0)].terminator
    else {
        panic!("expected guard branch");
    };
    let addresses = |block: &BasicBlock<RegionedAbsoluteAddr>| {
        block
            .instructions
            .iter()
            .filter_map(|inst| match inst {
                SIRInstruction::Store(address, ..) => Some(address.instance_id),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        addresses(&eu.blocks[&true_block.0]),
        vec![InstanceId(10), InstanceId(14)]
    );
    assert_eq!(
        addresses(&eu.blocks[&false_block.0]),
        vec![InstanceId(10), InstanceId(14)]
    );
}

#[test]
fn four_state_mode_is_non_destructive() {
    let mut eu = shared_dag_unit();
    let before = eu.clone();
    let options = PassOptions {
        four_state: true,
        ..Default::default()
    };

    GuardedRegionSinkingPass.run(&mut eu, &options);

    assert_unchanged(&before, &eu);
}

#[test]
fn trigger_only_zero_width_store_is_non_destructive() {
    let mut eu = shared_dag_unit();
    let head = eu.blocks.get_mut(&BlockId(0)).unwrap();
    let SIRInstruction::Store(_, _, width, _, triggers, _) = &mut head.instructions[5] else {
        panic!("fixture must end in a store");
    };
    *width = 0;
    *triggers = vec![TriggerIdWithKind {
        kind: DomainKind::ClockPosedge,
        id: 9,
    }];
    eu.verify_result().unwrap();
    let before = eu.clone();

    GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

    assert_unchanged(&before, &eu);
}

#[test]
fn a_second_run_does_not_rewrite_generated_regions() {
    let mut eu = shared_dag_unit();
    GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());
    eu.verify_result().unwrap();
    let once = eu.clone();

    GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

    assert_unchanged(&once, &eu);
}

#[test]
fn multi_bit_branch_guard_is_rejected() {
    let mut eu = shared_dag_unit();
    eu.register_map.insert(RegisterId(0), bit(8));
    assert_eq!(
        eu.verify_result().unwrap_err().invariant,
        "TYPE.BRANCH_CONDITION"
    );
}

#[test]
fn narrow_mux_arm_is_not_connected_directly_to_a_wider_store() {
    let mut eu = shared_dag_unit();
    eu.register_map.insert(RegisterId(2), bit(4));
    eu.register_map.insert(RegisterId(8), bit(4));
    eu.register_map.insert(RegisterId(9), bit(8));
    let head = eu.blocks.get_mut(&BlockId(0)).unwrap();
    head.instructions.insert(
        4,
        SIRInstruction::Unary(RegisterId(9), UnaryOp::Ident, RegisterId(2)),
    );
    let SIRInstruction::Mux(_, _, _, false_value) = &mut head.instructions[5] else {
        panic!("fixture must contain the distributed mux");
    };
    *false_value = RegisterId(9);
    let false_block = eu.blocks.get_mut(&BlockId(2)).unwrap();
    let SIRInstruction::Store(_, _, width, _, _, _) = &mut false_block.instructions[0] else {
        panic!("fixture false block must store its parameter");
    };
    *width = 4;
    eu.verify_result().unwrap();

    GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

    eu.verify_result().unwrap();
    assert!(
        eu.blocks
            .values()
            .flat_map(|block| &block.instructions)
            .all(|instruction| {
                let SIRInstruction::Store(_, _, width, value, _, _) = instruction else {
                    return true;
                };
                *width == eu.register_map[value].width()
            })
    );
}

#[test]
fn removable_mux_cannot_also_supply_a_dynamic_store_offset() {
    let mut eu = shared_dag_unit();
    let head = eu.blocks.get_mut(&BlockId(0)).unwrap();
    let SIRInstruction::Store(_, offset, _, _, _, _) = &mut head.instructions[5] else {
        panic!("fixture must end in the distributed store");
    };
    *offset = SIROffset::Dynamic(RegisterId(6));
    eu.verify_result().unwrap();
    let before = eu.clone();

    GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

    assert_unchanged(&before, &eu);
}

#[test]
fn one_removable_mux_cannot_supply_another_store_offset() {
    let mut eu = shared_dag_unit();
    eu.register_map.insert(RegisterId(9), bit(8));
    let head = eu.blocks.get_mut(&BlockId(0)).unwrap();
    head.instructions.insert(
        5,
        SIRInstruction::Mux(RegisterId(9), RegisterId(0), RegisterId(4), RegisterId(2)),
    );
    let SIRInstruction::Store(_, first_offset, _, _, _, _) = &mut head.instructions[6] else {
        panic!("fixture first store must remain a store");
    };
    *first_offset = SIROffset::Dynamic(RegisterId(9));
    head.instructions.push(SIRInstruction::Store(
        address(14),
        SIROffset::Static(0),
        8,
        RegisterId(9),
        Vec::new(),
        Vec::new(),
    ));
    eu.verify_result().unwrap();
    let before = eu.clone();

    GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

    assert_unchanged(&before, &eu);
}

#[test]
fn moves_a_guarded_value_diamond_under_its_selected_use() {
    let mut register_map = HashMap::default();
    register_map.insert(RegisterId(0), bit(1));
    register_map.insert(RegisterId(1), bit(1));
    for register in 2..=9 {
        register_map.insert(RegisterId(register), bit(64));
    }
    let mut blocks = HashMap::default();
    insert_block(
        &mut blocks,
        0,
        vec![
            RegisterId(0),
            RegisterId(1),
            RegisterId(2),
            RegisterId(3),
            RegisterId(4),
            RegisterId(8),
        ],
        Vec::new(),
        SIRTerminator::Branch {
            cond: RegisterId(0),
            true_block: (BlockId(1), Vec::new()),
            false_block: (BlockId(2), Vec::new()),
        },
    );
    insert_block(
        &mut blocks,
        1,
        Vec::new(),
        Vec::new(),
        SIRTerminator::Jump(BlockId(3), vec![RegisterId(4)]),
    );
    insert_block(
        &mut blocks,
        2,
        Vec::new(),
        vec![SIRInstruction::Binary(
            RegisterId(5),
            RegisterId(2),
            BinaryOp::DivU,
            RegisterId(3),
        )],
        SIRTerminator::Jump(BlockId(3), vec![RegisterId(5)]),
    );
    insert_block(
        &mut blocks,
        3,
        vec![RegisterId(6)],
        Vec::new(),
        SIRTerminator::Branch {
            cond: RegisterId(1),
            true_block: (BlockId(4), Vec::new()),
            false_block: (BlockId(5), Vec::new()),
        },
    );
    insert_block(
        &mut blocks,
        4,
        Vec::new(),
        vec![SIRInstruction::Unary(
            RegisterId(7),
            UnaryOp::BitNot,
            RegisterId(6),
        )],
        SIRTerminator::Jump(BlockId(6), vec![RegisterId(7)]),
    );
    insert_block(
        &mut blocks,
        5,
        Vec::new(),
        Vec::new(),
        SIRTerminator::Jump(BlockId(6), vec![RegisterId(8)]),
    );
    insert_block(
        &mut blocks,
        6,
        vec![RegisterId(9)],
        vec![SIRInstruction::Store(
            address(99),
            SIROffset::Static(0),
            64,
            RegisterId(9),
            Vec::new(),
            Vec::new(),
        )],
        SIRTerminator::Return,
    );
    let mut eu = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks,
        register_map,
    };
    eu.verify_result().unwrap();

    sink_deferred_value_diamonds(&mut eu);

    eu.verify_result().unwrap();
    assert!(!eu.blocks.contains_key(&BlockId(1)));
    assert!(!eu.blocks.contains_key(&BlockId(2)));
    assert!(matches!(
        eu.blocks[&BlockId(0)].terminator,
        SIRTerminator::Jump(BlockId(3), ref args) if args.is_empty()
    ));
    assert!(eu.blocks[&BlockId(3)].params.is_empty());
    assert!(matches!(
        eu.blocks[&BlockId(4)].terminator,
        SIRTerminator::Branch {
            cond: RegisterId(0),
            ..
        }
    ));
    let divide_block = eu
        .blocks
        .values()
        .find(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    instruction,
                    SIRInstruction::Binary(RegisterId(5), _, BinaryOp::DivU, _)
                )
            })
        })
        .expect("the guarded divide must retain one definition");
    let SIRTerminator::Branch { false_block, .. } = &eu.blocks[&BlockId(4)].terminator else {
        unreachable!()
    };
    assert_eq!(false_block.0, divide_block.id);
    let continuation = eu
        .blocks
        .values()
        .find(|block| block.params == vec![RegisterId(6)])
        .expect("the selected leaf must merge the guarded value locally");
    assert!(continuation.instructions.iter().any(|instruction| {
        matches!(
            instruction,
            SIRInstruction::Unary(RegisterId(7), UnaryOp::BitNot, RegisterId(6))
        )
    }));
}

#[test]
fn sinks_a_pure_multi_block_region_to_its_only_phi_use() {
    let mut register_map = HashMap::default();
    register_map.insert(RegisterId(0), bit(1));
    register_map.insert(RegisterId(1), bit(1));
    for register in 2..=21 {
        register_map.insert(RegisterId(register), bit(8));
    }
    let mut blocks = HashMap::default();
    insert_block(
        &mut blocks,
        0,
        vec![RegisterId(0), RegisterId(1), RegisterId(2)],
        vec![
            SIRInstruction::Store(
                address(80),
                SIROffset::Static(0),
                8,
                RegisterId(2),
                Vec::new(),
                Vec::new(),
            ),
            SIRInstruction::Binary(RegisterId(4), RegisterId(2), BinaryOp::Mul, RegisterId(2)),
        ],
        SIRTerminator::Branch {
            cond: RegisterId(1),
            true_block: (BlockId(1), Vec::new()),
            false_block: (BlockId(2), Vec::new()),
        },
    );
    insert_block(
        &mut blocks,
        1,
        Vec::new(),
        Vec::new(),
        SIRTerminator::Branch {
            cond: RegisterId(1),
            true_block: (BlockId(3), Vec::new()),
            false_block: (BlockId(4), Vec::new()),
        },
    );
    insert_block(
        &mut blocks,
        2,
        Vec::new(),
        Vec::new(),
        SIRTerminator::Branch {
            cond: RegisterId(1),
            true_block: (BlockId(5), Vec::new()),
            false_block: (BlockId(6), Vec::new()),
        },
    );
    for (block, value, register) in [
        (3, None, 4),
        (4, Some(4), 10),
        (5, Some(5), 11),
        (6, Some(6), 12),
    ] {
        insert_block(
            &mut blocks,
            block,
            Vec::new(),
            value
                .map(|value| {
                    vec![SIRInstruction::Imm(
                        RegisterId(register),
                        SIRValue::new(value as u8),
                    )]
                })
                .unwrap_or_default(),
            SIRTerminator::Jump(BlockId(7), vec![RegisterId(register)]),
        );
    }
    insert_block(
        &mut blocks,
        7,
        vec![RegisterId(20)],
        Vec::new(),
        SIRTerminator::Branch {
            cond: RegisterId(0),
            true_block: (BlockId(8), Vec::new()),
            false_block: (BlockId(9), vec![RegisterId(2)]),
        },
    );
    insert_block(
        &mut blocks,
        8,
        Vec::new(),
        vec![SIRInstruction::Store(
            address(81),
            SIROffset::Static(0),
            8,
            RegisterId(2),
            Vec::new(),
            Vec::new(),
        )],
        SIRTerminator::Return,
    );
    insert_block(
        &mut blocks,
        9,
        vec![RegisterId(21)],
        vec![SIRInstruction::Store(
            address(82),
            SIROffset::Static(0),
            8,
            RegisterId(20),
            Vec::new(),
            Vec::new(),
        )],
        SIRTerminator::Return,
    );
    let mut eu = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks,
        register_map,
    };
    eu.verify_result().unwrap();
    let before = eu.clone();

    sink_deferred_value_regions(&mut eu);

    eu.verify_result().unwrap();
    for exception in [0, 1] {
        for input in [0, 1] {
            assert_eq!(
                execute(&before, exception, input),
                execute(&eu, exception, input)
            );
        }
    }
    assert!(matches!(
        eu.blocks[&BlockId(0)].terminator,
        SIRTerminator::Jump(BlockId(7), ref arguments) if arguments.is_empty()
    ));
    assert!(eu.blocks[&BlockId(7)].params.is_empty());
    assert_eq!(eu.blocks[&BlockId(9)].params, vec![RegisterId(21)]);
    assert!(
        !eu.blocks[&BlockId(0)]
            .instructions
            .iter()
            .any(|instruction| def_reg(instruction) == Some(RegisterId(4)))
    );
    assert!(
        eu.blocks[&BlockId(9)]
            .instructions
            .iter()
            .any(|instruction| def_reg(instruction) == Some(RegisterId(4)))
    );
}

#[test]
fn moves_a_serial_sequence_of_guarded_values_in_one_cfg_plan() {
    let mut register_map = HashMap::default();
    for register in 0..=3 {
        register_map.insert(RegisterId(register), bit(1));
    }
    for register in 4..=17 {
        register_map.insert(RegisterId(register), bit(64));
    }
    let mut blocks = HashMap::default();
    insert_block(
        &mut blocks,
        0,
        vec![
            RegisterId(0),
            RegisterId(1),
            RegisterId(2),
            RegisterId(3),
            RegisterId(4),
            RegisterId(5),
            RegisterId(6),
            RegisterId(9),
            RegisterId(10),
            RegisterId(11),
            RegisterId(16),
        ],
        Vec::new(),
        SIRTerminator::Branch {
            cond: RegisterId(0),
            true_block: (BlockId(1), Vec::new()),
            false_block: (BlockId(2), Vec::new()),
        },
    );
    insert_block(
        &mut blocks,
        1,
        Vec::new(),
        Vec::new(),
        SIRTerminator::Jump(BlockId(3), vec![RegisterId(6)]),
    );
    insert_block(
        &mut blocks,
        2,
        Vec::new(),
        vec![SIRInstruction::Binary(
            RegisterId(7),
            RegisterId(4),
            BinaryOp::DivS,
            RegisterId(5),
        )],
        SIRTerminator::Jump(BlockId(3), vec![RegisterId(7)]),
    );
    insert_block(
        &mut blocks,
        3,
        vec![RegisterId(8)],
        Vec::new(),
        SIRTerminator::Branch {
            cond: RegisterId(1),
            true_block: (BlockId(4), Vec::new()),
            false_block: (BlockId(5), Vec::new()),
        },
    );
    insert_block(
        &mut blocks,
        4,
        Vec::new(),
        Vec::new(),
        SIRTerminator::Jump(BlockId(6), vec![RegisterId(11)]),
    );
    insert_block(
        &mut blocks,
        5,
        Vec::new(),
        vec![SIRInstruction::Binary(
            RegisterId(12),
            RegisterId(9),
            BinaryOp::RemU,
            RegisterId(10),
        )],
        SIRTerminator::Jump(BlockId(6), vec![RegisterId(12)]),
    );
    insert_block(
        &mut blocks,
        6,
        vec![RegisterId(13)],
        Vec::new(),
        SIRTerminator::Branch {
            cond: RegisterId(2),
            true_block: (BlockId(7), Vec::new()),
            false_block: (BlockId(8), Vec::new()),
        },
    );
    insert_block(
        &mut blocks,
        7,
        Vec::new(),
        vec![SIRInstruction::Unary(
            RegisterId(14),
            UnaryOp::BitNot,
            RegisterId(8),
        )],
        SIRTerminator::Jump(BlockId(11), vec![RegisterId(14)]),
    );
    insert_block(
        &mut blocks,
        8,
        Vec::new(),
        Vec::new(),
        SIRTerminator::Branch {
            cond: RegisterId(3),
            true_block: (BlockId(9), Vec::new()),
            false_block: (BlockId(10), Vec::new()),
        },
    );
    insert_block(
        &mut blocks,
        9,
        Vec::new(),
        vec![SIRInstruction::Unary(
            RegisterId(15),
            UnaryOp::BitNot,
            RegisterId(13),
        )],
        SIRTerminator::Jump(BlockId(11), vec![RegisterId(15)]),
    );
    insert_block(
        &mut blocks,
        10,
        Vec::new(),
        Vec::new(),
        SIRTerminator::Jump(BlockId(11), vec![RegisterId(16)]),
    );
    insert_block(
        &mut blocks,
        11,
        vec![RegisterId(17)],
        vec![SIRInstruction::Store(
            address(100),
            SIROffset::Static(0),
            64,
            RegisterId(17),
            Vec::new(),
            Vec::new(),
        )],
        SIRTerminator::Return,
    );
    let mut eu = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks,
        register_map,
    };
    eu.verify_result().unwrap();

    sink_deferred_value_diamonds(&mut eu);

    eu.verify_result().unwrap();
    for removed in [1, 2, 4, 5] {
        assert!(!eu.blocks.contains_key(&BlockId(removed)));
    }
    assert!(matches!(
        eu.blocks[&BlockId(0)].terminator,
        SIRTerminator::Jump(BlockId(3), ref args) if args.is_empty()
    ));
    assert!(matches!(
        eu.blocks[&BlockId(3)].terminator,
        SIRTerminator::Jump(BlockId(6), ref args) if args.is_empty()
    ));
    assert!(eu.blocks[&BlockId(3)].params.is_empty());
    assert!(eu.blocks[&BlockId(6)].params.is_empty());
    assert!(matches!(
        eu.blocks[&BlockId(7)].terminator,
        SIRTerminator::Branch {
            cond: RegisterId(0),
            ..
        }
    ));
    assert!(matches!(
        eu.blocks[&BlockId(9)].terminator,
        SIRTerminator::Branch {
            cond: RegisterId(1),
            ..
        }
    ));
}

#[test]
fn malformed_input_is_non_destructive() {
    let mut eu = shared_dag_unit();
    eu.blocks.remove(&BlockId(2));
    assert!(eu.verify_result().is_err());
    let before = eu.clone();

    GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

    assert_unchanged(&before, &eu);
}

#[test]
fn sinks_normal_arithmetic_to_the_final_priority_leaf() {
    let mut register_map = HashMap::default();
    register_map.insert(RegisterId(0), bit(1));
    register_map.insert(RegisterId(1), bit(1));
    for reg in 2..=5 {
        register_map.insert(RegisterId(reg), bit(8));
    }
    let mut blocks = HashMap::default();
    insert_block(
        &mut blocks,
        0,
        vec![RegisterId(0), RegisterId(1), RegisterId(2)],
        vec![
            SIRInstruction::Binary(RegisterId(3), RegisterId(2), BinaryOp::Mul, RegisterId(2)),
            SIRInstruction::Binary(RegisterId(4), RegisterId(3), BinaryOp::Add, RegisterId(2)),
            SIRInstruction::Imm(RegisterId(5), SIRValue::new(0u8)),
        ],
        SIRTerminator::Branch {
            cond: RegisterId(0),
            true_block: (BlockId(1), Vec::new()),
            false_block: (BlockId(2), Vec::new()),
        },
    );
    insert_block(
        &mut blocks,
        1,
        Vec::new(),
        vec![SIRInstruction::Store(
            address(70),
            SIROffset::Static(0),
            8,
            RegisterId(5),
            Vec::new(),
            Vec::new(),
        )],
        SIRTerminator::Jump(BlockId(5), Vec::new()),
    );
    insert_block(
        &mut blocks,
        2,
        Vec::new(),
        Vec::new(),
        SIRTerminator::Branch {
            cond: RegisterId(1),
            true_block: (BlockId(3), Vec::new()),
            false_block: (BlockId(4), Vec::new()),
        },
    );
    insert_block(
        &mut blocks,
        3,
        Vec::new(),
        vec![SIRInstruction::Store(
            address(70),
            SIROffset::Static(0),
            8,
            RegisterId(2),
            Vec::new(),
            Vec::new(),
        )],
        SIRTerminator::Jump(BlockId(5), Vec::new()),
    );
    insert_block(
        &mut blocks,
        4,
        Vec::new(),
        vec![SIRInstruction::Store(
            address(70),
            SIROffset::Static(0),
            8,
            RegisterId(4),
            Vec::new(),
            Vec::new(),
        )],
        SIRTerminator::Jump(BlockId(5), Vec::new()),
    );
    insert_block(
        &mut blocks,
        5,
        Vec::new(),
        Vec::new(),
        SIRTerminator::Return,
    );
    let mut eu = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks,
        register_map,
    };
    eu.verify_result().unwrap();

    GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

    eu.verify_result().unwrap();
    assert!(
        !eu.blocks[&BlockId(0)]
            .instructions
            .iter()
            .any(|instruction| {
                matches!(def_reg(instruction), Some(RegisterId(3) | RegisterId(4)))
            })
    );
    assert!(
        eu.blocks[&BlockId(4)]
            .instructions
            .iter()
            .any(|instruction| {
                matches!(instruction, SIRInstruction::Binary(RegisterId(3), ..))
            })
    );
    assert!(
        eu.blocks[&BlockId(4)]
            .instructions
            .iter()
            .any(|instruction| {
                matches!(instruction, SIRInstruction::Binary(RegisterId(4), ..))
            })
    );
    assert!(
        eu.blocks[&BlockId(1)]
            .instructions
            .iter()
            .any(|instruction| matches!(instruction, SIRInstruction::Imm(RegisterId(5), ..)))
    );
}

#[test]
fn direct_join_does_not_prove_the_branch_was_taken() {
    for direct_true in [false, true] {
        let mut register_map = HashMap::default();
        register_map.insert(RegisterId(0), bit(1));
        for reg in 1..=2 {
            register_map.insert(RegisterId(reg), bit(8));
        }
        let mut blocks = HashMap::default();
        insert_block(
            &mut blocks,
            0,
            vec![RegisterId(0), RegisterId(1)],
            vec![SIRInstruction::Binary(
                RegisterId(2),
                RegisterId(1),
                BinaryOp::Mul,
                RegisterId(1),
            )],
            SIRTerminator::Branch {
                cond: RegisterId(0),
                true_block: (BlockId(if direct_true { 1 } else { 2 }), vec![]),
                false_block: (BlockId(if direct_true { 2 } else { 1 }), vec![]),
            },
        );
        insert_block(
            &mut blocks,
            1,
            vec![],
            vec![SIRInstruction::Store(
                address(80),
                SIROffset::Static(0),
                8,
                RegisterId(2),
                vec![],
                vec![],
            )],
            SIRTerminator::Jump(BlockId(3), vec![]),
        );
        insert_block(
            &mut blocks,
            2,
            vec![],
            vec![],
            SIRTerminator::Jump(BlockId(3), vec![]),
        );
        // Both outcomes reach block 5, one directly and one through block 4.
        // The final use is unconditional for either direct-edge orientation.
        insert_block(
            &mut blocks,
            3,
            vec![],
            vec![],
            SIRTerminator::Branch {
                cond: RegisterId(0),
                true_block: (BlockId(if direct_true { 5 } else { 4 }), vec![]),
                false_block: (BlockId(if direct_true { 4 } else { 5 }), vec![]),
            },
        );
        insert_block(
            &mut blocks,
            4,
            vec![],
            vec![],
            SIRTerminator::Jump(BlockId(5), vec![]),
        );
        insert_block(
            &mut blocks,
            5,
            vec![],
            vec![SIRInstruction::Store(
                address(81),
                SIROffset::Static(0),
                8,
                RegisterId(2),
                vec![],
                vec![],
            )],
            SIRTerminator::Return,
        );
        let mut eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        };
        eu.verify_result().unwrap();
        let before = eu.clone();
        sink_pure_values_with_predicate_repair(&mut eu);
        eu.verify_result().unwrap();
        for condition in [0, 1] {
            assert_eq!(execute(&before, condition, 7), execute(&eu, condition, 7));
        }
    }
}

#[test]
fn switch_arm_does_not_prove_shared_payload_is_unobservable_on_other_arms() {
    // Both the selected case and the post-switch store need the square.
    // Their common outer guard does not imply that this case was taken;
    // the post-switch store must retain the square on the default edge.
    let mut register_map = HashMap::default();
    register_map.insert(RegisterId(0), bit(1));
    for reg in 1..=4 {
        register_map.insert(RegisterId(reg), bit(8));
    }
    let mut blocks = HashMap::default();
    insert_block(
        &mut blocks,
        0,
        vec![RegisterId(0), RegisterId(1), RegisterId(2)],
        vec![
            SIRInstruction::Binary(RegisterId(3), RegisterId(1), BinaryOp::Mul, RegisterId(1)),
            SIRInstruction::Imm(RegisterId(4), SIRValue::new(0u8)),
        ],
        SIRTerminator::Branch {
            cond: RegisterId(0),
            true_block: (BlockId(1), vec![]),
            false_block: (BlockId(5), vec![]),
        },
    );
    insert_block(
        &mut blocks,
        1,
        vec![],
        vec![],
        SIRTerminator::Switch {
            selector: RegisterId(2),
            cases: vec![SIRSwitchCase {
                value: 0u8.into(),
                target: BlockId(2),
            }],
            default: BlockId(3),
        },
    );
    for (block, source) in [(2, RegisterId(3)), (3, RegisterId(4)), (4, RegisterId(3))] {
        insert_block(
            &mut blocks,
            block,
            vec![],
            vec![SIRInstruction::Store(
                address(if block == 4 { 81 } else { 80 }),
                SIROffset::Static(0),
                8,
                source,
                vec![],
                vec![],
            )],
            SIRTerminator::Jump(BlockId(if block == 4 { 5 } else { 4 }), vec![]),
        );
    }
    insert_block(&mut blocks, 5, vec![], vec![], SIRTerminator::Return);
    let mut eu = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks,
        register_map,
    };
    eu.verify_result().unwrap();
    let before = eu.clone();
    sink_pure_values_with_predicate_repair(&mut eu);
    eu.verify_result().unwrap();
    for condition in [0, 1] {
        for input in [0, 7, 255] {
            for selector in [0, 1, 2, 255] {
                assert_eq!(
                    execute_with_inputs(&before, condition, input, selector),
                    execute_with_inputs(&eu, condition, input, selector)
                );
            }
        }
    }
}

#[test]
fn repairs_conditional_availability_across_repeated_priority_regions() {
    let mut register_map = HashMap::default();
    register_map.insert(RegisterId(0), bit(1));
    for reg in 1..=3 {
        register_map.insert(RegisterId(reg), bit(8));
    }
    let mut blocks = HashMap::default();
    insert_block(
        &mut blocks,
        0,
        vec![RegisterId(0), RegisterId(1)],
        vec![
            SIRInstruction::Binary(RegisterId(2), RegisterId(1), BinaryOp::Mul, RegisterId(1)),
            SIRInstruction::Imm(RegisterId(3), SIRValue::new(0u8)),
        ],
        SIRTerminator::Branch {
            cond: RegisterId(0),
            true_block: (BlockId(1), Vec::new()),
            false_block: (BlockId(2), Vec::new()),
        },
    );
    for (id, source) in [(1, RegisterId(3)), (2, RegisterId(2))] {
        insert_block(
            &mut blocks,
            id,
            Vec::new(),
            vec![SIRInstruction::Store(
                address(80),
                SIROffset::Static(0),
                8,
                source,
                Vec::new(),
                Vec::new(),
            )],
            SIRTerminator::Jump(BlockId(3), Vec::new()),
        );
    }
    insert_block(
        &mut blocks,
        3,
        Vec::new(),
        Vec::new(),
        SIRTerminator::Branch {
            cond: RegisterId(0),
            true_block: (BlockId(4), Vec::new()),
            false_block: (BlockId(5), Vec::new()),
        },
    );
    for (id, source) in [(4, RegisterId(3)), (5, RegisterId(2))] {
        insert_block(
            &mut blocks,
            id,
            Vec::new(),
            vec![SIRInstruction::Store(
                address(81),
                SIROffset::Static(0),
                8,
                source,
                Vec::new(),
                Vec::new(),
            )],
            SIRTerminator::Jump(BlockId(6), Vec::new()),
        );
    }
    insert_block(
        &mut blocks,
        6,
        Vec::new(),
        Vec::new(),
        SIRTerminator::Return,
    );
    let mut eu = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks,
        register_map,
    };
    eu.verify_result().unwrap();
    let before = eu.clone();

    sink_pure_values_with_predicate_repair(&mut eu);

    eu.verify_result().unwrap();
    assert_eq!(execute(&before, 0, 7), execute(&eu, 0, 7));
    assert_eq!(execute(&before, 1, 7), execute(&eu, 1, 7));
    assert!(
        !eu.blocks[&BlockId(0)]
            .instructions
            .iter()
            .any(|instruction| matches!(instruction, SIRInstruction::Binary(RegisterId(2), ..)))
    );
    assert!(
        eu.blocks[&BlockId(2)]
            .instructions
            .iter()
            .any(|instruction| matches!(instruction, SIRInstruction::Binary(RegisterId(2), ..)))
    );
    assert_eq!(eu.blocks[&BlockId(3)].params.len(), 2);
    assert!(matches!(
        eu.blocks[&BlockId(5)].instructions.last(),
        Some(SIRInstruction::Store(_, _, 8, source, _, _)) if *source != RegisterId(2)
    ));
}

#[test]
fn prunes_priority_phi_payloads_on_early_exit_edges() {
    let mut register_map = HashMap::default();
    for reg in [0, 1, 4, 5] {
        register_map.insert(RegisterId(reg), bit(1));
    }
    for reg in [2, 3, 6] {
        register_map.insert(RegisterId(reg), bit(8));
    }
    let mut blocks = HashMap::default();
    insert_block(
        &mut blocks,
        0,
        vec![RegisterId(0), RegisterId(1), RegisterId(2)],
        vec![SIRInstruction::Binary(
            RegisterId(3),
            RegisterId(2),
            BinaryOp::Mul,
            RegisterId(2),
        )],
        SIRTerminator::Branch {
            cond: RegisterId(0),
            true_block: (BlockId(1), Vec::new()),
            false_block: (BlockId(2), Vec::new()),
        },
    );
    insert_block(
        &mut blocks,
        1,
        Vec::new(),
        Vec::new(),
        SIRTerminator::Jump(
            BlockId(5),
            vec![RegisterId(0), RegisterId(1), RegisterId(3)],
        ),
    );
    insert_block(
        &mut blocks,
        2,
        Vec::new(),
        Vec::new(),
        SIRTerminator::Branch {
            cond: RegisterId(1),
            true_block: (BlockId(3), Vec::new()),
            false_block: (BlockId(4), Vec::new()),
        },
    );
    for id in [3, 4] {
        insert_block(
            &mut blocks,
            id,
            Vec::new(),
            Vec::new(),
            SIRTerminator::Jump(
                BlockId(5),
                vec![RegisterId(0), RegisterId(1), RegisterId(3)],
            ),
        );
    }
    insert_block(
        &mut blocks,
        5,
        vec![RegisterId(4), RegisterId(5), RegisterId(6)],
        Vec::new(),
        SIRTerminator::Branch {
            cond: RegisterId(4),
            true_block: (BlockId(6), Vec::new()),
            false_block: (BlockId(7), Vec::new()),
        },
    );
    insert_block(
        &mut blocks,
        6,
        Vec::new(),
        Vec::new(),
        SIRTerminator::Jump(BlockId(9), Vec::new()),
    );
    insert_block(
        &mut blocks,
        7,
        Vec::new(),
        Vec::new(),
        SIRTerminator::Branch {
            cond: RegisterId(5),
            true_block: (BlockId(8), Vec::new()),
            false_block: (BlockId(10), Vec::new()),
        },
    );
    insert_block(
        &mut blocks,
        8,
        Vec::new(),
        Vec::new(),
        SIRTerminator::Jump(BlockId(9), Vec::new()),
    );
    insert_block(
        &mut blocks,
        10,
        Vec::new(),
        vec![SIRInstruction::Store(
            address(82),
            SIROffset::Static(0),
            8,
            RegisterId(6),
            Vec::new(),
            Vec::new(),
        )],
        SIRTerminator::Jump(BlockId(9), Vec::new()),
    );
    insert_block(
        &mut blocks,
        9,
        Vec::new(),
        Vec::new(),
        SIRTerminator::Return,
    );
    let mut eu = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks,
        register_map,
    };
    eu.verify_result().unwrap();

    sink_pure_values_with_predicate_repair(&mut eu);

    eu.verify_result().unwrap();
    assert!(
        !eu.blocks[&BlockId(0)]
            .instructions
            .iter()
            .any(|instruction| def_reg(instruction) == Some(RegisterId(3)))
    );
    assert!(
        eu.blocks[&BlockId(4)]
            .instructions
            .iter()
            .any(|instruction| def_reg(instruction) == Some(RegisterId(3)))
    );
    for early_exit in [BlockId(1), BlockId(3)] {
        let SIRTerminator::Jump(_, arguments) = &eu.blocks[&early_exit].terminator else {
            unreachable!()
        };
        assert_ne!(arguments[2], RegisterId(3));
    }
    let SIRTerminator::Jump(_, arguments) = &eu.blocks[&BlockId(4)].terminator else {
        unreachable!()
    };
    assert_eq!(arguments[2], RegisterId(3));
}

#[test]
fn block_id_overflow_is_non_destructive() {
    let mut eu = shared_dag_unit();
    let terminal = eu.blocks.remove(&BlockId(3)).unwrap();
    let max = BlockId(usize::MAX);
    eu.blocks.insert(
        max,
        BasicBlock {
            id: max,
            ..terminal
        },
    );
    for id in [BlockId(1), BlockId(2)] {
        eu.blocks.get_mut(&id).unwrap().terminator = SIRTerminator::Jump(max, Vec::new());
    }
    eu.verify_result().unwrap();
    let before = eu.clone();

    GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

    assert_unchanged(&before, &eu);
}

#[test]
fn native_block_id_overflow_is_non_destructive() {
    let mut eu = shared_dag_unit();
    let terminal = eu.blocks.remove(&BlockId(3)).unwrap();
    let max = BlockId(u32::MAX as usize);
    eu.blocks.insert(
        max,
        BasicBlock {
            id: max,
            ..terminal
        },
    );
    for id in [BlockId(1), BlockId(2)] {
        eu.blocks.get_mut(&id).unwrap().terminator = SIRTerminator::Jump(max, Vec::new());
    }
    eu.verify_result().unwrap();
    let before = eu.clone();

    GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

    assert_unchanged(&before, &eu);
}
