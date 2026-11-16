use super::*;
use crate::ir::{BasicBlock, InstanceId};
use celox_design::StateObjectId as VarId;

fn bit(width: usize) -> RegisterType {
    RegisterType::Bit {
        width,
        signed: false,
    }
}

fn address(_var: u32) -> RegionedAbsoluteAddr {
    address_in_instance(0)
}

fn address_in_instance(instance: usize) -> RegionedAbsoluteAddr {
    RegionedAbsoluteAddr {
        region: STABLE_REGION,
        instance_id: InstanceId(instance),
        var_id: VarId::default(),
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
fn forwards_across_a_basic_block_boundary() {
    let addr = address(0);
    let mut eu = unit(
        [
            BasicBlock {
                id: BlockId(0),
                params: vec![RegisterId(0)],
                instructions: vec![SIRInstruction::Store(
                    addr,
                    SIROffset::Static(0),
                    8,
                    RegisterId(0),
                    Vec::new(),
                    Vec::new(),
                )],
                terminator: SIRTerminator::Jump(BlockId(1), Vec::new()),
            },
            BasicBlock {
                id: BlockId(1),
                params: Vec::new(),
                instructions: vec![
                    SIRInstruction::Load(RegisterId(1), addr, SIROffset::Static(0), 8),
                    SIRInstruction::Unary(RegisterId(2), UnaryOp::Ident, RegisterId(1)),
                ],
                terminator: SIRTerminator::Return,
            },
        ],
        [
            (RegisterId(0), bit(8)),
            (RegisterId(1), bit(8)),
            (RegisterId(2), bit(8)),
        ],
    );

    assert!(forward_stable_static_slots(&mut eu));
    eu.verify_result().unwrap();
    assert!(
        eu.blocks[&BlockId(1)]
            .instructions
            .iter()
            .all(|instruction| !matches!(instruction, SIRInstruction::Load(..)))
    );
    assert!(matches!(
        eu.blocks[&BlockId(1)].instructions[0],
        SIRInstruction::Unary(_, UnaryOp::Ident, RegisterId(0))
    ));
}

#[test]
fn inserts_a_pruned_phi_at_a_diamond_join() {
    let addr = address(0);
    let mut eu = unit(
        [
            BasicBlock {
                id: BlockId(0),
                params: vec![RegisterId(0), RegisterId(1), RegisterId(2)],
                instructions: Vec::new(),
                terminator: SIRTerminator::Branch {
                    cond: RegisterId(0),
                    true_block: (BlockId(1), Vec::new()),
                    false_block: (BlockId(2), Vec::new()),
                },
            },
            BasicBlock {
                id: BlockId(1),
                params: Vec::new(),
                instructions: vec![SIRInstruction::Store(
                    addr,
                    SIROffset::Static(0),
                    8,
                    RegisterId(1),
                    Vec::new(),
                    Vec::new(),
                )],
                terminator: SIRTerminator::Jump(BlockId(3), Vec::new()),
            },
            BasicBlock {
                id: BlockId(2),
                params: Vec::new(),
                instructions: vec![SIRInstruction::Store(
                    addr,
                    SIROffset::Static(0),
                    8,
                    RegisterId(2),
                    Vec::new(),
                    Vec::new(),
                )],
                terminator: SIRTerminator::Jump(BlockId(3), Vec::new()),
            },
            BasicBlock {
                id: BlockId(3),
                params: Vec::new(),
                instructions: vec![
                    SIRInstruction::Load(RegisterId(3), addr, SIROffset::Static(0), 8),
                    SIRInstruction::Unary(RegisterId(4), UnaryOp::Ident, RegisterId(3)),
                ],
                terminator: SIRTerminator::Return,
            },
        ],
        [
            (RegisterId(0), bit(1)),
            (RegisterId(1), bit(8)),
            (RegisterId(2), bit(8)),
            (RegisterId(3), bit(8)),
            (RegisterId(4), bit(8)),
        ],
    );

    let mut promoted = eu.clone();
    assert!(promote_fused_comb_static_slots(&mut promoted).unwrap());
    promoted.verify_result().unwrap();

    assert!(forward_stable_static_slots(&mut eu));
    eu.verify_result().unwrap();
    let join_param = eu.blocks[&BlockId(3)].params[0];
    assert!(matches!(
        &eu.blocks[&BlockId(1)].terminator,
        SIRTerminator::Jump(BlockId(3), arguments) if arguments == &[RegisterId(1)]
    ));
    assert!(matches!(
        &eu.blocks[&BlockId(2)].terminator,
        SIRTerminator::Jump(BlockId(3), arguments) if arguments == &[RegisterId(2)]
    ));
    assert!(matches!(
        eu.blocks[&BlockId(3)].instructions[0],
        SIRInstruction::Unary(_, UnaryOp::Ident, source) if source == join_param
    ));
    // Both arms define this comb temporary before the join. Promotion
    // must produce the same phi and uses, while removing its state stores.
    for block in eu.blocks.values_mut() {
        block
            .instructions
            .retain(|instruction| !matches!(instruction, SIRInstruction::Store(..)));
    }
    assert_eq!(promoted.blocks, eu.blocks);
    assert_eq!(promoted.register_map, eu.register_map);
}

#[test]
fn loads_the_original_value_on_a_clean_diamond_edge() {
    let addr = address(0);
    let mut eu = unit(
        [
            BasicBlock {
                id: BlockId(0),
                params: vec![RegisterId(0), RegisterId(1)],
                instructions: Vec::new(),
                terminator: SIRTerminator::Branch {
                    cond: RegisterId(0),
                    true_block: (BlockId(1), Vec::new()),
                    false_block: (BlockId(2), Vec::new()),
                },
            },
            BasicBlock {
                id: BlockId(1),
                params: Vec::new(),
                instructions: vec![SIRInstruction::Store(
                    addr,
                    SIROffset::Static(0),
                    8,
                    RegisterId(1),
                    Vec::new(),
                    Vec::new(),
                )],
                terminator: SIRTerminator::Jump(BlockId(3), Vec::new()),
            },
            BasicBlock {
                id: BlockId(2),
                params: Vec::new(),
                instructions: Vec::new(),
                terminator: SIRTerminator::Jump(BlockId(3), Vec::new()),
            },
            BasicBlock {
                id: BlockId(3),
                params: Vec::new(),
                instructions: vec![
                    SIRInstruction::Load(RegisterId(2), addr, SIROffset::Static(0), 8),
                    SIRInstruction::Unary(RegisterId(3), UnaryOp::Ident, RegisterId(2)),
                ],
                terminator: SIRTerminator::Return,
            },
        ],
        [
            (RegisterId(0), bit(1)),
            (RegisterId(1), bit(8)),
            (RegisterId(2), bit(8)),
            (RegisterId(3), bit(8)),
        ],
    );

    assert!(forward_stable_static_slots(&mut eu));
    eu.verify_result().unwrap();
    assert!(matches!(
        eu.blocks[&BlockId(2)].instructions.as_slice(),
        [SIRInstruction::Load(_, loaded_addr, SIROffset::Static(0), 8)] if *loaded_addr == addr
    ));
}

#[test]
fn rejects_an_address_with_multiple_static_shapes() {
    let addr = address(0);
    let mut eu = unit(
        [BasicBlock {
            id: BlockId(0),
            params: vec![RegisterId(0)],
            instructions: vec![
                SIRInstruction::Store(
                    addr,
                    SIROffset::Static(0),
                    8,
                    RegisterId(0),
                    Vec::new(),
                    Vec::new(),
                ),
                SIRInstruction::Load(RegisterId(1), addr, SIROffset::Static(0), 4),
            ],
            terminator: SIRTerminator::Return,
        }],
        [(RegisterId(0), bit(8)), (RegisterId(1), bit(4))],
    );

    assert!(!forward_stable_static_slots(&mut eu));
    eu.verify_result().unwrap();
}

#[test]
fn forwards_independent_fragments_of_the_same_address() {
    let addr = address(0);
    let mut eu = unit(
        [BasicBlock {
            id: BlockId(0),
            params: vec![RegisterId(0), RegisterId(1)],
            instructions: vec![
                SIRInstruction::Store(
                    addr,
                    SIROffset::Static(0),
                    8,
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
                SIRInstruction::Load(RegisterId(2), addr, SIROffset::Static(0), 8),
                SIRInstruction::Load(RegisterId(3), addr, SIROffset::Static(8), 8),
                SIRInstruction::Unary(RegisterId(4), UnaryOp::Ident, RegisterId(2)),
                SIRInstruction::Unary(RegisterId(5), UnaryOp::Ident, RegisterId(3)),
            ],
            terminator: SIRTerminator::Return,
        }],
        (0..6).map(|register| (RegisterId(register), bit(8))),
    );

    assert!(forward_stable_static_slots(&mut eu));
    eu.verify_result().unwrap();
    assert!(
        eu.blocks[&BlockId(0)]
            .instructions
            .iter()
            .all(|instruction| !matches!(instruction, SIRInstruction::Load(..)))
    );
    assert!(matches!(
        &eu.blocks[&BlockId(0)].instructions[2..],
        [
            SIRInstruction::Unary(_, UnaryOp::Ident, RegisterId(0)),
            SIRInstruction::Unary(_, UnaryOp::Ident, RegisterId(1)),
        ]
    ));
}

#[test]
fn overlapping_fragment_store_kills_the_wider_slot() {
    let addr = address(0);
    let mut eu = unit(
        [BasicBlock {
            id: BlockId(0),
            params: vec![RegisterId(0), RegisterId(1)],
            instructions: vec![
                SIRInstruction::Store(
                    addr,
                    SIROffset::Static(0),
                    8,
                    RegisterId(0),
                    Vec::new(),
                    Vec::new(),
                ),
                SIRInstruction::Store(
                    addr,
                    SIROffset::Static(4),
                    4,
                    RegisterId(1),
                    Vec::new(),
                    Vec::new(),
                ),
                SIRInstruction::Load(RegisterId(2), addr, SIROffset::Static(0), 8),
                SIRInstruction::Unary(RegisterId(3), UnaryOp::Ident, RegisterId(2)),
            ],
            terminator: SIRTerminator::Return,
        }],
        [
            (RegisterId(0), bit(8)),
            (RegisterId(1), bit(4)),
            (RegisterId(2), bit(8)),
            (RegisterId(3), bit(8)),
        ],
    );

    assert!(!forward_stable_static_slots(&mut eu));
    eu.verify_result().unwrap();
    assert!(matches!(
        eu.blocks[&BlockId(0)].instructions[2],
        SIRInstruction::Load(..)
    ));
}

#[test]
fn commit_source_read_does_not_kill_a_stable_fragment() {
    let stable = address(0);
    let mut working = stable;
    working.region = WORKING_REGION;
    let mut eu = unit(
        [BasicBlock {
            id: BlockId(0),
            params: vec![RegisterId(0)],
            instructions: vec![
                SIRInstruction::Store(
                    stable,
                    SIROffset::Static(0),
                    8,
                    RegisterId(0),
                    Vec::new(),
                    Vec::new(),
                ),
                SIRInstruction::Commit(stable, working, SIROffset::Static(0), 8, Vec::new()),
                SIRInstruction::Load(RegisterId(1), stable, SIROffset::Static(0), 8),
                SIRInstruction::Unary(RegisterId(2), UnaryOp::Ident, RegisterId(1)),
            ],
            terminator: SIRTerminator::Return,
        }],
        [
            (RegisterId(0), bit(8)),
            (RegisterId(1), bit(8)),
            (RegisterId(2), bit(8)),
        ],
    );

    assert!(forward_stable_static_slots(&mut eu));
    eu.verify_result().unwrap();
    assert!(matches!(
        eu.blocks[&BlockId(0)].instructions[2],
        SIRInstruction::Unary(_, UnaryOp::Ident, RegisterId(0))
    ));
}

#[test]
fn promotes_eval_apply_working_round_trip_without_an_unused_seed_load() {
    let stable = address(0);
    let mut working = stable;
    working.region = WORKING_REGION;
    let mut eu = unit(
        [BasicBlock {
            id: BlockId(0),
            params: vec![RegisterId(0)],
            instructions: vec![
                SIRInstruction::Commit(stable, working, SIROffset::Static(0), 8, Vec::new()),
                SIRInstruction::Store(
                    working,
                    SIROffset::Static(0),
                    8,
                    RegisterId(0),
                    Vec::new(),
                    Vec::new(),
                ),
                SIRInstruction::Commit(working, stable, SIROffset::Static(0), 8, Vec::new()),
            ],
            terminator: SIRTerminator::Return,
        }],
        [(RegisterId(0), bit(8))],
    );

    assert!(promote_eval_apply_working_round_trips(&mut eu));
    eu.verify_result().unwrap();
    assert!(
        eu.blocks[&BlockId(0)]
            .instructions
            .iter()
            .all(|instruction| {
                !matches!(
                    instruction,
                    SIRInstruction::Load(_, address, _, _)
                        | SIRInstruction::Store(address, _, _, _, _, _)
                        if address.region == WORKING_REGION
                ) && !matches!(instruction, SIRInstruction::Commit(..))
            })
    );
    assert!(
        eu.blocks[&BlockId(0)]
            .instructions
            .iter()
            .all(|instruction| !matches!(instruction, SIRInstruction::Load(..)))
    );
    assert!(matches!(
        eu.blocks[&BlockId(0)].instructions.last(),
        Some(SIRInstruction::Store(address, SIROffset::Static(0), 8, RegisterId(0), _, _))
            if *address == stable
    ));
}

fn working_reset_diamond(seed: bool, mixed_types: bool) -> ExecutionUnit<RegionedAbsoluteAddr> {
    let stable = address(0);
    let working = RegionedAbsoluteAddr {
        region: WORKING_REGION,
        ..stable
    };
    let store = |register| {
        SIRInstruction::Store(
            working,
            SIROffset::Static(0),
            8,
            register,
            Vec::new(),
            Vec::new(),
        )
    };
    unit(
        [
            BasicBlock {
                id: BlockId(0),
                params: vec![RegisterId(0), RegisterId(1), RegisterId(2)],
                instructions: if seed {
                    vec![SIRInstruction::Commit(
                        stable,
                        working,
                        SIROffset::Static(0),
                        8,
                        Vec::new(),
                    )]
                } else {
                    Vec::new()
                },
                terminator: SIRTerminator::Branch {
                    cond: RegisterId(0),
                    true_block: (BlockId(1), Vec::new()),
                    false_block: (BlockId(2), Vec::new()),
                },
            },
            BasicBlock {
                id: BlockId(1),
                params: Vec::new(),
                instructions: vec![store(RegisterId(1))],
                terminator: SIRTerminator::Jump(BlockId(3), Vec::new()),
            },
            BasicBlock {
                id: BlockId(2),
                params: Vec::new(),
                instructions: vec![store(RegisterId(2))],
                terminator: SIRTerminator::Jump(BlockId(3), Vec::new()),
            },
            BasicBlock {
                id: BlockId(3),
                params: Vec::new(),
                instructions: vec![
                    // Another FF must still see pre-edge state at this point.
                    SIRInstruction::Load(RegisterId(3), stable, SIROffset::Static(0), 8),
                    SIRInstruction::Store(
                        address_in_instance(1),
                        SIROffset::Static(0),
                        8,
                        RegisterId(3),
                        Vec::new(),
                        Vec::new(),
                    ),
                    SIRInstruction::Commit(working, stable, SIROffset::Static(0), 8, Vec::new()),
                ],
                terminator: SIRTerminator::Return,
            },
        ],
        [
            (RegisterId(0), bit(1)),
            (RegisterId(1), bit(8)),
            (
                RegisterId(2),
                if mixed_types {
                    RegisterType::Logic { width: 8 }
                } else {
                    bit(8)
                },
            ),
            (RegisterId(3), bit(8)),
        ],
    )
}

#[test]
fn promotes_fully_defined_working_joins_after_seed_dse_and_type_mixing() {
    for (seed, mixed_types) in [(false, false), (true, true), (false, true)] {
        let mut eu = working_reset_diamond(seed, mixed_types);
        eu.verify_result().unwrap();
        assert!(
            promote_eval_apply_working_round_trips(&mut eu),
            "seed={seed}, mixed_types={mixed_types}"
        );
        eu.verify_result().unwrap();
        assert!(eu.blocks.values().flat_map(|block| &block.instructions).all(|instruction| !matches!(instruction,
                SIRInstruction::Load(_, address, ..) | SIRInstruction::Store(address, ..) if address.region == WORKING_REGION
            ) && !matches!(instruction, SIRInstruction::Commit(..))));
        let join = &eu.blocks[&BlockId(3)].instructions;
        let old_read = join.iter().position(|instruction| matches!(instruction, SIRInstruction::Load(_, address, ..) if *address == address_in_instance(0))).unwrap();
        let publish = join.iter().position(|instruction| matches!(instruction, SIRInstruction::Store(address, ..) if *address == address_in_instance(0))).unwrap();
        assert!(old_read < publish);
    }
}

#[test]
fn keeps_seedless_working_storage_when_a_path_has_no_definition() {
    let mut eu = working_reset_diamond(false, true);
    eu.blocks.get_mut(&BlockId(2)).unwrap().instructions.clear();
    let before = eu.clone();
    assert!(!promote_eval_apply_working_round_trips(&mut eu));
    assert_eq!(eu.blocks, before.blocks);
    assert_eq!(eu.register_map, before.register_map);
}

#[test]
fn eval_apply_writeback_eliminates_a_merge_only_phi() {
    let stable = address(0);
    let mut working = stable;
    working.region = WORKING_REGION;
    let mut eu = unit(
        [
            BasicBlock {
                id: BlockId(0),
                params: vec![RegisterId(0), RegisterId(1)],
                instructions: vec![SIRInstruction::Commit(
                    stable,
                    working,
                    SIROffset::Static(0),
                    8,
                    Vec::new(),
                )],
                terminator: SIRTerminator::Branch {
                    cond: RegisterId(0),
                    true_block: (BlockId(1), Vec::new()),
                    false_block: (BlockId(2), Vec::new()),
                },
            },
            BasicBlock {
                id: BlockId(1),
                params: Vec::new(),
                instructions: vec![SIRInstruction::Store(
                    working,
                    SIROffset::Static(0),
                    8,
                    RegisterId(1),
                    Vec::new(),
                    Vec::new(),
                )],
                terminator: SIRTerminator::Jump(BlockId(3), Vec::new()),
            },
            BasicBlock {
                id: BlockId(2),
                params: Vec::new(),
                instructions: Vec::new(),
                terminator: SIRTerminator::Jump(BlockId(3), Vec::new()),
            },
            BasicBlock {
                id: BlockId(3),
                params: Vec::new(),
                instructions: vec![SIRInstruction::Commit(
                    working,
                    stable,
                    SIROffset::Static(0),
                    8,
                    Vec::new(),
                )],
                terminator: SIRTerminator::Return,
            },
        ],
        [(RegisterId(0), bit(1)), (RegisterId(1), bit(8))],
    );

    assert!(promote_eval_apply_working_round_trips(&mut eu));
    eu.verify_result().unwrap();
    assert!(eu.blocks[&BlockId(3)].params.is_empty());
    assert!(eu.blocks[&BlockId(3)].instructions.is_empty());
    assert!(matches!(
        eu.blocks[&BlockId(1)].instructions.as_slice(),
        [SIRInstruction::Store(address, SIROffset::Static(0), 8, RegisterId(1), _, _)]
            if *address == stable
    ));
    assert!(eu.blocks[&BlockId(2)].instructions.is_empty());
}

#[test]
fn writeback_keeps_an_unchanged_stable_value_when_it_has_triggers() {
    let stable = address(0);
    let trigger = TriggerIdWithKind {
        kind: DomainKind::Other,
        id: 7,
    };
    let mut eu = unit(
        [
            BasicBlock {
                id: BlockId(0),
                params: vec![RegisterId(0), RegisterId(1)],
                instructions: Vec::new(),
                terminator: SIRTerminator::Branch {
                    cond: RegisterId(0),
                    true_block: (BlockId(1), Vec::new()),
                    false_block: (BlockId(2), Vec::new()),
                },
            },
            BasicBlock {
                id: BlockId(1),
                params: Vec::new(),
                instructions: Vec::new(),
                terminator: SIRTerminator::Jump(BlockId(3), vec![RegisterId(1)]),
            },
            BasicBlock {
                id: BlockId(2),
                params: Vec::new(),
                instructions: vec![SIRInstruction::Load(
                    RegisterId(2),
                    stable,
                    SIROffset::Static(0),
                    8,
                )],
                terminator: SIRTerminator::Jump(BlockId(3), vec![RegisterId(2)]),
            },
            BasicBlock {
                id: BlockId(3),
                params: vec![RegisterId(3)],
                instructions: vec![SIRInstruction::Store(
                    stable,
                    SIROffset::Static(0),
                    8,
                    RegisterId(3),
                    vec![trigger],
                    Vec::new(),
                )],
                terminator: SIRTerminator::Return,
            },
        ],
        [
            (RegisterId(0), bit(1)),
            (RegisterId(1), bit(8)),
            (RegisterId(2), bit(8)),
            (RegisterId(3), bit(8)),
        ],
    );
    let mut passthroughs = HashMap::default();
    passthroughs.insert(
        RegisterId(2),
        StateFragment::from_access(stable, 0, 8, &bit(8)),
    );

    assert!(sink_phi_writebacks_to_predecessors(&mut eu, &passthroughs));
    eu.verify_result().unwrap();
    assert!(matches!(
        eu.blocks[&BlockId(2)].instructions.as_slice(),
        [
            SIRInstruction::Load(RegisterId(2), _, SIROffset::Static(0), 8),
            SIRInstruction::Store(_, SIROffset::Static(0), 8, RegisterId(2), triggers, _),
        ] if triggers == &[trigger]
    ));
}

#[test]
fn writeback_motion_preserves_a_phi_used_as_a_dynamic_load_offset() {
    let writeback = address_in_instance(0);
    let indexed = address_in_instance(1);
    let mut eu = unit(
        [
            BasicBlock {
                id: BlockId(0),
                params: vec![RegisterId(0), RegisterId(1), RegisterId(2)],
                instructions: Vec::new(),
                terminator: SIRTerminator::Branch {
                    cond: RegisterId(0),
                    true_block: (BlockId(1), Vec::new()),
                    false_block: (BlockId(2), Vec::new()),
                },
            },
            BasicBlock {
                id: BlockId(1),
                params: Vec::new(),
                instructions: Vec::new(),
                terminator: SIRTerminator::Jump(BlockId(3), vec![RegisterId(1)]),
            },
            BasicBlock {
                id: BlockId(2),
                params: Vec::new(),
                instructions: Vec::new(),
                terminator: SIRTerminator::Jump(BlockId(3), vec![RegisterId(2)]),
            },
            BasicBlock {
                id: BlockId(3),
                params: vec![RegisterId(3)],
                instructions: vec![
                    SIRInstruction::Load(
                        RegisterId(4),
                        indexed,
                        SIROffset::Dynamic(RegisterId(3)),
                        1,
                    ),
                    SIRInstruction::Store(
                        writeback,
                        SIROffset::Static(0),
                        8,
                        RegisterId(3),
                        Vec::new(),
                        Vec::new(),
                    ),
                ],
                terminator: SIRTerminator::Return,
            },
        ],
        [
            (RegisterId(0), bit(1)),
            (RegisterId(1), bit(8)),
            (RegisterId(2), bit(8)),
            (RegisterId(3), bit(8)),
            (RegisterId(4), bit(1)),
        ],
    );

    eu.verify_result().unwrap();
    assert!(!sink_phi_writebacks_to_predecessors(
        &mut eu,
        &HashMap::default()
    ));
    eu.verify_result().unwrap();
}

#[test]
fn forwards_after_an_overlapping_kill_is_exactly_redefined() {
    let addr = address(0);
    let mut eu = unit(
        [BasicBlock {
            id: BlockId(0),
            params: vec![RegisterId(0), RegisterId(1), RegisterId(2)],
            instructions: vec![
                SIRInstruction::Store(
                    addr,
                    SIROffset::Static(0),
                    8,
                    RegisterId(0),
                    Vec::new(),
                    Vec::new(),
                ),
                SIRInstruction::Store(
                    addr,
                    SIROffset::Static(4),
                    4,
                    RegisterId(1),
                    Vec::new(),
                    Vec::new(),
                ),
                SIRInstruction::Store(
                    addr,
                    SIROffset::Static(0),
                    8,
                    RegisterId(2),
                    Vec::new(),
                    Vec::new(),
                ),
                SIRInstruction::Load(RegisterId(3), addr, SIROffset::Static(0), 8),
                SIRInstruction::Unary(RegisterId(4), UnaryOp::Ident, RegisterId(3)),
            ],
            terminator: SIRTerminator::Return,
        }],
        [
            (RegisterId(0), bit(8)),
            (RegisterId(1), bit(4)),
            (RegisterId(2), bit(8)),
            (RegisterId(3), bit(8)),
            (RegisterId(4), bit(8)),
        ],
    );

    assert!(forward_stable_static_slots(&mut eu));
    eu.verify_result().unwrap();
    assert!(matches!(
        eu.blocks[&BlockId(0)].instructions.last(),
        Some(SIRInstruction::Unary(_, UnaryOp::Ident, RegisterId(2)))
    ));
}

#[test]
fn path_local_kill_materializes_only_the_killed_phi_edge() {
    let addr = address(0);
    let mut eu = unit(
        [
            BasicBlock {
                id: BlockId(0),
                params: vec![RegisterId(0), RegisterId(1), RegisterId(2)],
                instructions: vec![SIRInstruction::Store(
                    addr,
                    SIROffset::Static(0),
                    8,
                    RegisterId(1),
                    Vec::new(),
                    Vec::new(),
                )],
                terminator: SIRTerminator::Branch {
                    cond: RegisterId(0),
                    true_block: (BlockId(1), Vec::new()),
                    false_block: (BlockId(2), Vec::new()),
                },
            },
            BasicBlock {
                id: BlockId(1),
                params: Vec::new(),
                instructions: vec![SIRInstruction::Store(
                    addr,
                    SIROffset::Static(4),
                    4,
                    RegisterId(2),
                    Vec::new(),
                    Vec::new(),
                )],
                terminator: SIRTerminator::Jump(BlockId(3), Vec::new()),
            },
            BasicBlock {
                id: BlockId(2),
                params: Vec::new(),
                instructions: Vec::new(),
                terminator: SIRTerminator::Jump(BlockId(3), Vec::new()),
            },
            BasicBlock {
                id: BlockId(3),
                params: Vec::new(),
                instructions: vec![
                    SIRInstruction::Load(RegisterId(3), addr, SIROffset::Static(0), 8),
                    SIRInstruction::Unary(RegisterId(4), UnaryOp::Ident, RegisterId(3)),
                ],
                terminator: SIRTerminator::Return,
            },
        ],
        [
            (RegisterId(0), bit(1)),
            (RegisterId(1), bit(8)),
            (RegisterId(2), bit(4)),
            (RegisterId(3), bit(8)),
            (RegisterId(4), bit(8)),
        ],
    );

    assert!(forward_stable_static_slots(&mut eu));
    eu.verify_result().unwrap();
    assert!(matches!(
        eu.blocks[&BlockId(1)].instructions.as_slice(),
        [SIRInstruction::Store(..), SIRInstruction::Load(_, loaded, SIROffset::Static(0), 8)]
            if *loaded == addr
    ));
    assert!(eu.blocks[&BlockId(2)].instructions.is_empty());
    assert_eq!(eu.blocks[&BlockId(3)].params.len(), 1);
    assert!(matches!(
        eu.blocks[&BlockId(3)].instructions.as_slice(),
        [SIRInstruction::Unary(_, UnaryOp::Ident, source)]
            if *source == eu.blocks[&BlockId(3)].params[0]
    ));
}

#[test]
fn promotes_multiple_disjoint_working_fragments() {
    let stable = address(0);
    let mut working = stable;
    working.region = WORKING_REGION;
    let mut eu = unit(
        [BasicBlock {
            id: BlockId(0),
            params: vec![RegisterId(0), RegisterId(1)],
            instructions: vec![
                SIRInstruction::Commit(stable, working, SIROffset::Static(0), 8, Vec::new()),
                SIRInstruction::Commit(stable, working, SIROffset::Static(8), 8, Vec::new()),
                SIRInstruction::Store(
                    working,
                    SIROffset::Static(0),
                    8,
                    RegisterId(0),
                    Vec::new(),
                    Vec::new(),
                ),
                SIRInstruction::Store(
                    working,
                    SIROffset::Static(8),
                    8,
                    RegisterId(1),
                    Vec::new(),
                    Vec::new(),
                ),
                SIRInstruction::Commit(working, stable, SIROffset::Static(0), 8, Vec::new()),
                SIRInstruction::Commit(working, stable, SIROffset::Static(8), 8, Vec::new()),
            ],
            terminator: SIRTerminator::Return,
        }],
        [(RegisterId(0), bit(8)), (RegisterId(1), bit(8))],
    );

    assert!(promote_eval_apply_working_round_trips(&mut eu));
    eu.verify_result().unwrap();
    assert_eq!(eu.blocks[&BlockId(0)].instructions.len(), 2);
    assert!(eu.blocks[&BlockId(0)].instructions.iter().all(|instruction| {
            matches!(instruction, SIRInstruction::Store(addr, _, 8, _, _, _) if addr.region == STABLE_REGION)
        }));
}

#[test]
fn promotes_overlapping_full_and_partial_working_stores_by_atomic_range() {
    let stable = address(0);
    let mut working = stable;
    working.region = WORKING_REGION;
    let mut eu = unit(
        [
            BasicBlock {
                id: BlockId(0),
                params: vec![RegisterId(0), RegisterId(1), RegisterId(2)],
                instructions: vec![SIRInstruction::Commit(
                    stable,
                    working,
                    SIROffset::Static(0),
                    8,
                    Vec::new(),
                )],
                terminator: SIRTerminator::Branch {
                    cond: RegisterId(0),
                    true_block: (BlockId(1), Vec::new()),
                    false_block: (BlockId(2), Vec::new()),
                },
            },
            BasicBlock {
                id: BlockId(1),
                params: Vec::new(),
                instructions: vec![SIRInstruction::Store(
                    working,
                    SIROffset::Static(2),
                    3,
                    RegisterId(1),
                    Vec::new(),
                    Vec::new(),
                )],
                terminator: SIRTerminator::Jump(BlockId(3), Vec::new()),
            },
            BasicBlock {
                id: BlockId(2),
                params: Vec::new(),
                instructions: vec![SIRInstruction::Store(
                    working,
                    SIROffset::Static(0),
                    8,
                    RegisterId(2),
                    Vec::new(),
                    Vec::new(),
                )],
                terminator: SIRTerminator::Jump(BlockId(3), Vec::new()),
            },
            BasicBlock {
                id: BlockId(3),
                params: Vec::new(),
                instructions: vec![SIRInstruction::Commit(
                    working,
                    stable,
                    SIROffset::Static(0),
                    8,
                    Vec::new(),
                )],
                terminator: SIRTerminator::Return,
            },
        ],
        [
            (RegisterId(0), bit(1)),
            (RegisterId(1), bit(3)),
            (RegisterId(2), bit(8)),
        ],
    );

    assert!(promote_eval_apply_working_round_trips(&mut eu));
    eu.verify_result().unwrap();
    assert!(eu.blocks.values().all(|block| {
        block.instructions.iter().all(|instruction| {
            !matches!(
                instruction,
                SIRInstruction::Load(_, address, _, _)
                    | SIRInstruction::Store(address, _, _, _, _, _)
                    if address.region == WORKING_REGION
            ) && !matches!(instruction, SIRInstruction::Commit(..))
        })
    }));
    assert!(eu.blocks.values().any(|block| {
        block.instructions.iter().any(|instruction| {
            matches!(
                instruction,
                SIRInstruction::Store(address, SIROffset::Static(2), 3, _, _, _)
                    if *address == stable
            )
        })
    }));
}

#[test]
fn promotion_preserves_an_old_stable_read_until_apply() {
    let stable = address(0);
    let mut working = stable;
    working.region = WORKING_REGION;
    let mut eu = unit(
        [BasicBlock {
            id: BlockId(0),
            params: vec![RegisterId(0)],
            instructions: vec![
                SIRInstruction::Commit(stable, working, SIROffset::Static(0), 8, Vec::new()),
                SIRInstruction::Store(
                    working,
                    SIROffset::Static(0),
                    8,
                    RegisterId(0),
                    Vec::new(),
                    Vec::new(),
                ),
                SIRInstruction::Load(RegisterId(1), stable, SIROffset::Static(0), 8),
                SIRInstruction::Unary(RegisterId(2), UnaryOp::Ident, RegisterId(1)),
                SIRInstruction::Commit(working, stable, SIROffset::Static(0), 8, Vec::new()),
            ],
            terminator: SIRTerminator::Return,
        }],
        [
            (RegisterId(0), bit(8)),
            (RegisterId(1), bit(8)),
            (RegisterId(2), bit(8)),
        ],
    );

    assert!(promote_eval_apply_working_round_trips(&mut eu));
    eu.verify_result().unwrap();
    let instructions = &eu.blocks[&BlockId(0)].instructions;
    let old_read = instructions
            .iter()
            .position(|instruction| {
                matches!(instruction, SIRInstruction::Load(RegisterId(1), addr, _, 8) if *addr == stable)
            })
            .unwrap();
    let apply = instructions
            .iter()
            .position(|instruction| {
                matches!(instruction, SIRInstruction::Store(addr, _, 8, RegisterId(0), _, _) if *addr == stable)
            })
            .unwrap();
    assert!(old_read < apply);
}

#[test]
fn effectful_working_store_rejects_promotion_without_mutation() {
    let stable = address(0);
    let mut working = stable;
    working.region = WORKING_REGION;
    let mut eu = unit(
        [BasicBlock {
            id: BlockId(0),
            params: vec![RegisterId(0)],
            instructions: vec![
                SIRInstruction::Commit(stable, working, SIROffset::Static(0), 8, Vec::new()),
                SIRInstruction::Store(
                    working,
                    SIROffset::Static(0),
                    8,
                    RegisterId(0),
                    Vec::new(),
                    vec![7],
                ),
                SIRInstruction::Commit(working, stable, SIROffset::Static(0), 8, Vec::new()),
            ],
            terminator: SIRTerminator::Return,
        }],
        [(RegisterId(0), RegisterType::Logic { width: 8 })],
    );
    let before = eu.blocks.clone();
    let registers_before = eu.register_map.clone();

    assert!(!promote_eval_apply_working_round_trips(&mut eu));
    assert_eq!(eu.blocks, before);
    assert_eq!(eu.register_map, registers_before);
}

#[test]
fn promotes_loop_carried_four_state_working_value() {
    let stable = address(0);
    let mut working = stable;
    working.region = WORKING_REGION;
    let logic = RegisterType::Logic { width: 8 };
    let mut eu = unit(
        [
            BasicBlock {
                id: BlockId(0),
                params: vec![RegisterId(0), RegisterId(1)],
                instructions: vec![SIRInstruction::Commit(
                    stable,
                    working,
                    SIROffset::Static(0),
                    8,
                    Vec::new(),
                )],
                terminator: SIRTerminator::Jump(BlockId(1), Vec::new()),
            },
            BasicBlock {
                id: BlockId(1),
                params: Vec::new(),
                instructions: vec![
                    SIRInstruction::Load(RegisterId(2), working, SIROffset::Static(0), 8),
                    SIRInstruction::Unary(RegisterId(3), UnaryOp::Ident, RegisterId(2)),
                ],
                terminator: SIRTerminator::Branch {
                    cond: RegisterId(0),
                    true_block: (BlockId(2), Vec::new()),
                    false_block: (BlockId(3), Vec::new()),
                },
            },
            BasicBlock {
                id: BlockId(2),
                params: Vec::new(),
                instructions: vec![SIRInstruction::Store(
                    working,
                    SIROffset::Static(0),
                    8,
                    RegisterId(1),
                    Vec::new(),
                    Vec::new(),
                )],
                terminator: SIRTerminator::Jump(BlockId(1), Vec::new()),
            },
            BasicBlock {
                id: BlockId(3),
                params: Vec::new(),
                instructions: vec![SIRInstruction::Commit(
                    working,
                    stable,
                    SIROffset::Static(0),
                    8,
                    Vec::new(),
                )],
                terminator: SIRTerminator::Return,
            },
        ],
        [
            (RegisterId(0), bit(1)),
            (RegisterId(1), logic.clone()),
            (RegisterId(2), logic.clone()),
            (RegisterId(3), logic),
        ],
    );

    assert!(promote_eval_apply_working_round_trips(&mut eu));
    eu.verify_result().unwrap();
    assert!(eu.blocks[&BlockId(1)].params.len() == 1);
    assert!(
        eu.blocks
            .values()
            .all(|block| block.instructions.iter().all(|instruction| {
                !matches!(
                    instruction,
                    SIRInstruction::Load(_, addr, _, _)
                        | SIRInstruction::Store(addr, _, _, _, _, _)
                        if addr.region == WORKING_REGION
                ) && !matches!(instruction, SIRInstruction::Commit(..))
            }))
    );
}
