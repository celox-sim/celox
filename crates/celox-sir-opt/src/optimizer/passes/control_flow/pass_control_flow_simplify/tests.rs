use super::*;
use crate::ir::InstanceId;
use celox_design::StateObjectId as VarId;

#[test]
fn float_operations_do_not_inherit_integer_algebraic_identities() {
    use celox_design::{FloatBinaryOp as F, FloatPrecision as P};
    let source = RegisterId(0);
    let zero = RegisterId(1);
    let dst = RegisterId(2);
    let mut values = LatticeValues::default();
    values.insert(
        source,
        LatticeValue::Constant(SIRValue::new((-0.0f64).to_bits())),
    );
    values.insert(zero, LatticeValue::Constant(SIRValue::new(0u8)));
    let analysis = Analysis {
        executable: HashSet::default(),
        values,
    };
    for four_state in [false, true] {
        for (op, rhs, width) in [
            (F::Add, zero, 64),
            (F::Sub, source, 64),
            (F::Eq, source, 1),
            (F::Ne, source, 1),
        ] {
            let types = [
                (
                    source,
                    RegisterType::Bit {
                        width: 64,
                        signed: false,
                    },
                ),
                (
                    zero,
                    RegisterType::Bit {
                        width: 64,
                        signed: false,
                    },
                ),
                (
                    dst,
                    RegisterType::Bit {
                        width,
                        signed: false,
                    },
                ),
            ]
            .into_iter()
            .collect();
            let instruction = SIRInstruction::Binary(
                dst,
                source,
                BinaryOp::Float {
                    op,
                    precision: P::F64,
                },
                rhs,
            );
            assert!(algebraic_replacement(&instruction, &analysis, &types, four_state).is_none());
        }
    }
}

#[test]
fn sccp_tracks_backedge_parameters_without_retaining_local_use_sets() {
    let entry = BasicBlock {
        id: BlockId(0),
        params: vec![],
        instructions: vec![SIRInstruction::Imm(RegisterId(0), SIRValue::new(0u8))],
        terminator: SIRTerminator::Jump(BlockId(1), vec![RegisterId(0)]),
    };
    let mut instructions = vec![SIRInstruction::Imm(RegisterId(2), SIRValue::new(1u8))];
    let mut previous = RegisterId(1);
    for id in 3..259 {
        instructions.push(SIRInstruction::Binary(
            RegisterId(id),
            previous,
            BinaryOp::Add,
            RegisterId(2),
        ));
        previous = RegisterId(id);
    }
    instructions.push(SIRInstruction::Load(
        RegisterId(600),
        address(),
        SIROffset::Static(0),
        1,
    ));
    let body = BasicBlock {
        id: BlockId(1),
        params: vec![RegisterId(1)],
        instructions,
        terminator: SIRTerminator::Branch {
            cond: RegisterId(600),
            true_block: (BlockId(1), vec![previous]),
            false_block: (BlockId(2), vec![previous]),
        },
    };
    let exit = BasicBlock {
        id: BlockId(2),
        params: vec![RegisterId(700)],
        instructions: vec![SIRInstruction::Store(
            address_instance(1),
            SIROffset::Static(0),
            32,
            RegisterId(700),
            vec![],
            vec![],
        )],
        terminator: SIRTerminator::Return,
    };
    let mut eu = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [entry, body, exit]
            .into_iter()
            .map(|block| (block.id, block))
            .collect(),
        register_map: (0..701).map(|id| (RegisterId(id), bit(32))).collect(),
    };
    eu.register_map.insert(RegisterId(600), bit(1));
    eu.verify_result().unwrap();
    let users = sccp_use_blocks(&eu);
    assert_eq!(users.len(), 2);
    assert_eq!(users[&RegisterId(1)], [BlockId(1)].into_iter().collect());
    assert_eq!(users[&RegisterId(700)], [BlockId(2)].into_iter().collect());
    for four_state in [false, true] {
        let analysis = analyze(&eu, four_state);
        assert_eq!(analysis.executable.len(), 3);
        for register in [RegisterId(1), previous, RegisterId(700)] {
            assert_eq!(
                analysis.values.get(&register),
                Some(&LatticeValue::Overdefined)
            );
        }
    }
}

