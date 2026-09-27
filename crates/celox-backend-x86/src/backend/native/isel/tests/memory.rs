use super::*;

#[test]
fn widened_whole_native_variable_loads_at_physical_width() {
    let function = lower_widened_whole_variable_load(32);
    let instructions = &function.blocks[0].insts;

    assert_eq!(instructions.len(), 2);
    assert!(matches!(
        instructions[0],
        MInst::Load {
            dst: VReg(0),
            base: BaseReg::SimState,
            offset: 0,
            size: OpSize::S32,
        }
    ));
    assert!(matches!(instructions[1], MInst::Return));
    assert!(matches!(
        function.spill_descs[0].kind,
        SpillKind::SimState {
            bit_offset: 0,
            width_bits: 32,
            ..
        }
    ));
}

#[test]
fn widened_whole_non_native_variable_keeps_explicit_mask() {
    let function = lower_widened_whole_variable_load(27);
    let instructions = &function.blocks[0].insts;

    assert_eq!(instructions.len(), 3);
    assert!(matches!(
        instructions[0],
        MInst::Load {
            dst: VReg(1),
            base: BaseReg::SimState,
            offset: 0,
            size: OpSize::S64,
        }
    ));
    assert!(matches!(instructions[2], MInst::Return));
    assert!(matches!(
        instructions[1],
        MInst::AndImm32 {
            dst: VReg(0),
            src: VReg(1),
            imm: 0x07ff_ffff,
        }
    ));
}

#[test]
fn recomposes_contiguous_element_quotient_and_remainder_into_byte_index() {
    let (source, array_index) = quotient_remainder_array_load(8);
    assert_eq!(array_index, source);
}

#[test]
fn preserves_element_quotient_and_remainder_when_storage_has_padding() {
    let (source, array_index) = quotient_remainder_array_load(16);
    assert_ne!(array_index, source);
}

#[test]
fn constant_zero_dynamic_element_bit_offset_uses_direct_byte_index() {
    let input_abs = AbsoluteAddr {
        instance_id: InstanceId(0),
        var_id: VarId::default(),
    };
    let mut array_var = VarId::default();
    array_var.0 += 1;
    let array_abs = AbsoluteAddr {
        instance_id: InstanceId(0),
        var_id: array_var,
    };
    let mut output_var = array_var;
    output_var.0 += 1;
    let output_abs = AbsoluteAddr {
        instance_id: InstanceId(0),
        var_id: output_var,
    };
    let input = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, input_abs);
    let array = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, array_abs);
    let output = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, output_abs);
    let index = RegisterId(0);
    let zero = RegisterId(1);
    let loaded = RegisterId(2);
    let unit = ExecutionUnit {
        entry_block_id: SirBlockId(0),
        blocks: [(
            SirBlockId(0),
            BasicBlock {
                id: SirBlockId(0),
                params: vec![],
                instructions: vec![
                    SIRInstruction::Load(index, input, SIROffset::Static(0), 32),
                    SIRInstruction::Imm(zero, SIRValue::new(0u8)),
                    SIRInstruction::Load(
                        loaded,
                        array,
                        SIROffset::Element {
                            index,
                            element_width: 64,
                            bit_offset: 0,
                            dynamic_bit_offset: Some(zero),
                        },
                        54,
                    ),
                    SIRInstruction::Store(output, SIROffset::Static(0), 54, loaded, vec![], vec![]),
                ],
                terminator: SIRTerminator::Return,
            },
        )]
        .into_iter()
        .collect(),
        register_map: [
            (
                index,
                RegisterType::Bit {
                    width: 32,
                    signed: true,
                },
            ),
            (
                zero,
                RegisterType::Bit {
                    width: 64,
                    signed: false,
                },
            ),
            (loaded, RegisterType::Logic { width: 54 }),
        ]
        .into_iter()
        .collect(),
    };
    unit.verify();

    let mut layout = empty_layout();
    layout.mode = MemoryLayoutMode::ElementStrided;
    layout.offsets = [(input_abs, 0), (array_abs, 8), (output_abs, 136)]
        .into_iter()
        .collect();
    layout.widths = [(input_abs, 32), (array_abs, 1024), (output_abs, 54)]
        .into_iter()
        .collect();
    layout.is_4states = [(input_abs, false), (array_abs, false), (output_abs, false)]
        .into_iter()
        .collect();
    layout.unpacked_arrays.insert(
        array_abs,
        celox_state_layout::UnpackedArrayLayout {
            element_width: 64,
            element_count: 16,
            element_stride: 8,
            plane_size: 128,
        },
    );
    layout.total_size = 144;
    layout.working_base_offset = 144;
    layout.sparse_base_offset = 144;
    layout.merged_total_size = 144;
    layout.triggered_bits_offset = 144;
    layout.scratch_base_offset = 144;

    let mut function = lower_execution_unit(&unit, &layout, false);
    function.verify();
    let instructions = function
        .blocks
        .iter()
        .flat_map(|block| &block.insts)
        .collect::<Vec<_>>();
    let byte_index = instructions
        .iter()
        .find_map(|instruction| match instruction {
            MInst::LoadIndexed {
                index,
                offset: 8,
                size: OpSize::S64,
                ..
            } => Some(*index),
            _ => None,
        })
        .expect("direct indexed array load");
    assert!(instructions.iter().any(|instruction| matches!(
        instruction,
        MInst::ShlImm {
            dst,
            imm: 3,
            ..
        } if *dst == byte_index
    )));

    let source_index = instructions
        .iter()
        .find_map(|instruction| match instruction {
            MInst::Load {
                dst,
                offset: 0,
                size: OpSize::S32,
                ..
            } => Some(*dst),
            _ => None,
        })
        .expect("source index load");
    mir_opt::optimize(&mut function);
    function.verify();
    assert!(
        function
            .blocks
            .iter()
            .flat_map(|block| &block.insts)
            .any(|instruction| matches!(
                instruction,
                MInst::LoadIndexed {
                    index,
                    scale: 8,
                    offset: 8,
                    size: OpSize::S64,
                    ..
                } if *index == source_index
            ))
    );
}

