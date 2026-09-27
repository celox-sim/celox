use super::*;

#[test]
fn lowers_bit_packed_field_compare_to_word_swar() {
    if !crate::native::features::X86Features::detect().bmi2() {
        return;
    }

    const LANES: usize = 32;
    const FIELD_WIDTH: usize = 12;
    const MATCH: u64 = 0x300;
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
    let mut next_register = 0usize;
    let mut register_map = HashMap::default();
    let mut allocate = |width: usize| {
        let register = RegisterId(next_register);
        next_register += 1;
        register_map.insert(
            register,
            RegisterType::Bit {
                width,
                signed: false,
            },
        );
        register
    };
    let constant = allocate(FIELD_WIDTH);
    let mut instructions = vec![SIRInstruction::Imm(constant, SIRValue::new(MATCH))];
    let mut predicates = Vec::with_capacity(LANES);
    for lane in 0..LANES {
        let field = allocate(FIELD_WIDTH);
        let predicate = allocate(1);
        instructions.push(SIRInstruction::Load(
            field,
            input,
            SIROffset::PackedElements {
                bit_offset: lane * FIELD_WIDTH,
                element_width: FIELD_WIDTH,
            },
            FIELD_WIDTH,
        ));
        instructions.push(SIRInstruction::Binary(
            predicate,
            field,
            BinaryOp::Eq,
            constant,
        ));
        predicates.push(predicate);
    }
    predicates.reverse();
    let packed = allocate(LANES);
    instructions.push(SIRInstruction::Concat(packed, predicates));
    instructions.push(SIRInstruction::Store(
        output,
        SIROffset::Static(0),
        LANES,
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
        register_map,
    };
    unit.verify();

    let input_bytes = (LANES * FIELD_WIDTH).div_ceil(8);
    let output_offset = input_bytes;
    let total_size = output_offset + LANES.div_ceil(8);
    let mut layout = empty_layout();
    layout.offsets = [(input_abs, 0), (output_abs, output_offset)]
        .into_iter()
        .collect();
    layout.widths = [(input_abs, LANES * FIELD_WIDTH), (output_abs, LANES)]
        .into_iter()
        .collect();
    layout.is_4states = [(input_abs, false), (output_abs, false)]
        .into_iter()
        .collect();
    layout.total_size = total_size;
    layout.working_base_offset = total_size;
    layout.sparse_base_offset = total_size;
    layout.merged_total_size = total_size;
    layout.triggered_bits_offset = total_size;
    layout.scratch_base_offset = total_size;

    let mut function = lower_execution_unit(&unit, &layout, false);
    function.verify();
    let instructions = function
        .blocks
        .iter()
        .flat_map(|block| &block.insts)
        .collect::<Vec<_>>();
    assert_eq!(
        instructions
            .iter()
            .filter(|instruction| matches!(instruction, MInst::Pext { .. }))
            .count(),
        LANES.div_ceil(64 / FIELD_WIDTH)
    );
    assert!(
        !instructions
            .iter()
            .any(|instruction| matches!(instruction, MInst::Cmp { .. }))
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

    let mut packed_input = BigUint::ZERO;
    let mut expected = 0u32;
    for lane in 0..LANES {
        let value = if lane.is_multiple_of(3) {
            expected |= 1u32 << lane;
            MATCH
        } else {
            (lane as u64 * 37 + 1) & mask_for_width(FIELD_WIDTH)
        };
        packed_input |= BigUint::from(value) << (lane * FIELD_WIDTH);
    }
    let mut state = vec![0u8; total_size];
    let bytes = packed_input.to_bytes_le();
    state[..bytes.len()].copy_from_slice(&bytes);
    assert_eq!(unsafe { jit.call(&mut state) }, 0);
    assert_eq!(
        u32::from_le_bytes(state[output_offset..output_offset + 4].try_into().unwrap()),
        expected
    );
}

#[test]
fn selects_complete_byte_affine_predicate_pack() {
    let base = RegisterId(0);
    let rhs = RegisterId(1);
    let packed = RegisterId(2);
    let byte_type = RegisterType::Bit {
        width: 8,
        signed: false,
    };
    let predicate_type = RegisterType::Bit {
        width: 1,
        signed: false,
    };
    let mut register_map = HashMap::default();
    register_map.insert(base, byte_type.clone());
    register_map.insert(rhs, byte_type.clone());
    register_map.insert(
        packed,
        RegisterType::Bit {
            width: 16,
            signed: false,
        },
    );
    let mut instructions = Vec::new();
    let mut predicates = Vec::new();
    let mut next_register = 3usize;
    for lane in 0..16 {
        let increment = RegisterId(next_register);
        let affine = RegisterId(next_register + 1);
        let predicate = RegisterId(next_register + 2);
        next_register += 3;
        register_map.insert(increment, byte_type.clone());
        register_map.insert(affine, byte_type.clone());
        register_map.insert(predicate, predicate_type.clone());
        instructions.push(SIRInstruction::Imm(increment, SIRValue::new(lane as u8)));
        instructions.push(SIRInstruction::Binary(
            affine,
            base,
            BinaryOp::Add,
            increment,
        ));
        instructions.push(SIRInstruction::Binary(
            predicate,
            affine,
            BinaryOp::LtU,
            rhs,
        ));
        predicates.push(predicate);
    }
    predicates.reverse();
    instructions.push(SIRInstruction::Concat(packed, predicates));
    let block = BasicBlock {
        id: SirBlockId(0),
        params: vec![base, rhs],
        instructions,
        terminator: SIRTerminator::Return,
    };
    let mut blocks = HashMap::default();
    blocks.insert(SirBlockId(0), block);
    let unit = ExecutionUnit {
        entry_block_id: SirBlockId(0),
        blocks,
        register_map,
    };
    unit.verify();

    let function = lower_execution_unit(&unit, &empty_layout(), false);
    let affine_compares = function
        .blocks
        .iter()
        .flat_map(|block| block.insts.iter())
        .filter(|instruction| matches!(instruction, MInst::PackedByteAffineCompare { .. }))
        .count();
    assert_eq!(affine_compares, 1);
}

#[test]
fn combines_packed_bits_stored_to_one_byte_lanes() {
    let absolute = AbsoluteAddr {
        instance_id: InstanceId(0),
        var_id: VarId::default(),
    };
    let address = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, absolute);
    let source = RegisterId(0);
    let mut instructions = vec![SIRInstruction::Imm(source, SIRValue::new(0b1010_0101u8))];
    let mut register_map = HashMap::default();
    register_map.insert(
        source,
        RegisterType::Bit {
            width: 8,
            signed: false,
        },
    );
    for lane in 0..8 {
        let slice = RegisterId(lane + 1);
        register_map.insert(
            slice,
            RegisterType::Bit {
                width: 1,
                signed: false,
            },
        );
        instructions.push(SIRInstruction::Slice(slice, source, lane, 1));
        instructions.push(SIRInstruction::Store(
            address,
            SIROffset::Static(lane),
            1,
            slice,
            Vec::new(),
            Vec::new(),
        ));
    }
    let block = BasicBlock {
        id: SirBlockId(0),
        params: Vec::new(),
        instructions,
        terminator: SIRTerminator::Return,
    };
    let mut blocks = HashMap::default();
    blocks.insert(SirBlockId(0), block);
    let unit = ExecutionUnit {
        entry_block_id: SirBlockId(0),
        blocks,
        register_map,
    };
    unit.verify();

    let mut layout = empty_layout();
    layout.mode = MemoryLayoutMode::ElementStrided;
    layout.offsets.insert(absolute, 0);
    layout.widths.insert(absolute, 8);
    layout.is_4states.insert(absolute, false);
    layout.unpacked_arrays.insert(
        absolute,
        celox_state_layout::UnpackedArrayLayout {
            element_width: 1,
            element_count: 8,
            element_stride: 1,
            plane_size: 8,
        },
    );
    layout.total_size = 8;
    layout.working_base_offset = 8;
    layout.sparse_base_offset = 8;
    layout.merged_total_size = 8;
    layout.triggered_bits_offset = 8;
    layout.scratch_base_offset = 8;

    let plans =
        find_packed_bit_store_plans(&unit.blocks[&SirBlockId(0)], &unit.register_map, &layout);
    assert_eq!(plans.roots.len(), 1);
    assert_eq!(plans.skip_indices.len(), 16);
    let plan = plans.roots.values().next().unwrap();
    assert_eq!(plan.source, source);
    assert_eq!(plan.lane_count, 8);

    if crate::native::features::X86Features::detect().bmi2() {
        let function = lower_execution_unit(&unit, &layout, false);
        function.verify();
        assert_eq!(
            function
                .blocks
                .iter()
                .flat_map(|block| &block.insts)
                .filter(|instruction| matches!(instruction, MInst::Pdep { .. }))
                .count(),
            1
        );
        assert!(
            function
                .blocks
                .iter()
                .flat_map(|block| &block.insts)
                .any(|instruction| matches!(
                    instruction,
                    MInst::Store {
                        offset: 0,
                        size: OpSize::S64,
                        ..
                    }
                ))
        );
        assert!(!function.blocks.iter().flat_map(|block| &block.insts).any(
            |instruction| matches!(
                instruction,
                MInst::Store {
                    size: OpSize::S8,
                    ..
                }
            )
        ));
    }
}

#[test]
fn recognizes_dynamic_constant_lane_offsets_in_packed_layout() {
    let lhs_abs = AbsoluteAddr {
        instance_id: InstanceId(0),
        var_id: VarId::default(),
    };
    let mut rhs_var = VarId::default();
    rhs_var.0 += 1;
    let rhs_abs = AbsoluteAddr {
        instance_id: InstanceId(0),
        var_id: rhs_var,
    };
    let lhs_addr = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, lhs_abs);
    let rhs_addr = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, rhs_abs);
    let mut next_register = 0usize;
    let mut register_map = HashMap::default();
    let mut allocate = |ty: RegisterType| {
        let register = RegisterId(next_register);
        next_register += 1;
        register_map.insert(register, ty);
        register
    };
    let eight = allocate(RegisterType::Logic { width: 8 });
    let mut instructions = vec![SIRInstruction::Imm(eight, SIRValue::new(8u8))];
    let mut predicates = Vec::with_capacity(16);
    for lane in 0u8..16 {
        let lane_register = allocate(RegisterType::Logic { width: 8 });
        let offset = allocate(RegisterType::Logic { width: 8 });
        let lhs = allocate(RegisterType::Logic { width: 8 });
        let rhs = allocate(RegisterType::Logic { width: 8 });
        let predicate = allocate(RegisterType::Bit {
            width: 1,
            signed: false,
        });
        instructions.extend([
            SIRInstruction::Imm(lane_register, SIRValue::new(lane)),
            SIRInstruction::Binary(offset, lane_register, BinaryOp::Mul, eight),
            SIRInstruction::Load(lhs, lhs_addr, SIROffset::Dynamic(offset), 8),
            SIRInstruction::Load(rhs, rhs_addr, SIROffset::Dynamic(offset), 8),
            SIRInstruction::Binary(predicate, lhs, BinaryOp::LtU, rhs),
        ]);
        predicates.push(predicate);
    }
    predicates.reverse();
    let packed = allocate(RegisterType::Logic { width: 16 });
    instructions.push(SIRInstruction::Concat(packed, predicates));
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
        register_map,
    };
    unit.verify();

    let mut layout = empty_layout();
    layout.offsets = [(lhs_abs, 0), (rhs_abs, 16)].into_iter().collect();
    layout.widths = [(lhs_abs, 128), (rhs_abs, 128)].into_iter().collect();
    layout.is_4states = [(lhs_abs, false), (rhs_abs, false)].into_iter().collect();
    layout.total_size = 32;
    layout.working_base_offset = 32;
    layout.sparse_base_offset = 32;
    layout.merged_total_size = 32;
    layout.triggered_bits_offset = 32;
    layout.scratch_base_offset = 32;

    let function = lower_execution_unit(&unit, &layout, false);
    function.verify();
    assert_eq!(
        function
            .blocks
            .iter()
            .flat_map(|block| &block.insts)
            .filter(|instruction| matches!(
                instruction,
                MInst::PackedLaneCompare {
                    kind: CmpKind::LtU,
                    rhs: PackedLaneCompareRhs::Memory { .. },
                    lane_count: 16,
                    element_stride: 1,
                    field_width: 8,
                    ..
                }
            ))
            .count(),
        1
    );
}
