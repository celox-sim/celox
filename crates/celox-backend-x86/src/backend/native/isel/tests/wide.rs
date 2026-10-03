use super::*;

#[test]
fn four_state_constant_binary_preserves_unknown_mask() {
    let output_var = VarId::default();
    let output_abs = AbsoluteAddr {
        instance_id: InstanceId(0),
        var_id: output_var,
    };
    let output = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, output_abs);
    let lhs = RegisterId(0);
    let rhs = RegisterId(1);
    let result = RegisterId(2);
    let logic4 = RegisterType::Logic { width: 4 };
    let unit = ExecutionUnit {
        entry_block_id: SirBlockId(0),
        blocks: [(
            SirBlockId(0),
            BasicBlock {
                id: SirBlockId(0),
                params: vec![],
                instructions: vec![
                    SIRInstruction::Imm(lhs, SIRValue::new_four_state(0b1010u8, 0u8)),
                    SIRInstruction::Imm(rhs, SIRValue::new_four_state(0b0010u8, 0b0011u8)),
                    SIRInstruction::Binary(result, lhs, BinaryOp::Xor, rhs),
                    SIRInstruction::Store(output, SIROffset::Static(0), 4, result, vec![], vec![]),
                ],
                terminator: SIRTerminator::Return,
            },
        )]
        .into_iter()
        .collect(),
        register_map: [
            (lhs, logic4.clone()),
            (rhs, logic4.clone()),
            (result, logic4),
        ]
        .into_iter()
        .collect(),
    };
    unit.verify();

    let mut layout = empty_layout();
    layout.four_state = true;
    layout.offsets.insert(output_abs, 0);
    layout.widths.insert(output_abs, 4);
    layout.is_4states.insert(output_abs, true);
    layout.total_size = 2;
    layout.working_base_offset = 2;
    layout.sparse_base_offset = 2;
    layout.merged_total_size = 2;
    layout.triggered_bits_offset = 2;
    layout.scratch_base_offset = 2;

    let mut function = lower_execution_unit(&unit, &layout, true);
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
    let mut state = vec![0u8; 2];
    assert_eq!(unsafe { jit.call(&mut state) }, 0);
    assert_eq!(state[0] & 0b1111, 0b1011, "value plane");
    assert_eq!(state[1] & 0b1111, 0b0011, "mask plane");
}

