use super::*;
use crate::ir::{BasicBlock, BlockId, InstanceId, SIRTerminator, STABLE_REGION};
use celox_design::StateObjectId as VarId;

fn test_addr() -> RegionedAbsoluteAddr {
    RegionedAbsoluteAddr {
        region: STABLE_REGION,
        instance_id: InstanceId(0),
        var_id: VarId::default(),
    }
}

fn make_eu(
    instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
    register_map: HashMap<RegisterId, RegisterType>,
) -> ExecutionUnit<RegionedAbsoluteAddr> {
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

fn packed_conditional_store_chain_fixture(
    stored_value: bool,
) -> ExecutionUnit<RegionedAbsoluteAddr> {
    const LANES: usize = 8;
    let logic = |width| RegisterType::Logic { width };
    let constant = RegisterId(0);
    let predicates = (0..LANES)
        .map(|lane| RegisterId(1 + lane))
        .collect::<Vec<_>>();
    let conditions = (0..LANES)
        .map(|lane| RegisterId(1 + LANES + lane))
        .collect::<Vec<_>>();
    let mut register_map = [(constant, logic(1))]
        .into_iter()
        .chain(
            predicates
                .iter()
                .copied()
                .map(|register| (register, logic(1))),
        )
        .chain(conditions.iter().copied().map(|register| {
            (
                register,
                RegisterType::Bit {
                    width: 1,
                    signed: false,
                },
            )
        }))
        .collect::<HashMap<_, _>>();
    let mut blocks = HashMap::default();

    for lane in 0..LANES {
        let head = BlockId(lane * 3);
        let store_arm = BlockId(lane * 3 + 1);
        let empty_arm = BlockId(lane * 3 + 2);
        let merge = BlockId(lane * 3 + 3);
        let mut instructions = Vec::new();
        if lane == 0 {
            instructions.push(SIRInstruction::Imm(
                constant,
                SIRValue::new(u8::from(stored_value)),
            ));
            for (index, &predicate) in predicates.iter().enumerate() {
                instructions.push(SIRInstruction::Imm(
                    predicate,
                    SIRValue::new((index & 1) as u8),
                ));
            }
        }
        instructions.push(SIRInstruction::Unary(
            conditions[lane],
            UnaryOp::ToTwoState,
            predicates[lane],
        ));
        blocks.insert(
            head,
            BasicBlock {
                id: head,
                params: Vec::new(),
                instructions,
                terminator: SIRTerminator::Branch {
                    cond: conditions[lane],
                    true_block: (store_arm, Vec::new()),
                    false_block: (empty_arm, Vec::new()),
                },
            },
        );
        blocks.insert(
            store_arm,
            BasicBlock {
                id: store_arm,
                params: Vec::new(),
                instructions: vec![SIRInstruction::Store(
                    test_addr(),
                    SIROffset::Static(lane),
                    1,
                    constant,
                    Vec::new(),
                    Vec::new(),
                )],
                terminator: SIRTerminator::Jump(merge, Vec::new()),
            },
        );
        blocks.insert(
            empty_arm,
            BasicBlock {
                id: empty_arm,
                params: Vec::new(),
                instructions: Vec::new(),
                terminator: SIRTerminator::Jump(merge, Vec::new()),
            },
        );
    }
    let endpoint = BlockId(LANES * 3);
    blocks.insert(
        endpoint,
        BasicBlock {
            id: endpoint,
            params: Vec::new(),
            instructions: Vec::new(),
            terminator: SIRTerminator::Return,
        },
    );
    // Keep the mutable binding intentional: adding generated result types
    // in the test below must not depend on a fixed register ID.
    register_map.shrink_to_fit();
    ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks,
        register_map,
    }
}

#[test]
fn collapses_serial_conditional_zero_stores_to_one_packed_rmw() {
    let mut eu = packed_conditional_store_chain_fixture(false);
    assert_eq!(eu.verify_result(), Ok(()));

    assert_eq!(
        collapse_packed_conditional_store_chains_with(&mut eu, |_, _, _| true),
        1
    );
    assert_eq!(eu.verify_result(), Ok(()));
    assert_eq!(eu.blocks.len(), 2);
    assert_eq!(
        eu.blocks[&BlockId(0)].terminator,
        SIRTerminator::Jump(BlockId(24), Vec::new())
    );

    let instructions = &eu.blocks[&BlockId(0)].instructions;
    assert!(instructions.iter().any(|instruction| {
        matches!(
            instruction,
            SIRInstruction::Concat(_, arguments)
                if arguments
                    == &(1..=8)
                        .rev()
                        .map(RegisterId)
                        .collect::<Vec<_>>()
        )
    }));
    assert!(instructions.iter().any(|instruction| {
        matches!(
            instruction,
            SIRInstruction::Load(
                _,
                address,
                SIROffset::PackedElements {
                    bit_offset: 0,
                    element_width: 1,
                },
                8,
            ) if *address == test_addr()
        )
    }));
    assert!(instructions.iter().any(|instruction| {
        matches!(
            instruction,
            SIRInstruction::Store(
                address,
                SIROffset::PackedElements {
                    bit_offset: 0,
                    element_width: 1,
                },
                8,
                _,
                triggers,
                captures,
            ) if *address == test_addr() && triggers.is_empty() && captures.is_empty()
        )
    }));
    assert!(instructions.iter().any(|instruction| {
        matches!(instruction, SIRInstruction::Binary(_, _, BinaryOp::And, _))
    }));
}

#[test]
fn collapses_serial_conditional_one_stores_to_packed_or() {
    let mut eu = packed_conditional_store_chain_fixture(true);
    assert_eq!(
        collapse_packed_conditional_store_chains_with(&mut eu, |_, _, _| true),
        1
    );
    assert_eq!(eu.verify_result(), Ok(()));
    let instructions = &eu.blocks[&BlockId(0)].instructions;
    assert!(instructions.iter().any(|instruction| {
        matches!(instruction, SIRInstruction::Binary(_, _, BinaryOp::Or, _))
    }));
    assert!(!instructions.iter().any(|instruction| {
        matches!(instruction, SIRInstruction::Unary(_, UnaryOp::BitNot, _))
    }));
}

