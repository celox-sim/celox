use super::*;
use crate::ir::{
    BasicBlock, BinaryOp, InstanceId, RegisterType, SIROffset, SIRValue, STABLE_REGION, UnaryOp,
};
use celox_design::StateObjectId as VarId;

fn bit(width: usize) -> RegisterType {
    RegisterType::Bit {
        width,
        signed: false,
    }
}

fn address(variable: u32) -> RegionedAbsoluteAddr {
    RegionedAbsoluteAddr {
        region: STABLE_REGION,
        instance_id: InstanceId(0),
        var_id: VarId::from_raw(variable),
    }
}

fn block(
    id: usize,
    instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
    terminator: SIRTerminator,
) -> BasicBlock<RegionedAbsoluteAddr> {
    BasicBlock {
        id: BlockId(id),
        params: Vec::new(),
        instructions,
        terminator,
    }
}

fn unit(
    blocks: impl IntoIterator<Item = BasicBlock<RegionedAbsoluteAddr>>,
    registers: impl IntoIterator<Item = (RegisterId, RegisterType)>,
) -> ExecutionUnit<RegionedAbsoluteAddr> {
    ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: blocks.into_iter().map(|block| (block.id, block)).collect(),
        register_map: registers.into_iter().collect(),
    }
}

#[test]
fn equal_expressions_remain_distinct_occurrences() {
    let eu = unit(
        [block(
            0,
            vec![
                SIRInstruction::Imm(RegisterId(0), SIRValue::new(7u8)),
                SIRInstruction::Imm(RegisterId(1), SIRValue::new(7u8)),
                SIRInstruction::Binary(RegisterId(2), RegisterId(0), BinaryOp::Add, RegisterId(1)),
            ],
            SIRTerminator::Return,
        )],
        [
            (RegisterId(0), bit(8)),
            (RegisterId(1), bit(8)),
            (RegisterId(2), bit(8)),
        ],
    );
    let analysis = PlacementAnalysis::analyze(&eu).unwrap();
    let first = analysis.value_for_register(RegisterId(0)).unwrap();
    let second = analysis.value_for_register(RegisterId(1)).unwrap();

    assert_ne!(first, second);
    assert_ne!(
        analysis.value(first).unwrap().origin,
        analysis.value(second).unwrap().origin
    );
    assert_eq!(
        analysis
            .value(analysis.value_for_register(RegisterId(2)).unwrap())
            .unwrap()
            .operands,
        vec![first, second]
    );
}

#[test]
fn pure_value_analysis_uses_the_forward_only_cfg() {
    let eu = unit(
        [block(
            0,
            vec![SIRInstruction::Imm(RegisterId(0), SIRValue::new(7u8))],
            SIRTerminator::Return,
        )],
        [(RegisterId(0), bit(8))],
    );
    let analysis = PlacementAnalysis::analyze_pure_values(&eu).unwrap();
    assert!(analysis.cfg.postdominators.is_none());
    assert!(analysis.cfg.controllers.iter().all(Vec::is_empty));
    assert!(analysis.effects.is_empty());
    assert!(analysis.effect_phis.is_empty());
    assert!(matches!(
        analysis
            .value(analysis.value_for_register(RegisterId(0)).unwrap())
            .unwrap()
            .safety,
        ValueSafety::Pure
    ));
}

#[test]
fn state_tokens_distinguish_loads_separated_by_a_store() {
    let state = address(0);
    let eu = unit(
        [block(
            0,
            vec![
                SIRInstruction::Imm(RegisterId(0), SIRValue::new(9u8)),
                SIRInstruction::Load(RegisterId(1), state, SIROffset::Static(0), 8),
                SIRInstruction::Store(
                    state,
                    SIROffset::Static(0),
                    8,
                    RegisterId(0),
                    vec![],
                    vec![],
                ),
                SIRInstruction::Load(RegisterId(2), state, SIROffset::Static(0), 8),
            ],
            SIRTerminator::Return,
        )],
        [
            (RegisterId(0), bit(8)),
            (RegisterId(1), bit(8)),
            (RegisterId(2), bit(8)),
        ],
    );
    let analysis = PlacementAnalysis::analyze(&eu).unwrap();
    let token = |register| match analysis
        .value(analysis.value_for_register(register).unwrap())
        .unwrap()
        .safety
    {
        ValueSafety::StateRead(token) => token,
        other => panic!("expected versioned state read, got {other:?}"),
    };

    assert_ne!(token(RegisterId(1)).version, token(RegisterId(2)).version);
}