#[test]
fn correlated_fact_intersections_match_set_intersection_and_share_subsets() {
    let mut cases = vec![Arc::new(CorrelatedFacts::default())];
    for seed in 0..24 {
        let mut facts = CorrelatedFacts::default();
        for value in 0..64 {
            if (value + seed) % 3 != 0 {
                Arc::make_mut(&mut facts.booleans)
                    .insert(RegisterId(value), (value + seed) % 5 != 0);
            }
            if (value + seed) % 4 != 0 {
                Arc::make_mut(&mut facts.equalities).insert(
                    RegisterId(value),
                    ExactCaseConstant::new(65, vec![value as u64, (seed % 2) as u64]),
                );
            }
        }
        let mut subset = facts.clone();
        Arc::make_mut(&mut subset.booleans).retain(|register, _| register.0 % 2 == 0);
        Arc::make_mut(&mut subset.equalities).retain(|register, _| register.0 % 2 == 0);
        cases.push(Arc::new(facts));
        cases.push(Arc::new(subset));
    }
    for left in &cases {
        for right in &cases {
            let mut expected = (**left).clone();
            Arc::make_mut(&mut expected.booleans)
                .retain(|register, truth| right.booleans.get(register) == Some(truth));
            Arc::make_mut(&mut expected.equalities)
                .retain(|register, constant| right.equalities.get(register) == Some(constant));
            let actual = CorrelatedFacts::intersection(left, right);
            assert_eq!(*actual, expected);
            if expected == **left || expected == **right {
                assert!(Arc::ptr_eq(&actual, left) || Arc::ptr_eq(&actual, right));
            }
        }
    }
    let left = &cases[1];
    let mut right = (**left).clone();
    let original_truth = left.booleans[&RegisterId(1)];
    Arc::make_mut(&mut right.booleans).insert(RegisterId(1), !original_truth);
    assert_eq!(left.booleans[&RegisterId(1)], original_truth);
    let actual = CorrelatedFacts::intersection(left, &Arc::new(right));
    assert!(!actual.booleans.contains_key(&RegisterId(1)));
    assert!(Arc::ptr_eq(&actual.equalities, &left.equalities));
}

fn bit(width: usize) -> RegisterType {
    RegisterType::Bit {
        width,
        signed: false,
    }
}

fn address() -> RegionedAbsoluteAddr {
    address_instance(0)
}

fn address_instance(instance: usize) -> RegionedAbsoluteAddr {
    RegionedAbsoluteAddr {
        region: 0,
        instance_id: InstanceId(instance),
        var_id: VarId::default(),
    }
}

struct CaseLadder {
    eu: ExecutionUnit<RegionedAbsoluteAddr>,
    arms: Vec<BlockId>,
    decisions: Vec<BlockId>,
    constants: Vec<RegisterId>,
    final_block: BlockId,
}

fn case_ladder(values: &[usize], effectful_decision: Option<usize>) -> CaseLadder {
    assert!(!values.is_empty());
    let width =
        usize::BITS as usize - values.iter().copied().max().unwrap().leading_zeros() as usize;
    let width = width.max(1);
    let selector = RegisterId(0);
    let final_block = BlockId(values.len() * 3);
    let mut next_register = 1usize;
    let mut blocks = HashMap::default();
    let mut register_map = HashMap::default();
    let mut arms = Vec::new();
    let mut decisions = Vec::new();
    let mut constants = Vec::new();
    register_map.insert(selector, bit(width));

    for (index, &value) in values.iter().enumerate() {
        let decision = BlockId(index * 3);
        let arm = BlockId(index * 3 + 1);
        let miss = BlockId(index * 3 + 2);
        let next = if index + 1 == values.len() {
            final_block
        } else {
            BlockId((index + 1) * 3)
        };
        decisions.push(decision);
        arms.push(arm);

        let constant = RegisterId(next_register);
        let comparison = RegisterId(next_register + 1);
        let condition = RegisterId(next_register + 2);
        next_register += 3;
        constants.push(constant);
        register_map.insert(constant, bit(width));
        register_map.insert(comparison, bit(1));
        register_map.insert(condition, bit(1));
        let mut instructions = Vec::new();
        if index == 0 {
            instructions.push(SIRInstruction::Load(
                selector,
                address_instance(1),
                SIROffset::Static(0),
                width,
            ));
        }
        if effectful_decision == Some(index) {
            let side_value = RegisterId(next_register);
            next_register += 1;
            register_map.insert(side_value, bit(1));
            instructions.extend([
                SIRInstruction::Imm(side_value, SIRValue::new(1u8)),
                SIRInstruction::Store(
                    address_instance(10_000 + index),
                    SIROffset::Static(0),
                    1,
                    side_value,
                    Vec::new(),
                    Vec::new(),
                ),
            ]);
        }
        instructions.extend([
            SIRInstruction::Imm(constant, SIRValue::new(value)),
            SIRInstruction::Binary(comparison, selector, BinaryOp::Eq, constant),
            SIRInstruction::Unary(condition, UnaryOp::ToTwoState, comparison),
        ]);
        blocks.insert(
            decision,
            BasicBlock {
                id: decision,
                params: Vec::new(),
                instructions,
                terminator: SIRTerminator::Branch {
                    cond: condition,
                    true_block: (arm, Vec::new()),
                    false_block: (miss, Vec::new()),
                },
            },
        );

        let arm_value = RegisterId(next_register);
        next_register += 1;
        register_map.insert(arm_value, bit(8));
        blocks.insert(
            arm,
            BasicBlock {
                id: arm,
                params: Vec::new(),
                instructions: vec![
                    SIRInstruction::Imm(arm_value, SIRValue::new((index & 0xff) as u8)),
                    SIRInstruction::Store(
                        address_instance(100 + index),
                        SIROffset::Static(0),
                        8,
                        arm_value,
                        Vec::new(),
                        Vec::new(),
                    ),
                ],
                terminator: SIRTerminator::Jump(next, Vec::new()),
            },
        );
        blocks.insert(
            miss,
            BasicBlock {
                id: miss,
                params: Vec::new(),
                instructions: Vec::new(),
                terminator: SIRTerminator::Jump(next, Vec::new()),
            },
        );
    }
    blocks.insert(
        final_block,
        BasicBlock {
            id: final_block,
            params: Vec::new(),
            instructions: Vec::new(),
            terminator: SIRTerminator::Return,
        },
    );
    let eu = ExecutionUnit {
        blocks,
        entry_block_id: BlockId(0),
        register_map,
    };
    eu.verify_result().unwrap();
    CaseLadder {
        eu,
        arms,
        decisions,
        constants,
        final_block,
    }
}