#[test]
fn wide_dynamic_element_load_retains_its_bit_offset() {
    let array_abs = AbsoluteAddr {
        instance_id: InstanceId(0),
        var_id: VarId::default(),
    };
    let array = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, array_abs);
    let index = RegisterId(0);
    let loaded = RegisterId(1);
    let unit = ExecutionUnit {
        entry_block_id: SirBlockId(0),
        blocks: [(
            SirBlockId(0),
            BasicBlock {
                id: SirBlockId(0),
                params: vec![],
                instructions: vec![
                    SIRInstruction::Imm(index, SIRValue::new(1u8)),
                    SIRInstruction::Load(
                        loaded,
                        array,
                        SIROffset::Element {
                            index,
                            element_width: 128,
                            bit_offset: 0,
                            dynamic_bit_offset: None,
                        },
                        128,
                    ),
                ],
                terminator: SIRTerminator::Return,
            },
        )]
        .into_iter()
        .collect(),
        register_map: [
            (
                index,
                RegisterType::Bit {
                    width: 1,
                    signed: false,
                },
            ),
            (loaded, RegisterType::Logic { width: 128 }),
        ]
        .into_iter()
        .collect(),
    };
    unit.verify();

    let mut layout = empty_layout();
    layout.mode = MemoryLayoutMode::ElementStrided;
    layout.offsets.insert(array_abs, 0);
    layout.widths.insert(array_abs, 256);
    layout.is_4states.insert(array_abs, false);
    layout.unpacked_arrays.insert(
        array_abs,
        celox_state_layout::UnpackedArrayLayout {
            element_width: 128,
            element_count: 2,
            element_stride: 16,
            plane_size: 32,
        },
    );
    layout.total_size = 32;
    layout.working_base_offset = 32;
    layout.sparse_base_offset = 32;
    layout.merged_total_size = 32;
    layout.triggered_bits_offset = 32;
    layout.scratch_base_offset = 32;

    let function = lower_execution_unit(&unit, &layout, false);
    function.verify();
    assert!(
        function
            .blocks
            .iter()
            .flat_map(|block| &block.insts)
            .filter(|instruction| matches!(
                instruction,
                MInst::LoadIndexed {
                    size: OpSize::S64,
                    ..
                }
            ))
            .count()
            >= 2
    );
}

#[test]
fn full_dynamic_padded_element_uses_native_indexed_load_and_store() {
    let array_abs = AbsoluteAddr {
        instance_id: InstanceId(0),
        var_id: VarId::default(),
    };
    let mut output_var = VarId::default();
    output_var.0 += 1;
    let output_abs = AbsoluteAddr {
        instance_id: InstanceId(0),
        var_id: output_var,
    };
    let array = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, array_abs);
    let output = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, output_abs);
    let index = RegisterId(0);
    let value = RegisterId(1);
    let loaded = RegisterId(2);
    let expected = RegisterId(3);
    let matches = RegisterId(4);
    let element_offset = SIROffset::Element {
        index,
        element_width: 12,
        bit_offset: 0,
        dynamic_bit_offset: None,
    };
    let unit = ExecutionUnit {
        entry_block_id: SirBlockId(0),
        blocks: [(
            SirBlockId(0),
            BasicBlock {
                id: SirBlockId(0),
                params: vec![],
                instructions: vec![
                    SIRInstruction::Imm(index, SIRValue::new(1u8)),
                    SIRInstruction::Load(loaded, array, element_offset.clone(), 12),
                    SIRInstruction::Imm(expected, SIRValue::new(0x123u16)),
                    SIRInstruction::Binary(matches, loaded, BinaryOp::Eq, expected),
                    SIRInstruction::Store(output, SIROffset::Static(0), 1, matches, vec![], vec![]),
                    SIRInstruction::Imm(value, SIRValue::new(0xabcu16)),
                    SIRInstruction::Store(array, element_offset, 12, value, vec![], vec![]),
                ],
                terminator: SIRTerminator::Return,
            },
        )]
        .into_iter()
        .collect(),
        register_map: [
            (
                index,
                RegisterType::Bit {
                    width: 1,
                    signed: false,
                },
            ),
            (value, RegisterType::Logic { width: 12 }),
            (loaded, RegisterType::Logic { width: 12 }),
            (expected, RegisterType::Logic { width: 12 }),
            (
                matches,
                RegisterType::Bit {
                    width: 1,
                    signed: false,
                },
            ),
        ]
        .into_iter()
        .collect(),
    };
    unit.verify();

    let mut layout = empty_layout();
    layout.mode = MemoryLayoutMode::ElementStrided;
    layout.offsets = [(array_abs, 0), (output_abs, 8)].into_iter().collect();
    layout.widths = [(array_abs, 24), (output_abs, 1)].into_iter().collect();
    layout.is_4states = [(array_abs, false), (output_abs, false)]
        .into_iter()
        .collect();
    layout.unpacked_arrays.insert(
        array_abs,
        celox_state_layout::UnpackedArrayLayout {
            element_width: 12,
            element_count: 2,
            element_stride: 2,
            plane_size: 4,
        },
    );
    layout.total_size = 16;
    layout.working_base_offset = 16;
    layout.sparse_base_offset = 16;
    layout.merged_total_size = 16;
    layout.triggered_bits_offset = 16;
    layout.scratch_base_offset = 16;

    let mut function = lower_execution_unit(&unit, &layout, false);
    function.verify();
    assert_eq!(
        function
            .blocks
            .iter()
            .flat_map(|block| &block.insts)
            .filter(|inst| matches!(
                inst,
                MInst::LoadIndexed {
                    size: OpSize::S16,
                    ..
                }
            ))
            .count(),
        1
    );
    assert_eq!(
        function
            .blocks
            .iter()
            .flat_map(|block| &block.insts)
            .filter(|inst| matches!(
                inst,
                MInst::StoreIndexed {
                    size: OpSize::S16,
                    ..
                }
            ))
            .count(),
        1
    );
    assert!(
        function
            .blocks
            .iter()
            .flat_map(|block| &block.insts)
            .all(|inst| !matches!(
                inst,
                MInst::LoadIndexed {
                    size: OpSize::S32,
                    ..
                }
            ))
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

    let mut state = vec![0u8; 16];
    state[2..4].copy_from_slice(&0xf123u16.to_le_bytes());
    assert_eq!(unsafe { jit.call(&mut state) }, 0);
    assert_eq!(u16::from_le_bytes(state[2..4].try_into().unwrap()), 0x0abc);
    assert_eq!(state[8] & 1, 1);
}