#[test]
fn read_only_state_can_sink_to_an_unchanged_edge() {
    let input = address(0);
    let eu = unit(
        [
            block(
                0,
                vec![
                    SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8)),
                    SIRInstruction::Load(RegisterId(1), input, SIROffset::Static(0), 8),
                ],
                SIRTerminator::Branch {
                    cond: RegisterId(0),
                    true_block: (BlockId(1), vec![]),
                    false_block: (BlockId(2), vec![]),
                },
            ),
            block(
                1,
                vec![SIRInstruction::Unary(
                    RegisterId(2),
                    UnaryOp::Ident,
                    RegisterId(1),
                )],
                SIRTerminator::Return,
            ),
            block(2, vec![], SIRTerminator::Return),
        ],
        [
            (RegisterId(0), bit(1)),
            (RegisterId(1), bit(8)),
            (RegisterId(2), bit(8)),
        ],
    );
    let analysis = PlacementAnalysis::analyze(&eu).unwrap();
    let value = analysis.value_for_register(RegisterId(1)).unwrap();

    assert!(analysis.can_sink_to_edge(value, BlockId(0)));
    assert_eq!(
        analysis.sink_bounds(value).unwrap(),
        PlacementBounds {
            earliest: BlockId(0),
            latest: BlockId(1),
            legal_blocks: vec![BlockId(0), BlockId(1)],
        }
    );
}

#[test]
fn exact_state_version_can_reload_without_original_load_dominance() {
    let input = address(0);
    let eu = unit(
        [
            block(
                0,
                vec![SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8))],
                SIRTerminator::Branch {
                    cond: RegisterId(0),
                    true_block: (BlockId(1), vec![]),
                    false_block: (BlockId(2), vec![]),
                },
            ),
            block(
                1,
                vec![SIRInstruction::Load(
                    RegisterId(1),
                    input,
                    SIROffset::Static(0),
                    8,
                )],
                SIRTerminator::Jump(BlockId(3), vec![]),
            ),
            block(2, vec![], SIRTerminator::Jump(BlockId(3), vec![])),
            block(3, vec![], SIRTerminator::Return),
        ],
        [(RegisterId(0), bit(1)), (RegisterId(1), bit(8))],
    );
    let analysis = PlacementAnalysis::analyze(&eu).unwrap();
    let value = analysis.value_for_register(RegisterId(1)).unwrap();

    assert!(!analysis.can_sink_to_block(value, BlockId(3)));
    assert!(analysis.can_materialize_state_read_at_block(value, BlockId(3)));
}

#[test]
fn aggregate_ssa_frontier_is_first_block_after_all_definitions() {
    let first = address(0);
    let second = address(1);
    let eu = unit(
        [
            block(
                0,
                vec![SIRInstruction::Load(
                    RegisterId(0),
                    first,
                    SIROffset::Static(0),
                    8,
                )],
                SIRTerminator::Jump(BlockId(1), vec![]),
            ),
            block(
                1,
                vec![SIRInstruction::Load(
                    RegisterId(1),
                    second,
                    SIROffset::Static(0),
                    8,
                )],
                SIRTerminator::Jump(BlockId(2), vec![]),
            ),
            block(2, vec![], SIRTerminator::Return),
        ],
        [(RegisterId(0), bit(8)), (RegisterId(1), bit(8))],
    );
    let analysis = PlacementAnalysis::analyze(&eu).unwrap();
    let values = [
        analysis.value_for_register(RegisterId(0)).unwrap(),
        analysis.value_for_register(RegisterId(1)).unwrap(),
    ];

    assert_eq!(
        analysis.earliest_common_dominating_value_block(&values, BlockId(2)),
        Some(BlockId(1))
    );
}