fn constant_branch_unit() -> ExecutionUnit<RegionedAbsoluteAddr> {
    let mut register_map = HashMap::default();
    for register in 0..=4 {
        register_map.insert(RegisterId(register), bit(1));
    }
    ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [
            BasicBlock {
                id: BlockId(0),
                params: Vec::new(),
                instructions: vec![
                    SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8)),
                    SIRInstruction::Imm(RegisterId(1), SIRValue::new(1u8)),
                    SIRInstruction::Imm(RegisterId(2), SIRValue::new(0u8)),
                ],
                terminator: SIRTerminator::Branch {
                    cond: RegisterId(0),
                    true_block: (BlockId(1), Vec::new()),
                    false_block: (BlockId(2), Vec::new()),
                },
            },
            BasicBlock {
                id: BlockId(1),
                params: Vec::new(),
                instructions: vec![SIRInstruction::Mux(
                    RegisterId(3),
                    RegisterId(0),
                    RegisterId(1),
                    RegisterId(2),
                )],
                terminator: SIRTerminator::Return,
            },
            BasicBlock {
                id: BlockId(2),
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
        ]
        .into_iter()
        .map(|block| (block.id, block))
        .collect(),
        register_map,
    }
}

#[test]
fn folds_constant_branch_and_drops_unreachable_arm() {
    let mut eu = constant_branch_unit();
    eu.verify_result().unwrap();

    ControlFlowSimplifyPass.run(&mut eu, &PassOptions::default());

    eu.verify_result().unwrap();
    assert!(!eu.blocks.contains_key(&BlockId(2)));
    assert!(matches!(
        eu.blocks[&BlockId(0)].terminator,
        SIRTerminator::Jump(BlockId(1), _)
    ));
    assert!(
        !eu.blocks[&BlockId(1)]
            .instructions
            .iter()
            .any(|instruction| matches!(instruction, SIRInstruction::Mux(..)))
    );
    assert!(!eu.register_map.contains_key(&RegisterId(2)));
}

#[test]
fn folds_two_state_self_comparison_through_logical_annihilator() {
    let mut eu = constant_branch_unit();
    eu.register_map.insert(RegisterId(5), bit(1));
    eu.register_map.insert(RegisterId(6), bit(1));
    eu.blocks.get_mut(&BlockId(0)).unwrap().instructions = vec![
        SIRInstruction::Load(RegisterId(0), address_instance(1), SIROffset::Static(0), 1),
        SIRInstruction::Binary(RegisterId(5), RegisterId(0), BinaryOp::Ne, RegisterId(0)),
        SIRInstruction::Binary(
            RegisterId(6),
            RegisterId(0),
            BinaryOp::LogicAnd,
            RegisterId(5),
        ),
        SIRInstruction::Imm(RegisterId(1), SIRValue::new(1u8)),
        SIRInstruction::Imm(RegisterId(2), SIRValue::new(0u8)),
    ];
    eu.blocks.get_mut(&BlockId(0)).unwrap().terminator = SIRTerminator::Branch {
        cond: RegisterId(6),
        true_block: (BlockId(1), Vec::new()),
        false_block: (BlockId(2), Vec::new()),
    };
    eu.verify_result().unwrap();

    ControlFlowSimplifyPass.run(&mut eu, &PassOptions::default());

    eu.verify_result().unwrap();
    assert!(!eu.blocks.contains_key(&BlockId(1)));
    assert!(matches!(
        eu.blocks[&BlockId(0)].terminator,
        SIRTerminator::Jump(BlockId(2), _)
    ));
}

