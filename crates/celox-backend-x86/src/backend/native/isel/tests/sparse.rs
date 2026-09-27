use super::*;

#[test]
fn first_sparse_element_write_uses_entry_memoryssa_and_commits_exactly() {
    const STABLE: usize = 0;
    const SPARSE: usize = 32;
    const DIRTY: usize = 64;
    const SUMMARY: usize = 72;
    const ACTIVE_BITS: usize = 88;
    const STATE_SIZE: usize = 96;

    let absolute = AbsoluteAddr {
        instance_id: InstanceId(0),
        var_id: VarId::default(),
    };
    let sparse = RegionedAbsoluteAddr::from_absolute_addr(crate::SPARSE_WORKING_REGION, absolute);
    let stable = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, absolute);
    let value = RegisterId(0);
    let unit = ExecutionUnit {
        entry_block_id: SirBlockId(0),
        blocks: [(
            SirBlockId(0),
            BasicBlock {
                id: SirBlockId(0),
                params: vec![],
                instructions: vec![
                    SIRInstruction::Imm(value, SIRValue::new(1u8)),
                    SIRInstruction::Store(sparse, SIROffset::Static(1), 1, value, vec![], vec![]),
                    SIRInstruction::Commit(sparse, stable, SIROffset::Static(0), 4, vec![]),
                ],
                terminator: SIRTerminator::Return,
            },
        )]
        .into_iter()
        .collect(),
        register_map: [(
            value,
            RegisterType::Bit {
                width: 1,
                signed: false,
            },
        )]
        .into_iter()
        .collect(),
    };
    unit.verify();

    let mut layout = empty_layout();
    layout.mode = MemoryLayoutMode::ElementStrided;
    layout.offsets.insert(absolute, STABLE);
    layout.widths.insert(absolute, 4);
    layout.is_4states.insert(absolute, false);
    layout.unpacked_arrays.insert(
        absolute,
        celox_state_layout::UnpackedArrayLayout {
            element_width: 1,
            element_count: 4,
            element_stride: 1,
            plane_size: 4,
        },
    );
    layout.total_size = SPARSE;
    layout.working_base_offset = SPARSE;
    layout.sparse_base_offset = SPARSE;
    layout.sparse_offsets.insert(absolute, 0);
    layout.sparse_layouts.insert(
        absolute,
        celox_state_layout::SparseWorkingLayout {
            active_index: 0,
            chunk_count: 1,
            dirty_words_offset: DIRTY,
            dirty_word_count: 1,
            summary_words_offset: SUMMARY,
            summary_word_count: 1,
        },
    );
    layout.sparse_active_bits_offset = ACTIVE_BITS;
    layout.sparse_active_capacity = 1;
    layout.merged_total_size = STATE_SIZE;
    layout.triggered_bits_offset = STATE_SIZE;
    layout.scratch_base_offset = STATE_SIZE;

    let mut function = lower_execution_unit(&unit, &layout, false);
    function.verify();
    let dirty_loads = function
        .blocks
        .iter()
        .flat_map(|block| &block.insts)
        .filter(|instruction| {
            matches!(
                instruction,
                MInst::LoadIndexed {
                    base: BaseReg::SimState,
                    offset,
                    ..
                } if *offset == DIRTY as i32
            )
        })
        .count();
    assert_eq!(dirty_loads, 0, "first write must not inspect dirty state");

    mir_legalize::legalize(&mut function);
    mir_opt::optimize(&mut function);
    let allocation = regalloc::run_regalloc(&mut function).unwrap();
    mir_opt::post_regalloc_peephole(&mut function, &allocation.assignment);
    function.verify();
    let emitted = emit::emit(
        &function,
        &allocation.assignment,
        allocation.spill_frame_size,
    )
    .unwrap();
    let jit = JitCode::new(&emitted.code).unwrap();

    let mut state = vec![0u8; STATE_SIZE];
    state[STABLE] = 1;
    state[STABLE + 3] = 1;
    state[SPARSE..SPARSE + 8].copy_from_slice(&u64::MAX.to_le_bytes());
    assert_eq!(unsafe { jit.call(&mut state) }, 0);
    // Only bit 0 of each padded slot belongs to the RTL value. Padding is
    // backend-owned and may be canonicalized by a whole-element store.
    assert_eq!(state[STABLE + 2] & 1, 0);
    assert_eq!(state[STABLE] & 1, 1);
    assert_eq!(state[STABLE + 1] & 1, 1);
    assert_eq!(state[STABLE + 3] & 1, 1);
    assert_eq!(&state[DIRTY..DIRTY + 8], &[0; 8]);
    assert_eq!(&state[SUMMARY..SUMMARY + 8], &[0; 8]);
    assert_eq!(&state[ACTIVE_BITS..ACTIVE_BITS + 8], &[0; 8]);
}