#[test]
fn aggregate_state_frontier_stops_before_a_branch_write() {
    let state = address(0);
    let eu = unit(
        [
            block(
                0,
                vec![
                    SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8)),
                    SIRInstruction::Imm(RegisterId(1), SIRValue::new(9u8)),
                    SIRInstruction::Load(RegisterId(2), state, SIROffset::Static(0), 8),
                ],
                SIRTerminator::Branch {
                    cond: RegisterId(0),
                    true_block: (BlockId(1), vec![]),
                    false_block: (BlockId(2), vec![]),
                },
            ),
            block(
                1,
                vec![SIRInstruction::Store(
                    state,
                    SIROffset::Static(0),
                    8,
                    RegisterId(1),
                    vec![],
                    vec![],
                )],
                SIRTerminator::Jump(BlockId(3), vec![]),
            ),
            block(2, vec![], SIRTerminator::Jump(BlockId(3), vec![])),
            block(3, vec![], SIRTerminator::Return),
        ],
        [
            (RegisterId(0), bit(1)),
            (RegisterId(1), bit(8)),
            (RegisterId(2), bit(8)),
        ],
    );
    let analysis = PlacementAnalysis::analyze(&eu).unwrap();
    let value = analysis.value_for_register(RegisterId(2)).unwrap();

    assert!(!analysis.can_materialize_state_read_at_block(value, BlockId(3)));
    assert_eq!(
        analysis.latest_common_state_materialization_block(&[value], BlockId(3)),
        Some(BlockId(0))
    );
}

#[test]
fn read_only_state_can_sink_within_the_same_loop_iteration() {
    let input = address(0);
    let eu = unit(
        [
            block(
                0,
                vec![
                    SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8)),
                    SIRInstruction::Imm(RegisterId(1), SIRValue::new(0u8)),
                ],
                SIRTerminator::Jump(BlockId(1), vec![]),
            ),
            block(
                1,
                vec![SIRInstruction::Load(
                    RegisterId(2),
                    input,
                    SIROffset::Static(0),
                    8,
                )],
                SIRTerminator::Branch {
                    cond: RegisterId(0),
                    true_block: (BlockId(2), vec![]),
                    false_block: (BlockId(3), vec![]),
                },
            ),
            block(
                2,
                vec![SIRInstruction::Unary(
                    RegisterId(3),
                    UnaryOp::Ident,
                    RegisterId(2),
                )],
                SIRTerminator::Jump(BlockId(3), vec![]),
            ),
            block(
                3,
                vec![],
                SIRTerminator::Branch {
                    cond: RegisterId(1),
                    true_block: (BlockId(1), vec![]),
                    false_block: (BlockId(4), vec![]),
                },
            ),
            block(4, vec![], SIRTerminator::Return),
        ],
        [
            (RegisterId(0), bit(1)),
            (RegisterId(1), bit(1)),
            (RegisterId(2), bit(8)),
            (RegisterId(3), bit(8)),
        ],
    );
    let analysis = PlacementAnalysis::analyze(&eu).unwrap();
    let value = analysis.value_for_register(RegisterId(2)).unwrap();

    assert_eq!(
        analysis.sink_bounds(value).unwrap().legal_blocks,
        vec![BlockId(1), BlockId(2)]
    );
}