#[test]
fn preallocates_vregs_in_sir_register_order() {
    let low = RegisterId(2);
    let middle = RegisterId(7);
    let high = RegisterId(9);
    let register_type = RegisterType::Bit {
        width: 64,
        signed: false,
    };
    let eu = ExecutionUnit {
        entry_block_id: SirBlockId(0),
        blocks: [(
            SirBlockId(0),
            BasicBlock {
                id: SirBlockId(0),
                params: vec![],
                instructions: vec![
                    SIRInstruction::Imm(middle, SIRValue::new(7u8)),
                    SIRInstruction::Imm(low, SIRValue::new(2u8)),
                    SIRInstruction::Binary(high, low, BinaryOp::Add, middle),
                ],
                terminator: SIRTerminator::Return,
            },
        )]
        .into_iter()
        .collect(),
        // Deliberately insert in neither source nor RegisterId order.
        register_map: [
            (high, register_type.clone()),
            (low, register_type.clone()),
            (middle, register_type),
        ]
        .into_iter()
        .collect(),
    };
    eu.verify();

    let function = lower_execution_unit(&eu, &empty_layout(), false);
    let instructions = &function.blocks[0].insts;
    assert!(matches!(
        instructions[0],
        MInst::LoadImm {
            dst: VReg(1),
            value: 7
        }
    ));
    assert!(matches!(
        instructions[1],
        MInst::LoadImm {
            dst: VReg(0),
            value: 2
        }
    ));
    assert!(
        instructions
            .iter()
            .any(|instruction| instruction.def() == Some(VReg(2))),
        "r9 must use the third preallocated VReg: {instructions:?}"
    );
}

#[test]
fn static_unaligned_64_bit_load_preserves_crossing_bits() {
    assert_eq!(execute_unaligned_64_bit_load(false), 0xfedc_ba98_8000_0004);
}

#[test]
fn dynamic_unaligned_64_bit_load_preserves_crossing_bits() {
    assert_eq!(execute_unaligned_64_bit_load(true), 0xfedc_ba98_8000_0004);
}

#[test]
fn repeated_dynamic_loads_share_one_block_local_state_word() {
    let input_abs = AbsoluteAddr {
        instance_id: InstanceId(0),
        var_id: VarId::default(),
    };
    let mut output_var = VarId::default();
    output_var.0 += 1;
    let output_abs = AbsoluteAddr {
        instance_id: InstanceId(0),
        var_id: output_var,
    };
    let input = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, input_abs);
    let output = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, output_abs);
    let offsets = [RegisterId(0), RegisterId(1), RegisterId(2), RegisterId(3)];
    let bits = [RegisterId(4), RegisterId(5), RegisterId(6), RegisterId(7)];
    let packed = RegisterId(8);
    let mut instructions = Vec::new();
    for (index, (&offset, &bit)) in offsets.iter().zip(&bits).enumerate() {
        instructions.push(SIRInstruction::Imm(offset, SIRValue::new(index as u8)));
        instructions.push(SIRInstruction::Load(
            bit,
            input,
            SIROffset::Dynamic(offset),
            1,
        ));
    }
    instructions.push(SIRInstruction::Concat(
        packed,
        bits.iter().rev().copied().collect(),
    ));
    instructions.push(SIRInstruction::Store(
        output,
        SIROffset::Static(0),
        4,
        packed,
        vec![],
        vec![],
    ));
    let unit = ExecutionUnit {
        entry_block_id: SirBlockId(0),
        blocks: [(
            SirBlockId(0),
            BasicBlock {
                id: SirBlockId(0),
                params: vec![],
                instructions,
                terminator: SIRTerminator::Return,
            },
        )]
        .into_iter()
        .collect(),
        register_map: offsets
            .into_iter()
            .map(|register| {
                (
                    register,
                    RegisterType::Bit {
                        width: 5,
                        signed: false,
                    },
                )
            })
            .chain(bits.into_iter().map(|register| {
                (
                    register,
                    RegisterType::Bit {
                        width: 1,
                        signed: false,
                    },
                )
            }))
            .chain([(
                packed,
                RegisterType::Bit {
                    width: 4,
                    signed: false,
                },
            )])
            .collect(),
    };
    unit.verify();

    let mut layout = empty_layout();
    layout.offsets = [(input_abs, 0), (output_abs, 4)].into_iter().collect();
    layout.widths = [(input_abs, 32), (output_abs, 4)].into_iter().collect();
    layout.is_4states = [(input_abs, false), (output_abs, false)]
        .into_iter()
        .collect();
    layout.total_size = 5;
    layout.working_base_offset = 5;
    layout.sparse_base_offset = 5;
    layout.merged_total_size = 5;
    layout.triggered_bits_offset = 5;
    layout.scratch_base_offset = 5;

    let plan = block_dynamic_load_cache_plans(&unit.blocks[&SirBlockId(0)], &layout);
    assert_eq!(plan.addresses, [input].into_iter().collect());
    let mut function = lower_execution_unit(&unit, &layout, false);
    assert_eq!(
        function
            .blocks
            .iter()
            .flat_map(|block| &block.insts)
            .filter(|instruction| matches!(
                instruction,
                MInst::Load {
                    offset: 0,
                    size: OpSize::S32,
                    ..
                }
            ))
            .count(),
        1
    );
    assert!(
        !function
            .blocks
            .iter()
            .flat_map(|block| &block.insts)
            .any(|instruction| matches!(instruction, MInst::LoadIndexed { .. }))
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
    let mut state = vec![0u8; layout.total_size];
    state[0] = 0b1010;
    assert_eq!(unsafe { jit.call(&mut state) }, 0);
    assert_eq!(state[4] & 0xf, 0b1010);

    let mut writing = unit.blocks[&SirBlockId(0)].clone();
    writing.instructions.push(SIRInstruction::Store(
        input,
        SIROffset::Static(0),
        1,
        bits[0],
        vec![],
        vec![],
    ));
    assert!(
        block_dynamic_load_cache_plans(&writing, &layout)
            .addresses
            .is_empty()
    );
}