#[test]
fn whole_sparse_zero_overwrite_clears_padded_single_chunk_arrays() {
    const SPARSE: usize = 32;
    const DIRTY: usize = 64;
    const SUMMARY: usize = 72;
    const ACTIVE: usize = 88;
    const STATE_SIZE: usize = 104;
    for (element_width, element_count) in [(1, 2), (1, 4), (3, 4), (7, 8)] {
        for four_state in [false, true] {
            let absolute = AbsoluteAddr {
                instance_id: InstanceId(0),
                var_id: VarId::default(),
            };
            let sparse =
                RegionedAbsoluteAddr::from_absolute_addr(crate::SPARSE_WORKING_REGION, absolute);
            let stable = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, absolute);
            let width = element_width * element_count;
            let zero = RegisterId(0);
            let unit = ExecutionUnit {
                entry_block_id: SirBlockId(0),
                blocks: [(
                    SirBlockId(0),
                    BasicBlock {
                        id: SirBlockId(0),
                        params: vec![],
                        instructions: vec![
                            SIRInstruction::Imm(zero, SIRValue::new(0u8)),
                            SIRInstruction::Store(
                                sparse,
                                SIROffset::PackedElements {
                                    bit_offset: 0,
                                    element_width,
                                },
                                width,
                                zero,
                                vec![],
                                vec![],
                            ),
                            SIRInstruction::Commit(
                                sparse,
                                stable,
                                SIROffset::Static(0),
                                width,
                                vec![],
                            ),
                        ],
                        terminator: SIRTerminator::Return,
                    },
                )]
                .into_iter()
                .collect(),
                register_map: [(zero, RegisterType::Logic { width })]
                    .into_iter()
                    .collect(),
            };
            unit.verify();
            let mut layout = empty_layout();
            layout.mode = MemoryLayoutMode::ElementStrided;
            layout.four_state = four_state;
            layout.offsets.insert(absolute, 0);
            layout.widths.insert(absolute, width);
            layout.is_4states.insert(absolute, four_state);
            layout.unpacked_arrays.insert(
                absolute,
                celox_state_layout::UnpackedArrayLayout {
                    element_width,
                    element_count,
                    element_stride: 1,
                    plane_size: element_count,
                },
            );
            layout.total_size = SPARSE;
            layout.working_base_offset = SPARSE;
            layout.sparse_base_offset = SPARSE;
            layout.sparse_offsets.insert(absolute, 0);
            layout.sparse_layouts.insert(
                absolute,
                celox_state_layout::SparseWorkingLayout {
                    active_index: 0,
                    chunk_count: 1,
                    dirty_words_offset: DIRTY,
                    dirty_word_count: 1,
                    summary_words_offset: SUMMARY,
                    summary_word_count: 1,
                },
            );
            layout.sparse_active_bits_offset = ACTIVE;
            layout.sparse_active_capacity = 1;
            layout.merged_total_size = STATE_SIZE;
            layout.triggered_bits_offset = STATE_SIZE;
            layout.scratch_base_offset = STATE_SIZE;
            let mut function = lower_execution_unit(&unit, &layout, four_state);
            function.verify();
            mir_legalize::legalize(&mut function);
            mir_opt::optimize(&mut function);
            let allocation = regalloc::run_regalloc(&mut function).unwrap();
            mir_opt::post_regalloc_peephole(&mut function, &allocation.assignment);
            function.verify();
            let emitted = emit::emit(
                &function,
                &allocation.assignment,
                allocation.spill_frame_size,
            )
            .unwrap();
            let jit = JitCode::new(&emitted.code).unwrap();
            let mut state = [0xffu8; STATE_SIZE];
            state[DIRTY..DIRTY + 8].fill(0);
            state[SUMMARY..SUMMARY + 8].fill(0);
            state[ACTIVE..ACTIVE + 8].fill(0);
            assert_eq!(unsafe { jit.call(&mut state) }, 0);
            let physical_size = element_count * if four_state { 2 } else { 1 };
            for byte in &state[..physical_size] {
                assert_eq!(
                    *byte & ((1 << element_width) - 1) as u8,
                    0,
                    "width={element_width}, count={element_count}, four_state={four_state}"
                );
            }
            assert_eq!(state[physical_size], 0xff);
            assert_eq!(state[SPARSE + physical_size], 0xff);
            assert_eq!(&state[DIRTY..DIRTY + 8], &[0; 8]);
            assert_eq!(&state[SUMMARY..SUMMARY + 8], &[0; 8]);
            assert_eq!(&state[ACTIVE..ACTIVE + 8], &[0; 8]);
        }
    }
}

