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

#[test]
fn folds_consecutive_slices_from_same_register() {
    let mut register_map = HashMap::default();
    register_map.insert(
        RegisterId(0),
        RegisterType::Bit {
            width: 32,
            signed: false,
        },
    );
    for reg in 1..=3 {
        register_map.insert(
            RegisterId(reg),
            RegisterType::Bit {
                width: 4,
                signed: false,
            },
        );
    }
    register_map.insert(
        RegisterId(4),
        RegisterType::Bit {
            width: 12,
            signed: false,
        },
    );

    let instructions = vec![
        SIRInstruction::Slice(RegisterId(1), RegisterId(0), 0, 4),
        SIRInstruction::Slice(RegisterId(2), RegisterId(0), 4, 4),
        SIRInstruction::Slice(RegisterId(3), RegisterId(0), 8, 4),
        SIRInstruction::Concat(
            RegisterId(4),
            vec![RegisterId(3), RegisterId(2), RegisterId(1)],
        ),
        SIRInstruction::RuntimeEvent {
            site_id: 0,
            args: vec![RegisterId(4)],
        },
    ];

    let mut eu = make_eu(instructions, register_map);
    ConcatFoldingPass::default().run(&mut eu, &PassOptions::default());
    let block = eu.blocks.get(&BlockId(0)).unwrap();

    assert!(
        block
            .instructions
            .iter()
            .any(|inst| matches!(inst, SIRInstruction::Slice(_, RegisterId(0), 0, 12)))
    );
    assert!(block.instructions.iter().any(|inst| matches!(
        inst,
        SIRInstruction::Concat(RegisterId(4), args) if args.len() == 1
    )));
}

#[test]
fn splits_slice_runs_at_the_fold_width_without_dropping_operands() {
    const SOURCE_WIDTH: usize = 129;
    const MAX_FOLD_WIDTH: usize = 128;
    let bit = |width| RegisterType::Bit {
        width,
        signed: false,
    };
    let register_map = HashMap::from_iter([
        (RegisterId(0), bit(SOURCE_WIDTH)),
        (RegisterId(1), bit(64)),
        (RegisterId(2), bit(64)),
        (RegisterId(3), bit(1)),
        (RegisterId(4), bit(SOURCE_WIDTH)),
    ]);
    let instructions = vec![
        SIRInstruction::Slice(RegisterId(1), RegisterId(0), 0, 64),
        SIRInstruction::Slice(RegisterId(2), RegisterId(0), 64, 64),
        SIRInstruction::Slice(RegisterId(3), RegisterId(0), 128, 1),
        SIRInstruction::Concat(
            RegisterId(4),
            vec![RegisterId(3), RegisterId(2), RegisterId(1)],
        ),
        SIRInstruction::RuntimeEvent {
            site_id: 0,
            args: vec![RegisterId(4)],
        },
    ];

    let mut eu = make_eu(instructions, register_map);
    eu.blocks
        .get_mut(&BlockId(0))
        .unwrap()
        .params
        .push(RegisterId(0));
    eu.verify_result().unwrap();
    ConcatFoldingPass::new(Arc::default(), MAX_FOLD_WIDTH).run(&mut eu, &PassOptions::default());
    eu.verify_result().unwrap();

    let concat_args = eu.blocks[&BlockId(0)]
        .instructions
        .iter()
        .find_map(|instruction| match instruction {
            SIRInstruction::Concat(RegisterId(4), args) => Some(args),
            _ => None,
        })
        .unwrap();
    assert_eq!(concat_args.len(), 2);
    assert_eq!(
        concat_args
            .iter()
            .map(|argument| eu.register_map[argument].width())
            .sum::<usize>(),
        SOURCE_WIDTH,
    );
    assert!(
        eu.blocks[&BlockId(0)]
            .instructions
            .iter()
            .any(|instruction| {
                matches!(
                    instruction,
                    SIRInstruction::Slice(_, RegisterId(0), 0, MAX_FOLD_WIDTH)
                )
            })
    );
}