#[test]
fn keeps_four_state_self_comparison_dynamic() {
    let mut eu = constant_branch_unit();
    eu.register_map.insert(RegisterId(5), bit(1));
    eu.register_map.insert(RegisterId(6), bit(1));
    eu.blocks.get_mut(&BlockId(0)).unwrap().instructions = vec![
        SIRInstruction::Load(RegisterId(0), address_instance(1), SIROffset::Static(0), 1),
        SIRInstruction::Binary(RegisterId(5), RegisterId(0), BinaryOp::Ne, RegisterId(0)),
        SIRInstruction::Unary(RegisterId(6), UnaryOp::ToTwoState, RegisterId(5)),
        SIRInstruction::Imm(RegisterId(1), SIRValue::new(1u8)),
        SIRInstruction::Imm(RegisterId(2), SIRValue::new(0u8)),
    ];
    eu.blocks.get_mut(&BlockId(0)).unwrap().terminator = SIRTerminator::Branch {
        cond: RegisterId(6),
        true_block: (BlockId(1), Vec::new()),
        false_block: (BlockId(2), Vec::new()),
    };
    eu.verify_result().unwrap();
    let options = PassOptions {
        four_state: true,
        ..PassOptions::default()
    };

    ControlFlowSimplifyPass.run(&mut eu, &options);

    eu.verify_result().unwrap();
    assert!(matches!(
        eu.blocks[&BlockId(0)].terminator,
        SIRTerminator::Branch { .. }
    ));
    assert!(eu.blocks.contains_key(&BlockId(1)));
    assert!(eu.blocks.contains_key(&BlockId(2)));
}

#[test]
fn materializes_proven_sccp_constants_in_sir() {
    for zero_on_left in [false, true] {
        let mut eu = constant_branch_unit();
        eu.register_map
            .insert(RegisterId(0), RegisterType::Logic { width: 1 });
        let (lhs, rhs) = if zero_on_left {
            (RegisterId(1), RegisterId(0))
        } else {
            (RegisterId(0), RegisterId(1))
        };
        eu.blocks.get_mut(&BlockId(0)).unwrap().instructions = vec![
            SIRInstruction::Load(RegisterId(0), address_instance(1), SIROffset::Static(0), 1),
            SIRInstruction::Imm(RegisterId(1), SIRValue::new(0u8)),
            SIRInstruction::Binary(RegisterId(3), lhs, BinaryOp::LogicAnd, rhs),
            SIRInstruction::Store(
                address_instance(2),
                SIROffset::Static(0),
                1,
                RegisterId(3),
                Vec::new(),
                Vec::new(),
            ),
        ];
        eu.blocks.get_mut(&BlockId(0)).unwrap().terminator = SIRTerminator::Return;
        eu.blocks.retain(|id, _| *id == BlockId(0));
        eu.verify_result().unwrap();
        let options = PassOptions {
            four_state: true,
            ..PassOptions::default()
        };

        ControlFlowSimplifyPass.run(&mut eu, &options);

        eu.verify_result().unwrap();
        assert!(matches!(
            eu.blocks[&BlockId(0)].instructions.as_slice(),
            [
                SIRInstruction::Imm(RegisterId(3), value),
                SIRInstruction::Store(_, _, 1, RegisterId(3), ..)
            ] if value == &SIRValue::new(0u8)
        ));
    }
}

