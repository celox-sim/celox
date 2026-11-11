use super::*;
use crate::ir::{InstanceId, STABLE_REGION};
use celox_design::StateObjectId as VarId;

fn address() -> RegionedAbsoluteAddr {
    RegionedAbsoluteAddr {
        region: STABLE_REGION,
        instance_id: InstanceId(0),
        var_id: VarId::default(),
    }
}

fn bit(width: usize) -> RegisterType {
    RegisterType::Bit {
        width,
        signed: false,
    }
}

/// Whether the load after `Store(address, 42); Call; Load(address)`
/// survives store-load forwarding, for a call whose callee writes
/// `written`.
fn load_survives_call_writing(written: Vec<RegionedAbsoluteAddr>) -> bool {
    let mut builder = celox_sir::SIRBuilder::new();
    let before = builder.alloc_bit(8, false);
    let after = builder.alloc_bit(8, false);
    let frame = builder.alloc_bit(32, false);
    builder.emit(SIRInstruction::Imm(before, SIRValue::new(42u8)));
    builder.emit(SIRInstruction::Imm(frame, SIRValue::new(0u8)));
    builder.emit(SIRInstruction::Store(
        address(),
        SIROffset::Static(0),
        8,
        before,
        Vec::new(),
        Vec::new(),
    ));
    builder.emit(SIRInstruction::Call {
        function: celox_sir::FunctionId(0),
        frame,
        args: Vec::new(),
        outputs: Vec::new(),
        dst: None,
        effects: std::sync::Arc::new(celox_sir::CallEffects {
            writes: written,
            ..Default::default()
        }),
    });
    builder.emit(SIRInstruction::Load(
        after,
        address(),
        SIROffset::Static(0),
        8,
    ));
    builder.emit(SIRInstruction::Store(
        address(),
        SIROffset::Static(8),
        8,
        after,
        Vec::new(),
        Vec::new(),
    ));
    builder.seal_block(SIRTerminator::Return);
    let (blocks, register_map, _) = builder.drain();
    let mut unit = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks,
        register_map,
    };
    StoreLoadForwardingPass.run(&mut unit, &PassOptions::default());
    unit.verify_result().unwrap();
    unit.blocks
        .values()
        .flat_map(|b| &b.instructions)
        .any(|inst| matches!(inst, SIRInstruction::Load(dst, ..) if *dst == after))
}

#[test]
fn a_call_invalidates_the_stores_its_callee_writes() {
    assert!(load_survives_call_writing(vec![address()]));
    let other = RegionedAbsoluteAddr {
        var_id: VarId(1),
        ..address()
    };
    assert!(!load_survives_call_writing(vec![other]));
}

#[test]
fn native_kernel_call_invalidates_prior_state_stores() {
    let mut builder = celox_sir::SIRBuilder::new();
    let before = builder.alloc_bit(8, false);
    let after = builder.alloc_bit(8, false);
    builder.emit(SIRInstruction::Imm(before, SIRValue::new(42u8)));
    builder.emit(SIRInstruction::Store(
        address(),
        SIROffset::Static(0),
        8,
        before,
        Vec::new(),
        Vec::new(),
    ));
    builder.emit(SIRInstruction::ExternCall {
        dst: None,
        func: celox_sir::extern_abi::NATIVE_KERNEL_CALL_TAG,
        args: Vec::new(),
    });
    builder.emit(SIRInstruction::Load(
        after,
        address(),
        SIROffset::Static(0),
        8,
    ));
    builder.emit(SIRInstruction::Store(
        address(),
        SIROffset::Static(8),
        8,
        after,
        Vec::new(),
        Vec::new(),
    ));
    builder.seal_block(SIRTerminator::Return);
    let (blocks, register_map, _) = builder.drain();
    let mut unit = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks,
        register_map,
    };
    StoreLoadForwardingPass.run(&mut unit, &PassOptions::default());
    unit.verify_result().unwrap();
    assert!(
        unit.blocks
            .values()
            .flat_map(|b| &b.instructions)
            .any(|inst| matches!(inst, SIRInstruction::Load(dst, ..) if *dst == after))
    );
    let saved = unit.clone();
    super::super::pass_eliminate_working_round_trip::eliminate_working_round_trip(&mut unit, &[]);
    assert_eq!(unit.blocks, saved.blocks);
    assert!(
        !super::super::pass_global_store_load_forwarding::promote_eval_apply_working_round_trips(
            &mut unit
        )
    );
    assert_eq!(unit.blocks, saved.blocks);
}