#[test]
fn keeps_conditional_zero_store_chain_when_layout_is_not_packed() {
    let mut eu = packed_conditional_store_chain_fixture(false);
    assert_eq!(
        collapse_packed_conditional_store_chains_with(&mut eu, |_, _, _| false),
        0
    );
    assert_eq!(eu.blocks.len(), 25);
    assert_eq!(eu.verify_result(), Ok(()));
}

#[test]
fn packs_byte_enable_mux_lanes_into_one_word_blend() {
    let unsigned = |width| RegisterType::Bit {
        width,
        signed: false,
    };
    let mut register_map = [
        (RegisterId(0), unsigned(8)),
        (RegisterId(1), unsigned(64)),
        (RegisterId(2), unsigned(64)),
    ]
    .into_iter()
    .collect::<HashMap<_, _>>();
    let mut instructions = Vec::new();
    let mut lanes = Vec::new();
    let mut next = 3usize;
    for lane in 0..8 {
        let condition = RegisterId(next);
        let then_lane = RegisterId(next + 1);
        let else_lane = RegisterId(next + 2);
        let result = RegisterId(next + 3);
        next += 4;
        register_map.insert(condition, unsigned(1));
        register_map.insert(then_lane, unsigned(8));
        register_map.insert(else_lane, unsigned(8));
        register_map.insert(result, unsigned(8));
        instructions.push(SIRInstruction::Slice(condition, RegisterId(0), lane, 1));
        instructions.push(SIRInstruction::Slice(then_lane, RegisterId(1), lane * 8, 8));
        instructions.push(SIRInstruction::Slice(else_lane, RegisterId(2), lane * 8, 8));
        instructions.push(SIRInstruction::Mux(result, condition, then_lane, else_lane));
        lanes.push(result);
    }
    lanes.reverse();
    let output = RegisterId(next);
    register_map.insert(output, unsigned(64));
    instructions.push(SIRInstruction::Concat(output, lanes));
    instructions.push(SIRInstruction::RuntimeEvent {
        site_id: 0,
        args: vec![output],
    });

    let mut eu = make_eu(instructions, register_map);
    eu.blocks.get_mut(&BlockId(0)).unwrap().params =
        vec![RegisterId(0), RegisterId(1), RegisterId(2)];
    VectorizeConcatPass::default().run(&mut eu, &PassOptions::default());
    eu.verify();

    let instructions = &eu.blocks[&BlockId(0)].instructions;
    assert!(!instructions.iter().any(|instruction| matches!(
        instruction,
        SIRInstruction::Concat(..) | SIRInstruction::Slice(..) | SIRInstruction::Mux(..)
    )));
    assert_eq!(
        instructions
            .iter()
            .filter(|instruction| matches!(
                instruction,
                SIRInstruction::Binary(_, _, BinaryOp::Mul, _)
            ))
            .count(),
        1
    );
    assert_eq!(
        instructions
            .iter()
            .filter(|instruction| matches!(
                instruction,
                SIRInstruction::Binary(_, _, BinaryOp::Xor, _)
            ))
            .count(),
        2
    );

    for enable in 0u64..=u8::MAX.into() {
        let spread28 = (enable | (enable << 28)) & 0x0000_000f_0000_000f;
        let spread14 = (spread28 | (spread28 << 14)) & 0x0003_0003_0003_0003;
        let spread7 = (spread14 | (spread14 << 7)) & 0x0101_0101_0101_0101;
        let actual = spread7 * 0xff;
        let expected = (0..8).fold(0u64, |mask, lane| {
            mask | if enable & (1 << lane) != 0 {
                0xff << (lane * 8)
            } else {
                0
            }
        });
        assert_eq!(actual, expected);
    }
}

#[test]
fn folds_mixed_width_contiguous_slices_back_to_one_slice() {
    let unsigned = |width| RegisterType::Bit {
        width,
        signed: false,
    };
    let register_map = [
        (RegisterId(0), unsigned(16)),
        (RegisterId(1), unsigned(3)),
        (RegisterId(2), unsigned(5)),
        (RegisterId(3), unsigned(8)),
    ]
    .into_iter()
    .collect();
    let mut eu = make_eu(
        vec![
            SIRInstruction::Slice(RegisterId(1), RegisterId(0), 3, 3),
            SIRInstruction::Slice(RegisterId(2), RegisterId(0), 6, 5),
            SIRInstruction::Concat(RegisterId(3), vec![RegisterId(2), RegisterId(1)]),
            SIRInstruction::RuntimeEvent {
                site_id: 0,
                args: vec![RegisterId(3)],
            },
        ],
        register_map,
    );
    eu.blocks.get_mut(&BlockId(0)).unwrap().params = vec![RegisterId(0)];

    VectorizeConcatPass::default().run(&mut eu, &PassOptions::default());

    eu.verify();
    assert_eq!(
        eu.blocks[&BlockId(0)].instructions,
        [
            SIRInstruction::Slice(RegisterId(3), RegisterId(0), 3, 8),
            SIRInstruction::RuntimeEvent {
                site_id: 0,
                args: vec![RegisterId(3)],
            },
        ]
    );
}