#[test]
fn schedule_late_does_not_move_an_outer_value_into_a_nested_loop() {
    let eu = unit(
        [
            block(
                0,
                vec![
                    SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8)),
                    SIRInstruction::Imm(RegisterId(1), SIRValue::new(0u8)),
                    SIRInstruction::Imm(RegisterId(2), SIRValue::new(7u8)),
                ],
                SIRTerminator::Jump(BlockId(1), vec![]),
            ),
            block(
                1,
                vec![SIRInstruction::Unary(
                    RegisterId(3),
                    UnaryOp::BitNot,
                    RegisterId(2),
                )],
                SIRTerminator::Jump(BlockId(2), vec![]),
            ),
            block(
                2,
                vec![],
                SIRTerminator::Branch {
                    cond: RegisterId(0),
                    true_block: (BlockId(3), vec![]),
                    false_block: (BlockId(4), vec![]),
                },
            ),
            block(
                3,
                vec![SIRInstruction::Unary(
                    RegisterId(4),
                    UnaryOp::Ident,
                    RegisterId(3),
                )],
                SIRTerminator::Jump(BlockId(2), vec![]),
            ),
            block(
                4,
                vec![],
                SIRTerminator::Branch {
                    cond: RegisterId(1),
                    true_block: (BlockId(1), vec![]),
                    false_block: (BlockId(5), vec![]),
                },
            ),
            block(5, vec![], SIRTerminator::Return),
        ],
        [
            (RegisterId(0), bit(1)),
            (RegisterId(1), bit(1)),
            (RegisterId(2), bit(8)),
            (RegisterId(3), bit(8)),
            (RegisterId(4), bit(8)),
        ],
    );
    let analysis = PlacementAnalysis::analyze(&eu).unwrap();
    let value = analysis.value_for_register(RegisterId(3)).unwrap();

    assert_eq!(
        analysis.sink_bounds(value).unwrap().legal_blocks,
        vec![BlockId(1)]
    );
}

#[test]
fn write_after_load_closes_the_state_execution_domain() {
    let state = address(0);
    let eu = unit(
        [
            block(
                0,
                vec![
                    SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8)),
                    SIRInstruction::Imm(RegisterId(1), SIRValue::new(3u8)),
                    SIRInstruction::Load(RegisterId(2), state, SIROffset::Static(0), 8),
                    SIRInstruction::Store(
                        state,
                        SIROffset::Static(0),
                        8,
                        RegisterId(1),
                        vec![],
                        vec![],
                    ),
                ],
                SIRTerminator::Branch {
                    cond: RegisterId(0),
                    true_block: (BlockId(1), vec![]),
                    false_block: (BlockId(2), vec![]),
                },
            ),
            block(
                1,
                vec![SIRInstruction::Unary(
                    RegisterId(3),
                    UnaryOp::Ident,
                    RegisterId(2),
                )],
                SIRTerminator::Return,
            ),
            block(2, vec![], SIRTerminator::Return),
        ],
        [
            (RegisterId(0), bit(1)),
            (RegisterId(1), bit(8)),
            (RegisterId(2), bit(8)),
            (RegisterId(3), bit(8)),
        ],
    );
    let analysis = PlacementAnalysis::analyze(&eu).unwrap();
    let value = analysis.value_for_register(RegisterId(2)).unwrap();

    assert!(!analysis.can_sink_to_edge(value, BlockId(0)));
    assert_eq!(
        analysis.sink_bounds(value).unwrap().legal_blocks,
        vec![BlockId(0)]
    );
}

#[test]
fn overlapping_partial_store_changes_the_load_token() {
    let state = address(0);
    let eu = unit(
        [
            block(
                0,
                vec![
                    SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8)),
                    SIRInstruction::Imm(RegisterId(1), SIRValue::new(0xaau8)),
                    SIRInstruction::Load(RegisterId(2), state, SIROffset::Static(0), 16),
                    SIRInstruction::Store(
                        state,
                        SIROffset::Static(8),
                        8,
                        RegisterId(1),
                        vec![],
                        vec![],
                    ),
                ],
                SIRTerminator::Branch {
                    cond: RegisterId(0),
                    true_block: (BlockId(1), vec![]),
                    false_block: (BlockId(2), vec![]),
                },
            ),
            block(
                1,
                vec![SIRInstruction::Unary(
                    RegisterId(3),
                    UnaryOp::Ident,
                    RegisterId(2),
                )],
                SIRTerminator::Return,
            ),
            block(2, vec![], SIRTerminator::Return),
        ],
        [
            (RegisterId(0), bit(1)),
            (RegisterId(1), bit(8)),
            (RegisterId(2), bit(16)),
            (RegisterId(3), bit(16)),
        ],
    );
    let analysis = PlacementAnalysis::analyze(&eu).unwrap();

    assert!(!analysis.can_sink_to_edge(
        analysis.value_for_register(RegisterId(2)).unwrap(),
        BlockId(0)
    ));
}