#[test]
fn removes_deep_dead_producer_chain_and_preserves_live_store() {
    let mut instructions = vec![
        SIRInstruction::Imm(RegisterId(0), SIRValue::new(3u8)),
        SIRInstruction::Load(RegisterId(1), address(), SIROffset::Static(0), 8),
    ];
    for register in 2..8194 {
        instructions.push(SIRInstruction::Binary(
            RegisterId(register),
            RegisterId(register - 1),
            BinaryOp::Add,
            RegisterId(0),
        ));
    }
    instructions.push(SIRInstruction::Store(
        address(),
        SIROffset::Static(0),
        8,
        RegisterId(0),
        Vec::new(),
        Vec::new(),
    ));
    let mut eu = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [(
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: Vec::new(),
                instructions,
                terminator: SIRTerminator::Return,
            },
        )]
        .into_iter()
        .collect(),
        register_map: (0..8194)
            .map(|register| (RegisterId(register), bit(8)))
            .collect(),
    };
    StoreLoadForwardingPass.run(&mut eu, &PassOptions::default());
    eu.verify();
    assert_eq!(
        eu.blocks[&BlockId(0)].instructions,
        vec![
            SIRInstruction::Imm(RegisterId(0), SIRValue::new(3u8)),
            SIRInstruction::Store(
                address(),
                SIROffset::Static(0),
                8,
                RegisterId(0),
                Vec::new(),
                Vec::new()
            ),
        ]
    );
    assert_eq!(eu.register_map.len(), 1);
}

#[test]
fn does_not_forward_a_truncated_store_as_a_wider_source_register() {
    let addr = address();
    let mut instructions = vec![
        SIRInstruction::Store(
            addr,
            SIROffset::Static(0),
            32,
            RegisterId(0),
            Vec::new(),
            Vec::new(),
        ),
        SIRInstruction::Load(RegisterId(1), addr, SIROffset::Static(0), 32),
        SIRInstruction::Binary(RegisterId(3), RegisterId(1), BinaryOp::Eq, RegisterId(2)),
    ];
    let register_map = [
        (RegisterId(0), bit(64)),
        (RegisterId(1), bit(32)),
        (RegisterId(2), bit(32)),
        (RegisterId(3), bit(1)),
    ]
    .into_iter()
    .collect();

    forward_and_simplify(&mut instructions, &register_map, false);

    assert!(matches!(
        instructions[2],
        SIRInstruction::Binary(_, RegisterId(1), BinaryOp::Eq, RegisterId(2))
    ));
}

#[test]
fn forwards_a_store_when_source_and_load_types_match() {
    let addr = address();
    let mut instructions = vec![
        SIRInstruction::Store(
            addr,
            SIROffset::Static(0),
            32,
            RegisterId(0),
            Vec::new(),
            Vec::new(),
        ),
        SIRInstruction::Load(RegisterId(1), addr, SIROffset::Static(0), 32),
        SIRInstruction::Binary(RegisterId(3), RegisterId(1), BinaryOp::Eq, RegisterId(2)),
    ];
    let register_map = [
        (RegisterId(0), bit(32)),
        (RegisterId(1), bit(32)),
        (RegisterId(2), bit(32)),
        (RegisterId(3), bit(1)),
    ]
    .into_iter()
    .collect();

    forward_and_simplify(&mut instructions, &register_map, false);

    assert!(matches!(
        instructions[2],
        SIRInstruction::Binary(_, RegisterId(0), BinaryOp::Eq, RegisterId(2))
    ));
}

