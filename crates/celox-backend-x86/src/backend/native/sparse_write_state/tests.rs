use super::*;
use crate::HashMap;
use crate::{
    BasicBlock, InstanceId, RegisterId, RegisterType, SIROffset, SIRTerminator, STABLE_REGION,
};
use celox_design::StateObjectId as VarId;
use celox_state_layout::MemoryLayoutMode;

fn address(region: u32, variable: u32) -> RegionedAbsoluteAddr {
    RegionedAbsoluteAddr {
        region,
        instance_id: InstanceId(0),
        var_id: VarId::from_raw(variable),
    }
}

fn store(variable: u32, source: RegisterId) -> SIRInstruction<RegionedAbsoluteAddr> {
    store_at(variable, 0, source)
}

fn store_at(
    variable: u32,
    bit_offset: usize,
    source: RegisterId,
) -> SIRInstruction<RegionedAbsoluteAddr> {
    SIRInstruction::Store(
        address(crate::SPARSE_WORKING_REGION, variable),
        SIROffset::Static(bit_offset),
        1,
        source,
        vec![],
        vec![],
    )
}

fn commit(variable: u32) -> SIRInstruction<RegionedAbsoluteAddr> {
    SIRInstruction::Commit(
        address(crate::SPARSE_WORKING_REGION, variable),
        address(STABLE_REGION, variable),
        SIROffset::Static(0),
        1,
        vec![],
    )
}

fn eu(
    blocks: impl IntoIterator<Item = (BlockId, BasicBlock<RegionedAbsoluteAddr>)>,
) -> ExecutionUnit<RegionedAbsoluteAddr> {
    ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: blocks.into_iter().collect(),
        register_map: [(
            RegisterId(0),
            RegisterType::Bit {
                width: 1,
                signed: false,
            },
        )]
        .into_iter()
        .collect::<HashMap<_, _>>(),
    }
}

fn layout() -> MemoryLayout {
    let sparse_layouts = (0..=1)
        .map(|variable| {
            (
                address(crate::SPARSE_WORKING_REGION, variable).absolute_addr(),
                celox_state_layout::SparseWorkingLayout {
                    active_index: variable as usize,
                    chunk_count: 128,
                    dirty_words_offset: 0,
                    dirty_word_count: 1,
                    summary_words_offset: 0,
                    summary_word_count: 1,
                },
            )
        })
        .collect();
    MemoryLayout {
        trace: None,
        settle: None,
        four_state: false,
        mode: MemoryLayoutMode::Packed,
        unpacked_arrays: HashMap::default(),
        offsets: HashMap::default(),
        widths: HashMap::default(),
        is_4states: HashMap::default(),
        total_size: 0,
        working_offsets: HashMap::default(),
        working_base_offset: 0,
        sparse_offsets: HashMap::default(),
        sparse_base_offset: 0,
        sparse_layouts,
        sparse_active_bits_offset: 0,
        sparse_active_capacity: 0,
        merged_total_size: 0,
        triggered_bits_offset: 0,
        triggered_bits_total_size: 0,
        scratch_base_offset: 0,
        scratch_size: 0,
        runtime_event_capacity: 0,
        runtime_event_slot_size: 0,
        runtime_event_buffer_size: 0,
        runtime_event_site_layouts: vec![],
        call_stack: None,
    }
}

fn analyze(
    unit: &ExecutionUnit<RegionedAbsoluteAddr>,
    commit_block: BlockId,
    commit_start: usize,
) -> SparseWriteStates {
    SparseWriteStates::analyze(unit, &layout(), commit_block, commit_start).unwrap()
}

fn zero_fill_layout() -> MemoryLayout {
    let mut layout = layout();
    for variable in 0..=1 {
        let object = address(crate::SPARSE_WORKING_REGION, variable).absolute_addr();
        layout.widths.insert(object, 128);
        let sparse = layout.sparse_layouts.get_mut(&object).unwrap();
        sparse.chunk_count = 2;
        sparse.dirty_word_count = 1;
        sparse.summary_word_count = 1;
    }
    layout
}