#[test]
fn folds_slices_back_to_their_concat_inputs() {
    let bit = |width| RegisterType::Bit {
        width,
        signed: false,
    };
    let mut register_map = HashMap::default();
    register_map.insert(RegisterId(0), bit(52));
    register_map.insert(RegisterId(1), bit(11));
    register_map.insert(RegisterId(2), bit(1));
    register_map.insert(RegisterId(3), bit(64));
    register_map.insert(RegisterId(4), bit(52));
    register_map.insert(RegisterId(5), bit(6));
    register_map.insert(RegisterId(6), bit(1));

    let instructions = vec![
        SIRInstruction::Concat(
            RegisterId(3),
            vec![RegisterId(2), RegisterId(1), RegisterId(0)],
        ),
        SIRInstruction::Slice(RegisterId(4), RegisterId(3), 0, 52),
        SIRInstruction::Slice(RegisterId(5), RegisterId(3), 55, 6),
        SIRInstruction::Slice(RegisterId(6), RegisterId(3), 63, 1),
        SIRInstruction::RuntimeEvent {
            site_id: 0,
            args: vec![RegisterId(3), RegisterId(4), RegisterId(5), RegisterId(6)],
        },
    ];

    let mut eu = make_eu(instructions, register_map);
    ConcatFoldingPass::default().run(&mut eu, &PassOptions::default());

    let instructions = &eu.blocks[&BlockId(0)].instructions;
    assert!(instructions.iter().any(|instruction| matches!(
        instruction,
        SIRInstruction::Unary(RegisterId(4), UnaryOp::Ident, RegisterId(0))
    )));
    assert!(instructions.iter().any(|instruction| matches!(
        instruction,
        SIRInstruction::Slice(RegisterId(5), RegisterId(1), 3, 6)
    )));
    assert!(instructions.iter().any(|instruction| matches!(
        instruction,
        SIRInstruction::Unary(RegisterId(6), UnaryOp::Ident, RegisterId(2))
    )));
}

#[test]
fn folds_consecutive_slices_from_same_loaded_value() {
    let addr = test_addr();
    let mut register_map = HashMap::default();
    register_map.insert(
        RegisterId(0),
        RegisterType::Bit {
            width: 32,
            signed: false,
        },
    );
    for reg in 1..=3 {
        register_map.insert(
            RegisterId(reg),
            RegisterType::Bit {
                width: 4,
                signed: false,
            },
        );
    }
    register_map.insert(
        RegisterId(4),
        RegisterType::Bit {
            width: 12,
            signed: false,
        },
    );

    let instructions = vec![
        SIRInstruction::Load(RegisterId(0), addr, SIROffset::Static(16), 32),
        SIRInstruction::Slice(RegisterId(1), RegisterId(0), 0, 4),
        SIRInstruction::Slice(RegisterId(2), RegisterId(0), 4, 4),
        SIRInstruction::Slice(RegisterId(3), RegisterId(0), 8, 4),
        SIRInstruction::Concat(
            RegisterId(4),
            vec![RegisterId(3), RegisterId(2), RegisterId(1)],
        ),
        SIRInstruction::RuntimeEvent {
            site_id: 0,
            args: vec![RegisterId(4)],
        },
    ];

    let mut eu = make_eu(instructions, register_map);
    ConcatFoldingPass::default().run(&mut eu, &PassOptions::default());
    let block = eu.blocks.get(&BlockId(0)).unwrap();

    assert!(block.instructions.iter().any(|inst| matches!(
        inst,
        SIRInstruction::Load(_, a, SIROffset::Static(16), 12) if *a == addr
    )));
    assert!(block.instructions.iter().any(|inst| matches!(
        inst,
        SIRInstruction::Concat(RegisterId(4), args) if args.len() == 1
    )));
}