#[test]
fn forwards_packed_elements_as_an_exact_logical_range() {
    let addr = address();
    let packed = SIROffset::PackedElements {
        bit_offset: 0,
        element_width: 1,
    };
    let mut instructions = vec![
        SIRInstruction::Store(
            addr,
            packed.clone(),
            32,
            RegisterId(0),
            Vec::new(),
            Vec::new(),
        ),
        SIRInstruction::Load(RegisterId(1), addr, packed, 32),
        SIRInstruction::Binary(RegisterId(3), RegisterId(1), BinaryOp::Eq, RegisterId(2)),
    ];
    let register_map = [
        (RegisterId(0), bit(32)),
        (RegisterId(1), bit(32)),
        (RegisterId(2), bit(32)),
        (RegisterId(3), bit(1)),
    ]
    .into_iter()
    .collect();

    forward_and_simplify(&mut instructions, &register_map, false);

    assert!(matches!(
        instructions[2],
        SIRInstruction::Binary(_, RegisterId(0), BinaryOp::Eq, RegisterId(2))
    ));
}

#[test]
fn slices_a_static_element_load_from_a_packed_store() {
    let addr = address();
    let mut instructions = vec![
        SIRInstruction::Store(
            addr,
            SIROffset::PackedElements {
                bit_offset: 0,
                element_width: 8,
            },
            32,
            RegisterId(0),
            Vec::new(),
            Vec::new(),
        ),
        SIRInstruction::Load(RegisterId(1), addr, SIROffset::Static(8), 8),
        SIRInstruction::Store(
            address(),
            SIROffset::Static(40),
            8,
            RegisterId(1),
            Vec::new(),
            Vec::new(),
        ),
    ];
    let register_map = [(RegisterId(0), bit(32)), (RegisterId(1), bit(8))]
        .into_iter()
        .collect();

    forward_and_simplify(&mut instructions, &register_map, false);

    assert_eq!(
        instructions[1],
        SIRInstruction::Slice(RegisterId(1), RegisterId(0), 8, 8)
    );
}

#[test]
fn forwards_unsigned_bit_to_logic_in_two_state_mode() {
    let addr = address();
    let mut instructions = vec![
        SIRInstruction::Store(
            addr,
            SIROffset::Static(0),
            32,
            RegisterId(0),
            Vec::new(),
            Vec::new(),
        ),
        SIRInstruction::Load(RegisterId(1), addr, SIROffset::Static(0), 32),
        SIRInstruction::Binary(RegisterId(3), RegisterId(1), BinaryOp::Eq, RegisterId(2)),
    ];
    let register_map = [
        (RegisterId(0), bit(32)),
        (RegisterId(1), RegisterType::Logic { width: 32 }),
        (RegisterId(2), RegisterType::Logic { width: 32 }),
        (RegisterId(3), bit(1)),
    ]
    .into_iter()
    .collect();

    forward_and_simplify(&mut instructions, &register_map, false);

    assert!(matches!(
        instructions[2],
        SIRInstruction::Binary(_, RegisterId(0), BinaryOp::Eq, RegisterId(2))
    ));
}

#[test]
fn preserves_bit_to_logic_round_trip_in_four_state_mode() {
    let addr = address();
    let mut instructions = vec![
        SIRInstruction::Store(
            addr,
            SIROffset::Static(0),
            32,
            RegisterId(0),
            Vec::new(),
            Vec::new(),
        ),
        SIRInstruction::Load(RegisterId(1), addr, SIROffset::Static(0), 32),
        SIRInstruction::Binary(RegisterId(3), RegisterId(1), BinaryOp::Eq, RegisterId(2)),
    ];
    let register_map = [
        (RegisterId(0), bit(32)),
        (RegisterId(1), RegisterType::Logic { width: 32 }),
        (RegisterId(2), RegisterType::Logic { width: 32 }),
        (RegisterId(3), bit(1)),
    ]
    .into_iter()
    .collect();

    forward_and_simplify(&mut instructions, &register_map, true);

    assert!(matches!(
        instructions[2],
        SIRInstruction::Binary(_, RegisterId(1), BinaryOp::Eq, RegisterId(2))
    ));
}