fn zero_fill_unit(
    instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
) -> ExecutionUnit<RegionedAbsoluteAddr> {
    ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [(
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: vec![],
                instructions,
                terminator: SIRTerminator::Return,
            },
        )]
        .into_iter()
        .collect(),
        register_map: [RegisterId(0), RegisterId(1)]
            .map(|register| (register, RegisterType::Logic { width: 64 }))
            .into_iter()
            .collect(),
    }
}

fn zero_store(variable: u32, bit_offset: usize) -> SIRInstruction<RegionedAbsoluteAddr> {
    zero_store_in_region(crate::SPARSE_WORKING_REGION, variable, bit_offset)
}

fn zero_store_in_region(
    region: u32,
    variable: u32,
    bit_offset: usize,
) -> SIRInstruction<RegionedAbsoluteAddr> {
    SIRInstruction::Store(
        address(region, variable),
        SIROffset::Static(bit_offset),
        64,
        RegisterId(0),
        vec![],
        vec![],
    )
}

#[test]
fn zero_fill_crosses_only_unrelated_operations() {
    let unit = zero_fill_unit(vec![
        SIRInstruction::Imm(RegisterId(0), crate::SIRValue::new(0u8)),
        zero_store(0, 0),
        zero_store(1, 0),
        SIRInstruction::RuntimeEvent {
            site_id: 0,
            args: vec![],
        },
        zero_store(0, 64),
        zero_store(1, 64),
    ]);

    let plans = find_sparse_zero_fills(&unit, &zero_fill_layout());
    for instruction in [1, 2, 4, 5] {
        assert!(plans.is_member(BlockId(0), instruction));
    }
    assert!(plans.is_dead_zero_definition(BlockId(0), 0));
    assert_eq!(
        plans.root(BlockId(0), 4),
        Some(address(crate::SPARSE_WORKING_REGION, 0))
    );
    assert_eq!(
        plans.root(BlockId(0), 5),
        Some(address(crate::SPARSE_WORKING_REGION, 1))
    );
}

#[test]
fn direct_stable_zero_fill_keeps_the_sparse_origin_bulk_plan() {
    let unit = zero_fill_unit(vec![
        SIRInstruction::Imm(RegisterId(0), crate::SIRValue::new(0u8)),
        zero_store_in_region(STABLE_REGION, 0, 0),
        zero_store_in_region(STABLE_REGION, 0, 64),
    ]);

    let plans = find_sparse_zero_fills(&unit, &zero_fill_layout());
    assert!(plans.is_member(BlockId(0), 1));
    assert!(plans.is_member(BlockId(0), 2));
    assert_eq!(plans.root(BlockId(0), 2), Some(address(STABLE_REGION, 0)));
    assert!(plans.is_dead_zero_definition(BlockId(0), 0));
}

#[test]
fn direct_stable_zero_fill_does_not_require_a_sparse_home() {
    let unit = zero_fill_unit(vec![
        SIRInstruction::Imm(RegisterId(0), crate::SIRValue::new(0u8)),
        zero_store_in_region(STABLE_REGION, 0, 0),
        zero_store_in_region(STABLE_REGION, 0, 64),
    ]);
    let mut layout = zero_fill_layout();
    layout
        .sparse_layouts
        .remove(&address(STABLE_REGION, 0).absolute_addr());

    let plans = find_sparse_zero_fills(&unit, &layout);
    assert!(plans.is_member(BlockId(0), 1));
    assert!(plans.is_member(BlockId(0), 2));
    assert_eq!(plans.root(BlockId(0), 2), Some(address(STABLE_REGION, 0)));
    assert!(plans.is_dead_zero_definition(BlockId(0), 0));
}

#[test]
fn same_object_read_prevents_zero_fill_reordering() {
    let unit = zero_fill_unit(vec![
        SIRInstruction::Imm(RegisterId(0), crate::SIRValue::new(0u8)),
        zero_store(0, 0),
        SIRInstruction::Load(
            RegisterId(1),
            address(crate::SPARSE_WORKING_REGION, 0),
            SIROffset::Static(0),
            64,
        ),
        zero_store(0, 64),
    ]);

    let plans = find_sparse_zero_fills(&unit, &zero_fill_layout());
    assert!(!plans.is_member(BlockId(0), 1));
    assert!(!plans.is_member(BlockId(0), 3));
}