#[test]
fn static_scalar_load_store_alignment_matrix() {
    verify_scalar_alignment_matrix(false, false);
}

#[test]
fn dynamic_scalar_load_store_alignment_matrix() {
    verify_scalar_alignment_matrix(true, false);
}

#[test]
fn static_four_state_scalar_load_store_alignment_matrix() {
    verify_scalar_alignment_matrix(false, true);
}

#[test]
fn dynamic_four_state_scalar_load_store_alignment_matrix() {
    verify_scalar_alignment_matrix(true, true);
}

#[test]
fn static_wide_load_store_alignment_matrix() {
    verify_wide_alignment_matrix(false);
}

#[test]
fn dynamic_wide_load_store_alignment_matrix() {
    verify_wide_alignment_matrix(true);
}

#[test]
fn narrowed_wide_binary_store_does_not_write_source_width() {
    let source_var = VarId::default();
    let mut destination_var = source_var;
    destination_var.0 += 1;
    let source_abs = AbsoluteAddr {
        instance_id: InstanceId(0),
        var_id: source_var,
    };
    let destination_abs = AbsoluteAddr {
        instance_id: InstanceId(0),
        var_id: destination_var,
    };
    let source_addr = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, source_abs);
    let destination_addr = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, destination_abs);

    let source = RegisterId(0);
    let shift_amount = RegisterId(1);
    let shifted = RegisterId(2);
    let width_mask = RegisterId(3);
    let narrowed = RegisterId(4);
    let bit_type = |width| RegisterType::Bit {
        width,
        signed: false,
    };
    let eu = ExecutionUnit {
        entry_block_id: SirBlockId(0),
        blocks: [(
            SirBlockId(0),
            BasicBlock {
                id: SirBlockId(0),
                params: vec![],
                instructions: vec![
                    SIRInstruction::Load(source, source_addr, SIROffset::Static(0), 309),
                    SIRInstruction::Imm(shift_amount, SIRValue::new(35u8)),
                    SIRInstruction::Binary(shifted, source, BinaryOp::Shr, shift_amount),
                    SIRInstruction::Imm(
                        width_mask,
                        SIRValue::new((BigUint::from(1u8) << 274usize) - BigUint::from(1u8)),
                    ),
                    SIRInstruction::Binary(narrowed, shifted, BinaryOp::And, width_mask),
                    SIRInstruction::Store(
                        destination_addr,
                        SIROffset::Static(35),
                        274,
                        narrowed,
                        vec![],
                        vec![],
                    ),
                ],
                terminator: SIRTerminator::Return,
            },
        )]
        .into_iter()
        .collect(),
        register_map: [
            (source, bit_type(309)),
            (shift_amount, bit_type(8)),
            (shifted, bit_type(309)),
            (width_mask, bit_type(274)),
            (narrowed, bit_type(274)),
        ]
        .into_iter()
        .collect(),
    };
    eu.verify();

    let layout = MemoryLayout {
        trace: None,
        four_state: false,
        mode: MemoryLayoutMode::Packed,
        unpacked_arrays: HashMap::default(),
        offsets: [(source_abs, 0), (destination_abs, 64)]
            .into_iter()
            .collect(),
        widths: [(source_abs, 309), (destination_abs, 344)]
            .into_iter()
            .collect(),
        is_4states: [(source_abs, false), (destination_abs, false)]
            .into_iter()
            .collect(),
        total_size: 112,
        working_offsets: HashMap::default(),
        working_base_offset: 112,
        sparse_offsets: HashMap::default(),
        sparse_base_offset: 112,
        sparse_layouts: HashMap::default(),
        sparse_active_bits_offset: 112,
        sparse_active_capacity: 0,
        merged_total_size: 112,
        triggered_bits_offset: 112,
        triggered_bits_total_size: 0,
        scratch_base_offset: 112,
        scratch_size: 0,
        runtime_event_capacity: 0,
        runtime_event_slot_size: 0,
        runtime_event_buffer_size: 0,
        runtime_event_site_layouts: vec![],
    };
    let mut function = lower_execution_unit(&eu, &layout, false);
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

    let mut state = vec![0xa5u8; 112];
    for (index, byte) in state[..39].iter_mut().enumerate() {
        *byte = (index as u8).wrapping_mul(73).wrapping_add(0x5b);
    }
    let before = state.clone();
    let mut expected = before[64..107].to_vec();
    for bit in 0..274 {
        let value = get_bits(&before[..39], bit + 35, 1);
        set_bits(&mut expected, bit + 35, 1, value);
    }

    assert_eq!(unsafe { jit.call(&mut state) }, 0);
    for bit in 0..344 {
        assert_eq!(
            get_bits(&state[64..107], bit, 1),
            get_bits(&expected, bit, 1),
            "destination bit {bit} differs"
        );
    }
    assert_eq!(&state[107..], &before[107..], "store exceeded its variable");
}