#[test]
fn grouped_high_bit_extract_uses_wide_enough_shift_amount() {
    let mut register_map = HashMap::default();
    register_map.insert(
        RegisterId(0),
        RegisterType::Bit {
            width: 64,
            signed: false,
        },
    );
    for reg in 1..=5 {
        register_map.insert(
            RegisterId(reg),
            RegisterType::Bit {
                width: 1,
                signed: false,
            },
        );
    }
    register_map.insert(
        RegisterId(6),
        RegisterType::Bit {
            width: 5,
            signed: false,
        },
    );
    let mut eu = make_eu(
        vec![
            SIRInstruction::Slice(RegisterId(1), RegisterId(0), 42, 1),
            SIRInstruction::Slice(RegisterId(2), RegisterId(0), 41, 1),
            SIRInstruction::Slice(RegisterId(3), RegisterId(0), 40, 1),
            SIRInstruction::Slice(RegisterId(4), RegisterId(0), 43, 1),
            SIRInstruction::Imm(RegisterId(5), SIRValue::new(0u8)),
            SIRInstruction::Concat(
                RegisterId(6),
                vec![
                    RegisterId(4),
                    RegisterId(1),
                    RegisterId(2),
                    RegisterId(3),
                    RegisterId(5),
                ],
            ),
            SIRInstruction::RuntimeEvent {
                site_id: 0,
                args: vec![RegisterId(6)],
            },
        ],
        register_map,
    );
    eu.blocks.get_mut(&BlockId(0)).unwrap().params = vec![RegisterId(0)];

    VectorizeConcatPass::default().run(&mut eu, &PassOptions::default());

    eu.verify();
    assert!(eu.blocks[&BlockId(0)].instructions.iter().any(|inst| {
        let SIRInstruction::Imm(dst, value) = inst else {
            return false;
        };
        value.payload == BigUint::from(40u8) && eu.register_map[dst].width() >= 6
    }));
}

#[test]
fn sign_extend_concat_from_shifted_low_msb() {
    let mut register_map = HashMap::default();
    register_map.insert(
        RegisterId(0),
        RegisterType::Bit {
            width: 8,
            signed: false,
        },
    );
    register_map.insert(
        RegisterId(1),
        RegisterType::Bit {
            width: 16,
            signed: false,
        },
    );
    for reg in 2..=5 {
        register_map.insert(
            RegisterId(reg),
            RegisterType::Bit {
                width: 1,
                signed: false,
            },
        );
    }

    let instructions = vec![
        SIRInstruction::Imm(RegisterId(2), SIRValue::new(7u64)),
        SIRInstruction::Binary(RegisterId(3), RegisterId(0), BinaryOp::Shr, RegisterId(2)),
        SIRInstruction::Imm(RegisterId(4), SIRValue::new(1u64)),
        SIRInstruction::Binary(RegisterId(5), RegisterId(3), BinaryOp::And, RegisterId(4)),
        SIRInstruction::Concat(
            RegisterId(1),
            vec![
                RegisterId(5),
                RegisterId(5),
                RegisterId(5),
                RegisterId(5),
                RegisterId(5),
                RegisterId(5),
                RegisterId(5),
                RegisterId(5),
                RegisterId(0),
            ],
        ),
        SIRInstruction::RuntimeEvent {
            site_id: 0,
            args: vec![RegisterId(1)],
        },
    ];

    let mut eu = make_eu(instructions, register_map);
    VectorizeConcatPass::default().run(&mut eu, &PassOptions::default());
    let block = eu.blocks.get(&BlockId(0)).unwrap();

    assert!(block.instructions.iter().any(|inst| matches!(
        inst,
        SIRInstruction::Binary(RegisterId(1), _, BinaryOp::Sar, _)
    )));
    assert!(
        !block
            .instructions
            .iter()
            .any(|inst| matches!(inst, SIRInstruction::Concat(..)))
    );
}

#[test]
fn sign_extend_concat_from_masked_low_part() {
    let mut register_map = HashMap::default();
    for (reg, width) in [
        (0, 64),
        (1, 64),
        (2, 8),
        (3, 64),
        (4, 1),
        (5, 1),
        (6, 8),
        (7, 8),
    ] {
        register_map.insert(
            RegisterId(reg),
            RegisterType::Bit {
                width,
                signed: false,
            },
        );
    }

    let mut concat_args = vec![RegisterId(5); 56];
    concat_args.push(RegisterId(7));
    let instructions = vec![
        SIRInstruction::Imm(RegisterId(2), SIRValue::new(7u64)),
        SIRInstruction::Binary(RegisterId(3), RegisterId(0), BinaryOp::Shr, RegisterId(2)),
        SIRInstruction::Imm(RegisterId(4), SIRValue::new(1u64)),
        SIRInstruction::Binary(RegisterId(5), RegisterId(3), BinaryOp::And, RegisterId(4)),
        SIRInstruction::Imm(RegisterId(6), SIRValue::new(0xffu64)),
        SIRInstruction::Binary(RegisterId(7), RegisterId(0), BinaryOp::And, RegisterId(6)),
        SIRInstruction::Concat(RegisterId(1), concat_args),
        SIRInstruction::RuntimeEvent {
            site_id: 0,
            args: vec![RegisterId(1)],
        },
    ];

    let mut eu = make_eu(instructions, register_map);
    VectorizeConcatPass::default().run(&mut eu, &PassOptions::default());
    let block = eu.blocks.get(&BlockId(0)).unwrap();

    assert!(block.instructions.iter().any(|inst| matches!(
        inst,
        SIRInstruction::Binary(RegisterId(1), _, BinaryOp::Sar, _)
    )));
    assert!(
        !block
            .instructions
            .iter()
            .any(|inst| matches!(inst, SIRInstruction::Concat(..)))
    );
}