#[test]
fn partial_zero_overwrite_is_not_a_fill() {
    let unit = zero_fill_unit(vec![
        SIRInstruction::Imm(RegisterId(0), crate::SIRValue::new(0u8)),
        zero_store(0, 0),
    ]);

    let plans = find_sparse_zero_fills(&unit, &zero_fill_layout());
    assert!(!plans.is_member(BlockId(0), 1));
    assert!(!plans.is_dead_zero_definition(BlockId(0), 0));
}

#[test]
fn zero_fill_elides_its_wide_concat_tree() {
    let mut unit = zero_fill_unit(vec![
        SIRInstruction::Imm(RegisterId(0), crate::SIRValue::new(0u8)),
        SIRInstruction::Concat(RegisterId(1), vec![RegisterId(0), RegisterId(0)]),
        SIRInstruction::Store(
            address(crate::SPARSE_WORKING_REGION, 0),
            SIROffset::Static(0),
            128,
            RegisterId(1),
            vec![],
            vec![],
        ),
    ]);
    unit.register_map
        .insert(RegisterId(1), RegisterType::Logic { width: 128 });

    let plans = find_sparse_zero_fills(&unit, &zero_fill_layout());
    assert!(plans.is_member(BlockId(0), 2));
    assert!(plans.is_dead_zero_definition(BlockId(0), 1));
    assert!(plans.is_dead_zero_definition(BlockId(0), 0));
}

#[test]
fn straight_line_marks_only_each_objects_first_store() {
    let unit = eu([(
        BlockId(0),
        BasicBlock {
            id: BlockId(0),
            params: vec![],
            instructions: vec![
                store(0, RegisterId(0)),
                store(0, RegisterId(0)),
                store_at(0, 64, RegisterId(0)),
                store_at(0, 4096, RegisterId(0)),
                store(1, RegisterId(0)),
                commit(0),
                commit(1),
            ],
            terminator: SIRTerminator::Return,
        },
    )]);
    let facts = analyze(&unit, BlockId(0), 5);

    assert_eq!(facts.state(BlockId(0), 0), SparseWriteState::First);
    assert_eq!(facts.state(BlockId(0), 1), SparseWriteState::Active);
    assert_eq!(facts.state(BlockId(0), 2), SparseWriteState::Active);
    assert_eq!(facts.state(BlockId(0), 3), SparseWriteState::Active);
    assert_eq!(facts.state(BlockId(0), 4), SparseWriteState::First);
    assert_eq!(facts.chunk_state(BlockId(0), 0), SparseChunkState::Clean);
    assert_eq!(facts.chunk_state(BlockId(0), 1), SparseChunkState::Dirty);
    assert_eq!(facts.chunk_state(BlockId(0), 2), SparseChunkState::Clean);
    assert_eq!(facts.chunk_state(BlockId(0), 3), SparseChunkState::Clean);
    assert_eq!(facts.chunk_state(BlockId(0), 4), SparseChunkState::Clean);
    assert_eq!(
        facts.dirty_word_state(BlockId(0), 0),
        SparseChunkState::Clean
    );
    assert_eq!(
        facts.dirty_word_state(BlockId(0), 1),
        SparseChunkState::Dirty
    );
    assert_eq!(
        facts.dirty_word_state(BlockId(0), 2),
        SparseChunkState::Dirty
    );
    assert_eq!(
        facts.dirty_word_state(BlockId(0), 3),
        SparseChunkState::Clean
    );
    assert_eq!(
        facts.dirty_word_state(BlockId(0), 4),
        SparseChunkState::Clean
    );
}