#[test]
fn whole_sparse_zero_overwrite_uses_one_physical_fill() {
    const STABLE: usize = 0;
    const SPARSE: usize = 40;
    const DIRTY: usize = 72;
    const SUMMARY: usize = 80;
    const ACTIVE_BITS: usize = 96;
    const STATE_SIZE: usize = 112;

    let absolute = AbsoluteAddr {
        instance_id: InstanceId(0),
        var_id: VarId::default(),
    };
    let sparse = RegionedAbsoluteAddr::from_absolute_addr(crate::SPARSE_WORKING_REGION, absolute);
    let stable = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, absolute);
    let zero = RegisterId(0);
    let wide_zero = RegisterId(1);
    let unit = ExecutionUnit {
        entry_block_id: SirBlockId(0),
        blocks: [(
            SirBlockId(0),
            BasicBlock {
                id: SirBlockId(0),
                params: vec![],
                instructions: vec![
                    SIRInstruction::Imm(zero, SIRValue::new(0u8)),
                    SIRInstruction::Concat(wide_zero, vec![zero; 4]),
                    SIRInstruction::Store(
                        sparse,
                        SIROffset::PackedElements {
                            bit_offset: 0,
                            element_width: 51,
                        },
                        204,
                        wide_zero,
                        vec![],
                        vec![],
                    ),
                    SIRInstruction::Commit(sparse, stable, SIROffset::Static(0), 204, vec![]),
                ],
                terminator: SIRTerminator::Return,
            },
        )]
        .into_iter()
        .collect(),
        register_map: [
            (zero, RegisterType::Logic { width: 51 }),
            (wide_zero, RegisterType::Logic { width: 204 }),
        ]
        .into_iter()
        .collect(),
    };
    unit.verify();

    let mut layout = empty_layout();
    layout.mode = MemoryLayoutMode::ElementStrided;
    layout.offsets.insert(absolute, STABLE);
    layout.widths.insert(absolute, 204);
    layout.is_4states.insert(absolute, false);
    layout.unpacked_arrays.insert(
        absolute,
        celox_state_layout::UnpackedArrayLayout {
            element_width: 51,
            element_count: 4,
            element_stride: 8,
            plane_size: 32,
        },
    );
    layout.total_size = SPARSE;
    layout.working_base_offset = SPARSE;
    layout.sparse_base_offset = SPARSE;
    layout.sparse_offsets.insert(absolute, 0);
    layout.sparse_layouts.insert(
        absolute,
        celox_state_layout::SparseWorkingLayout {
            active_index: 0,
            chunk_count: 4,
            dirty_words_offset: DIRTY,
            dirty_word_count: 1,
            summary_words_offset: SUMMARY,
            summary_word_count: 1,
        },
    );
    layout.sparse_active_bits_offset = ACTIVE_BITS;
    layout.sparse_active_capacity = 1;
    layout.merged_total_size = STATE_SIZE;
    layout.triggered_bits_offset = STATE_SIZE;
    layout.scratch_base_offset = STATE_SIZE;

    let mut direct_unit = unit.clone();
    celox_sir_opt::optimizer::pass_eliminate_working_round_trip::eliminate_working_round_trip(
        &mut direct_unit,
        &[],
    );
    direct_unit.verify();
    assert!(matches!(
        direct_unit.blocks[&SirBlockId(0)].instructions.as_slice(),
        [SIRInstruction::Imm(..), SIRInstruction::Concat(..), SIRInstruction::Store(address, ..)]
            if address.region == STABLE_REGION
    ));
    let mut direct_function = lower_execution_unit(&direct_unit, &layout, false);
    direct_function.verify();
    assert_eq!(
        direct_function
            .blocks
            .iter()
            .flat_map(|block| &block.insts)
            .filter(|instruction| matches!(
                instruction,
                MInst::MemFill {
                    dst_offset,
                    byte_len: 32,
                    value: 0,
                } if *dst_offset == STABLE as i32
            ))
            .count(),
        1,
        "direct publication must retain the bulk-zero lowering"
    );
    assert!(
        !direct_function
            .blocks
            .iter()
            .flat_map(|block| &block.insts)
            .any(|instruction| matches!(
                instruction,
                MInst::SparseMarkActive { .. }
                    | MInst::SparseCommit { .. }
                    | MInst::SparseCommitWorklist { .. }
            ))
    );

    mir_legalize::legalize(&mut direct_function);
    mir_opt::optimize(&mut direct_function);
    let direct_allocation = regalloc::run_regalloc(&mut direct_function).unwrap();
    mir_opt::post_regalloc_peephole(&mut direct_function, &direct_allocation.assignment);
    direct_function.verify();
    let direct_emitted = emit::emit(
        &direct_function,
        &direct_allocation.assignment,
        direct_allocation.spill_frame_size,
    )
    .unwrap();
    let direct_jit = JitCode::new(&direct_emitted.code).unwrap();
    let mut direct_state = vec![0xa5u8; STATE_SIZE];
    let direct_sentinel = direct_state[STABLE + 32];
    assert_eq!(unsafe { direct_jit.call(&mut direct_state) }, 0);
    assert_eq!(&direct_state[STABLE..STABLE + 32], &[0; 32]);
    assert_eq!(direct_state[STABLE + 32], direct_sentinel);
    assert_eq!(
        &direct_state[SPARSE..STATE_SIZE],
        &[0xa5; STATE_SIZE - SPARSE]
    );

    let mut eval_only_unit = unit.clone();
    let removed = eval_only_unit
        .blocks
        .get_mut(&SirBlockId(0))
        .unwrap()
        .instructions
        .pop();
    assert!(matches!(removed, Some(SIRInstruction::Commit(..))));
    eval_only_unit.verify();
    let eval_only_function = lower_execution_unit(&eval_only_unit, &layout, false);
    eval_only_function.verify();
    assert_eq!(
        eval_only_function
            .blocks
            .iter()
            .flat_map(|block| &block.insts)
            .filter(|instruction| matches!(
                instruction,
                MInst::MemFill {
                    dst_offset,
                    byte_len: 32,
                    value: 0,
                } if *dst_offset == SPARSE as i32
            ))
            .count(),
        1,
        "an eval-only function must not require a local sparse commit"
    );

    let mut function = lower_execution_unit(&unit, &layout, false);
    function.verify();
    assert_eq!(
        function
            .blocks
            .iter()
            .flat_map(|block| &block.insts)
            .filter(|instruction| matches!(
                instruction,
                MInst::MemFill {
                    dst_offset,
                    byte_len: 32,
                    value: 0,
                } if *dst_offset == SPARSE as i32
            ))
            .count(),
        1
    );

    mir_legalize::legalize(&mut function);
    mir_opt::optimize(&mut function);
    let allocation = regalloc::run_regalloc(&mut function).unwrap();
    mir_opt::post_regalloc_peephole(&mut function, &allocation.assignment);
    function.verify();
    let emitted = emit::emit(
        &function,
        &allocation.assignment,
        allocation.spill_frame_size,
    )
    .unwrap();
    let jit = JitCode::new(&emitted.code).unwrap();

    let mut state = vec![0xa5u8; STATE_SIZE];
    state[DIRTY..DIRTY + 8].fill(0);
    state[SUMMARY..SUMMARY + 8].fill(0);
    state[ACTIVE_BITS..ACTIVE_BITS + 8].fill(0);
    let sentinel = state[STABLE + 32];
    assert_eq!(unsafe { jit.call(&mut state) }, 0);
    assert_eq!(&state[STABLE..STABLE + 32], &[0; 32]);
    assert_eq!(state[STABLE + 32], sentinel);
    assert_eq!(&state[DIRTY..DIRTY + 8], &[0; 8]);
    assert_eq!(&state[SUMMARY..SUMMARY + 8], &[0; 8]);
    assert_eq!(&state[ACTIVE_BITS..ACTIVE_BITS + 8], &[0; 8]);
}