#[test]
fn rewrites_logical_identity_to_boolean_reduction() {
    let mut eu = constant_branch_unit();
    eu.register_map
        .insert(RegisterId(0), RegisterType::Logic { width: 8 });
    eu.register_map
        .insert(RegisterId(1), RegisterType::Logic { width: 8 });
    eu.blocks.get_mut(&BlockId(0)).unwrap().instructions = vec![
        SIRInstruction::Load(RegisterId(0), address_instance(1), SIROffset::Static(0), 8),
        SIRInstruction::Imm(RegisterId(1), SIRValue::new(0u8)),
        SIRInstruction::Binary(
            RegisterId(3),
            RegisterId(0),
            BinaryOp::LogicOr,
            RegisterId(1),
        ),
        SIRInstruction::Store(
            address_instance(2),
            SIROffset::Static(0),
            1,
            RegisterId(3),
            Vec::new(),
            Vec::new(),
        ),
    ];
    eu.blocks.get_mut(&BlockId(0)).unwrap().terminator = SIRTerminator::Return;
    eu.blocks.retain(|id, _| *id == BlockId(0));
    eu.verify_result().unwrap();

    ControlFlowSimplifyPass.run(&mut eu, &PassOptions::default());

    eu.verify_result().unwrap();
    assert!(matches!(
        eu.blocks[&BlockId(0)].instructions.as_slice(),
        [
            SIRInstruction::Load(RegisterId(0), ..),
            SIRInstruction::Unary(RegisterId(3), UnaryOp::Or, RegisterId(0)),
            SIRInstruction::Store(_, _, 1, RegisterId(3), ..)
        ]
    ));
}

#[test]
fn does_not_fold_an_overdefined_join_condition() {
    let mut eu = constant_branch_unit();
    eu.blocks.get_mut(&BlockId(0)).unwrap().instructions[0] =
        SIRInstruction::Load(RegisterId(0), address(), SIROffset::Static(0), 1);
    eu.blocks.get_mut(&BlockId(1)).unwrap().instructions.clear();
    eu.blocks.get_mut(&BlockId(1)).unwrap().terminator =
        SIRTerminator::Jump(BlockId(3), Vec::new());
    eu.blocks.get_mut(&BlockId(2)).unwrap().instructions.clear();
    eu.blocks.get_mut(&BlockId(2)).unwrap().terminator =
        SIRTerminator::Jump(BlockId(3), Vec::new());
    eu.blocks.insert(
        BlockId(3),
        BasicBlock {
            id: BlockId(3),
            params: Vec::new(),
            instructions: vec![
                SIRInstruction::Mux(RegisterId(3), RegisterId(0), RegisterId(1), RegisterId(2)),
                SIRInstruction::Store(
                    address(),
                    SIROffset::Static(0),
                    1,
                    RegisterId(3),
                    Vec::new(),
                    Vec::new(),
                ),
            ],
            terminator: SIRTerminator::Return,
        },
    );
    eu.verify_result().unwrap();

    ControlFlowSimplifyPass.run(&mut eu, &PassOptions::default());

    eu.verify_result().unwrap();
    assert!(matches!(
        eu.blocks[&BlockId(0)].terminator,
        SIRTerminator::Branch { .. }
    ));
    assert!(
        eu.blocks[&BlockId(3)]
            .instructions
            .iter()
            .any(|instruction| matches!(instruction, SIRInstruction::Mux(..)))
    );
}

#[test]
fn uses_a_dominating_dynamic_branch_to_remove_a_mux_arm() {
    let mut eu = constant_branch_unit();
    eu.blocks.get_mut(&BlockId(0)).unwrap().instructions[0] =
        SIRInstruction::Load(RegisterId(0), address(), SIROffset::Static(0), 1);
    eu.blocks.get_mut(&BlockId(2)).unwrap().instructions.clear();
    eu.blocks.get_mut(&BlockId(2)).unwrap().terminator = SIRTerminator::Return;
    eu.verify_result().unwrap();

    ControlFlowSimplifyPass.run(&mut eu, &PassOptions::default());

    eu.verify_result().unwrap();
    assert!(matches!(
        eu.blocks[&BlockId(0)].terminator,
        SIRTerminator::Branch { .. }
    ));
    assert!(
        !eu.blocks[&BlockId(1)]
            .instructions
            .iter()
            .any(|instruction| matches!(instruction, SIRInstruction::Mux(..)))
    );
}

