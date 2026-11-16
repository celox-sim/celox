use super::*;
use crate::HashMap;
use celox_design::StateObjectId as VarId;

fn addr(region: u32) -> RegionedAbsoluteAddr {
    RegionedAbsoluteAddr {
        region,
        instance_id: InstanceId(0),
        var_id: VarId::from_raw(0),
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

fn sparse_store() -> SIRInstruction<RegionedAbsoluteAddr> {
    SIRInstruction::Store(
        addr(SPARSE_WORKING_REGION),
        SIROffset::Element {
            index: RegisterId(0),
            element_width: 8,
            bit_offset: 0,
            dynamic_bit_offset: None,
        },
        8,
        RegisterId(1),
        Vec::new(),
        Vec::new(),
    )
}

fn sparse_commit() -> SIRInstruction<RegionedAbsoluteAddr> {
    SIRInstruction::Commit(
        addr(SPARSE_WORKING_REGION),
        addr(STABLE_REGION),
        SIROffset::Static(0),
        64,
        Vec::new(),
    )
}

fn working_seed(offset: SIROffset, bits: usize) -> SIRInstruction<RegionedAbsoluteAddr> {
    SIRInstruction::Commit(
        addr(STABLE_REGION),
        addr(WORKING_REGION),
        offset,
        bits,
        Vec::new(),
    )
}

fn working_apply(offset: SIROffset, bits: usize) -> SIRInstruction<RegionedAbsoluteAddr> {
    SIRInstruction::Commit(
        addr(WORKING_REGION),
        addr(STABLE_REGION),
        offset,
        bits,
        Vec::new(),
    )
}

fn indexed_working_store() -> SIRInstruction<RegionedAbsoluteAddr> {
    SIRInstruction::Store(
        addr(WORKING_REGION),
        SIROffset::Element {
            index: RegisterId(0),
            element_width: 8,
            bit_offset: 0,
            dynamic_bit_offset: None,
        },
        8,
        RegisterId(1),
        Vec::new(),
        Vec::new(),
    )
}

fn eu(blocks: Vec<BasicBlock<RegionedAbsoluteAddr>>) -> ExecutionUnit<RegionedAbsoluteAddr> {
    let mut register_map = HashMap::default();
    register_map.insert(
        RegisterId(0),
        RegisterType::Bit {
            width: 3,
            signed: false,
        },
    );
    register_map.insert(RegisterId(1), RegisterType::Logic { width: 8 });
    register_map.insert(RegisterId(2), RegisterType::Logic { width: 8 });
    register_map.insert(
        RegisterId(3),
        RegisterType::Bit {
            width: 1,
            signed: false,
        },
    );
    ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: blocks.into_iter().map(|block| (block.id, block)).collect(),
        register_map,
    }
}

#[test]
fn indexed_working_round_trip_is_redirected_when_the_event_has_no_hazard() {
    let mut unit = eu(vec![block(
        0,
        vec![
            working_seed(SIROffset::Static(0), 64),
            indexed_working_store(),
            working_apply(SIROffset::Static(0), 64),
        ],
        SIRTerminator::Return,
    )]);

    eliminate_working_round_trip(&mut unit, &[]);

    assert!(matches!(
        unit.blocks[&BlockId(0)].instructions.as_slice(),
        [SIRInstruction::Store(address, SIROffset::Element { .. }, ..)]
            if address.region == STABLE_REGION
    ));
}

#[test]
fn fully_defined_working_slot_does_not_require_a_seed() {
    let mut unit = eu(vec![block(
        0,
        vec![
            indexed_working_store(),
            working_apply(SIROffset::Static(0), 64),
        ],
        SIRTerminator::Return,
    )]);

    eliminate_working_round_trip(&mut unit, &[]);

    assert!(matches!(
        unit.blocks[&BlockId(0)].instructions.as_slice(),
        [SIRInstruction::Store(address, SIROffset::Element { .. }, ..)]
            if address.region == STABLE_REGION
    ));
}

#[test]
fn unseeded_working_load_keeps_the_private_slot() {
    let mut unit = eu(vec![block(
        0,
        vec![
            SIRInstruction::Load(RegisterId(2), addr(WORKING_REGION), SIROffset::Static(0), 8),
            indexed_working_store(),
            working_apply(SIROffset::Static(0), 64),
        ],
        SIRTerminator::Return,
    )]);

    eliminate_working_round_trip(&mut unit, &[]);

    assert!(matches!(
        unit.blocks[&BlockId(0)].instructions.as_slice(),
        [
            SIRInstruction::Load(_, address, ..),
            SIRInstruction::Store(..),
            SIRInstruction::Commit(..)
        ] if address.region == WORKING_REGION
    ));
}

#[test]
fn indexed_working_round_trip_stays_private_across_an_old_stable_read() {
    let mut unit = eu(vec![block(
        0,
        vec![
            working_seed(SIROffset::Static(0), 64),
            indexed_working_store(),
            SIRInstruction::Load(RegisterId(2), addr(STABLE_REGION), SIROffset::Static(0), 8),
            working_apply(SIROffset::Static(0), 64),
        ],
        SIRTerminator::Return,
    )]);

    eliminate_working_round_trip(&mut unit, &[]);

    assert!(matches!(
        unit.blocks[&BlockId(0)].instructions.as_slice(),
        [
            SIRInstruction::Commit(..),
            SIRInstruction::Store(address, SIROffset::Element { .. }, ..),
            SIRInstruction::Load(..),
            SIRInstruction::Commit(..),
        ] if address.region == WORKING_REGION
    ));
}

#[test]
fn dynamically_addressed_seed_and_apply_are_not_removed() {
    let dynamic = || SIROffset::Element {
        index: RegisterId(0),
        element_width: 8,
        bit_offset: 0,
        dynamic_bit_offset: None,
    };
    let mut unit = eu(vec![block(
        0,
        vec![
            working_seed(dynamic(), 8),
            indexed_working_store(),
            working_apply(dynamic(), 8),
        ],
        SIRTerminator::Return,
    )]);

    eliminate_working_round_trip(&mut unit, &[]);

    assert_eq!(unit.blocks[&BlockId(0)].instructions.len(), 3);
    assert!(matches!(
        unit.blocks[&BlockId(0)].instructions[1],
        SIRInstruction::Store(address, ..) if address.region == WORKING_REGION
    ));
}

#[test]
fn indexed_sparse_round_trip_is_redirected_when_the_event_has_no_hazard() {
    let mut unit = eu(vec![
        block(
            0,
            vec![
                SIRInstruction::Load(RegisterId(2), addr(STABLE_REGION), SIROffset::Static(0), 8),
                sparse_store(),
            ],
            SIRTerminator::Jump(BlockId(1), Vec::new()),
        ),
        block(1, vec![sparse_commit()], SIRTerminator::Return),
    ]);

    eliminate_working_round_trip(&mut unit, &[BlockId(1)]);

    assert!(matches!(
        unit.blocks[&BlockId(0)].instructions.as_slice(),
        [SIRInstruction::Load(..), SIRInstruction::Store(address, SIROffset::Element { .. }, ..)]
            if address.region == STABLE_REGION
    ));
    assert!(unit.blocks[&BlockId(1)].instructions.is_empty());
}

#[test]
fn indexed_sparse_round_trip_stays_private_across_an_old_stable_read() {
    let mut unit = eu(vec![
        block(
            0,
            vec![sparse_store()],
            SIRTerminator::Jump(BlockId(1), Vec::new()),
        ),
        block(
            1,
            vec![
                SIRInstruction::Load(RegisterId(2), addr(STABLE_REGION), SIROffset::Static(0), 8),
                sparse_commit(),
            ],
            SIRTerminator::Return,
        ),
    ]);

    eliminate_working_round_trip(&mut unit, &[BlockId(1)]);

    assert!(matches!(
        unit.blocks[&BlockId(0)].instructions[0],
        SIRInstruction::Store(address, ..) if address.region == SPARSE_WORKING_REGION
    ));
    assert!(matches!(
        unit.blocks[&BlockId(1)].instructions[1],
        SIRInstruction::Commit(..)
    ));
}

#[test]
fn sparse_round_trip_preserves_order_across_two_evaluators() {
    let mut unit = eu(vec![
        block(
            0,
            vec![sparse_store()],
            SIRTerminator::Jump(BlockId(1), Vec::new()),
        ),
        block(
            1,
            vec![sparse_store()],
            SIRTerminator::Jump(BlockId(2), Vec::new()),
        ),
        block(2, vec![sparse_commit()], SIRTerminator::Return),
    ]);

    eliminate_working_round_trip(&mut unit, &[BlockId(1), BlockId(2)]);

    assert!(matches!(
        unit.blocks[&BlockId(0)].instructions[0],
        SIRInstruction::Store(address, ..) if address.region == STABLE_REGION
    ));
    assert!(matches!(
        unit.blocks[&BlockId(1)].instructions[0],
        SIRInstruction::Store(address, ..) if address.region == STABLE_REGION
    ));
    assert!(unit.blocks[&BlockId(2)].instructions.is_empty());
}

#[test]
fn ordinary_round_trip_preserves_store_order_across_two_evaluators() {
    let store = |source| {
        SIRInstruction::Store(
            addr(WORKING_REGION),
            SIROffset::Static(0),
            8,
            source,
            Vec::new(),
            Vec::new(),
        )
    };
    let mut unit = eu(vec![
        block(
            0,
            vec![working_seed(SIROffset::Static(0), 8), store(RegisterId(1))],
            SIRTerminator::Jump(BlockId(1), Vec::new()),
        ),
        block(
            1,
            vec![store(RegisterId(2)), working_apply(SIROffset::Static(0), 8)],
            SIRTerminator::Return,
        ),
    ]);

    eliminate_working_round_trip(&mut unit, &[BlockId(1)]);

    assert!(matches!(
        unit.blocks[&BlockId(0)].instructions.as_slice(),
        [SIRInstruction::Store(address, _, 8, RegisterId(1), _, _)]
            if address.region == STABLE_REGION
    ));
    assert!(matches!(
        unit.blocks[&BlockId(1)].instructions.as_slice(),
        [SIRInstruction::Store(address, _, 8, RegisterId(2), _, _)]
            if address.region == STABLE_REGION
    ));
}

#[test]
fn sparse_round_trip_is_not_redirected_on_an_unpublished_exit_path() {
    let mut unit = eu(vec![
        block(
            0,
            Vec::new(),
            SIRTerminator::Branch {
                cond: RegisterId(3),
                true_block: (BlockId(1), Vec::new()),
                false_block: (BlockId(2), Vec::new()),
            },
        ),
        block(1, vec![sparse_store()], SIRTerminator::Return),
        block(2, vec![sparse_commit()], SIRTerminator::Return),
    ]);

    eliminate_working_round_trip(&mut unit, &[BlockId(2)]);

    assert!(matches!(
        unit.blocks[&BlockId(1)].instructions[0],
        SIRInstruction::Store(address, ..) if address.region == SPARSE_WORKING_REGION
    ));
    assert!(matches!(
        unit.blocks[&BlockId(2)].instructions[0],
        SIRInstruction::Commit(..)
    ));
}
#[test]
fn partial_event_keeps_publication_of_another_producers_fragment() {
    let mut unit = eu(vec![block(
        0,
        vec![
            working_seed(SIROffset::Static(24), 8),
            SIRInstruction::Store(
                addr(WORKING_REGION),
                SIROffset::Static(24),
                8,
                RegisterId(1),
                Vec::new(),
                Vec::new(),
            ),
            working_apply(SIROffset::Static(16), 8),
            working_apply(SIROffset::Static(24), 8),
        ],
        SIRTerminator::Return,
    )]);
    let before = unit.clone();
    eliminate_partial_working_round_trip(&mut unit);
    assert_eq!(unit, before);
}

#[test]
fn partial_event_requires_a_definition_on_every_branch() {
    for both_paths in [false, true] {
        let store = || {
            SIRInstruction::Store(
                addr(WORKING_REGION),
                SIROffset::Static(0),
                8,
                RegisterId(1),
                Vec::new(),
                Vec::new(),
            )
        };
        let mut unit = eu(vec![
            block(
                0,
                vec![],
                SIRTerminator::Branch {
                    cond: RegisterId(3),
                    true_block: (BlockId(1), vec![]),
                    false_block: (BlockId(2), vec![]),
                },
            ),
            block(1, vec![store()], SIRTerminator::Jump(BlockId(3), vec![])),
            block(
                2,
                if both_paths { vec![store()] } else { vec![] },
                SIRTerminator::Jump(BlockId(3), vec![]),
            ),
            block(
                3,
                vec![working_apply(SIROffset::Static(0), 8)],
                SIRTerminator::Return,
            ),
        ]);
        let before = unit.clone();
        eliminate_partial_working_round_trip(&mut unit);
        if both_paths {
            assert!(unit.blocks[&BlockId(3)].instructions.is_empty());
            assert!(
                matches!(unit.blocks[&BlockId(1)].instructions[0],SIRInstruction::Store(address,..) if address.region==STABLE_REGION)
            );
        } else {
            assert_eq!(unit, before);
        }
    }
}

#[test]
fn partial_event_keeps_unseeded_dynamic_pending_values() {
    let mut unit = eu(vec![block(
        0,
        vec![
            indexed_working_store(),
            working_apply(SIROffset::Static(0), 64),
        ],
        SIRTerminator::Return,
    )]);
    let before = unit.clone();
    eliminate_partial_working_round_trip(&mut unit);
    assert_eq!(unit, before);
}

#[test]
fn partial_event_can_redirect_a_locally_seeded_dynamic_update() {
    let mut unit = eu(vec![block(
        0,
        vec![
            working_seed(SIROffset::Static(0), 64),
            indexed_working_store(),
            working_apply(SIROffset::Static(0), 64),
        ],
        SIRTerminator::Return,
    )]);
    eliminate_partial_working_round_trip(&mut unit);
    assert!(
        matches!(unit.blocks[&BlockId(0)].instructions.as_slice(),[SIRInstruction::Store(address,SIROffset::Element {..},..)] if address.region==STABLE_REGION)
    );
}
#[test]
fn partial_event_keeps_reads_of_another_producers_pending_fragment() {
    let mut unit = eu(vec![block(
        0,
        vec![
            working_seed(SIROffset::Static(0), 8),
            SIRInstruction::Load(RegisterId(2), addr(WORKING_REGION), SIROffset::Static(8), 8),
            working_apply(SIROffset::Static(0), 8),
        ],
        SIRTerminator::Return,
    )]);
    let before = unit.clone();
    eliminate_partial_working_round_trip(&mut unit);
    assert_eq!(unit, before);
}