#[test]
fn dominating_sparse_store_reuses_active_single_chunk_state() {
    const STABLE: usize = 0;
    const SPARSE: usize = 8;
    const DIRTY: usize = 16;
    const SUMMARY: usize = 24;
    const ACTIVE_BITS: usize = 40;
    const STATE_SIZE: usize = 48;

    let absolute = AbsoluteAddr {
        instance_id: InstanceId(0),
        var_id: VarId::default(),
    };
    let sparse = RegionedAbsoluteAddr::from_absolute_addr(crate::SPARSE_WORKING_REGION, absolute);
    let stable = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, absolute);
    let value = RegisterId(0);
    let unit = ExecutionUnit {
        entry_block_id: SirBlockId(0),
        blocks: [(
            SirBlockId(0),
            BasicBlock {
                id: SirBlockId(0),
                params: vec![],
                instructions: vec![
                    SIRInstruction::Imm(value, SIRValue::new(1u8)),
                    SIRInstruction::Store(sparse, SIROffset::Static(1), 1, value, vec![], vec![]),
                    SIRInstruction::Store(sparse, SIROffset::Static(2), 1, value, vec![], vec![]),
                    SIRInstruction::Commit(sparse, stable, SIROffset::Static(0), 8, vec![]),
                ],
                terminator: SIRTerminator::Return,
            },
        )]
        .into_iter()
        .collect(),
        register_map: [(
            value,
            RegisterType::Bit {
                width: 1,
                signed: false,
            },
        )]
        .into_iter()
        .collect(),
    };
    unit.verify();

    let mut layout = empty_layout();
    layout.offsets.insert(absolute, STABLE);
    layout.widths.insert(absolute, 8);
    layout.is_4states.insert(absolute, false);
    layout.total_size = SPARSE;
    layout.working_base_offset = SPARSE;
    layout.sparse_base_offset = SPARSE;
    layout.sparse_offsets.insert(absolute, 0);
    layout.sparse_layouts.insert(
        absolute,
        celox_state_layout::SparseWorkingLayout {
            active_index: 0,
            chunk_count: 1,
            dirty_words_offset: DIRTY,
            dirty_word_count: 1,
            summary_words_offset: SUMMARY,
            summary_word_count: 1,
        },
    );
    layout.sparse_active_bits_offset = ACTIVE_BITS;
    layout.sparse_active_capacity = 1;
    layout.merged_total_size = STATE_SIZE;
    layout.triggered_bits_offset = STATE_SIZE;
    layout.scratch_base_offset = STATE_SIZE;

    let mut function = lower_execution_unit(&unit, &layout, false);
    function.verify();
    assert_eq!(
        function
            .blocks
            .iter()
            .flat_map(|block| &block.insts)
            .filter(|instruction| matches!(instruction, MInst::SparseMarkActive { .. }))
            .count(),
        1,
        "the dominating Store proves that the object is already active"
    );

    mir_legalize::legalize(&mut function);
    mir_opt::optimize(&mut function);
    let allocation = regalloc::run_regalloc(&mut function).unwrap();
    mir_opt::post_regalloc_peephole(&mut function, &allocation.assignment);
    function.verify();
    let emitted = emit::emit(
        &function,
        &allocation.assignment,
        allocation.spill_frame_size,
    )
    .unwrap();
    let jit = JitCode::new(&emitted.code).unwrap();

    let mut state = vec![0u8; STATE_SIZE];
    state[STABLE] = 0xa0;
    state[SPARSE..SPARSE + 8].fill(0xff);
    assert_eq!(unsafe { jit.call(&mut state) }, 0);
    assert_eq!(state[STABLE], 0xa6);
    assert_eq!(&state[DIRTY..DIRTY + 8], &[0; 8]);
    assert_eq!(&state[SUMMARY..SUMMARY + 8], &[0; 8]);
    assert_eq!(&state[ACTIVE_BITS..ACTIVE_BITS + 8], &[0; 8]);
}

