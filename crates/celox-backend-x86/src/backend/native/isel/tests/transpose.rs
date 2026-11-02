use super::*;

#[test]
fn tiled_transpose_preserves_each_value_and_mask_bit() {
    for (rows, columns) in [(8usize, 8usize), (9, 16), (16, 32), (38, 32), (63, 64)] {
        for (four_state, bmi2) in [(false, false), (true, false), (false, true), (true, true)] {
            let features = crate::native::features::X86Features::detect();
            if bmi2 && !features.bmi2() {
                continue;
            }
            let mut layout = empty_layout();
            layout.four_state = four_state;
            let mut next_offset = 0;
            let mut addresses = Vec::new();
            for index in 0..rows + columns {
                let width = if index < rows { columns } else { rows };
                let absolute = AbsoluteAddr {
                    instance_id: InstanceId(0),
                    var_id: VarId(index as u32),
                };
                layout.offsets.insert(absolute, next_offset);
                layout.widths.insert(absolute, width);
                layout.is_4states.insert(absolute, four_state);
                addresses.push((
                    RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, absolute),
                    next_offset,
                    width.div_ceil(8),
                ));
                next_offset += width.div_ceil(8) * if four_state { 2 } else { 1 };
            }
            layout.total_size = next_offset;
            layout.working_base_offset = next_offset;
            layout.sparse_base_offset = next_offset;
            layout.merged_total_size = next_offset;
            layout.triggered_bits_offset = next_offset;
            layout.scratch_base_offset = next_offset;
            let mut instructions = Vec::new();
            let mut types = HashMap::default();
            for (row, &(address, _, _)) in addresses.iter().take(rows).enumerate() {
                instructions.push(SIRInstruction::Load(
                    RegisterId(row),
                    address,
                    SIROffset::Static(0),
                    columns,
                ));
                types.insert(RegisterId(row), RegisterType::Logic { width: columns });
            }
            let mut next_register = rows;
            for column in 0..columns {
                let mut pieces = Vec::new();
                for row in 0..rows {
                    let piece = RegisterId(next_register);
                    next_register += 1;
                    types.insert(piece, RegisterType::Logic { width: 1 });
                    instructions.push(SIRInstruction::Slice(piece, RegisterId(row), column, 1));
                    pieces.push(piece);
                }
                pieces.reverse();
                let result = RegisterId(next_register);
                next_register += 1;
                types.insert(result, RegisterType::Logic { width: rows });
                instructions.push(SIRInstruction::Concat(result, pieces));
                instructions.push(SIRInstruction::Store(
                    addresses[rows + column].0,
                    SIROffset::Static(0),
                    rows,
                    result,
                    vec![],
                    vec![],
                ));
            }
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
                register_map: types,
            };
            unit.verify();
            let (mut function, _) = lower_execution_unit_for_target(
                &unit,
                &layout,
                four_state,
                &Default::default(),
                features.with_bmi2(bmi2),
                None,
            );
            function.verify();
            assert!(
                function
                    .blocks
                    .iter()
                    .flat_map(|b| &b.insts)
                    .any(|i| matches!(i, MInst::Xor { .. } | MInst::Pext { .. })),
                "expected butterfly transpose"
            );
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
            let mut state = vec![0u8; next_offset + 65536];
            let mut random = 0x324a_f209_8153_07d7u64;
            // Distinct value/mask bytes cover all four states independently.
            // All 64 basis vectors additionally detect every orientation error.
            for trial in 0..96 {
                state[..next_offset].fill(0);
                for (row, &(_, offset, bytes)) in addresses.iter().take(rows).enumerate() {
                    for plane in 0..if four_state { 2 } else { 1 } {
                        for byte in 0..bytes {
                            random ^= random << 13;
                            random ^= random >> 7;
                            random ^= random << 17;
                            state[offset + plane * bytes + byte] = if trial < 64 {
                                u8::from(row % 8 == trial / 8 && byte == 0) << (trial % 8)
                            } else {
                                random as u8
                            };
                        }
                    }
                }
                let original = state[..next_offset].to_vec();
                assert_eq!(unsafe { jit.call(&mut state) }, 0);
                for column in 0..columns {
                    let (_, output, output_bytes) = addresses[rows + column];
                    for row in 0..rows {
                        let (_, input, input_bytes) = addresses[row];
                        for plane in 0..if four_state { 2 } else { 1 } {
                            let expected = (original[input + plane * input_bytes + column / 8]
                                >> (column % 8))
                                & 1;
                            let actual =
                                (state[output + plane * output_bytes + row / 8] >> (row % 8)) & 1;
                            assert_eq!(
                                actual, expected,
                                "{rows}x{columns}, four_state={four_state}, trial={trial}, row={row}, column={column}, plane={plane}"
                            );
                        }
                    }
                }
            }
        }
    }
}