#[test]
fn schedule_late_keeps_a_value_shared_by_both_arms_in_the_head() {
    let eu = unit(
        [
            block(
                0,
                vec![
                    SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8)),
                    SIRInstruction::Imm(RegisterId(1), SIRValue::new(7u8)),
                ],
                SIRTerminator::Branch {
                    cond: RegisterId(0),
                    true_block: (BlockId(1), vec![]),
                    false_block: (BlockId(2), vec![]),
                },
            ),
            block(
                1,
                vec![SIRInstruction::Unary(
                    RegisterId(2),
                    UnaryOp::Ident,
                    RegisterId(1),
                )],
                SIRTerminator::Return,
            ),
            block(
                2,
                vec![SIRInstruction::Unary(
                    RegisterId(3),
                    UnaryOp::Ident,
                    RegisterId(1),
                )],
                SIRTerminator::Return,
            ),
        ],
        [
            (RegisterId(0), bit(1)),
            (RegisterId(1), bit(8)),
            (RegisterId(2), bit(8)),
            (RegisterId(3), bit(8)),
        ],
    );
    let analysis = PlacementAnalysis::analyze(&eu).unwrap();
    let bounds = analysis
        .sink_bounds(analysis.value_for_register(RegisterId(1)).unwrap())
        .unwrap();

    assert_eq!(bounds.latest, BlockId(0));
    assert_eq!(bounds.legal_blocks, vec![BlockId(0)]);
}

#[test]
fn merge_arguments_are_edge_uses_and_parameters_are_pinned_occurrences() {
    let mut merge = block(
        3,
        vec![SIRInstruction::Unary(
            RegisterId(3),
            UnaryOp::Ident,
            RegisterId(2),
        )],
        SIRTerminator::Return,
    );
    merge.params.push(RegisterId(2));
    let eu = unit(
        [
            block(
                0,
                vec![
                    SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8)),
                    SIRInstruction::Imm(RegisterId(1), SIRValue::new(7u8)),
                ],
                SIRTerminator::Branch {
                    cond: RegisterId(0),
                    true_block: (BlockId(1), vec![]),
                    false_block: (BlockId(2), vec![]),
                },
            ),
            block(
                1,
                vec![],
                SIRTerminator::Jump(BlockId(3), vec![RegisterId(1)]),
            ),
            block(
                2,
                vec![],
                SIRTerminator::Jump(BlockId(3), vec![RegisterId(1)]),
            ),
            merge,
        ],
        [
            (RegisterId(0), bit(1)),
            (RegisterId(1), bit(8)),
            (RegisterId(2), bit(8)),
            (RegisterId(3), bit(8)),
        ],
    );
    let analysis = PlacementAnalysis::analyze(&eu).unwrap();
    let incoming = analysis
        .value(analysis.value_for_register(RegisterId(1)).unwrap())
        .unwrap();
    let mut edge_blocks = incoming
        .uses
        .iter()
        .filter_map(|site| match site {
            ValueUse::EdgeArgument { predecessor, .. } => Some(*predecessor),
            _ => None,
        })
        .collect::<Vec<_>>();
    edge_blocks.sort_unstable();

    assert_eq!(edge_blocks, vec![BlockId(1), BlockId(2)]);
    assert_eq!(
        analysis.sink_bounds(incoming.id).unwrap().latest,
        BlockId(0)
    );
    assert_eq!(
        analysis
            .value(analysis.value_for_register(RegisterId(2)).unwrap())
            .unwrap()
            .safety,
        ValueSafety::Pinned(PinReason::BlockParameter)
    );
}