#[test]
fn chunk_memoryssa_preserves_disjoint_and_repeated_sparse_writes() {
    const STABLE: usize = 0;
    const SPARSE: usize = 32;
    const DIRTY: usize = 64;
    const SUMMARY: usize = 72;
    const ACTIVE_BITS: usize = 88;
    const STATE_SIZE: usize = 96;

    let absolute = AbsoluteAddr {
        instance_id: InstanceId(0),
        var_id: VarId::default(),
    };
    let sparse = RegionedAbsoluteAddr::from_absolute_addr(crate::SPARSE_WORKING_REGION, absolute);
    let stable = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, absolute);
    let one = RegisterId(0);
    let unit = ExecutionUnit {
        entry_block_id: SirBlockId(0),
        blocks: [(
            SirBlockId(0),
            BasicBlock {
                id: SirBlockId(0),
                params: vec![],
                instructions: vec![
                    SIRInstruction::Imm(one, SIRValue::new(1u8)),
                    SIRInstruction::Store(sparse, SIROffset::Static(0), 1, one, vec![], vec![]),
                    SIRInstruction::Store(sparse, SIROffset::Static(64), 1, one, vec![], vec![]),
                    SIRInstruction::Store(sparse, SIROffset::Static(65), 1, one, vec![], vec![]),
                    SIRInstruction::Store(sparse, SIROffset::Static(128), 1, one, vec![], vec![]),
                    SIRInstruction::Store(sparse, SIROffset::Static(192), 1, one, vec![], vec![]),
                    SIRInstruction::Commit(sparse, stable, SIROffset::Static(0), 256, vec![]),
                ],
                terminator: SIRTerminator::Return,
            },
        )]
        .into_iter()
        .collect(),
        register_map: [(
            one,
            RegisterType::Bit {
                width: 1,
                signed: false,
            },
        )]
        .into_iter()
        .collect(),
    };
    unit.verify();

    let mut layout = empty_layout();
    layout.offsets.insert(absolute, STABLE);
    layout.widths.insert(absolute, 256);
    layout.is_4states.insert(absolute, false);
    layout.total_size = SPARSE;
    layout.working_base_offset = SPARSE;
    layout.sparse_base_offset = SPARSE;
    layout.sparse_offsets.insert(absolute, 0);
    layout.sparse_layouts.insert(
        absolute,
        celox_state_layout::SparseWorkingLayout {
            active_index: 0,
            chunk_count: 4,
            dirty_words_offset: DIRTY,
            dirty_word_count: 1,
            summary_words_offset: SUMMARY,
            summary_word_count: 1,
        },
    );
    layout.sparse_active_bits_offset = ACTIVE_BITS;
    layout.sparse_active_capacity = 1;
    layout.merged_total_size = STATE_SIZE;
    layout.triggered_bits_offset = STATE_SIZE;
    layout.scratch_base_offset = STATE_SIZE;

    let mut function = lower_execution_unit(&unit, &layout, false);
    function.verify();
    assert_eq!(
        function
            .blocks
            .iter()
            .flat_map(|block| &block.insts)
            .filter(|instruction| matches!(instruction, MInst::SparseMarkActive { .. }))
            .count(),
        1,
        "only the object's first Store may mark it active"
    );
    assert_eq!(
        function
            .blocks
            .iter()
            .flat_map(|block| &block.insts)
            .filter(|instruction| {
                matches!(
                    instruction,
                    MInst::Store {
                        base: BaseReg::SimState,
                        offset,
                        ..
                    }
                    | MInst::StoreIndexed {
                        base: BaseReg::SimState,
                        offset,
                        ..
                    }
                    | MInst::OrStoreIndexed {
                        base: BaseReg::SimState,
                        offset,
                        ..
                    } if *offset == SUMMARY as i32
                )
            })
            .count(),
        1,
        "one dirty-word run needs one summary update"
    );
    assert_eq!(
        function
            .blocks
            .iter()
            .flat_map(|block| &block.insts)
            .filter(|instruction| {
                matches!(
                    instruction,
                    MInst::LoadIndexed {
                        base: BaseReg::SimState,
                        offset,
                        ..
                    } if *offset == DIRTY as i32
                )
            })
            .count(),
        0,
        "preserving a dirty word must not materialize it in a VReg"
    );
    assert_eq!(
        function
            .blocks
            .iter()
            .flat_map(|block| &block.insts)
            .filter(|instruction| {
                matches!(
                    instruction,
                    MInst::Store {
                        base: BaseReg::SimState,
                        offset,
                        ..
                    }
                    | MInst::StoreIndexed {
                        base: BaseReg::SimState,
                        offset,
                        ..
                    }
                    | MInst::OrStoreIndexed {
                        base: BaseReg::SimState,
                        offset,
                        ..
                    } if *offset == DIRTY as i32
                )
            })
            .count(),
        2,
        "each straight-line clean-chunk run needs one bitmap update"
    );
    let dirty_mask = function
        .blocks
        .iter()
        .flat_map(|block| &block.insts)
        .find_map(|instruction| match instruction {
            MInst::Store {
                base: BaseReg::SimState,
                offset,
                src,
                size: OpSize::S64,
            } if *offset == DIRTY as i32 => Some(*src),
            _ => None,
        })
        .expect("clean entry should use a direct batched dirty-word store");
    assert!(
        function
            .blocks
            .iter()
            .flat_map(|block| &block.insts)
            .any(|instruction| matches!(
                instruction,
                MInst::LoadImm { dst, value: 0b11 } if *dst == dirty_mask
            ))
    );
    let dirty_or_mask = function
        .blocks
        .iter()
        .flat_map(|block| &block.insts)
        .find_map(|instruction| match instruction {
            MInst::OrStoreIndexed {
                base: BaseReg::SimState,
                offset,
                src,
                size: OpSize::S64,
                ..
            } if *offset == DIRTY as i32 => Some(*src),
            _ => None,
        })
        .expect("an active dirty word should preserve the first batch");
    assert!(
        function
            .blocks
            .iter()
            .flat_map(|block| &block.insts)
            .any(|instruction| matches!(
                instruction,
                MInst::LoadImm { dst, value: 0b1100 } if *dst == dirty_or_mask
            ))
    );
    assert!(
        function
            .blocks
            .iter()
            .flat_map(|block| &block.insts)
            .all(|instruction| !matches!(instruction, MInst::Select { .. })),
        "proved clean/dirty chunks must not select stable versus working storage"
    );

    mir_legalize::legalize(&mut function);
    mir_opt::optimize(&mut function);
    let allocation = regalloc::run_regalloc(&mut function).unwrap();
    mir_opt::post_regalloc_peephole(&mut function, &allocation.assignment);
    function.verify();
    let emitted = emit::emit(
        &function,
        &allocation.assignment,
        allocation.spill_frame_size,
    )
    .unwrap();
    let jit = JitCode::new(&emitted.code).unwrap();

    let mut state = vec![0u8; STATE_SIZE];
    state[STABLE..STABLE + 8].copy_from_slice(&0x10u64.to_le_bytes());
    state[STABLE + 8..STABLE + 16].copy_from_slice(&0x20u64.to_le_bytes());
    state[STABLE + 16..STABLE + 24].copy_from_slice(&0x30u64.to_le_bytes());
    state[STABLE + 24..STABLE + 32].copy_from_slice(&0x40u64.to_le_bytes());
    state[SPARSE..SPARSE + 32].fill(0xff);
    assert_eq!(unsafe { jit.call(&mut state) }, 0);
    assert_eq!(
        u64::from_le_bytes(state[STABLE..STABLE + 8].try_into().unwrap()),
        0x11
    );
    assert_eq!(
        u64::from_le_bytes(state[STABLE + 8..STABLE + 16].try_into().unwrap()),
        0x23
    );
    assert_eq!(
        u64::from_le_bytes(state[STABLE + 16..STABLE + 24].try_into().unwrap()),
        0x31
    );
    assert_eq!(
        u64::from_le_bytes(state[STABLE + 24..STABLE + 32].try_into().unwrap()),
        0x41
    );
    assert_eq!(&state[DIRTY..DIRTY + 8], &[0; 8]);
    assert_eq!(&state[SUMMARY..SUMMARY + 8], &[0; 8]);
    assert_eq!(&state[ACTIVE_BITS..ACTIVE_BITS + 8], &[0; 8]);
}