#[test]
fn sign_extend_concat_from_load_msb() {
    let addr = test_addr();
    let mut register_map = HashMap::default();
    register_map.insert(
        RegisterId(0),
        RegisterType::Bit {
            width: 8,
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
    register_map.insert(
        RegisterId(2),
        RegisterType::Bit {
            width: 16,
            signed: false,
        },
    );

    let instructions = vec![
        SIRInstruction::Load(RegisterId(0), addr, SIROffset::Static(4), 8),
        SIRInstruction::Load(RegisterId(1), addr, SIROffset::Static(11), 1),
        SIRInstruction::Concat(
            RegisterId(2),
            vec![
                RegisterId(1),
                RegisterId(1),
                RegisterId(1),
                RegisterId(1),
                RegisterId(1),
                RegisterId(1),
                RegisterId(1),
                RegisterId(1),
                RegisterId(0),
            ],
        ),
        SIRInstruction::RuntimeEvent {
            site_id: 0,
            args: vec![RegisterId(2)],
        },
    ];

    let mut eu = make_eu(instructions, register_map);
    VectorizeConcatPass::default().run(&mut eu, &PassOptions::default());
    let block = eu.blocks.get(&BlockId(0)).unwrap();

    assert!(block.instructions.iter().any(|inst| matches!(
        inst,
        SIRInstruction::Binary(RegisterId(2), _, BinaryOp::Sar, _)
    )));
}

#[test]
fn broadcast_bit_uses_word_select_instead_of_shift_sign_extension() {
    const WIDTH: usize = 32;
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
            width: WIDTH,
            signed: false,
        },
    );
    let instructions = vec![
        SIRInstruction::Concat(RegisterId(1), vec![RegisterId(0); WIDTH]),
        SIRInstruction::RuntimeEvent {
            site_id: 0,
            args: vec![RegisterId(1)],
        },
    ];

    let mut eu = make_eu(instructions, register_map);
    eu.blocks.get_mut(&BlockId(0)).unwrap().params = vec![RegisterId(0)];
    VectorizeConcatPass::default().run(&mut eu, &PassOptions::default());
    eu.verify();

    let instructions = &eu.blocks[&BlockId(0)].instructions;
    assert!(instructions.iter().any(|inst| matches!(
        inst,
        SIRInstruction::Mux(RegisterId(1), RegisterId(0), _, _)
    )));
    assert!(!instructions.iter().any(|inst| matches!(
        inst,
        SIRInstruction::Binary(_, _, BinaryOp::Shl | BinaryOp::Sar, _) | SIRInstruction::Concat(..)
    )));
}

#[test]
fn lifts_concat_of_bitwise_and_lanes() {
    let mut register_map = HashMap::default();
    register_map.insert(
        RegisterId(0),
        RegisterType::Bit {
            width: 8,
            signed: false,
        },
    );
    register_map.insert(
        RegisterId(1),
        RegisterType::Bit {
            width: 8,
            signed: false,
        },
    );
    for reg in 2..=10 {
        register_map.insert(
            RegisterId(reg),
            RegisterType::Bit {
                width: 1,
                signed: false,
            },
        );
    }
    register_map.insert(
        RegisterId(11),
        RegisterType::Bit {
            width: 3,
            signed: false,
        },
    );

    let instructions = vec![
        SIRInstruction::Slice(RegisterId(2), RegisterId(0), 0, 1),
        SIRInstruction::Slice(RegisterId(3), RegisterId(0), 1, 1),
        SIRInstruction::Slice(RegisterId(4), RegisterId(0), 2, 1),
        SIRInstruction::Slice(RegisterId(5), RegisterId(1), 0, 1),
        SIRInstruction::Slice(RegisterId(6), RegisterId(1), 1, 1),
        SIRInstruction::Slice(RegisterId(7), RegisterId(1), 2, 1),
        SIRInstruction::Binary(RegisterId(8), RegisterId(2), BinaryOp::And, RegisterId(5)),
        SIRInstruction::Binary(RegisterId(9), RegisterId(3), BinaryOp::And, RegisterId(6)),
        SIRInstruction::Binary(RegisterId(10), RegisterId(4), BinaryOp::And, RegisterId(7)),
        SIRInstruction::Concat(
            RegisterId(11),
            vec![RegisterId(10), RegisterId(9), RegisterId(8)],
        ),
        SIRInstruction::RuntimeEvent {
            site_id: 0,
            args: vec![RegisterId(11)],
        },
    ];

    let mut eu = make_eu(instructions, register_map);
    VectorizeConcatPass::default().run(&mut eu, &PassOptions::default());
    let block = eu.blocks.get(&BlockId(0)).unwrap();

    let Some(SIRInstruction::Binary(RegisterId(11), lhs_vec, BinaryOp::And, rhs_vec)) =
        block.instructions.iter().find(|inst| {
            matches!(
                inst,
                SIRInstruction::Binary(RegisterId(11), _, BinaryOp::And, _)
            )
        })
    else {
        panic!("expected lifted word And");
    };

    assert!(block.instructions.iter().any(|inst| {
        matches!(
            inst,
            SIRInstruction::Unary(dst, UnaryOp::Ident, RegisterId(0)) if *dst == *lhs_vec
        )
    }));
    assert!(block.instructions.iter().any(|inst| {
        matches!(
            inst,
            SIRInstruction::Unary(dst, UnaryOp::Ident, RegisterId(1)) if *dst == *rhs_vec
        )
    }));
    assert!(!block.instructions.iter().any(|inst| {
        matches!(
            inst,
            SIRInstruction::Concat(RegisterId(11), args)
                if args == &vec![RegisterId(10), RegisterId(9), RegisterId(8)]
        )
    }));
}