#[test]
fn full_static_native_access_must_fit_allocated_bytes_exactly() {
    for (width, expected) in [
        (0, None),
        (17, None),
        (24, None),
        (33, None),
        (40, None),
        (56, None),
        (65, None),
        (31, Some(OpSize::S32)),
        (32, Some(OpSize::S32)),
        (63, Some(OpSize::S64)),
        (64, Some(OpSize::S64)),
    ] {
        assert_eq!(
            ISelContext::exact_storage_access_size(width),
            expected,
            "width={width}"
        );
    }
}

fn lower_widened_whole_variable_load(variable_width: usize) -> MFunction {
    let absolute = AbsoluteAddr {
        instance_id: InstanceId(0),
        var_id: VarId::default(),
    };
    let address = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, absolute);
    let loaded = RegisterId(0);
    let unit = ExecutionUnit {
        entry_block_id: SirBlockId(0),
        blocks: [(
            SirBlockId(0),
            BasicBlock {
                id: SirBlockId(0),
                params: vec![],
                instructions: vec![SIRInstruction::Load(
                    loaded,
                    address,
                    SIROffset::Static(0),
                    64,
                )],
                terminator: SIRTerminator::Return,
            },
        )]
        .into_iter()
        .collect(),
        register_map: [(loaded, RegisterType::Logic { width: 64 })]
            .into_iter()
            .collect(),
    };
    unit.verify();

    let mut layout = empty_layout();
    let byte_width = variable_width.div_ceil(8);
    layout.offsets.insert(absolute, 0);
    layout.widths.insert(absolute, variable_width);
    layout.is_4states.insert(absolute, false);
    layout.total_size = byte_width;
    layout.working_base_offset = byte_width;
    layout.sparse_base_offset = byte_width;
    layout.sparse_active_bits_offset = byte_width;
    layout.merged_total_size = byte_width;
    layout.triggered_bits_offset = byte_width;
    layout.scratch_base_offset = byte_width;

    lower_execution_unit(&unit, &layout, false)
}

fn quotient_remainder_array_load(element_stride: usize) -> (VReg, VReg) {
    let input_abs = AbsoluteAddr {
        instance_id: InstanceId(0),
        var_id: VarId::default(),
    };
    let mut array_var = VarId::default();
    array_var.0 += 1;
    let array_abs = AbsoluteAddr {
        instance_id: InstanceId(0),
        var_id: array_var,
    };
    let input = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, input_abs);
    let array = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, array_abs);

    let source = RegisterId(0);
    let shift = RegisterId(1);
    let quotient = RegisterId(2);
    let lane_width = RegisterId(3);
    let product = RegisterId(4);
    let remainder_mask = RegisterId(5);
    let remainder = RegisterId(6);
    let loaded = RegisterId(7);
    let unit = ExecutionUnit {
        entry_block_id: SirBlockId(0),
        blocks: [(
            SirBlockId(0),
            BasicBlock {
                id: SirBlockId(0),
                params: vec![],
                instructions: vec![
                    SIRInstruction::Load(source, input, SIROffset::Static(0), 32),
                    SIRInstruction::Imm(shift, SIRValue::new(3u8)),
                    SIRInstruction::Binary(quotient, source, BinaryOp::Shr, shift),
                    SIRInstruction::Imm(lane_width, SIRValue::new(8u8)),
                    SIRInstruction::Binary(product, source, BinaryOp::Mul, lane_width),
                    SIRInstruction::Imm(remainder_mask, SIRValue::new(63u8)),
                    SIRInstruction::Binary(remainder, product, BinaryOp::And, remainder_mask),
                    SIRInstruction::Load(
                        loaded,
                        array,
                        SIROffset::Element {
                            index: quotient,
                            element_width: 64,
                            bit_offset: 0,
                            dynamic_bit_offset: Some(remainder),
                        },
                        8,
                    ),
                ],
                terminator: SIRTerminator::Return,
            },
        )]
        .into_iter()
        .collect(),
        register_map: (0..=6)
            .map(|register| {
                (
                    RegisterId(register),
                    RegisterType::Bit {
                        width: 32,
                        signed: false,
                    },
                )
            })
            .chain([(loaded, RegisterType::Logic { width: 8 })])
            .collect(),
    };
    unit.verify();

    let mut layout = empty_layout();
    layout.mode = MemoryLayoutMode::ElementStrided;
    layout.offsets = [(input_abs, 0), (array_abs, 8)].into_iter().collect();
    layout.widths = [(input_abs, 32), (array_abs, 128)].into_iter().collect();
    layout.is_4states = [(input_abs, false), (array_abs, false)]
        .into_iter()
        .collect();
    layout.unpacked_arrays.insert(
        array_abs,
        celox_state_layout::UnpackedArrayLayout {
            element_width: 64,
            element_count: 2,
            element_stride,
            plane_size: element_stride * 2,
        },
    );
    layout.total_size = 32;
    layout.working_base_offset = 32;
    layout.sparse_base_offset = 32;
    layout.merged_total_size = 32;
    layout.triggered_bits_offset = 32;
    layout.scratch_base_offset = 32;

    let function = lower_execution_unit(&unit, &layout, false);
    function.verify();
    let instructions = function
        .blocks
        .iter()
        .flat_map(|block| &block.insts)
        .collect::<Vec<_>>();
    let source_vreg = instructions
        .iter()
        .find_map(|instruction| match instruction {
            MInst::Load {
                dst,
                offset: 0,
                size: OpSize::S32,
                ..
            } => Some(*dst),
            _ => None,
        })
        .expect("source load");
    let array_index = instructions
        .iter()
        .find_map(|instruction| match instruction {
            MInst::LoadIndexed {
                index,
                offset: 8,
                size: OpSize::S8,
                ..
            } => Some(*index),
            _ => None,
        })
        .expect("array load");
    (source_vreg, array_index)
}