#[test]
fn observable_occurrences_are_chained_through_an_effect_phi() {
    let eu = unit(
        [
            block(
                0,
                vec![SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8))],
                SIRTerminator::Branch {
                    cond: RegisterId(0),
                    true_block: (BlockId(1), vec![]),
                    false_block: (BlockId(2), vec![]),
                },
            ),
            block(
                1,
                vec![SIRInstruction::RuntimeEvent {
                    site_id: 4,
                    args: vec![],
                }],
                SIRTerminator::Jump(BlockId(3), vec![]),
            ),
            block(2, vec![], SIRTerminator::Jump(BlockId(3), vec![])),
            block(
                3,
                vec![SIRInstruction::RuntimeEvent {
                    site_id: 5,
                    args: vec![],
                }],
                SIRTerminator::Return,
            ),
        ],
        [(RegisterId(0), bit(1))],
    );
    let analysis = PlacementAnalysis::analyze(&eu).unwrap();
    let branch_effect = analysis
        .effects
        .iter()
        .find(|effect| effect.location.block() == BlockId(1))
        .unwrap();
    let join_effect = analysis
        .effects
        .iter()
        .find(|effect| effect.location.block() == BlockId(3))
        .unwrap();

    assert_eq!(branch_effect.input, EffectToken::Entry);
    assert_eq!(branch_effect.control_domain, vec![BlockId(0)]);
    assert_eq!(join_effect.input, EffectToken::Phi(BlockId(3)));
    assert!(join_effect.control_domain.is_empty());
    assert_eq!(analysis.effect_phis.len(), 1);
    assert_eq!(analysis.effect_phis[0].block, BlockId(3));
    assert_eq!(analysis.effect_phis[0].incoming.len(), 2);
}

#[test]
fn dynamic_load_uses_an_address_wide_state_version() {
    let eu = unit(
        [block(
            0,
            vec![
                SIRInstruction::Imm(RegisterId(0), SIRValue::new(0u8)),
                SIRInstruction::Load(
                    RegisterId(1),
                    address(0),
                    SIROffset::Dynamic(RegisterId(0)),
                    8,
                ),
            ],
            SIRTerminator::Return,
        )],
        [(RegisterId(0), bit(8)), (RegisterId(1), bit(8))],
    );
    let analysis = PlacementAnalysis::analyze(&eu).unwrap();
    let value = analysis
        .value(analysis.value_for_register(RegisterId(1)).unwrap())
        .unwrap();

    let ValueSafety::StateRead(token) = value.safety else {
        panic!("dynamic load must receive a conservative state version");
    };
    assert!(token.fragment.dynamic);
}

#[test]
fn dynamic_load_does_not_sink_across_an_aliasing_store() {
    let state = address(0);
    let eu = unit(
        [
            block(
                0,
                vec![
                    SIRInstruction::Imm(RegisterId(0), SIRValue::new(0u8)),
                    SIRInstruction::Imm(RegisterId(1), SIRValue::new(1u8)),
                    SIRInstruction::Imm(RegisterId(2), SIRValue::new(9u8)),
                    SIRInstruction::Load(
                        RegisterId(3),
                        state,
                        SIROffset::Dynamic(RegisterId(0)),
                        8,
                    ),
                    SIRInstruction::Store(
                        state,
                        SIROffset::Static(0),
                        8,
                        RegisterId(2),
                        vec![],
                        vec![],
                    ),
                ],
                SIRTerminator::Branch {
                    cond: RegisterId(1),
                    true_block: (BlockId(1), vec![]),
                    false_block: (BlockId(2), vec![]),
                },
            ),
            block(
                1,
                vec![SIRInstruction::Unary(
                    RegisterId(4),
                    UnaryOp::Ident,
                    RegisterId(3),
                )],
                SIRTerminator::Return,
            ),
            block(2, vec![], SIRTerminator::Return),
        ],
        [
            (RegisterId(0), bit(8)),
            (RegisterId(1), bit(1)),
            (RegisterId(2), bit(8)),
            (RegisterId(3), bit(8)),
            (RegisterId(4), bit(8)),
        ],
    );
    let analysis = PlacementAnalysis::analyze(&eu).unwrap();
    let value = analysis.value_for_register(RegisterId(3)).unwrap();

    assert_eq!(
        analysis.sink_bounds(value).unwrap().legal_blocks,
        vec![BlockId(0)]
    );
}