#[test]
fn straight_line_clean_chunks_share_one_deferred_metadata_update() {
    let unit = eu([(
        BlockId(0),
        BasicBlock {
            id: BlockId(0),
            params: vec![],
            instructions: vec![
                store_at(0, 0, RegisterId(0)),
                store_at(0, 64, RegisterId(0)),
                store_at(0, 128, RegisterId(0)),
                commit(0),
            ],
            terminator: SIRTerminator::Return,
        },
    )]);
    let facts = analyze(&unit, BlockId(0), 3);

    assert_eq!(
        facts.metadata_action(BlockId(0), 0),
        SparseMetadataAction::Deferred
    );
    assert_eq!(
        facts.metadata_action(BlockId(0), 1),
        SparseMetadataAction::Deferred
    );
    assert_eq!(
        facts.metadata_action(BlockId(0), 2),
        SparseMetadataAction::Batch {
            dirty_word: 0,
            dirty_mask: 0b111,
            initial_write_state: SparseWriteState::First,
            initial_dirty_word_state: SparseChunkState::Clean,
        }
    );
}

#[test]
fn commit_closes_a_metadata_batch() {
    let unit = eu([(
        BlockId(0),
        BasicBlock {
            id: BlockId(0),
            params: vec![],
            instructions: vec![
                store_at(0, 0, RegisterId(0)),
                commit(0),
                store_at(0, 64, RegisterId(0)),
                store_at(0, 128, RegisterId(0)),
                commit(0),
            ],
            terminator: SIRTerminator::Return,
        },
    )]);
    let facts = analyze(&unit, BlockId(0), 4);

    assert_eq!(
        facts.metadata_action(BlockId(0), 0),
        SparseMetadataAction::Immediate
    );
    assert_eq!(
        facts.metadata_action(BlockId(0), 2),
        SparseMetadataAction::Deferred
    );
    assert_eq!(
        facts.metadata_action(BlockId(0), 3),
        SparseMetadataAction::Batch {
            dirty_word: 0,
            dirty_mask: 0b110,
            initial_write_state: SparseWriteState::First,
            initial_dirty_word_state: SparseChunkState::Clean,
        }
    );
}

#[test]
fn runtime_event_closes_a_metadata_batch() {
    let unit = eu([(
        BlockId(0),
        BasicBlock {
            id: BlockId(0),
            params: vec![],
            instructions: vec![
                store_at(0, 0, RegisterId(0)),
                SIRInstruction::RuntimeEvent {
                    site_id: 0,
                    args: vec![],
                },
                store_at(0, 64, RegisterId(0)),
                store_at(0, 128, RegisterId(0)),
                commit(0),
            ],
            terminator: SIRTerminator::Return,
        },
    )]);
    let facts = analyze(&unit, BlockId(0), 4);

    assert_eq!(
        facts.metadata_action(BlockId(0), 0),
        SparseMetadataAction::Immediate
    );
    assert_eq!(
        facts.metadata_action(BlockId(0), 2),
        SparseMetadataAction::Deferred
    );
    assert!(matches!(
        facts.metadata_action(BlockId(0), 3),
        SparseMetadataAction::Batch {
            dirty_mask: 0b110,
            ..
        }
    ));
}

#[test]
fn mutually_exclusive_arm_stores_are_both_first_writes() {
    let unit = eu([
        (
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: vec![],
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
                instructions: vec![store(0, RegisterId(0))],
                terminator: SIRTerminator::Jump(BlockId(3), vec![]),
            },
        ),
        (
            BlockId(2),
            BasicBlock {
                id: BlockId(2),
                params: vec![],
                instructions: vec![store(0, RegisterId(0))],
                terminator: SIRTerminator::Jump(BlockId(3), vec![]),
            },
        ),
        (
            BlockId(3),
            BasicBlock {
                id: BlockId(3),
                params: vec![],
                instructions: vec![store(0, RegisterId(0)), commit(0)],
                terminator: SIRTerminator::Return,
            },
        ),
    ]);
    let facts = analyze(&unit, BlockId(3), 1);

    assert_eq!(facts.state(BlockId(1), 0), SparseWriteState::First);
    assert_eq!(facts.state(BlockId(2), 0), SparseWriteState::First);
    assert_eq!(facts.state(BlockId(3), 0), SparseWriteState::Active);
    assert_eq!(facts.chunk_state(BlockId(1), 0), SparseChunkState::Clean);
    assert_eq!(facts.chunk_state(BlockId(2), 0), SparseChunkState::Clean);
    assert_eq!(facts.chunk_state(BlockId(3), 0), SparseChunkState::Dirty);
    assert_eq!(
        facts.dirty_word_state(BlockId(3), 0),
        SparseChunkState::Dirty
    );
}