#[test]
fn recursively_packs_deep_lane_boolean_dag_to_fixed_point() {
    const LANES: usize = 8;
    const SOURCES: usize = 6;

    let bit = |width| RegisterType::Bit {
        width,
        signed: false,
    };
    let mut register_map = HashMap::default();
    for source in 0..SOURCES {
        register_map.insert(RegisterId(source), bit(LANES));
    }

    let mut next_reg = SOURCES;
    let mut instructions = Vec::new();
    let mut lane_results = Vec::new();
    for lane in 0..LANES {
        let mut extracted = Vec::new();
        for source in 0..SOURCES {
            let dst = RegisterId(next_reg);
            next_reg += 1;
            register_map.insert(dst, bit(1));
            instructions.push(SIRInstruction::Slice(dst, RegisterId(source), lane, 1));
            extracted.push(dst);
        }

        let mut negate = |source: RegisterId| {
            let dst = RegisterId(next_reg);
            next_reg += 1;
            register_map.insert(dst, bit(1));
            instructions.push(SIRInstruction::Unary(dst, UnaryOp::LogicNot, source));
            dst
        };
        let not_2 = negate(extracted[2]);
        let not_4 = negate(extracted[4]);
        let not_5 = negate(extracted[5]);

        let mut and = |lhs: RegisterId, rhs: RegisterId| {
            let dst = RegisterId(next_reg);
            next_reg += 1;
            register_map.insert(dst, bit(1));
            instructions.push(SIRInstruction::Binary(dst, lhs, BinaryOp::LogicAnd, rhs));
            dst
        };
        let value = and(extracted[0], extracted[1]);
        let value = and(value, not_2);
        let value = and(value, extracted[3]);
        let value = and(value, not_4);
        lane_results.push(and(value, not_5));
    }

    lane_results.reverse();
    let result = RegisterId(next_reg);
    register_map.insert(result, bit(LANES));
    instructions.push(SIRInstruction::Concat(result, lane_results));
    instructions.push(SIRInstruction::RuntimeEvent {
        site_id: 0,
        args: vec![result],
    });

    let mut eu = make_eu(instructions, register_map);
    eu.blocks.get_mut(&BlockId(0)).unwrap().params = (0..SOURCES).map(RegisterId).collect();
    VectorizeConcatPass::default().run(&mut eu, &PassOptions::default());
    eu.verify();

    let instructions = &eu.blocks[&BlockId(0)].instructions;
    assert!(
        !instructions
            .iter()
            .any(|inst| matches!(inst, SIRInstruction::Concat(..) | SIRInstruction::Slice(..)))
    );
    assert_eq!(
        instructions
            .iter()
            .filter(|inst| matches!(inst, SIRInstruction::Unary(_, UnaryOp::BitNot, _)))
            .count(),
        3
    );
    assert_eq!(
        instructions
            .iter()
            .filter(|inst| matches!(inst, SIRInstruction::Binary(_, _, BinaryOp::And, _)))
            .count(),
        5
    );
    assert!(instructions.iter().any(|inst| {
        matches!(inst, SIRInstruction::Binary(dst, _, BinaryOp::And, _) if *dst == result)
    }));
}

#[test]
fn packs_profitable_boolean_dag_above_opaque_comparisons() {
    const LANES: usize = 8;
    const DEPTH: usize = 12;

    let bit = |width| RegisterType::Bit {
        width,
        signed: false,
    };
    let mut register_map = HashMap::default();
    let common = RegisterId(0);
    register_map.insert(common, bit(1));
    for lane in 0..LANES {
        register_map.insert(RegisterId(1 + lane * 2), bit(8));
        register_map.insert(RegisterId(2 + lane * 2), bit(8));
    }

    let mut next_reg = 1 + LANES * 2;
    let mut instructions = Vec::new();
    let mut lane_results = Vec::new();
    for lane in 0..LANES {
        let compare = RegisterId(next_reg);
        next_reg += 1;
        register_map.insert(compare, bit(1));
        instructions.push(SIRInstruction::Binary(
            compare,
            RegisterId(1 + lane * 2),
            BinaryOp::Eq,
            RegisterId(2 + lane * 2),
        ));

        let mut value = compare;
        for depth in 0..DEPTH {
            let next = RegisterId(next_reg);
            next_reg += 1;
            register_map.insert(next, bit(1));
            instructions.push(SIRInstruction::Binary(
                next,
                value,
                if depth % 2 == 0 {
                    BinaryOp::LogicAnd
                } else {
                    BinaryOp::LogicOr
                },
                common,
            ));
            value = next;
        }
        lane_results.push(value);
    }

    lane_results.reverse();
    let result = RegisterId(next_reg);
    register_map.insert(result, bit(LANES));
    instructions.push(SIRInstruction::Concat(result, lane_results));
    instructions.push(SIRInstruction::RuntimeEvent {
        site_id: 0,
        args: vec![result],
    });

    let mut eu = make_eu(instructions, register_map);
    eu.blocks.get_mut(&BlockId(0)).unwrap().params = (0..1 + LANES * 2).map(RegisterId).collect();
    VectorizeConcatPass::default().run(&mut eu, &PassOptions::default());
    eu.verify();

    let instructions = &eu.blocks[&BlockId(0)].instructions;
    assert_eq!(
        instructions
            .iter()
            .filter(|inst| matches!(inst, SIRInstruction::Binary(_, _, BinaryOp::Eq, _)))
            .count(),
        LANES
    );
    assert_eq!(
        instructions
            .iter()
            .filter(|inst| matches!(
                inst,
                SIRInstruction::Binary(
                    _,
                    _,
                    BinaryOp::And | BinaryOp::Or | BinaryOp::LogicAnd | BinaryOp::LogicOr,
                    _
                )
            ))
            .count(),
        DEPTH,
        "{instructions:#?}"
    );
    assert_eq!(
        instructions
            .iter()
            .filter(|inst| matches!(inst, SIRInstruction::Concat(..)))
            .count(),
        1,
        "only the profitable opaque comparison pack should remain"
    );
}

#[test]
fn packs_very_deep_lane_unary_dag_without_recursion() {
    const LANES: usize = 3;
    const DEPTH: usize = 8_192;

    let bit = |width| RegisterType::Bit {
        width,
        signed: false,
    };
    let mut register_map = HashMap::default();
    register_map.insert(RegisterId(0), bit(LANES));
    let mut next_reg = 1usize;
    let mut instructions = Vec::new();
    let mut lane_results = Vec::new();
    for lane in 0..LANES {
        let mut value = RegisterId(next_reg);
        next_reg += 1;
        register_map.insert(value, bit(1));
        instructions.push(SIRInstruction::Slice(value, RegisterId(0), lane, 1));
        for _ in 0..DEPTH {
            let next = RegisterId(next_reg);
            next_reg += 1;
            register_map.insert(next, bit(1));
            instructions.push(SIRInstruction::Unary(next, UnaryOp::LogicNot, value));
            value = next;
        }
        lane_results.push(value);
    }
    lane_results.reverse();
    let result = RegisterId(next_reg);
    register_map.insert(result, bit(LANES));
    instructions.push(SIRInstruction::Concat(result, lane_results));
    instructions.push(SIRInstruction::RuntimeEvent {
        site_id: 0,
        args: vec![result],
    });

    let mut eu = make_eu(instructions, register_map);
    eu.blocks.get_mut(&BlockId(0)).unwrap().params = vec![RegisterId(0)];
    VectorizeConcatPass::default().run(&mut eu, &PassOptions::default());
    eu.verify();

    let instructions = &eu.blocks[&BlockId(0)].instructions;
    assert_eq!(
        instructions
            .iter()
            .filter(|inst| matches!(inst, SIRInstruction::Unary(_, UnaryOp::BitNot, _)))
            .count(),
        DEPTH
    );
    assert!(
        !instructions
            .iter()
            .any(|inst| matches!(inst, SIRInstruction::Concat(..) | SIRInstruction::Slice(..)))
    );
}