fn execute_unaligned_64_bit_load(dynamic: bool) -> u64 {
    let input_var = VarId::default();
    let mut output_var = input_var;
    output_var.0 += 1;
    let input_abs = AbsoluteAddr {
        instance_id: InstanceId(0),
        var_id: input_var,
    };
    let output_abs = AbsoluteAddr {
        instance_id: InstanceId(0),
        var_id: output_var,
    };
    let input_addr = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, input_abs);
    let output_addr = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, output_abs);
    let offset = RegisterId(0);
    let loaded = RegisterId(1);
    let mut instructions = Vec::new();
    let load_offset = if dynamic {
        instructions.push(SIRInstruction::Imm(offset, SIRValue::new(6u8)));
        SIROffset::Dynamic(offset)
    } else {
        SIROffset::Static(6)
    };
    instructions.push(SIRInstruction::Load(loaded, input_addr, load_offset, 64));
    instructions.push(SIRInstruction::Store(
        output_addr,
        SIROffset::Static(0),
        64,
        loaded,
        vec![],
        vec![],
    ));
    let eu = ExecutionUnit {
        entry_block_id: SirBlockId(0),
        blocks: [(
            SirBlockId(0),
            BasicBlock {
                id: SirBlockId(0),
                params: vec![],
                instructions,
                terminator: SIRTerminator::Return,
            },
        )]
        .into_iter()
        .collect(),
        register_map: [
            (
                offset,
                RegisterType::Bit {
                    width: 7,
                    signed: false,
                },
            ),
            (
                loaded,
                RegisterType::Bit {
                    width: 64,
                    signed: false,
                },
            ),
        ]
        .into_iter()
        .collect(),
    };
    eu.verify();

    let layout = MemoryLayout {
        trace: None,
        four_state: false,
        mode: MemoryLayoutMode::Packed,
        unpacked_arrays: HashMap::default(),
        offsets: [(input_abs, 0), (output_abs, 16)].into_iter().collect(),
        widths: [(input_abs, 72), (output_abs, 64)].into_iter().collect(),
        is_4states: [(input_abs, false), (output_abs, false)]
            .into_iter()
            .collect(),
        total_size: 24,
        working_offsets: HashMap::default(),
        working_base_offset: 24,
        sparse_offsets: HashMap::default(),
        sparse_base_offset: 24,
        sparse_layouts: HashMap::default(),
        sparse_active_bits_offset: 24,
        sparse_active_capacity: 0,
        merged_total_size: 24,
        triggered_bits_offset: 24,
        triggered_bits_total_size: 0,
        scratch_base_offset: 24,
        scratch_size: 0,
        runtime_event_capacity: 0,
        runtime_event_slot_size: 0,
        runtime_event_buffer_size: 0,
        runtime_event_site_layouts: vec![],
    };
    let mut function = lower_execution_unit(&eu, &layout, false);
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
    let expected = 0xfedc_ba98_8000_0004u64;
    let input = (BigUint::from(expected) << 6usize).to_bytes_le();
    let mut state = vec![0u8; 24];
    state[..input.len()].copy_from_slice(&input);
    assert_eq!(unsafe { jit.call(&mut state) }, 0);
    u64::from_le_bytes(state[16..24].try_into().unwrap())
}