#[test]
fn repeated_msb_concat_uses_constant_work_in_both_planes() {
    let output_abs = AbsoluteAddr {
        instance_id: InstanceId(0),
        var_id: VarId::default(),
    };
    let output = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, output_abs);
    let sign = RegisterId(0);
    let low = RegisterId(1);
    let result = RegisterId(2);
    let low_value = 0x89ab_cdefu64;
    let low_mask = 0x00ff_00ffu64;

    for four_state in [false, true] {
        let unit = ExecutionUnit {
            entry_block_id: SirBlockId(0),
            blocks: [(
                SirBlockId(0),
                BasicBlock {
                    id: SirBlockId(0),
                    params: vec![],
                    instructions: vec![
                        SIRInstruction::Imm(
                            sign,
                            SIRValue::new_four_state(1u8, u8::from(four_state)),
                        ),
                        SIRInstruction::Imm(
                            low,
                            SIRValue::new_four_state(
                                low_value,
                                if four_state { low_mask } else { 0 },
                            ),
                        ),
                        SIRInstruction::Concat(
                            result,
                            std::iter::repeat_n(sign, 32)
                                .chain(std::iter::once(low))
                                .collect(),
                        ),
                        SIRInstruction::Store(
                            output,
                            SIROffset::Static(0),
                            64,
                            result,
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
                (sign, RegisterType::Logic { width: 1 }),
                (low, RegisterType::Logic { width: 32 }),
                (result, RegisterType::Logic { width: 64 }),
            ]
            .into_iter()
            .collect(),
        };
        unit.verify();

        let mut layout = empty_layout();
        layout.four_state = four_state;
        layout.offsets.insert(output_abs, 0);
        layout.widths.insert(output_abs, 64);
        layout.is_4states.insert(output_abs, four_state);
        layout.total_size = if four_state { 16 } else { 8 };
        layout.working_base_offset = layout.total_size;
        layout.sparse_base_offset = layout.total_size;
        layout.merged_total_size = layout.total_size;
        layout.triggered_bits_offset = layout.total_size;
        layout.scratch_base_offset = layout.total_size;

        let mut function = lower_execution_unit(&unit, &layout, four_state);
        function.verify();
        let instructions = function
            .blocks
            .iter()
            .flat_map(|block| &block.insts)
            .collect::<Vec<_>>();
        let expected_planes = if four_state { 2 } else { 1 };
        assert_eq!(
            instructions
                .iter()
                .filter(|instruction| matches!(instruction, MInst::Neg { .. }))
                .count(),
            expected_planes
        );
        assert_eq!(
            instructions
                .iter()
                .filter(|instruction| matches!(instruction, MInst::ShlImm { imm: 32, .. }))
                .count(),
            expected_planes
        );
        assert_eq!(
            instructions
                .iter()
                .filter(|instruction| matches!(instruction, MInst::Or { .. }))
                .count(),
            expected_planes
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
        assert_eq!(unsafe { jit.call(&mut state) }, 0);
        assert_eq!(
            u64::from_le_bytes(state[..8].try_into().unwrap()),
            0xffff_ffff_0000_0000 | low_value
        );
        if four_state {
            assert_eq!(
                u64::from_le_bytes(state[8..16].try_into().unwrap()),
                0xffff_ffff_0000_0000 | low_mask
            );
        }
    }
}

#[test]
fn narrow_repeated_msb_concat_uses_constant_work_in_both_planes() {
    let output_abs = AbsoluteAddr {
        instance_id: InstanceId(0),
        var_id: VarId::default(),
    };
    let output = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, output_abs);
    let repeated = RegisterId(0);
    let low = RegisterId(1);
    let result = RegisterId(2);

    for four_state in [false, true] {
        let unit = ExecutionUnit {
            entry_block_id: SirBlockId(0),
            blocks: [(
                SirBlockId(0),
                BasicBlock {
                    id: SirBlockId(0),
                    params: vec![],
                    instructions: vec![
                        SIRInstruction::Imm(
                            repeated,
                            SIRValue::new_four_state(1u8, u8::from(four_state)),
                        ),
                        SIRInstruction::Imm(
                            low,
                            SIRValue::new_four_state(0x5au8, if four_state { 0x0fu8 } else { 0u8 }),
                        ),
                        SIRInstruction::Concat(
                            result,
                            std::iter::repeat_n(repeated, 8)
                                .chain(std::iter::once(low))
                                .collect(),
                        ),
                        SIRInstruction::Store(
                            output,
                            SIROffset::Static(0),
                            16,
                            result,
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
                (repeated, RegisterType::Logic { width: 1 }),
                (low, RegisterType::Logic { width: 8 }),
                (result, RegisterType::Logic { width: 16 }),
            ]
            .into_iter()
            .collect(),
        };
        unit.verify();

        let mut layout = empty_layout();
        layout.four_state = four_state;
        layout.offsets.insert(output_abs, 0);
        layout.widths.insert(output_abs, 16);
        layout.is_4states.insert(output_abs, four_state);
        layout.total_size = if four_state { 4 } else { 2 };
        layout.working_base_offset = layout.total_size;
        layout.sparse_base_offset = layout.total_size;
        layout.merged_total_size = layout.total_size;
        layout.triggered_bits_offset = layout.total_size;
        layout.scratch_base_offset = layout.total_size;

        let mut function = lower_execution_unit(&unit, &layout, four_state);
        function.verify();
        let instructions = function
            .blocks
            .iter()
            .flat_map(|block| &block.insts)
            .collect::<Vec<_>>();
        let expected_planes = if four_state { 2 } else { 1 };
        assert_eq!(
            instructions
                .iter()
                .filter(|instruction| matches!(instruction, MInst::Neg { .. }))
                .count(),
            expected_planes
        );
        assert_eq!(
            instructions
                .iter()
                .filter(|instruction| matches!(instruction, MInst::AndImm32 { imm: 0xff00, .. }))
                .count(),
            expected_planes
        );
        assert_eq!(
            instructions
                .iter()
                .filter(|instruction| matches!(instruction, MInst::Or { .. }))
                .count(),
            expected_planes
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
        assert_eq!(unsafe { jit.call(&mut state) }, 0);
        assert_eq!(u16::from_le_bytes(state[..2].try_into().unwrap()), 0xff5a);
        if four_state {
            assert_eq!(u16::from_le_bytes(state[2..4].try_into().unwrap()), 0xff0f);
        }
    }
}

#[test]
fn static_padded_element_loads_mask_padding_before_concat() {
    let array_abs = AbsoluteAddr {
        instance_id: InstanceId(0),
        var_id: VarId(0),
    };
    let output_abs = AbsoluteAddr {
        var_id: VarId(1),
        ..array_abs
    };
    let array = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, array_abs);
    let output = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, output_abs);
    let low = RegisterId(0);
    let high = RegisterId(1);
    let result = RegisterId(2);
    for width in [24usize, 1, 3, 7, 9, 15, 17, 31, 33, 40, 48, 51, 57, 63] {
        for four_state in [false, true] {
            let stride = width.div_ceil(8).next_power_of_two();
            let plane_size = stride * 2;
            let result_width = width * 2;
            let unit = ExecutionUnit {
                entry_block_id: SirBlockId(0),
                blocks: [(
                    SirBlockId(0),
                    BasicBlock {
                        id: SirBlockId(0),
                        params: vec![],
                        instructions: vec![
                            SIRInstruction::Load(low, array, SIROffset::Static(0), width),
                            SIRInstruction::Load(high, array, SIROffset::Static(width), width),
                            SIRInstruction::Concat(result, vec![high, low]),
                            SIRInstruction::Store(
                                output,
                                SIROffset::Static(0),
                                result_width,
                                result,
                                vec![],
                                vec![],
                            ),
                        ],
                        terminator: SIRTerminator::Return,
                    },
                )]
                .into_iter()
                .collect(),
                register_map: [(low, width), (high, width), (result, result_width)]
                    .into_iter()
                    .map(|(register, width)| {
                        (
                            register,
                            if four_state {
                                RegisterType::Logic { width }
                            } else {
                                RegisterType::Bit {
                                    width,
                                    signed: false,
                                }
                            },
                        )
                    })
                    .collect(),
            };
            let mut layout = empty_layout();
            layout.four_state = four_state;
            layout.mode = MemoryLayoutMode::ElementStrided;
            layout.offsets = [(array_abs, 0), (output_abs, 32)].into_iter().collect();
            layout.widths = [(array_abs, result_width), (output_abs, result_width)]
                .into_iter()
                .collect();
            layout.is_4states = [(array_abs, four_state), (output_abs, four_state)]
                .into_iter()
                .collect();
            layout.unpacked_arrays.insert(
                array_abs,
                celox_state_layout::UnpackedArrayLayout {
                    element_width: width,
                    element_count: 2,
                    element_stride: stride,
                    plane_size,
                },
            );
            layout.total_size = 80;
            layout.working_base_offset = 80;
            layout.sparse_base_offset = 80;
            layout.merged_total_size = 80;
            layout.triggered_bits_offset = 80;
            layout.scratch_base_offset = 80;

            let mut function = lower_execution_unit(&unit, &layout, four_state);
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

            let mut state = vec![0xa5u8; 80];
            let element_mask = mask_for_width(width);
            let result_mask = (BigUint::from(1u8) << result_width) - 1u8;
            let planes = [
                [0x1234_5678_0012_3456u64, 0x5543_2100_0065_4321],
                [0x0001_0204_0008_1020, 0x0102_0400_0810_2000],
            ];
            for (plane, values) in planes
                .iter()
                .enumerate()
                .take(if four_state { 2 } else { 1 })
            {
                for (element, value) in values.iter().enumerate() {
                    let padded = value | !element_mask;
                    let start = plane * plane_size + element * stride;
                    state[start..start + stride].copy_from_slice(&padded.to_le_bytes()[..stride]);
                }
            }
            assert_eq!(unsafe { jit.call(&mut state) }, 0);
            for (plane, values) in planes
                .iter()
                .enumerate()
                .take(if four_state { 2 } else { 1 })
            {
                let start = 32 + plane * result_width.div_ceil(8);
                let actual =
                    BigUint::from_bytes_le(&state[start..start + result_width.div_ceil(8)])
                        & &result_mask;
                let expected = (BigUint::from(values[1] & element_mask) << width)
                    | BigUint::from(values[0] & element_mask);
                assert_eq!(
                    actual, expected,
                    "width={width}, four_state={four_state}, plane={plane}"
                );
            }
            assert_eq!(&state[64..], &[0xa5; 16]);
        }
    }
}

#[test]
fn wide_block_parameter_preallocation_ignores_block_map_order() {
    let source = RegisterId(0);
    let first_param = RegisterId(1);
    let second_param = RegisterId(2);
    let entry = BasicBlock {
        id: SirBlockId(0),
        params: vec![],
        instructions: vec![SIRInstruction::Imm(
            source,
            SIRValue::new(BigUint::from(0x1234u64)),
        )],
        terminator: SIRTerminator::Jump(SirBlockId(2), vec![source]),
    };
    let first = BasicBlock {
        id: SirBlockId(1),
        params: vec![first_param],
        instructions: vec![],
        terminator: SIRTerminator::Return,
    };
    let second = BasicBlock {
        id: SirBlockId(2),
        params: vec![second_param],
        instructions: vec![],
        terminator: SIRTerminator::Jump(SirBlockId(1), vec![second_param]),
    };
    let register_map = [source, first_param, second_param]
        .into_iter()
        .map(|register| {
            (
                register,
                RegisterType::Bit {
                    width: 128,
                    signed: false,
                },
            )
        })
        .collect::<HashMap<_, _>>();
    let make_eu = |blocks: Vec<(SirBlockId, BasicBlock<RegionedAbsoluteAddr>)>| ExecutionUnit {
        entry_block_id: SirBlockId(0),
        blocks: blocks.into_iter().collect(),
        register_map: register_map.clone(),
    };
    let forward = make_eu(vec![
        (SirBlockId(0), entry.clone()),
        (SirBlockId(1), first.clone()),
        (SirBlockId(2), second.clone()),
    ]);
    let reverse = make_eu(vec![
        (SirBlockId(2), second),
        (SirBlockId(1), first),
        (SirBlockId(0), entry),
    ]);

    let forward = lower_execution_unit(&forward, &empty_layout(), false);
    let reverse = lower_execution_unit(&reverse, &empty_layout(), false);

    assert_eq!(forward.to_string(), reverse.to_string());
}

#[test]
fn narrow_wide_shift_uses_only_chunks_covered_by_the_result() {
    let input_var = VarId::default();
    let output_var = VarId::from_raw(1);
    let crossing_output_var = VarId::from_raw(2);
    let input_abs = AbsoluteAddr {
        instance_id: InstanceId(0),
        var_id: input_var,
    };
    let output_abs = AbsoluteAddr {
        instance_id: InstanceId(0),
        var_id: output_var,
    };
    let crossing_output_abs = AbsoluteAddr {
        instance_id: InstanceId(0),
        var_id: crossing_output_var,
    };
    let input_addr = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, input_abs);
    let output_addr = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, output_abs);
    let crossing_output_addr =
        RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, crossing_output_abs);

    let wide = RegisterId(0);
    let bit_index = RegisterId(1);
    let gate = RegisterId(2);
    let sign_index = RegisterId(3);
    let shifted = RegisterId(4);
    let extended = RegisterId(5);
    let crossing_index = RegisterId(6);
    let crossing = RegisterId(7);
    let eu = ExecutionUnit {
        entry_block_id: SirBlockId(0),
        blocks: [(
            SirBlockId(0),
            BasicBlock {
                id: SirBlockId(0),
                params: vec![],
                instructions: vec![
                    SIRInstruction::Load(wide, input_addr, SIROffset::Static(0), 128),
                    SIRInstruction::Imm(bit_index, SIRValue::new(15u8)),
                    SIRInstruction::Binary(gate, wide, BinaryOp::Shr, bit_index),
                    SIRInstruction::Imm(sign_index, SIRValue::new(31u8)),
                    SIRInstruction::Binary(shifted, gate, BinaryOp::Shl, sign_index),
                    SIRInstruction::Binary(extended, shifted, BinaryOp::Sar, sign_index),
                    SIRInstruction::Imm(crossing_index, SIRValue::new(60u8)),
                    SIRInstruction::Binary(crossing, wide, BinaryOp::Shr, crossing_index),
                    SIRInstruction::Store(
                        output_addr,
                        SIROffset::Static(0),
                        32,
                        extended,
                        vec![],
                        vec![],
                    ),
                    SIRInstruction::Store(
                        crossing_output_addr,
                        SIROffset::Static(0),
                        8,
                        crossing,
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
            (
                wide,
                RegisterType::Bit {
                    width: 128,
                    signed: false,
                },
            ),
            (
                bit_index,
                RegisterType::Bit {
                    width: 8,
                    signed: false,
                },
            ),
            (
                gate,
                RegisterType::Bit {
                    width: 1,
                    signed: false,
                },
            ),
            (
                sign_index,
                RegisterType::Bit {
                    width: 6,
                    signed: false,
                },
            ),
            (
                shifted,
                RegisterType::Bit {
                    width: 32,
                    signed: true,
                },
            ),
            (
                extended,
                RegisterType::Bit {
                    width: 32,
                    signed: true,
                },
            ),
            (
                crossing_index,
                RegisterType::Bit {
                    width: 8,
                    signed: false,
                },
            ),
            (
                crossing,
                RegisterType::Bit {
                    width: 8,
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
        offsets: [(input_abs, 0), (output_abs, 16), (crossing_output_abs, 20)]
            .into_iter()
            .collect(),
        widths: [(input_abs, 128), (output_abs, 32), (crossing_output_abs, 8)]
            .into_iter()
            .collect(),
        is_4states: [
            (input_abs, false),
            (output_abs, false),
            (crossing_output_abs, false),
        ]
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
    function.verify();
    assert!(
        !function
            .blocks
            .iter()
            .flat_map(|block| &block.insts)
            .any(|instruction| matches!(instruction, MInst::ShlImm { imm: 49, .. })),
        "a one-bit extraction wholly inside the low word must not combine the next word"
    );
    assert!(
        function
            .blocks
            .iter()
            .flat_map(|block| &block.insts)
            .any(|instruction| matches!(instruction, MInst::ShlImm { imm: 4, .. })),
        "an eight-bit extraction starting at bit 60 must combine the next word"
    );
    mir_legalize::legalize(&mut function);
    function.verify();
    mir_opt::optimize(&mut function);
    function.verify();
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
    let mut state = vec![0u8; 24];
    state[1] = 0x80;
    state[7] = 0x80;
    state[8] = 0x01;
    assert_eq!(unsafe { jit.call(&mut state) }, 0);
    assert_eq!(&state[16..20], &u32::MAX.to_le_bytes());
    assert_eq!(state[20], 0x18);
}

#[test]
fn wide_shift_result_is_canonical_before_mux_condition() {
    let input_var = VarId::default();
    let output_var = VarId::from_raw(1);
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

    let wide = RegisterId(0);
    let bit_index = RegisterId(1);
    let gate = RegisterId(2);
    let then_value = RegisterId(3);
    let else_value = RegisterId(4);
    let selected = RegisterId(5);
    let eu = ExecutionUnit {
        entry_block_id: SirBlockId(0),
        blocks: [(
            SirBlockId(0),
            BasicBlock {
                id: SirBlockId(0),
                params: vec![],
                instructions: vec![
                    SIRInstruction::Load(wide, input_addr, SIROffset::Static(0), 128),
                    SIRInstruction::Imm(bit_index, SIRValue::new(65u8)),
                    SIRInstruction::Binary(gate, wide, BinaryOp::Shr, bit_index),
                    SIRInstruction::Imm(then_value, SIRValue::new(u32::MAX)),
                    SIRInstruction::Imm(else_value, SIRValue::new(0u8)),
                    SIRInstruction::Mux(selected, gate, then_value, else_value),
                    SIRInstruction::Store(
                        output_addr,
                        SIROffset::Static(0),
                        32,
                        selected,
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
            (
                wide,
                RegisterType::Bit {
                    width: 128,
                    signed: false,
                },
            ),
            (
                bit_index,
                RegisterType::Bit {
                    width: 8,
                    signed: false,
                },
            ),
            (
                gate,
                RegisterType::Bit {
                    width: 1,
                    signed: false,
                },
            ),
            (
                then_value,
                RegisterType::Bit {
                    width: 32,
                    signed: false,
                },
            ),
            (
                else_value,
                RegisterType::Bit {
                    width: 32,
                    signed: false,
                },
            ),
            (
                selected,
                RegisterType::Bit {
                    width: 32,
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
        widths: [(input_abs, 128), (output_abs, 32)].into_iter().collect(),
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
    function.verify();
    mir_legalize::legalize(&mut function);
    function.verify();
    mir_opt::optimize(&mut function);
    function.verify();
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
    let mut state = vec![0u8; 24];
    state[8] = 0x04; // bit 66 = 1, bit 65 = 0
    assert_eq!(unsafe { jit.call(&mut state) }, 0);
    assert_eq!(&state[16..20], &0u32.to_le_bytes());
}

#[test]
fn wide_repeated_msb_chunk_uses_constant_work_in_both_planes() {
    let output_abs = AbsoluteAddr {
        instance_id: InstanceId(0),
        var_id: VarId::default(),
    };
    let output = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, output_abs);
    let sign = RegisterId(0);
    let low = RegisterId(1);
    let result = RegisterId(2);
    let low_value = 0x89ab_cdef_0123_4567u64;
    let low_mask = 0x00ff_00ff_000f_000fu64;

    for four_state in [false, true] {
        let unit = ExecutionUnit {
            entry_block_id: SirBlockId(0),
            blocks: [(
                SirBlockId(0),
                BasicBlock {
                    id: SirBlockId(0),
                    params: vec![],
                    instructions: vec![
                        SIRInstruction::Imm(
                            sign,
                            SIRValue::new_four_state(1u8, u8::from(four_state)),
                        ),
                        SIRInstruction::Imm(
                            low,
                            SIRValue::new_four_state(
                                low_value,
                                if four_state { low_mask } else { 0 },
                            ),
                        ),
                        SIRInstruction::Concat(
                            result,
                            std::iter::repeat_n(sign, 64)
                                .chain(std::iter::once(low))
                                .collect(),
                        ),
                        SIRInstruction::Store(
                            output,
                            SIROffset::Static(0),
                            128,
                            result,
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
                (sign, RegisterType::Logic { width: 1 }),
                (low, RegisterType::Logic { width: 64 }),
                (result, RegisterType::Logic { width: 128 }),
            ]
            .into_iter()
            .collect(),
        };
        unit.verify();

        let mut layout = empty_layout();
        layout.four_state = four_state;
        layout.offsets.insert(output_abs, 0);
        layout.widths.insert(output_abs, 128);
        layout.is_4states.insert(output_abs, four_state);
        layout.total_size = if four_state { 32 } else { 16 };
        layout.working_base_offset = layout.total_size;
        layout.sparse_base_offset = layout.total_size;
        layout.merged_total_size = layout.total_size;
        layout.triggered_bits_offset = layout.total_size;
        layout.scratch_base_offset = layout.total_size;

        let mut function = lower_execution_unit(&unit, &layout, four_state);
        function.verify();
        let instructions = function
            .blocks
            .iter()
            .flat_map(|block| &block.insts)
            .collect::<Vec<_>>();
        let expected_planes = if four_state { 2 } else { 1 };
        assert_eq!(
            instructions
                .iter()
                .filter(|instruction| matches!(instruction, MInst::Neg { .. }))
                .count(),
            expected_planes,
            "{instructions:#?}"
        );
        assert_eq!(
            instructions
                .iter()
                .filter(|instruction| matches!(instruction, MInst::ShlImm { .. }))
                .count(),
            0,
            "{instructions:#?}"
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
        assert_eq!(unsafe { jit.call(&mut state) }, 0);
        assert_eq!(
            u64::from_le_bytes(state[..8].try_into().unwrap()),
            low_value
        );
        assert_eq!(
            u64::from_le_bytes(state[8..16].try_into().unwrap()),
            u64::MAX
        );
        if four_state {
            assert_eq!(
                u64::from_le_bytes(state[16..24].try_into().unwrap()),
                low_mask
            );
            assert_eq!(
                u64::from_le_bytes(state[24..32].try_into().unwrap()),
                u64::MAX
            );
        }
    }
}

fn runtime_wide_shift_fixture(
    width: usize,
    arithmetic: bool,
) -> (ExecutionUnit<RegionedAbsoluteAddr>, MemoryLayout) {
    let operations = if arithmetic {
        vec![BinaryOp::Shl, BinaryOp::Shr, BinaryOp::Sar]
    } else {
        vec![BinaryOp::Shl, BinaryOp::Shr]
    };
    let bytes = width.div_ceil(64) * 8;
    let mut layout = empty_layout();
    let mut addresses = Vec::new();
    for index in 0..operations.len() + 2 {
        let address = AbsoluteAddr {
            instance_id: InstanceId(0),
            var_id: VarId::from_raw(index as _),
        };
        let offset = match index {
            0 => 0,
            1 => bytes,
            _ => bytes + 8 + (index - 2) * bytes,
        };
        layout.offsets.insert(address, offset);
        layout
            .widths
            .insert(address, if index == 1 { 64 } else { width });
        layout.is_4states.insert(address, false);
        addresses.push(RegionedAbsoluteAddr::from_absolute_addr(
            STABLE_REGION,
            address,
        ));
    }
    let total = bytes * (operations.len() + 1) + 8;
    layout.total_size = total;
    layout.working_base_offset = total;
    layout.sparse_base_offset = total;
    layout.merged_total_size = total;
    layout.triggered_bits_offset = total;
    layout.scratch_base_offset = total;
    let mut instructions = vec![
        SIRInstruction::Load(RegisterId(0), addresses[0], SIROffset::Static(0), width),
        SIRInstruction::Load(RegisterId(1), addresses[1], SIROffset::Static(0), 64),
    ];
    for (index, operation) in operations.iter().enumerate() {
        let result = RegisterId(index + 2);
        instructions.push(SIRInstruction::Binary(
            result,
            RegisterId(0),
            *operation,
            RegisterId(1),
        ));
        instructions.push(SIRInstruction::Store(
            addresses[index + 2],
            SIROffset::Static(0),
            width,
            result,
            vec![],
            vec![],
        ));
    }
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
        register_map: (0..operations.len() + 2)
            .map(|index| {
                (
                    RegisterId(index),
                    RegisterType::Bit {
                        width: if index == 1 { 64 } else { width },
                        signed: index != 1 && arithmetic,
                    },
                )
            })
            .collect(),
    };
    eu.verify();
    (eu, layout)
}

#[test]
fn runtime_wide_shift_network_preserves_boundaries_and_sign_fill() {
    for width in [65usize, 128, 192, 257, 512] {
        // Non-word-aligned logical shifts also exercise the final partial word.
        let arithmetic = width.is_multiple_of(64);
        let (eu, layout) = runtime_wide_shift_fixture(width, arithmetic);
        let mut function = lower_execution_unit(&eu, &layout, false);
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
        let bytes = width.div_ceil(64) * 8;
        let mask = (BigUint::from(1u8) << width) - BigUint::from(1u8);
        let patterned = BigUint::from_bytes_le(
            &(0..bytes)
                .map(|i| (i.wrapping_mul(73) + 19) as u8)
                .collect::<Vec<_>>(),
        ) & &mask;
        let patterns = [
            BigUint::from(0u8),
            mask.clone(),
            BigUint::from(1u8) << (width - 1),
            patterned,
        ];
        for input in patterns {
            for shift in [
                0u64,
                1,
                31,
                63,
                64,
                65,
                127,
                width as u64 - 1,
                width as u64,
                width as u64 + 1,
                1023,
                u64::MAX,
            ] {
                let mut state = vec![0u8; layout.total_size];
                let encoded = input.to_bytes_le();
                state[..encoded.len()].copy_from_slice(&encoded);
                state[bytes..bytes + 8].copy_from_slice(&shift.to_le_bytes());
                assert_eq!(unsafe { jit.call(&mut state) }, 0);
                let left = if shift < width as u64 {
                    (&input << shift as usize) & &mask
                } else {
                    BigUint::from(0u8)
                };
                let right = if shift < width as u64 {
                    &input >> shift as usize
                } else {
                    BigUint::from(0u8)
                };
                let signed = if !input.bit(width as u64 - 1) {
                    right.clone()
                } else if shift >= width as u64 {
                    mask.clone()
                } else {
                    &right
                        | (&mask
                            ^ ((BigUint::from(1u8) << (width - shift as usize))
                                - BigUint::from(1u8)))
                };
                for (index, expected) in [left, right, signed]
                    .into_iter()
                    .take(if arithmetic { 3 } else { 2 })
                    .enumerate()
                {
                    let offset = bytes + 8 + index * bytes;
                    let actual = BigUint::from_bytes_le(&state[offset..offset + bytes]) & &mask;
                    assert_eq!(
                        actual, expected,
                        "width={width} shift={shift} operation={index} input={input}"
                    );
                }
            }
        }
    }
}

#[test]
fn runtime_wide_shift_instruction_growth_is_not_quadratic() {
    let mut counts = Vec::new();
    for width in [512usize, 1024, 2048, 4096] {
        let (eu, layout) = runtime_wide_shift_fixture(width, true);
        let function = lower_execution_unit(&eu, &layout, false);
        function.verify();
        counts.push(
            function
                .blocks
                .iter()
                .map(|block| block.insts.len())
                .sum::<usize>(),
        );
    }
    for pair in counts.windows(2) {
        assert!(
            pair[1] < 3 * pair[0],
            "doubling widths must not quadruple generated work: {counts:?}"
        );
    }
}