#[test]
fn does_not_fold_a_branch_successor_that_is_also_a_join() {
    let mut eu = constant_branch_unit();
    eu.blocks.get_mut(&BlockId(0)).unwrap().instructions[0] =
        SIRInstruction::Load(RegisterId(0), address(), SIROffset::Static(0), 1);
    // `b2` is the false edge of the branch, but it is also reachable from
    // the true edge through `b1`.  The condition is therefore not known
    // at b2.  This is the CFG shape emitted for a guarded loop exit: a
    // direct disabled path joins the path that evaluated the loop.
    eu.blocks.get_mut(&BlockId(1)).unwrap().instructions.clear();
    eu.blocks.get_mut(&BlockId(1)).unwrap().terminator =
        SIRTerminator::Jump(BlockId(2), Vec::new());
    eu.blocks.get_mut(&BlockId(2)).unwrap().instructions = vec![
        SIRInstruction::Mux(RegisterId(3), RegisterId(0), RegisterId(1), RegisterId(2)),
        SIRInstruction::Store(
            address(),
            SIROffset::Static(0),
            1,
            RegisterId(3),
            Vec::new(),
            Vec::new(),
        ),
    ];
    eu.verify_result().unwrap();

    ControlFlowSimplifyPass.run(&mut eu, &PassOptions::default());

    eu.verify_result().unwrap();
    assert!(
        eu.blocks[&BlockId(2)]
            .instructions
            .iter()
            .any(|instruction| matches!(instruction, SIRInstruction::Mux(..))),
        "a direct branch successor is not necessarily an edge-dominated block",
    );
}

#[test]
fn reapplies_sccp_after_dominance_mux_simplification() {
    let mut eu = constant_branch_unit();
    eu.blocks.get_mut(&BlockId(0)).unwrap().instructions[0] =
        SIRInstruction::Load(RegisterId(0), address(), SIROffset::Static(0), 1);
    eu.blocks.get_mut(&BlockId(1)).unwrap().terminator = SIRTerminator::Branch {
        cond: RegisterId(3),
        true_block: (BlockId(3), Vec::new()),
        false_block: (BlockId(4), Vec::new()),
    };
    eu.blocks.insert(
        BlockId(3),
        BasicBlock {
            id: BlockId(3),
            params: Vec::new(),
            instructions: Vec::new(),
            terminator: SIRTerminator::Return,
        },
    );
    eu.blocks.insert(
        BlockId(4),
        BasicBlock {
            id: BlockId(4),
            params: Vec::new(),
            instructions: Vec::new(),
            terminator: SIRTerminator::Return,
        },
    );
    eu.verify_result().unwrap();

    ControlFlowSimplifyPass.run(&mut eu, &PassOptions::default());

    eu.verify_result().unwrap();
    assert!(matches!(
        eu.blocks[&BlockId(1)].terminator,
        SIRTerminator::Jump(BlockId(3), _)
    ));
    assert!(!eu.blocks.contains_key(&BlockId(4)));
}

#[test]
fn follows_a_branch_predicate_through_a_block_argument() {
    let mut eu = constant_branch_unit();
    eu.register_map.insert(RegisterId(5), bit(1));
    eu.blocks.get_mut(&BlockId(0)).unwrap().instructions[0] =
        SIRInstruction::Load(RegisterId(0), address(), SIROffset::Static(0), 1);
    eu.blocks.get_mut(&BlockId(0)).unwrap().terminator = SIRTerminator::Branch {
        cond: RegisterId(0),
        true_block: (BlockId(1), vec![RegisterId(0)]),
        false_block: (BlockId(2), Vec::new()),
    };
    eu.blocks.get_mut(&BlockId(1)).unwrap().params = vec![RegisterId(4)];
    eu.blocks.get_mut(&BlockId(1)).unwrap().instructions = vec![
        SIRInstruction::Mux(RegisterId(5), RegisterId(4), RegisterId(1), RegisterId(2)),
        SIRInstruction::Store(
            address(),
            SIROffset::Static(0),
            1,
            RegisterId(5),
            Vec::new(),
            Vec::new(),
        ),
    ];
    eu.verify_result().unwrap();

    ControlFlowSimplifyPass.run(&mut eu, &PassOptions::default());

    eu.verify_result().unwrap();
    assert!(
        !eu.blocks[&BlockId(1)]
            .instructions
            .iter()
            .any(|instruction| matches!(instruction, SIRInstruction::Mux(..)))
    );
}

#[test]
fn threads_taken_case_arms_over_the_remaining_equality_spine() {
    let mut ladder = case_ladder(&[0, 1, 2, 3], None);

    ControlFlowSimplifyPass.run(&mut ladder.eu, &PassOptions::default());

    ladder.eu.verify_result().unwrap();
    for arm in ladder.arms {
        assert!(matches!(
            ladder.eu.blocks[&arm].terminator,
            SIRTerminator::Jump(target, _) if target == ladder.final_block
        ));
    }
}