fn verify_scalar_alignment_matrix(dynamic: bool, four_state: bool) {
    const SLOT: usize = 768;
    const CASE_STRIDE: usize = SLOT * 3;
    const DYNAMIC_OFFSETS: &[usize] = &[
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 13, 63, 64, 65, 309, 618, 927, 2163,
    ];
    let mut instructions = Vec::new();
    let mut register_map = HashMap::default();
    let mut offsets = HashMap::default();
    let mut widths = HashMap::default();
    let mut is_4states = HashMap::default();
    let mut cases = Vec::new();
    let mut next_reg = 0usize;
    let mut case_index = 0usize;

    for width in 1..=64usize {
        for &bit_offset in DYNAMIC_OFFSETS {
            let storage_width = bit_offset + width;
            let source_abs = AbsoluteAddr {
                instance_id: InstanceId(0),
                var_id: VarId::from_raw((case_index * 3) as u32),
            };
            let output_abs = AbsoluteAddr {
                instance_id: InstanceId(0),
                var_id: VarId::from_raw((case_index * 3 + 1) as u32),
            };
            let destination_abs = AbsoluteAddr {
                instance_id: InstanceId(0),
                var_id: VarId::from_raw((case_index * 3 + 2) as u32),
            };
            let base = case_index * CASE_STRIDE;
            offsets.insert(source_abs, base);
            offsets.insert(output_abs, base + SLOT);
            offsets.insert(destination_abs, base + SLOT * 2);
            widths.insert(source_abs, storage_width);
            widths.insert(output_abs, width);
            widths.insert(destination_abs, storage_width);
            is_4states.insert(source_abs, four_state);
            is_4states.insert(output_abs, four_state);
            is_4states.insert(destination_abs, four_state);

            let loaded = RegisterId(next_reg);
            next_reg += 1;
            register_map.insert(
                loaded,
                if four_state {
                    RegisterType::Logic { width }
                } else {
                    RegisterType::Bit {
                        width,
                        signed: false,
                    }
                },
            );
            let offset_operand = if dynamic {
                let offset_reg = RegisterId(next_reg);
                next_reg += 1;
                register_map.insert(
                    offset_reg,
                    RegisterType::Bit {
                        width: 12,
                        signed: false,
                    },
                );
                instructions.push(SIRInstruction::Imm(
                    offset_reg,
                    SIRValue::new(bit_offset as u64),
                ));
                SIROffset::Dynamic(offset_reg)
            } else {
                SIROffset::Static(bit_offset)
            };
            instructions.push(SIRInstruction::Load(
                loaded,
                RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, source_abs),
                offset_operand.clone(),
                width,
            ));
            instructions.push(SIRInstruction::Store(
                RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, output_abs),
                SIROffset::Static(0),
                width,
                loaded,
                vec![],
                vec![],
            ));
            instructions.push(SIRInstruction::Store(
                RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, destination_abs),
                offset_operand,
                width,
                loaded,
                vec![],
                vec![],
            ));
            cases.push((base, width, bit_offset, storage_width));
            case_index += 1;
        }
    }

    let total_size = case_index * CASE_STRIDE;
    let eu = ExecutionUnit {
        entry_block_id: SirBlockId(0),
        blocks: [(
            SirBlockId(0),
            BasicBlock {
                id: SirBlockId(0),
                params: vec![],
                instructions,
                terminator: SIRTerminator::Return,
            },
        )]
        .into_iter()
        .collect(),
        register_map,
    };
    eu.verify();
    let layout = MemoryLayout {
        trace: None,
        four_state,
        mode: MemoryLayoutMode::Packed,
        unpacked_arrays: HashMap::default(),
        offsets,
        widths,
        is_4states,
        total_size,
        working_offsets: HashMap::default(),
        working_base_offset: total_size,
        sparse_offsets: HashMap::default(),
        sparse_base_offset: total_size,
        sparse_layouts: HashMap::default(),
        sparse_active_bits_offset: total_size,
        sparse_active_capacity: 0,
        merged_total_size: total_size,
        triggered_bits_offset: total_size,
        triggered_bits_total_size: 0,
        scratch_base_offset: total_size,
        scratch_size: 0,
        runtime_event_capacity: 0,
        runtime_event_slot_size: 0,
        runtime_event_buffer_size: 0,
        runtime_event_site_layouts: vec![],
    };
    let mut function = lower_execution_unit(&eu, &layout, four_state);
    if dynamic {
        assert_indexed_state_accesses_have_alias_ranges(&function);
    }
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

    let mut state = vec![0u8; total_size];
    for (index, byte) in state.iter_mut().enumerate() {
        *byte = (index as u8).wrapping_mul(73).wrapping_add(0x5b);
    }
    let before = state.clone();
    assert_eq!(unsafe { jit.call(&mut state) }, 0);
    for (base, width, bit_offset, storage_width) in cases {
        let source_bytes = storage_width.div_ceil(8);
        let output_bytes = width.div_ceil(8);
        let expected = get_bits(&before[base..base + SLOT], bit_offset, width);
        assert_eq!(
            get_bits(&state[base + SLOT..base + SLOT * 2], 0, width),
            expected,
            "value load mismatch: dynamic={dynamic} four_state={four_state} width={width} bit_offset={bit_offset}"
        );
        let mut expected_destination = before[base + SLOT * 2..base + SLOT * 3].to_vec();
        set_bits(&mut expected_destination, bit_offset, width, expected);
        if four_state {
            let expected_mask =
                get_bits(&before[base + source_bytes..base + SLOT], bit_offset, width);
            assert_eq!(
                get_bits(
                    &state[base + SLOT + output_bytes..base + SLOT * 2],
                    0,
                    width,
                ),
                expected_mask,
                "mask load mismatch: dynamic={dynamic} width={width} bit_offset={bit_offset}"
            );
            set_bits(
                &mut expected_destination,
                source_bytes * 8 + bit_offset,
                width,
                expected_mask,
            );
        }
        let destination = &state[base + SLOT * 2..base + SLOT * 3];
        for bit in 0..storage_width {
            assert_eq!(
                get_bits(destination, bit, 1),
                get_bits(&expected_destination, bit, 1),
                "value store mismatch: dynamic={dynamic} four_state={four_state} width={width} bit_offset={bit_offset} bit={bit}"
            );
            if four_state {
                assert_eq!(
                    get_bits(destination, source_bytes * 8 + bit, 1),
                    get_bits(&expected_destination, source_bytes * 8 + bit, 1),
                    "mask store mismatch: dynamic={dynamic} width={width} bit_offset={bit_offset} bit={bit}"
                );
            }
        }
        let allocated_bytes = source_bytes * if four_state { 2 } else { 1 };
        assert_eq!(
            &destination[allocated_bytes..],
            &expected_destination[allocated_bytes..],
            "store clobbered adjacent storage: dynamic={dynamic} four_state={four_state} width={width} bit_offset={bit_offset}"
        );
    }
}