#[test]
fn preserves_four_state_one_bit_reductions() {
    for operation in [UnaryOp::And, UnaryOp::Or, UnaryOp::Xor] {
        let mut instructions = vec![
            SIRInstruction::Unary(RegisterId(1), operation, RegisterId(0)),
            SIRInstruction::Unary(RegisterId(2), UnaryOp::ToTwoState, RegisterId(1)),
        ];
        let register_map = [
            (RegisterId(0), RegisterType::Logic { width: 1 }),
            (RegisterId(1), RegisterType::Logic { width: 1 }),
            (RegisterId(2), bit(1)),
        ]
        .into_iter()
        .collect();

        forward_and_simplify(&mut instructions, &register_map, true);

        assert_eq!(
            instructions[1],
            SIRInstruction::Unary(RegisterId(2), UnaryOp::ToTwoState, RegisterId(1),)
        );
    }
}

#[test]
fn folds_two_state_double_boolean_negation() {
    let mut instructions = vec![
        SIRInstruction::Unary(RegisterId(1), UnaryOp::LogicNot, RegisterId(0)),
        SIRInstruction::Unary(RegisterId(2), UnaryOp::LogicNot, RegisterId(1)),
        SIRInstruction::Unary(RegisterId(3), UnaryOp::ToTwoState, RegisterId(2)),
        SIRInstruction::Store(
            address(),
            SIROffset::Static(0),
            1,
            RegisterId(3),
            Vec::new(),
            Vec::new(),
        ),
    ];
    let register_map = [
        (RegisterId(0), RegisterType::Logic { width: 1 }),
        (RegisterId(1), RegisterType::Logic { width: 1 }),
        (RegisterId(2), RegisterType::Logic { width: 1 }),
        (RegisterId(3), bit(1)),
    ]
    .into_iter()
    .collect();

    forward_and_simplify(&mut instructions, &register_map, false);

    assert_eq!(
        instructions[2],
        SIRInstruction::Unary(RegisterId(3), UnaryOp::ToTwoState, RegisterId(0))
    );
}

#[test]
fn folds_complemented_boolean_partition() {
    let mut instructions = vec![
        SIRInstruction::Unary(RegisterId(2), UnaryOp::LogicNot, RegisterId(1)),
        SIRInstruction::Binary(RegisterId(3), RegisterId(0), BinaryOp::And, RegisterId(1)),
        SIRInstruction::Binary(RegisterId(4), RegisterId(0), BinaryOp::And, RegisterId(2)),
        SIRInstruction::Binary(RegisterId(5), RegisterId(3), BinaryOp::Or, RegisterId(4)),
        SIRInstruction::Store(
            address(),
            SIROffset::Static(0),
            1,
            RegisterId(5),
            Vec::new(),
            Vec::new(),
        ),
    ];
    let register_map = (0..=5)
        .map(|register| (RegisterId(register), bit(1)))
        .collect();

    forward_and_simplify(&mut instructions, &register_map, false);

    assert!(matches!(
        instructions[4],
        SIRInstruction::Store(_, _, 1, RegisterId(0), _, _)
    ));
}

#[test]
fn applies_boolean_aliases_to_dominated_blocks_before_dce() {
    let register_map = (0..=2)
        .map(|register| (RegisterId(register), bit(1)))
        .collect();
    let mut eu = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [
            (
                BlockId(0),
                BasicBlock {
                    id: BlockId(0),
                    params: Vec::new(),
                    instructions: vec![
                        SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8)),
                        SIRInstruction::Unary(RegisterId(1), UnaryOp::LogicNot, RegisterId(0)),
                        SIRInstruction::Unary(RegisterId(2), UnaryOp::LogicNot, RegisterId(1)),
                    ],
                    terminator: SIRTerminator::Jump(BlockId(1), Vec::new()),
                },
            ),
            (
                BlockId(1),
                BasicBlock {
                    id: BlockId(1),
                    params: Vec::new(),
                    instructions: vec![SIRInstruction::Store(
                        address(),
                        SIROffset::Static(0),
                        1,
                        RegisterId(2),
                        Vec::new(),
                        Vec::new(),
                    )],
                    terminator: SIRTerminator::Return,
                },
            ),
        ]
        .into_iter()
        .collect(),
        register_map,
    };

    StoreLoadForwardingPass.run(&mut eu, &PassOptions::default());

    eu.verify();
    assert_eq!(
        eu.blocks[&BlockId(0)].instructions,
        vec![SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8))]
    );
    assert!(matches!(
        eu.blocks[&BlockId(1)].instructions.as_slice(),
        [SIRInstruction::Store(_, _, 1, RegisterId(0), _, _)]
    ));
}