#[test]
fn repeated_constant_in_the_suffix_prevents_threading_past_its_arm() {
    let mut ladder = case_ladder(&[0, 0, 1], None);
    let first_successor = ladder.decisions[1];

    ControlFlowSimplifyPass.run(&mut ladder.eu, &PassOptions::default());

    ladder.eu.verify_result().unwrap();
    assert!(matches!(
        ladder.eu.blocks[&ladder.arms[0]].terminator,
        SIRTerminator::Jump(target, _) if target == first_successor
    ));
    assert!(matches!(
        ladder.eu.blocks[&ladder.arms[1]].terminator,
        SIRTerminator::Jump(target, _) if target == ladder.final_block
    ));
}

#[test]
fn does_not_skip_an_effectful_decision_block() {
    let mut ladder = case_ladder(&[0, 1], Some(1));
    let effectful = ladder.decisions[1];

    ControlFlowSimplifyPass.run(&mut ladder.eu, &PassOptions::default());

    ladder.eu.verify_result().unwrap();
    assert!(matches!(
        ladder.eu.blocks[&ladder.arms[0]].terminator,
        SIRTerminator::Jump(target, _) if target == effectful
    ));
}

#[test]
fn does_not_thread_a_case_spine_in_four_state_mode() {
    let mut ladder = case_ladder(&[0, 1, 2, 3], None);
    let second_decision = ladder.decisions[1];
    let options = PassOptions {
        four_state: true,
        ..PassOptions::default()
    };

    ControlFlowSimplifyPass.run(&mut ladder.eu, &options);

    ladder.eu.verify_result().unwrap();
    assert!(matches!(
        ladder.eu.blocks[&ladder.arms[0]].terminator,
        SIRTerminator::Jump(target, _) if target == second_decision
    ));
}

#[test]
fn does_not_thread_through_a_cyclic_case_spine() {
    let mut ladder = case_ladder(&[0, 1, 2, 3], None);
    let second_decision = ladder.decisions[1];
    ladder
        .eu
        .blocks
        .get_mut(&ladder.final_block)
        .unwrap()
        .terminator = SIRTerminator::Jump(ladder.decisions[0], Vec::new());
    ladder.eu.verify_result().unwrap();

    ControlFlowSimplifyPass.run(&mut ladder.eu, &PassOptions::default());

    ladder.eu.verify_result().unwrap();
    assert!(matches!(
        ladder.eu.blocks[&ladder.arms[0]].terminator,
        SIRTerminator::Jump(target, _) if target == second_decision
    ));
}

#[test]
fn rematerializes_a_case_constant_needed_after_the_threaded_suffix() {
    let mut ladder = case_ladder(&[0, 1, 2], None);
    let old_constant = ladder.constants[1];
    ladder
        .eu
        .blocks
        .get_mut(&ladder.final_block)
        .unwrap()
        .instructions
        .push(SIRInstruction::Store(
            address_instance(20_000),
            SIROffset::Static(0),
            2,
            old_constant,
            Vec::new(),
            Vec::new(),
        ));
    ladder.eu.verify_result().unwrap();

    ControlFlowSimplifyPass.run(&mut ladder.eu, &PassOptions::default());

    ladder.eu.verify_result().unwrap();
    assert!(matches!(
        ladder.eu.blocks[&ladder.arms[0]].terminator,
        SIRTerminator::Jump(target, _) if target == ladder.final_block
    ));
    let final_instructions = &ladder.eu.blocks[&ladder.final_block].instructions;
    let new_constant = match final_instructions.as_slice() {
        [
            SIRInstruction::Imm(new, value),
            SIRInstruction::Store(_, _, _, source, ..),
        ] if new == source && *new != old_constant && value == &SIRValue::new(1u8) => *new,
        instructions => panic!("unexpected rematerialized final block: {instructions:?}"),
    };
    assert_eq!(ladder.eu.register_map[&new_constant], bit(2));
}

#[test]
fn rematerializes_a_live_out_predicate_dag_at_its_late_use() {
    let mut ladder = case_ladder(&[0, 1, 2], None);
    let old_predicate = match ladder.eu.blocks[&ladder.decisions[1]].terminator {
        SIRTerminator::Branch { cond, .. } => cond,
        _ => unreachable!(),
    };
    ladder
        .eu
        .blocks
        .get_mut(&ladder.final_block)
        .unwrap()
        .instructions
        .push(SIRInstruction::Store(
            address_instance(20_001),
            SIROffset::Static(0),
            1,
            old_predicate,
            Vec::new(),
            Vec::new(),
        ));
    ladder.eu.verify_result().unwrap();

    ControlFlowSimplifyPass.run(&mut ladder.eu, &PassOptions::default());

    ladder.eu.verify_result().unwrap();
    assert!(matches!(
        ladder.eu.blocks[&ladder.arms[0]].terminator,
        SIRTerminator::Jump(target, _) if target == ladder.final_block
    ));
    let final_instructions = &ladder.eu.blocks[&ladder.final_block].instructions;
    assert!(matches!(
        final_instructions.as_slice(),
        [
            SIRInstruction::Imm(constant, _),
            SIRInstruction::Binary(comparison, RegisterId(0), BinaryOp::Eq, rhs),
            SIRInstruction::Unary(condition, UnaryOp::ToTwoState, source),
            SIRInstruction::Store(_, _, 1, stored, ..),
        ] if constant == rhs
            && comparison == source
            && condition == stored
            && *condition != old_predicate
    ));
}