#[test]
fn static_commit_converts_between_strided_and_packed_array_storage() {
    let source_var = VarId::default();
    let mut packed_var = source_var;
    packed_var.0 += 1;
    let mut destination_var = packed_var;
    destination_var.0 += 1;
    let address = |var_id| AbsoluteAddr {
        instance_id: InstanceId(0),
        var_id,
    };
    let source_abs = address(source_var);
    let packed_abs = address(packed_var);
    let destination_abs = address(destination_var);
    let regioned = |absolute| RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, absolute);
    let eu = ExecutionUnit {
        entry_block_id: SirBlockId(0),
        blocks: [(
            SirBlockId(0),
            BasicBlock {
                id: SirBlockId(0),
                params: vec![],
                instructions: vec![
                    SIRInstruction::Commit(
                        regioned(source_abs),
                        regioned(packed_abs),
                        SIROffset::Static(0),
                        8,
                        vec![],
                    ),
                    SIRInstruction::Commit(
                        regioned(packed_abs),
                        regioned(destination_abs),
                        SIROffset::Static(0),
                        8,
                        vec![],
                    ),
                ],
                terminator: SIRTerminator::Return,
            },
        )]
        .into_iter()
        .collect(),
        register_map: HashMap::default(),
    };
    eu.verify();

    for four_state in [false, true] {
        let mut layout = empty_layout();
        layout.four_state = four_state;
        layout.mode = MemoryLayoutMode::ElementStrided;
        layout.offsets = [(source_abs, 0), (packed_abs, 16), (destination_abs, 24)]
            .into_iter()
            .collect();
        layout.widths = [source_abs, packed_abs, destination_abs]
            .into_iter()
            .map(|absolute| (absolute, 8))
            .collect();
        layout.is_4states = [source_abs, packed_abs, destination_abs]
            .into_iter()
            .map(|absolute| (absolute, four_state))
            .collect();
        let array_layout = celox_state_layout::UnpackedArrayLayout {
            element_width: 2,
            element_count: 4,
            element_stride: 1,
            plane_size: 4,
        };
        layout.unpacked_arrays.insert(source_abs, array_layout);
        layout.unpacked_arrays.insert(destination_abs, array_layout);
        layout.total_size = 40;
        layout.working_base_offset = 40;
        layout.sparse_base_offset = 40;
        layout.sparse_active_bits_offset = 40;
        layout.merged_total_size = 40;
        layout.triggered_bits_offset = 40;
        layout.scratch_base_offset = 40;

        let mut function = lower_execution_unit(&eu, &layout, four_state);
        mir_legalize::legalize(&mut function);
        mir_opt::optimize(&mut function);
        let allocation = regalloc::run_regalloc(&mut function).unwrap();
        mir_opt::post_regalloc_peephole(&mut function, &allocation.assignment);
        function.verify();
        let emitted = emit::emit(
            &function,
            &allocation.assignment,
            allocation.spill_frame_size,
        )
        .unwrap();
        let jit = JitCode::new(&emitted.code).unwrap();

        let mut state = vec![0xa8u8; 40];
        state[0..4].copy_from_slice(&[0xfc, 0xff, 0xfd, 0xfe]);
        state[24..28].fill(0xa8);
        if four_state {
            state[4..8].copy_from_slice(&[0xfd, 0xfe, 0xff, 0xfc]);
            state[28..32].fill(0x54);
        }
        assert_eq!(unsafe { jit.call(&mut state) }, 0);

        assert_eq!(state[16], 0x9c, "packed value, four_state={four_state}");
        assert_eq!(
            &state[24..28],
            &[0xa8, 0xab, 0xa9, 0xaa],
            "strided value, four_state={four_state}"
        );
        if four_state {
            assert_eq!(state[17], 0x39, "packed mask");
            assert_eq!(&state[28..32], &[0x55, 0x56, 0x57, 0x54]);
        }
    }
}