#[test]
fn resolves_cross_block_transitive_aliases_in_all_terminator_uses() {
    for four_state in [false, true] {
        for kind in 0..3 {
            let (terminator, expected) = match kind {
                0 => (
                    SIRTerminator::Jump(BlockId(2), vec![RegisterId(2)]),
                    SIRTerminator::Jump(BlockId(2), vec![RegisterId(0)]),
                ),
                1 => (
                    SIRTerminator::Branch {
                        cond: RegisterId(2),
                        true_block: (BlockId(2), vec![RegisterId(2)]),
                        false_block: (BlockId(3), vec![RegisterId(1)]),
                    },
                    SIRTerminator::Branch {
                        cond: RegisterId(0),
                        true_block: (BlockId(2), vec![RegisterId(0)]),
                        false_block: (BlockId(3), vec![RegisterId(0)]),
                    },
                ),
                _ => (
                    SIRTerminator::Switch {
                        selector: RegisterId(2),
                        cases: vec![SIRSwitchCase {
                            value: 1u8.into(),
                            target: BlockId(2),
                        }],
                        default: BlockId(3),
                    },
                    SIRTerminator::Switch {
                        selector: RegisterId(0),
                        cases: vec![SIRSwitchCase {
                            value: 1u8.into(),
                            target: BlockId(2),
                        }],
                        default: BlockId(3),
                    },
                ),
            };
            let mut blocks = vec![
                BasicBlock {
                    id: BlockId(0),
                    params: Vec::new(),
                    instructions: vec![
                        SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8)),
                        SIRInstruction::Unary(RegisterId(1), UnaryOp::Ident, RegisterId(0)),
                    ],
                    terminator: SIRTerminator::Jump(BlockId(1), Vec::new()),
                },
                BasicBlock {
                    id: BlockId(1),
                    params: Vec::new(),
                    instructions: vec![SIRInstruction::Unary(
                        RegisterId(2),
                        UnaryOp::Ident,
                        RegisterId(1),
                    )],
                    terminator,
                },
            ];
            for target in 2..=if kind == 0 { 2 } else { 3 } {
                let parameter = RegisterId(target + 1);
                let params = if kind == 2 {
                    Vec::new()
                } else {
                    vec![parameter]
                };
                blocks.push(BasicBlock {
                    id: BlockId(target),
                    params,
                    instructions: vec![SIRInstruction::Store(
                        address(),
                        SIROffset::Static(0),
                        1,
                        if kind == 2 { RegisterId(2) } else { parameter },
                        Vec::new(),
                        Vec::new(),
                    )],
                    terminator: SIRTerminator::Return,
                });
            }
            let mut eu = ExecutionUnit {
                entry_block_id: BlockId(0),
                blocks: blocks.into_iter().map(|block| (block.id, block)).collect(),
                register_map: (0..5)
                    .map(|register| (RegisterId(register), bit(1)))
                    .collect(),
            };
            eu.verify();
            StoreLoadForwardingPass.run(
                &mut eu,
                &PassOptions {
                    four_state,
                    ..PassOptions::default()
                },
            );
            eu.verify();
            assert_eq!(eu.blocks[&BlockId(1)].terminator, expected);
            assert!(!eu.register_map.contains_key(&RegisterId(1)));
            assert!(!eu.register_map.contains_key(&RegisterId(2)));
        }
    }
}