#[test]
fn packs_deep_identity_lanes_without_recursive_source_resolution() {
    const LANES: usize = 3;
    const DEPTH: usize = 20_000;

    let bit = |width| RegisterType::Bit {
        width,
        signed: false,
    };
    let mut register_map = HashMap::default();
    register_map.insert(RegisterId(0), bit(LANES));
    let mut next_reg = 1usize;
    let mut instructions = Vec::new();
    let mut lane_results = Vec::new();
    for lane in 0..LANES {
        let mut value = RegisterId(next_reg);
        next_reg += 1;
        register_map.insert(value, bit(1));
        instructions.push(SIRInstruction::Slice(value, RegisterId(0), lane, 1));
        for _ in 0..DEPTH {
            let next = RegisterId(next_reg);
            next_reg += 1;
            register_map.insert(next, bit(1));
            instructions.push(SIRInstruction::Unary(next, UnaryOp::Ident, value));
            value = next;
        }
        lane_results.push(value);
    }
    lane_results.reverse();
    let result = RegisterId(next_reg);
    register_map.insert(result, bit(LANES));
    instructions.push(SIRInstruction::Concat(result, lane_results));
    instructions.push(SIRInstruction::RuntimeEvent {
        site_id: 0,
        args: vec![result],
    });

    let mut eu = make_eu(instructions, register_map);
    eu.blocks.get_mut(&BlockId(0)).unwrap().params = vec![RegisterId(0)];
    VectorizeConcatPass::default().run(&mut eu, &PassOptions::default());
    eu.verify();

    let instructions = &eu.blocks[&BlockId(0)].instructions;
    assert_eq!(
        instructions
            .iter()
            .filter(|inst| matches!(inst, SIRInstruction::Unary(_, UnaryOp::Ident, _)))
            .count(),
        1
    );
    assert!(matches!(
        instructions.first(),
        Some(SIRInstruction::Unary(dst, UnaryOp::Ident, RegisterId(0))) if *dst == result
    ));
}

#[test]
fn shares_reconvergent_lane_dag_linearly() {
    const LANES: usize = 3;
    const DEPTH: usize = 2_048;

    let bit = |width| RegisterType::Bit {
        width,
        signed: false,
    };
    let mut register_map = HashMap::default();
    register_map.insert(RegisterId(0), bit(LANES));
    let mut next_reg = 1usize;
    let mut instructions = Vec::new();
    let mut lane_results = Vec::new();
    for lane in 0..LANES {
        let mut value = RegisterId(next_reg);
        next_reg += 1;
        register_map.insert(value, bit(1));
        instructions.push(SIRInstruction::Slice(value, RegisterId(0), lane, 1));
        for _ in 0..DEPTH {
            let next = RegisterId(next_reg);
            next_reg += 1;
            register_map.insert(next, bit(1));
            instructions.push(SIRInstruction::Binary(
                next,
                value,
                BinaryOp::LogicAnd,
                value,
            ));
            value = next;
        }
        lane_results.push(value);
    }
    lane_results.reverse();
    let result = RegisterId(next_reg);
    register_map.insert(result, bit(LANES));
    instructions.push(SIRInstruction::Concat(result, lane_results));
    instructions.push(SIRInstruction::RuntimeEvent {
        site_id: 0,
        args: vec![result],
    });

    let mut eu = make_eu(instructions, register_map);
    eu.blocks.get_mut(&BlockId(0)).unwrap().params = vec![RegisterId(0)];
    VectorizeConcatPass::default().run(&mut eu, &PassOptions::default());
    eu.verify();

    let instructions = &eu.blocks[&BlockId(0)].instructions;
    assert_eq!(
        instructions
            .iter()
            .filter(|inst| matches!(inst, SIRInstruction::Binary(_, _, BinaryOp::And, _)))
            .count(),
        DEPTH
    );
    assert!(instructions.len() <= DEPTH + 3, "{instructions:#?}");
}

#[test]
fn rejects_misaligned_shared_lane_product_before_code_growth() {
    // Each lane is a small layered modulo automaton. Following the LHS/RHS
    // edges synchronously represents subset sums modulo all lane moduli;
    // their product has far more tuple states than the sum of scalar
    // states. Structural definition credits must stop that product.
    const MODULI: [usize; 8] = [2, 3, 5, 7, 11, 13, 17, 19];
    const DEPTH: usize = 18;
    let lanes = MODULI.len();
    let bit = |width| RegisterType::Bit {
        width,
        signed: false,
    };
    let mut register_map = HashMap::default();
    register_map.insert(RegisterId(0), bit(lanes));
    let mut next_reg = 1usize;
    let mut instructions = Vec::new();
    let mut roots = Vec::new();

    for (lane, modulus) in MODULI.into_iter().enumerate() {
        let leaf = RegisterId(next_reg);
        next_reg += 1;
        register_map.insert(leaf, bit(1));
        instructions.push(SIRInstruction::Slice(leaf, RegisterId(0), lane, 1));
        let mut previous = vec![leaf; modulus];
        let mut weight = 1usize % modulus;
        for _ in 0..DEPTH {
            let mut current = Vec::with_capacity(modulus);
            for state in 0..modulus {
                let result = RegisterId(next_reg);
                next_reg += 1;
                register_map.insert(result, bit(1));
                instructions.push(SIRInstruction::Binary(
                    result,
                    previous[state],
                    BinaryOp::And,
                    previous[(state + weight) % modulus],
                ));
                current.push(result);
            }
            previous = current;
            weight = (weight * 2) % modulus;
        }
        roots.push(previous[0]);
    }

    roots.reverse();
    let result = RegisterId(next_reg);
    register_map.insert(result, bit(lanes));
    instructions.push(SIRInstruction::Concat(result, roots));
    instructions.push(SIRInstruction::RuntimeEvent {
        site_id: 0,
        args: vec![result],
    });
    let original_instruction_count = instructions.len();

    let mut eu = make_eu(instructions, register_map);
    eu.blocks.get_mut(&BlockId(0)).unwrap().params = vec![RegisterId(0)];
    VectorizeConcatPass::default().run(&mut eu, &PassOptions::default());
    eu.verify();

    let instructions = &eu.blocks[&BlockId(0)].instructions;
    assert_eq!(instructions.len(), original_instruction_count);
    assert!(instructions.iter().any(
        |instruction| matches!(instruction, SIRInstruction::Concat(dst, _) if *dst == result)
    ));
}