#[test]
fn full_narrow_commit_copies_private_padding_without_rmw() {
    let absolute = AbsoluteAddr {
        instance_id: InstanceId(0),
        var_id: VarId::default(),
    };
    let stable = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, absolute);
    let working = RegionedAbsoluteAddr::from_absolute_addr(crate::WORKING_REGION, absolute);
    let eu = ExecutionUnit {
        entry_block_id: SirBlockId(0),
        blocks: [(
            SirBlockId(0),
            BasicBlock {
                id: SirBlockId(0),
                params: vec![],
                instructions: vec![SIRInstruction::Commit(
                    stable,
                    working,
                    SIROffset::Static(0),
                    1,
                    vec![],
                )],
                terminator: SIRTerminator::Return,
            },
        )]
        .into_iter()
        .collect(),
        register_map: HashMap::default(),
    };
    let mut layout = empty_layout();
    layout.offsets.insert(absolute, 0);
    layout.widths.insert(absolute, 1);
    layout.is_4states.insert(absolute, false);
    layout.working_offsets.insert(absolute, 0);
    layout.working_base_offset = 8;
    layout.total_size = 8;
    layout.merged_total_size = 16;

    let function = lower_execution_unit(&eu, &layout, false);
    let instructions = function
        .blocks
        .iter()
        .flat_map(|block| block.insts.iter())
        .collect::<Vec<_>>();
    assert_eq!(
        instructions
            .iter()
            .filter(|instruction| matches!(
                instruction,
                MInst::Load {
                    offset: 0,
                    size: OpSize::S8,
                    ..
                }
            ))
            .count(),
        1
    );
    assert_eq!(
        instructions
            .iter()
            .filter(|instruction| matches!(
                instruction,
                MInst::Store {
                    offset: 8,
                    size: OpSize::S8,
                    ..
                }
            ))
            .count(),
        1
    );
    assert!(
        instructions
            .iter()
            .all(|instruction| !matches!(instruction, MInst::And { .. } | MInst::Or { .. }))
    );
}