#[test]
fn keeps_wide_reductions_and_signed_one_bit_sources() {
    let mut instructions = vec![
        SIRInstruction::Unary(RegisterId(1), UnaryOp::Or, RegisterId(0)),
        SIRInstruction::Unary(RegisterId(3), UnaryOp::Or, RegisterId(2)),
        SIRInstruction::Binary(RegisterId(5), RegisterId(1), BinaryOp::Or, RegisterId(3)),
    ];
    let register_map = [
        (RegisterId(0), RegisterType::Logic { width: 2 }),
        (RegisterId(1), RegisterType::Logic { width: 1 }),
        (
            RegisterId(2),
            RegisterType::Bit {
                width: 1,
                signed: true,
            },
        ),
        (RegisterId(3), bit(1)),
        (RegisterId(5), bit(1)),
    ]
    .into_iter()
    .collect();

    forward_and_simplify(&mut instructions, &register_map, false);

    assert_eq!(
        instructions[0],
        SIRInstruction::Unary(RegisterId(1), UnaryOp::Or, RegisterId(0))
    );
    assert_eq!(
        instructions[1],
        SIRInstruction::Unary(RegisterId(3), UnaryOp::Or, RegisterId(2))
    );
}

#[test]
fn does_not_forward_a_signed_bit_as_an_unsigned_logic_value() {
    let addr = address();
    let mut instructions = vec![
        SIRInstruction::Store(
            addr,
            SIROffset::Static(0),
            32,
            RegisterId(0),
            Vec::new(),
            Vec::new(),
        ),
        SIRInstruction::Load(RegisterId(1), addr, SIROffset::Static(0), 32),
        SIRInstruction::Binary(RegisterId(3), RegisterId(1), BinaryOp::Eq, RegisterId(2)),
    ];
    let register_map = [
        (
            RegisterId(0),
            RegisterType::Bit {
                width: 32,
                signed: true,
            },
        ),
        (RegisterId(1), RegisterType::Logic { width: 32 }),
        (RegisterId(2), RegisterType::Logic { width: 32 }),
        (RegisterId(3), bit(1)),
    ]
    .into_iter()
    .collect();

    forward_and_simplify(&mut instructions, &register_map, false);

    assert!(matches!(
        instructions[2],
        SIRInstruction::Binary(_, RegisterId(1), BinaryOp::Eq, RegisterId(2))
    ));
}

#[test]
fn forwards_a_contained_load_as_a_slice() {
    let addr = address();
    let mut instructions = vec![
        SIRInstruction::Store(
            addr,
            SIROffset::Static(0),
            64,
            RegisterId(0),
            Vec::new(),
            Vec::new(),
        ),
        SIRInstruction::Load(RegisterId(1), addr, SIROffset::Static(16), 16),
        SIRInstruction::Binary(RegisterId(3), RegisterId(1), BinaryOp::Eq, RegisterId(2)),
    ];
    let register_map = [
        (RegisterId(0), bit(64)),
        (RegisterId(1), bit(16)),
        (RegisterId(2), bit(16)),
        (RegisterId(3), bit(1)),
    ]
    .into_iter()
    .collect();

    forward_and_simplify(&mut instructions, &register_map, false);

    assert_eq!(
        instructions[1],
        SIRInstruction::Slice(RegisterId(1), RegisterId(0), 16, 16)
    );
}

#[test]
fn overlapping_store_invalidates_a_containing_definition() {
    let addr = address();
    let mut instructions = vec![
        SIRInstruction::Store(
            addr,
            SIROffset::Static(0),
            64,
            RegisterId(0),
            Vec::new(),
            Vec::new(),
        ),
        SIRInstruction::Store(
            addr,
            SIROffset::Static(8),
            8,
            RegisterId(1),
            Vec::new(),
            Vec::new(),
        ),
        SIRInstruction::Load(RegisterId(2), addr, SIROffset::Static(16), 16),
    ];
    let register_map = [
        (RegisterId(0), bit(64)),
        (RegisterId(1), bit(8)),
        (RegisterId(2), bit(16)),
    ]
    .into_iter()
    .collect();

    forward_and_simplify(&mut instructions, &register_map, false);

    assert!(matches!(instructions[2], SIRInstruction::Load(..)));
}