#[test]
fn many_independent_lane_roots_emit_linear_code() {
    const ROOTS: usize = 256;
    const LANES: usize = 3;
    const DEPTH: usize = 8;
    let bit = |width| RegisterType::Bit {
        width,
        signed: false,
    };
    let mut register_map = HashMap::default();
    let mut next_reg = ROOTS;
    let mut instructions = Vec::new();
    let mut results = Vec::new();

    for source in 0..ROOTS {
        register_map.insert(RegisterId(source), bit(LANES));
        let mut lane_results = Vec::new();
        for lane in 0..LANES {
            let mut value = RegisterId(next_reg);
            next_reg += 1;
            register_map.insert(value, bit(1));
            instructions.push(SIRInstruction::Slice(value, RegisterId(source), lane, 1));
            for _ in 0..DEPTH {
                let next = RegisterId(next_reg);
                next_reg += 1;
                register_map.insert(next, bit(1));
                instructions.push(SIRInstruction::Unary(next, UnaryOp::LogicNot, value));
                value = next;
            }
            lane_results.push(value);
        }
        lane_results.reverse();
        let result = RegisterId(next_reg);
        next_reg += 1;
        register_map.insert(result, bit(LANES));
        instructions.push(SIRInstruction::Concat(result, lane_results));
        instructions.push(SIRInstruction::RuntimeEvent {
            site_id: source as u32,
            args: vec![result],
        });
        results.push(result);
    }

    let mut eu = make_eu(instructions, register_map);
    eu.blocks.get_mut(&BlockId(0)).unwrap().params = (0..ROOTS).map(RegisterId).collect();
    VectorizeConcatPass::default().run(&mut eu, &PassOptions::default());
    eu.verify();

    let instructions = &eu.blocks[&BlockId(0)].instructions;
    assert!(
        !instructions
            .iter()
            .any(|instruction| matches!(instruction, SIRInstruction::Concat(..)))
    );
    assert_eq!(
        instructions
            .iter()
            .filter(|instruction| matches!(
                instruction,
                SIRInstruction::Unary(_, UnaryOp::BitNot, _)
            ))
            .count(),
        ROOTS * DEPTH
    );
    assert!(instructions.len() <= ROOTS * (DEPTH + 2));
    assert_eq!(results.len(), ROOTS);
}

#[test]
fn mark_sweep_removes_a_deep_dead_chain_in_one_call() {
    const DEPTH: usize = 20_000;

    let bit = RegisterType::Bit {
        width: 1,
        signed: false,
    };
    let mut register_map = HashMap::default();
    let mut instructions = Vec::with_capacity(DEPTH + 2);
    register_map.insert(RegisterId(0), bit.clone());
    instructions.push(SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8)));
    for index in 1..=DEPTH {
        register_map.insert(RegisterId(index), bit.clone());
        instructions.push(SIRInstruction::Unary(
            RegisterId(index),
            UnaryOp::Ident,
            RegisterId(index - 1),
        ));
    }
    instructions.push(SIRInstruction::RuntimeEvent {
        site_id: 0,
        args: Vec::new(),
    });

    let mut eu = make_eu(instructions, register_map);
    remove_dead_definitions(&mut eu);
    eu.verify();
    assert!(matches!(
        eu.blocks[&BlockId(0)].instructions.as_slice(),
        [SIRInstruction::RuntimeEvent { .. }]
    ));
}

#[test]
fn load_based_pack_rejects_an_address_written_by_the_unit() {
    let addr = test_addr();
    let mut register_map = HashMap::default();
    for reg in 0..3 {
        register_map.insert(
            RegisterId(reg),
            RegisterType::Bit {
                width: 1,
                signed: false,
            },
        );
    }
    register_map.insert(
        RegisterId(3),
        RegisterType::Bit {
            width: 3,
            signed: false,
        },
    );
    let mut eu = make_eu(
        vec![
            SIRInstruction::Load(RegisterId(0), addr, SIROffset::Static(0), 1),
            SIRInstruction::Load(RegisterId(1), addr, SIROffset::Static(1), 1),
            SIRInstruction::Load(RegisterId(2), addr, SIROffset::Static(2), 1),
            SIRInstruction::Concat(
                RegisterId(3),
                vec![RegisterId(2), RegisterId(1), RegisterId(0)],
            ),
            SIRInstruction::Store(
                addr,
                SIROffset::Static(0),
                3,
                RegisterId(3),
                Vec::new(),
                Vec::new(),
            ),
        ],
        register_map,
    );

    VectorizeConcatPass::default().run(&mut eu, &PassOptions::default());
    eu.verify();
    assert!(
        eu.blocks[&BlockId(0)]
            .instructions
            .iter()
            .any(|inst| matches!(inst, SIRInstruction::Concat(RegisterId(3), _)))
    );
}