#[test]
fn join_after_a_maybe_store_is_not_a_first_write() {
    let unit = eu([
        (
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: vec![],
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
                instructions: vec![store(0, RegisterId(0))],
                terminator: SIRTerminator::Jump(BlockId(3), vec![]),
            },
        ),
        (
            BlockId(2),
            BasicBlock {
                id: BlockId(2),
                params: vec![],
                instructions: vec![],
                terminator: SIRTerminator::Jump(BlockId(3), vec![]),
            },
        ),
        (
            BlockId(3),
            BasicBlock {
                id: BlockId(3),
                params: vec![],
                instructions: vec![store(0, RegisterId(0))],
                terminator: SIRTerminator::Jump(BlockId(4), vec![]),
            },
        ),
        (
            BlockId(4),
            BasicBlock {
                id: BlockId(4),
                params: vec![],
                instructions: vec![commit(0)],
                terminator: SIRTerminator::Return,
            },
        ),
    ]);
    let facts = analyze(&unit, BlockId(4), 0);

    assert_eq!(facts.state(BlockId(1), 0), SparseWriteState::First);
    assert_eq!(facts.state(BlockId(3), 0), SparseWriteState::Unknown);
    assert_eq!(facts.chunk_state(BlockId(3), 0), SparseChunkState::Unknown);
    assert_eq!(
        facts.dirty_word_state(BlockId(3), 0),
        SparseChunkState::Unknown
    );
}

#[test]
fn loop_backedge_prevents_a_first_write_certificate() {
    let unit = eu([
        (
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: vec![],
                instructions: vec![],
                terminator: SIRTerminator::Jump(BlockId(1), vec![]),
            },
        ),
        (
            BlockId(1),
            BasicBlock {
                id: BlockId(1),
                params: vec![],
                instructions: vec![],
                terminator: SIRTerminator::Branch {
                    cond: RegisterId(0),
                    true_block: (BlockId(2), vec![]),
                    false_block: (BlockId(3), vec![]),
                },
            },
        ),
        (
            BlockId(2),
            BasicBlock {
                id: BlockId(2),
                params: vec![],
                instructions: vec![store(0, RegisterId(0))],
                terminator: SIRTerminator::Jump(BlockId(1), vec![]),
            },
        ),
        (
            BlockId(3),
            BasicBlock {
                id: BlockId(3),
                params: vec![],
                instructions: vec![commit(0)],
                terminator: SIRTerminator::Return,
            },
        ),
    ]);
    let facts = analyze(&unit, BlockId(3), 0);

    assert_eq!(facts.state(BlockId(2), 0), SparseWriteState::Unknown);
    assert_eq!(facts.chunk_state(BlockId(2), 0), SparseChunkState::Unknown);
    assert_eq!(
        facts.dirty_word_state(BlockId(2), 0),
        SparseChunkState::Unknown
    );
}

#[test]
fn commit_reset_prevents_active_state_from_crossing_it() {
    let unit = eu([(
        BlockId(0),
        BasicBlock {
            id: BlockId(0),
            params: vec![],
            instructions: vec![
                store(0, RegisterId(0)),
                commit(0),
                store(0, RegisterId(0)),
                commit(0),
            ],
            terminator: SIRTerminator::Return,
        },
    )]);
    let facts = analyze(&unit, BlockId(0), 3);

    assert_eq!(facts.state(BlockId(0), 0), SparseWriteState::First);
    assert_eq!(facts.state(BlockId(0), 2), SparseWriteState::First);
    assert_eq!(facts.chunk_state(BlockId(0), 2), SparseChunkState::Clean);
    assert_eq!(
        facts.dirty_word_state(BlockId(0), 2),
        SparseChunkState::Clean
    );
}