#[test]
fn folding_unpacked_bit_elements_emits_explicit_packed_elements_load() {
    let addr = test_addr();
    let mut register_map = HashMap::default();
    let mut instructions = Vec::new();
    for lane in 0..64 {
        let register = RegisterId(lane);
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
            SIROffset::Static(lane),
            1,
        ));
    }
    let result = RegisterId(64);
    register_map.insert(
        result,
        RegisterType::Bit {
            width: 64,
            signed: false,
        },
    );
    instructions.push(SIRInstruction::Concat(
        result,
        (0..64).rev().map(RegisterId).collect(),
    ));
    instructions.push(SIRInstruction::RuntimeEvent {
        site_id: 0,
        args: vec![result],
    });

    let mut eu = make_eu(instructions, register_map);
    let element_widths = Arc::new(HashMap::from_iter([(addr.absolute_addr(), 1)]));
    ConcatFoldingPass::new(element_widths, 64).run(&mut eu, &PassOptions::default());

    assert!(eu.blocks[&BlockId(0)].instructions.iter().any(|inst| {
        matches!(
            inst,
            SIRInstruction::Load(
                _,
                address,
                SIROffset::PackedElements {
                    bit_offset: 0,
                    element_width: 1,
                },
                64,
            ) if *address == addr
        )
    }));
}

#[test]
fn packs_only_the_fields_observed_by_a_private_slice() {
    let [a, b, c, d, packed, result] = std::array::from_fn(RegisterId);
    let regs = [2, 3, 4, 1, 10, 7]
        .into_iter()
        .enumerate()
        .map(|(id, width)| {
            (
                RegisterId(id),
                RegisterType::Bit {
                    width,
                    signed: false,
                },
            )
        })
        .collect::<HashMap<_, _>>();
    for (offset, shared, expected) in [(1, false, true), (1, true, false), (2, false, false)] {
        let event_args = if shared {
            vec![packed, result]
        } else {
            vec![result]
        };
        let mut eu = make_eu(
            vec![
                SIRInstruction::Concat(packed, vec![a, b, c, d]),
                SIRInstruction::Slice(result, packed, offset, 7),
                SIRInstruction::RuntimeEvent {
                    site_id: 0,
                    args: event_args,
                },
            ],
            regs.clone(),
        );
        let changed = fold_private_concat_consumers(&mut eu, false);
        assert_eq!(changed, expected);
        crate::optimizer::remove_dead_sir_definitions(&mut eu);
        let inst = &eu.blocks[&BlockId(0)].instructions;
        assert_eq!(
            inst.iter().any(
                |i| matches!(i,SIRInstruction::Concat(id,args) if *id==result && args==&[b,c])
            ),
            expected
        );
        assert_eq!(
            inst.iter()
                .any(|i| matches!(i,SIRInstruction::Concat(id,_) if *id==packed)),
            !expected
        );
    }
}

#[test]
fn reduces_a_private_pair_of_bits_without_packing() {
    let [a, b, packed, result] = std::array::from_fn(RegisterId);
    for (reduction, op) in [
        (UnaryOp::And, BinaryOp::And),
        (UnaryOp::Or, BinaryOp::Or),
        (UnaryOp::Xor, BinaryOp::Xor),
    ] {
        for (shared, four_state) in [(false, false), (true, false), (false, true)] {
            let regs = [1, 1, 2, 1]
                .into_iter()
                .enumerate()
                .map(|(id, width)| {
                    (
                        RegisterId(id),
                        RegisterType::Bit {
                            width,
                            signed: false,
                        },
                    )
                })
                .collect();
            let args = if shared {
                vec![result, packed]
            } else {
                vec![result]
            };
            let mut eu = make_eu(
                vec![
                    SIRInstruction::Concat(packed, vec![a, b]),
                    SIRInstruction::Unary(result, reduction, packed),
                    SIRInstruction::RuntimeEvent { site_id: 0, args },
                ],
                regs,
            );
            assert_eq!(
                fold_private_concat_consumers(&mut eu, four_state),
                !shared && !four_state
            );
            crate::optimizer::remove_dead_sir_definitions(&mut eu);
            let inst = &eu.blocks[&BlockId(0)].instructions;
            assert_eq!(
                inst.contains(&SIRInstruction::Binary(result, a, op, b)),
                !shared && !four_state
            );
            assert_eq!(
                inst.iter().any(|i| matches!(i, SIRInstruction::Concat(..))),
                shared || four_state
            );
        }
    }
}
