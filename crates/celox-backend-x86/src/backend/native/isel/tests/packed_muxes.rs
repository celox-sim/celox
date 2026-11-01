use super::*;

#[test]
fn packed_mux_lanes_preserve_known_unknown_and_z_after_source_overwrites() {
    for use_mux in [false, true] {
        for offset in [0, 3, 65] {
            for (lane_width, lanes) in [
                (1, 2),
                (1, 5),
                (7, 3),
                (16, 3),
                (32, 2),
                (8, 17),
                (16, 9),
                (31, 6),
                (63, 3),
                (64, 2),
            ] {
                let width: usize = lane_width * lanes;
                let input_width = width + offset;
                let bytes = input_width.div_ceil(8);
                let mut layout = empty_layout();
                layout.four_state = true;
                let addresses: Vec<_> = (0..4)
                    .map(|index| {
                        let absolute = AbsoluteAddr {
                            instance_id: InstanceId(0),
                            var_id: VarId(index),
                        };
                        layout.offsets.insert(absolute, index as usize * bytes * 2);
                        layout
                            .widths
                            .insert(absolute, if index == 3 { 1 } else { input_width });
                        layout.is_4states.insert(absolute, true);
                        RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, absolute)
                    })
                    .collect();
                layout.total_size = bytes * 6 + 2;
                layout.working_base_offset = layout.total_size;
                layout.sparse_base_offset = layout.total_size;
                layout.merged_total_size = layout.total_size;
                layout.triggered_bits_offset = layout.total_size;
                layout.scratch_base_offset = layout.total_size;
                let mut types = HashMap::default();
                let mut instructions = vec![
                    SIRInstruction::Load(
                        RegisterId(0),
                        addresses[0],
                        SIROffset::Static(0),
                        input_width,
                    ),
                    SIRInstruction::Load(
                        RegisterId(1),
                        addresses[1],
                        SIROffset::Static(0),
                        input_width,
                    ),
                    SIRInstruction::Load(RegisterId(2), addresses[3], SIROffset::Static(0), 1),
                ];
                types.insert(RegisterId(0), RegisterType::Logic { width: input_width });
                types.insert(RegisterId(1), RegisterType::Logic { width: input_width });
                types.insert(RegisterId(2), RegisterType::Logic { width: 1 });
                let mut results = Vec::new();
                for lane in 0..lanes {
                    let a = RegisterId(3 + lane * 3);
                    let b = RegisterId(4 + lane * 3);
                    let result = RegisterId(5 + lane * 3);
                    for register in [a, b, result] {
                        types.insert(register, RegisterType::Logic { width: lane_width });
                    }
                    instructions.push(SIRInstruction::Slice(
                        a,
                        RegisterId(0),
                        offset + lane * lane_width,
                        lane_width,
                    ));
                    instructions.push(SIRInstruction::Slice(
                        b,
                        RegisterId(1),
                        offset + lane * lane_width,
                        lane_width,
                    ));
                    instructions.push(SIRInstruction::Mux(result, RegisterId(2), a, b));
                    results.push(if use_mux { result } else { a });
                }
                let output = RegisterId(3 + lanes * 3);
                let zero = RegisterId(4 + lanes * 3);
                types.insert(output, RegisterType::Logic { width });
                types.insert(zero, RegisterType::Logic { width });
                instructions.push(SIRInstruction::Imm(zero, SIRValue::new(0u8)));
                // Lifting must keep the SSA condition and both arms captured before
                // these writes, even when their physical homes would be reloadable.
                for &address in &addresses[..2] {
                    instructions.push(SIRInstruction::Store(
                        address,
                        SIROffset::Static(0),
                        width,
                        zero,
                        vec![],
                        vec![],
                    ));
                }
                instructions.push(SIRInstruction::Store(
                    addresses[3],
                    SIROffset::Static(0),
                    1,
                    zero,
                    vec![],
                    vec![],
                ));
                results.reverse();
                instructions.push(SIRInstruction::Concat(output, results));
                instructions.push(SIRInstruction::Store(
                    addresses[2],
                    SIROffset::Static(0),
                    width,
                    output,
                    vec![],
                    vec![],
                ));
                let unit = ExecutionUnit {
                    entry_block_id: SirBlockId(0),
                    register_map: types,
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
                };
                unit.verify();
                let mut function = lower_execution_unit(&unit, &layout, true);
                mir_legalize::legalize(&mut function);
                mir_opt::optimize(&mut function);
                let allocation = regalloc::run_regalloc(&mut function).unwrap();
                mir_opt::post_regalloc_peephole(&mut function, &allocation.assignment);
                let emitted = emit::emit(
                    &function,
                    &allocation.assignment,
                    allocation.spill_frame_size,
                )
                .unwrap();
                let jit = JitCode::new(&emitted.code).unwrap();
                let mut state = vec![0u8; layout.total_size + 65536];
                let mut random = 0xa614_72bf_9503_d89cu64;
                for trial in 0..320 {
                    let condition = trial % 4;
                    let mut planes = vec![vec![0u8; bytes]; 4];
                    for (plane, data) in planes.iter_mut().enumerate() {
                        for (index, byte) in data.iter_mut().enumerate() {
                            random ^= random << 13;
                            random ^= random >> 7;
                            random ^= random << 17;
                            *byte = if trial < 64 {
                                let symbol = ((trial / 4) >> ((plane / 2) * 2)) & 3;
                                if (symbol >> (plane % 2)) & 1 != 0 {
                                    255
                                } else {
                                    0
                                }
                            } else {
                                random as u8
                            };
                            state[plane * bytes + index] = *byte;
                        }
                    }
                    state[6 * bytes] = (condition & 1) as u8;
                    state[6 * bytes + 1] = ((condition >> 1) & 1) as u8;
                    assert_eq!(unsafe { jit.call(&mut state) }, 0);
                    for bit in 0..width {
                        let input_bit = bit + offset;
                        let read =
                            |plane: usize| (planes[plane][input_bit / 8] >> (input_bit % 8)) & 1;
                        let a = (read(0), read(1));
                        let b = (read(2), read(3));
                        let expected = if !use_mux {
                            a
                        } else if condition < 2 {
                            if condition == 1 { a } else { b }
                        } else if a == b {
                            a
                        } else {
                            (1, 1)
                        };
                        let actual = (
                            ((state[4 * bytes + bit / 8] >> (bit % 8)) & 1),
                            ((state[5 * bytes + bit / 8] >> (bit % 8)) & 1),
                        );
                        assert_eq!(
                            actual, expected,
                            "use_mux={use_mux} offset={offset} lane_width={lane_width} lanes={lanes} trial={trial} bit={bit}"
                        );
                    }
                }
            }
        }
    }
}