#[test]
fn load_based_pack_marks_cross_element_access_explicitly() {
    let addr = test_addr();
    let mut register_map = HashMap::default();
    let mut instructions = Vec::new();
    let mut args = Vec::new();
    for bit in 0..8 {
        let register = RegisterId(bit);
        register_map.insert(
            register,
            RegisterType::Bit {
                width: 1,
                signed: false,
            },
        );
        instructions.push(SIRInstruction::Load(
            register,
            addr,
            SIROffset::Static(bit),
            1,
        ));
        args.push(register);
    }
    args.reverse();
    register_map.insert(
        RegisterId(8),
        RegisterType::Bit {
            width: 8,
            signed: false,
        },
    );
    instructions.push(SIRInstruction::Concat(RegisterId(8), args));
    instructions.push(SIRInstruction::RuntimeEvent {
        site_id: 0,
        args: vec![RegisterId(8)],
    });
    let mut eu = make_eu(instructions, register_map);
    let element_widths = Arc::new(
        [(addr.absolute_addr(), 1)]
            .into_iter()
            .collect::<HashMap<_, _>>(),
    );

    VectorizeConcatPass::new(element_widths).run(&mut eu, &PassOptions::default());

    assert!(
        eu.blocks[&BlockId(0)]
            .instructions
            .iter()
            .any(|instruction| {
                matches!(
                    instruction,
                    SIRInstruction::Load(
                        RegisterId(8),
                        address,
                        SIROffset::PackedElements {
                            bit_offset: 0,
                            element_width: 1,
                        },
                        8,
                    ) if *address == addr
                )
            })
    );
}

#[test]
fn exposes_a_complete_scalar_bit_store_group_as_one_packed_sink() {
    let address = test_addr();
    let bit = RegisterType::Bit {
        width: 1,
        signed: false,
    };
    let mut register_map = HashMap::default();
    let mut instructions = Vec::new();
    for lane in 0..4 {
        register_map.insert(RegisterId(lane), bit.clone());
        instructions.push(SIRInstruction::Imm(
            RegisterId(lane),
            SIRValue::new((lane & 1) as u8),
        ));
        instructions.push(SIRInstruction::Store(
            address,
            SIROffset::Static(lane),
            1,
            RegisterId(lane),
            Vec::new(),
            Vec::new(),
        ));
    }
    register_map.insert(
        RegisterId(4),
        RegisterType::Bit {
            width: 4,
            signed: false,
        },
    );
    instructions.push(SIRInstruction::Concat(
        RegisterId(4),
        vec![RegisterId(3), RegisterId(2), RegisterId(1), RegisterId(0)],
    ));

    let mut eu = make_eu(instructions, register_map);
    let mut next_register = 4;
    assert!(expose_packed_bit_store_sinks_with_counter(
        &mut eu,
        &mut next_register
    ));
    eu.verify();

    let block = &eu.blocks[&BlockId(0)];
    let concat = block
        .instructions
        .iter()
        .position(|instruction| matches!(instruction, SIRInstruction::Concat(RegisterId(4), _)))
        .unwrap();
    assert_eq!(
        block.instructions[..concat]
            .iter()
            .filter(|instruction| matches!(instruction, SIRInstruction::Store(..)))
            .count(),
        0
    );
    for lane in 0..4 {
        let slice = RegisterId(5 + lane);
        assert!(matches!(
            &block.instructions[concat + 1 + lane * 2],
            SIRInstruction::Slice(dst, RegisterId(4), offset, 1)
                if *dst == slice && *offset == lane
        ));
        assert!(matches!(
            &block.instructions[concat + 2 + lane * 2],
            SIRInstruction::Store(
                stored,
                SIROffset::Static(offset),
                1,
                source,
                triggers,
                captures,
            ) if *stored == address
                && *offset == lane
                && *source == slice
                && triggers.is_empty()
                && captures.is_empty()
        ));
    }
}

#[test]
fn packed_bit_store_sink_stops_at_an_intervening_load() {
    let address = test_addr();
    let other = RegionedAbsoluteAddr {
        var_id: celox_design::StateObjectId::from_raw(1),
        ..address
    };
    let bit = RegisterType::Bit {
        width: 1,
        signed: false,
    };
    let mut register_map = HashMap::default();
    for register in 0..3 {
        register_map.insert(RegisterId(register), bit.clone());
    }
    register_map.insert(RegisterId(3), bit);
    register_map.insert(
        RegisterId(4),
        RegisterType::Bit {
            width: 3,
            signed: false,
        },
    );
    let instructions = vec![
        SIRInstruction::Store(
            address,
            SIROffset::Static(0),
            1,
            RegisterId(0),
            Vec::new(),
            Vec::new(),
        ),
        SIRInstruction::Load(RegisterId(3), other, SIROffset::Static(0), 1),
        SIRInstruction::Store(
            address,
            SIROffset::Static(1),
            1,
            RegisterId(1),
            Vec::new(),
            Vec::new(),
        ),
        SIRInstruction::Store(
            address,
            SIROffset::Static(2),
            1,
            RegisterId(2),
            Vec::new(),
            Vec::new(),
        ),
        SIRInstruction::Concat(
            RegisterId(4),
            vec![RegisterId(2), RegisterId(1), RegisterId(0)],
        ),
    ];
    let mut eu = make_eu(instructions, register_map);
    let mut next_register = 4;
    assert!(!expose_packed_bit_store_sinks_with_counter(
        &mut eu,
        &mut next_register
    ));
    assert_eq!(
        eu.blocks[&BlockId(0)]
            .instructions
            .iter()
            .filter(|instruction| matches!(instruction, SIRInstruction::Store(..)))
            .count(),
        3
    );
}

#[test]
fn keeps_concat_in_four_state_mode() {
    let mut register_map = HashMap::default();
    for (reg, width) in [(0, 1), (1, 1), (2, 1), (3, 3)] {
        register_map.insert(RegisterId(reg), RegisterType::Logic { width });
    }
    let instructions = vec![SIRInstruction::Concat(
        RegisterId(3),
        vec![RegisterId(2), RegisterId(1), RegisterId(0)],
    )];

    let mut eu = make_eu(instructions, register_map);
    VectorizeConcatPass::default().run(
        &mut eu,
        &PassOptions {
            four_state: true,
            ..PassOptions::default()
        },
    );
    let block = eu.blocks.get(&BlockId(0)).unwrap();

    assert!(matches!(
        block.instructions.as_slice(),
        [SIRInstruction::Concat(RegisterId(3), _)]
    ));
}