fn verify_wide_alignment_matrix(dynamic: bool) {
    const SLOT: usize = 384;
    const CASE_STRIDE: usize = SLOT * 3;
    const DYNAMIC_OFFSETS: &[usize] = &[
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 13, 63, 64, 65, 309, 618, 927, 2163,
    ];
    let tested_widths = [65usize, 72, 127, 128, 129, 255, 274, 309];
    let mut instructions = Vec::new();
    let mut register_map = HashMap::default();
    let mut offsets = HashMap::default();
    let mut widths = HashMap::default();
    let mut is_4states = HashMap::default();
    let mut cases = Vec::new();
    let mut next_reg = 0usize;
    let mut case_index = 0usize;

    for width in tested_widths {
        for &bit_offset in DYNAMIC_OFFSETS {
            let storage_width = bit_offset + width;
            let source_abs = AbsoluteAddr {
                instance_id: InstanceId(0),
                var_id: VarId::from_raw((case_index * 3) as u32),
            };
            let output_abs = AbsoluteAddr {
                instance_id: InstanceId(0),
                var_id: VarId::from_raw((case_index * 3 + 1) as u32),
            };
            let destination_abs = AbsoluteAddr {
                instance_id: InstanceId(0),
                var_id: VarId::from_raw((case_index * 3 + 2) as u32),
            };
            let base = case_index * CASE_STRIDE;
            offsets.insert(source_abs, base);
            offsets.insert(output_abs, base + SLOT);
            offsets.insert(destination_abs, base + SLOT * 2);
            widths.insert(source_abs, storage_width);
            widths.insert(output_abs, width);
            widths.insert(destination_abs, storage_width);
            is_4states.insert(source_abs, false);
            is_4states.insert(output_abs, false);
            is_4states.insert(destination_abs, false);

            let loaded = RegisterId(next_reg);
            next_reg += 1;
            register_map.insert(
                loaded,
                RegisterType::Bit {
                    width,
                    signed: false,
                },
            );
            let offset_operand = if dynamic {
                let offset_reg = RegisterId(next_reg);
                next_reg += 1;
                register_map.insert(
                    offset_reg,
                    RegisterType::Bit {
                        width: 12,
                        signed: false,
                    },
                );
                instructions.push(SIRInstruction::Imm(
                    offset_reg,
                    SIRValue::new(bit_offset as u64),
                ));
                SIROffset::Dynamic(offset_reg)
            } else {
                SIROffset::Static(bit_offset)
            };
            instructions.push(SIRInstruction::Load(
                loaded,
                RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, source_abs),
                offset_operand.clone(),
                width,
            ));
            instructions.push(SIRInstruction::Store(
                RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, output_abs),
                SIROffset::Static(0),
                width,
                loaded,
                vec![],
                vec![],
            ));
            instructions.push(SIRInstruction::Store(
                RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, destination_abs),
                offset_operand,
                width,
                loaded,
                vec![],
                vec![],
            ));
            cases.push((base, width, bit_offset));
            case_index += 1;
        }
    }

    let total_size = case_index * CASE_STRIDE;
    let eu = ExecutionUnit {
        entry_block_id: SirBlockId(0),
        blocks: [(
            SirBlockId(0),
            BasicBlock {
                id: SirBlockId(0),
                params: vec![],
                instructions,
                terminator: SIRTerminator::Return,
            },
        )]
        .into_iter()
        .collect(),
        register_map,
    };
    eu.verify();
    let layout = MemoryLayout {
        trace: None,
        four_state: false,
        mode: MemoryLayoutMode::Packed,
        unpacked_arrays: HashMap::default(),
        offsets,
        widths,
        is_4states,
        total_size,
        working_offsets: HashMap::default(),
        working_base_offset: total_size,
        sparse_offsets: HashMap::default(),
        sparse_base_offset: total_size,
        sparse_layouts: HashMap::default(),
        sparse_active_bits_offset: total_size,
        sparse_active_capacity: 0,
        merged_total_size: total_size,
        triggered_bits_offset: total_size,
        triggered_bits_total_size: 0,
        scratch_base_offset: total_size,
        scratch_size: 0,
        runtime_event_capacity: 0,
        runtime_event_slot_size: 0,
        runtime_event_buffer_size: 0,
        runtime_event_site_layouts: vec![],
    };
    let mut function = lower_execution_unit(&eu, &layout, false);
    if dynamic {
        assert_indexed_state_accesses_have_alias_ranges(&function);
    }
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
    let mut state = vec![0u8; total_size];
    for (index, byte) in state.iter_mut().enumerate() {
        *byte = (index as u8).wrapping_mul(73).wrapping_add(0x5b);
    }
    let before = state.clone();
    assert_eq!(unsafe { jit.call(&mut state) }, 0);
    for (base, width, bit_offset) in cases {
        let mut expected_destination = before[base + SLOT * 2..base + SLOT * 3].to_vec();
        for bit in 0..width {
            let source_bit = get_bits(&before[base..base + SLOT], bit_offset + bit, 1);
            set_bits(&mut expected_destination, bit_offset + bit, 1, source_bit);
            assert_eq!(
                get_bits(&state[base + SLOT..base + SLOT * 2], bit, 1),
                source_bit,
                "wide load mismatch: dynamic={dynamic} width={width} bit_offset={bit_offset} bit={bit}"
            );
        }
        assert_eq!(
            &state[base + SLOT * 2..base + SLOT * 3],
            expected_destination.as_slice(),
            "wide store mismatch: dynamic={dynamic} width={width} bit_offset={bit_offset}"
        );
    }
}