#[test]
fn dynamic_store_only_blocks_a_reaching_chunk_proof() {
    let unit = eu([(
        BlockId(0),
        BasicBlock {
            id: BlockId(0),
            params: vec![],
            instructions: vec![
                store(0, RegisterId(0)),
                SIRInstruction::Store(
                    address(crate::SPARSE_WORKING_REGION, 0),
                    SIROffset::Dynamic(RegisterId(0)),
                    1,
                    RegisterId(0),
                    vec![],
                    vec![],
                ),
                store_at(0, 64, RegisterId(0)),
                commit(0),
            ],
            terminator: SIRTerminator::Return,
        },
    )]);
    let facts = analyze(&unit, BlockId(0), 3);

    assert_eq!(facts.state(BlockId(0), 0), SparseWriteState::First);
    assert_eq!(facts.state(BlockId(0), 1), SparseWriteState::Active);
    assert_eq!(facts.state(BlockId(0), 2), SparseWriteState::Active);
    assert_eq!(facts.chunk_state(BlockId(0), 0), SparseChunkState::Clean);
    assert_eq!(facts.chunk_state(BlockId(0), 1), SparseChunkState::Unknown);
    assert_eq!(facts.chunk_state(BlockId(0), 2), SparseChunkState::Unknown);
    assert_eq!(
        facts.metadata_action(BlockId(0), 0),
        SparseMetadataAction::Immediate
    );
    assert_eq!(
        facts.metadata_action(BlockId(0), 2),
        SparseMetadataAction::Immediate
    );
}

#[test]
fn dynamic_store_on_a_sibling_path_does_not_block_a_clean_chunk() {
    let unit = eu([
        (
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: vec![],
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
                instructions: vec![SIRInstruction::Store(
                    address(crate::SPARSE_WORKING_REGION, 0),
                    SIROffset::Dynamic(RegisterId(0)),
                    1,
                    RegisterId(0),
                    vec![],
                    vec![],
                )],
                terminator: SIRTerminator::Jump(BlockId(3), vec![]),
            },
        ),
        (
            BlockId(2),
            BasicBlock {
                id: BlockId(2),
                params: vec![],
                instructions: vec![store_at(0, 64, RegisterId(0))],
                terminator: SIRTerminator::Jump(BlockId(3), vec![]),
            },
        ),
        (
            BlockId(3),
            BasicBlock {
                id: BlockId(3),
                params: vec![],
                instructions: vec![commit(0)],
                terminator: SIRTerminator::Return,
            },
        ),
    ]);
    let facts = analyze(&unit, BlockId(3), 0);

    assert_eq!(facts.state(BlockId(2), 0), SparseWriteState::First);
    assert_eq!(facts.chunk_state(BlockId(2), 0), SparseChunkState::Clean);
    assert_eq!(
        facts.dirty_word_state(BlockId(2), 0),
        SparseChunkState::Clean
    );
}

#[test]
fn static_multi_chunk_store_clobbers_only_its_exact_range() {
    let unit = eu([(
        BlockId(0),
        BasicBlock {
            id: BlockId(0),
            params: vec![],
            instructions: vec![
                SIRInstruction::Store(
                    address(crate::SPARSE_WORKING_REGION, 0),
                    SIROffset::Static(0),
                    128,
                    RegisterId(0),
                    vec![],
                    vec![],
                ),
                store_at(0, 64, RegisterId(0)),
                store_at(0, 128, RegisterId(0)),
                commit(0),
            ],
            terminator: SIRTerminator::Return,
        },
    )]);
    let facts = analyze(&unit, BlockId(0), 3);

    assert_eq!(facts.chunk_state(BlockId(0), 0), SparseChunkState::Unknown);
    assert_eq!(facts.chunk_state(BlockId(0), 1), SparseChunkState::Dirty);
    assert_eq!(facts.chunk_state(BlockId(0), 2), SparseChunkState::Clean);
    assert_eq!(
        facts.dirty_word_state(BlockId(0), 1),
        SparseChunkState::Dirty
    );
    assert_eq!(
        facts.dirty_word_state(BlockId(0), 2),
        SparseChunkState::Dirty
    );
}