fn add_second_selector_load(ladder: &mut CaseLadder) -> RegisterId {
    let second_decision = ladder.decisions[1];
    let selector = RegisterId(0);
    let width = ladder.eu.register_map[&selector].width();
    let next = ladder
        .eu
        .register_map
        .keys()
        .map(|register| register.0)
        .max()
        .unwrap()
        + 1;
    let reloaded = RegisterId(next);
    ladder.eu.register_map.insert(reloaded, bit(width));
    let block = ladder.eu.blocks.get_mut(&second_decision).unwrap();
    block.instructions.insert(
        0,
        SIRInstruction::Load(reloaded, address_instance(1), SIROffset::Static(0), width),
    );
    let comparison = block
        .instructions
        .iter_mut()
        .find(|instruction| matches!(instruction, SIRInstruction::Binary(..)))
        .unwrap();
    let SIRInstruction::Binary(_, lhs, BinaryOp::Eq, _) = comparison else {
        unreachable!();
    };
    *lhs = reloaded;
    ladder.eu.verify_result().unwrap();
    reloaded
}

#[test]
fn state_ssa_gvn_exposes_a_disjoint_store_case_for_threading() {
    let mut ladder = case_ladder(&[0, 1], None);
    let reloaded = add_second_selector_load(&mut ladder);

    super::super::pass_gvn::GvnPass.run(&mut ladder.eu, &PassOptions::default());
    PostGvnCfgCleanupPass.run(&mut ladder.eu, &PassOptions::default());

    ladder.eu.verify_result().unwrap();
    assert!(!ladder.eu.register_map.contains_key(&reloaded));
    assert!(matches!(
        ladder.eu.blocks[&ladder.arms[0]].terminator,
        SIRTerminator::Jump(target, _) if target == ladder.final_block
    ));
}

#[test]
fn same_slot_write_keeps_the_reload_and_prevents_correlated_threading() {
    let mut ladder = case_ladder(&[0, 1], None);
    let reloaded = add_second_selector_load(&mut ladder);
    let selector_width = ladder.eu.register_map[&RegisterId(0)].width();
    let write = RegisterId(
        ladder
            .eu
            .register_map
            .keys()
            .map(|register| register.0)
            .max()
            .unwrap()
            + 1,
    );
    ladder.eu.register_map.insert(write, bit(selector_width));
    ladder
        .eu
        .blocks
        .get_mut(&ladder.arms[0])
        .unwrap()
        .instructions
        .extend([
            SIRInstruction::Imm(write, SIRValue::new(1u8)),
            SIRInstruction::Store(
                address_instance(1),
                SIROffset::Static(0),
                selector_width,
                write,
                Vec::new(),
                Vec::new(),
            ),
        ]);
    ladder.eu.verify_result().unwrap();
    let second_decision = ladder.decisions[1];

    super::super::pass_gvn::GvnPass.run(&mut ladder.eu, &PassOptions::default());
    ControlFlowSimplifyPass.run(&mut ladder.eu, &PassOptions::default());

    ladder.eu.verify_result().unwrap();
    assert!(ladder.eu.register_map.contains_key(&reloaded));
    assert!(matches!(
        ladder.eu.blocks[&ladder.arms[0]].terminator,
        SIRTerminator::Jump(target, _) if target == second_decision
    ));
}

#[test]
fn threads_a_4096_case_spine_without_path_enumeration_or_recursion() {
    let values = (0..4096).collect::<Vec<_>>();
    let mut ladder = case_ladder(&values, None);

    ControlFlowSimplifyPass.run(&mut ladder.eu, &PassOptions::default());

    ladder.eu.verify_result().unwrap();
    assert!(matches!(
        ladder.eu.blocks[&ladder.arms[0]].terminator,
        SIRTerminator::Jump(target, _) if target == ladder.final_block
    ));
    assert!(matches!(
        ladder.eu.blocks[&ladder.arms[2048]].terminator,
        SIRTerminator::Jump(target, _) if target == ladder.final_block
    ));
}
