use super::*;

#[test]
fn slice_preserves_loaded_value_across_store() {
    for overlapping in [false, true] {
        for four_state in [false, true] {
            // Cover scalar, unaligned loads, cross-chunk and wide slices.
            for (source_width, load_offset, slice_offset, width) in [
                (1, 0, 0, 1),
                (32, 3, 7, 9),
                (130, 3, 60, 16),
                (130, 0, 31, 90),
            ] {
                let input_abs = AbsoluteAddr {
                    instance_id: InstanceId(0),
                    var_id: VarId::default(),
                };
                let mut output_abs = input_abs;
                output_abs.var_id.0 += 1;
                let input = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, input_abs);
                let output = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, output_abs);
                let old = RegisterId(0);
                let replacement = RegisterId(1);
                let slice = RegisterId(2);
                let unit = ExecutionUnit {
                    entry_block_id: SirBlockId(0),
                    blocks: [(
                        SirBlockId(0),
                        BasicBlock {
                            id: SirBlockId(0),
                            params: vec![],
                            instructions: vec![
                                SIRInstruction::Load(
                                    old,
                                    input,
                                    SIROffset::Static(load_offset),
                                    source_width,
                                ),
                                SIRInstruction::Imm(replacement, SIRValue::new(1u8)),
                                SIRInstruction::Store(
                                    if overlapping { input } else { output },
                                    SIROffset::Static(if overlapping {
                                        load_offset + slice_offset
                                    } else {
                                        0
                                    }),
                                    width,
                                    replacement,
                                    vec![],
                                    vec![],
                                ),
                                SIRInstruction::Slice(slice, old, slice_offset, width),
                                SIRInstruction::Store(
                                    output,
                                    SIROffset::Static(0),
                                    width,
                                    slice,
                                    vec![],
                                    vec![],
                                ),
                            ],
                            terminator: SIRTerminator::Return,
                        },
                    )]
                    .into_iter()
                    .collect(),
                    register_map: [(old, source_width), (replacement, width), (slice, width)]
                        .into_iter()
                        .map(|(r, width)| {
                            (
                                r,
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
                unit.verify();
                let input_width = source_width + load_offset;
                let mut layout = empty_layout();
                layout.offsets = [(input_abs, 0), (output_abs, 64)].into_iter().collect();
                layout.widths = [(input_abs, input_width), (output_abs, width)]
                    .into_iter()
                    .collect();
                layout.is_4states = [(input_abs, four_state), (output_abs, four_state)]
                    .into_iter()
                    .collect();
                layout.four_state = four_state;
                layout.total_size = 128;
                layout.merged_total_size = 128;
                let mut function = lower_execution_unit(&unit, &layout, four_state);
                // Compare the prefix with and without Slice: extracting a register
                // must not introduce additional state reads, including mask reads.
                let mut prefix = unit.clone();
                prefix
                    .blocks
                    .get_mut(&SirBlockId(0))
                    .unwrap()
                    .instructions
                    .truncate(3);
                let before = lower_execution_unit(&prefix, &layout, four_state);
                prefix
                    .blocks
                    .get_mut(&SirBlockId(0))
                    .unwrap()
                    .instructions
                    .push(SIRInstruction::Slice(slice, old, slice_offset, width));
                let after = lower_execution_unit(&prefix, &layout, four_state);
                let loads = |f: &MFunction| {
                    f.blocks[0]
                        .insts
                        .iter()
                        .filter(|inst| {
                            matches!(
                                inst,
                                MInst::Load {
                                    base: BaseReg::SimState,
                                    ..
                                }
                            )
                        })
                        .count()
                };
                if overlapping {
                    assert_eq!(
                        loads(&after),
                        loads(&before),
                        "slice reloaded overwritten state"
                    );
                } else {
                    assert!(
                        loads(&after) > loads(&before),
                        "safe direct slice load was lost"
                    );
                }
                if overlapping {
                    // Dynamic stores and commits also invalidate a static load.
                    for write in [
                        SIRInstruction::Store(
                            input,
                            SIROffset::Dynamic(replacement),
                            width,
                            replacement,
                            vec![],
                            vec![],
                        ),
                        SIRInstruction::Commit(
                            input,
                            input,
                            SIROffset::Static(load_offset),
                            width,
                            vec![],
                        ),
                    ] {
                        let instructions =
                            &mut prefix.blocks.get_mut(&SirBlockId(0)).unwrap().instructions;
                        instructions.truncate(3);
                        instructions[2] = write;
                        let before = lower_execution_unit(&prefix, &layout, four_state);
                        prefix
                            .blocks
                            .get_mut(&SirBlockId(0))
                            .unwrap()
                            .instructions
                            .push(SIRInstruction::Slice(slice, old, slice_offset, width));
                        let after = lower_execution_unit(&prefix, &layout, four_state);
                        assert_eq!(loads(&after), loads(&before));
                    }

                    // Region labels are not sufficient to prove non-aliasing.
                    let mut aliased_layout = layout.clone();
                    aliased_layout.working_base_offset = 0;
                    aliased_layout.working_offsets.insert(input_abs, 0);
                    let alias =
                        RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION + 1, input_abs);
                    let instructions =
                        &mut prefix.blocks.get_mut(&SirBlockId(0)).unwrap().instructions;
                    instructions.truncate(3);
                    instructions[2] = SIRInstruction::Store(
                        alias,
                        SIROffset::Static(load_offset),
                        width,
                        replacement,
                        vec![],
                        vec![],
                    );
                    let before = lower_execution_unit(&prefix, &aliased_layout, four_state);
                    prefix
                        .blocks
                        .get_mut(&SirBlockId(0))
                        .unwrap()
                        .instructions
                        .push(SIRInstruction::Slice(slice, old, slice_offset, width));
                    let after = lower_execution_unit(&prefix, &aliased_layout, four_state);
                    assert_eq!(loads(&after), loads(&before));

                    // A successor block cannot reuse an origin without proving
                    // memory unchanged on every incoming path.
                    let mut split = unit.clone();
                    let entry = split.blocks.get_mut(&SirBlockId(0)).unwrap();
                    let instructions = entry.instructions.split_off(3);
                    entry.terminator = SIRTerminator::Jump(SirBlockId(1), vec![]);
                    split.blocks.insert(
                        SirBlockId(1),
                        BasicBlock {
                            id: SirBlockId(1),
                            params: vec![],
                            instructions,
                            terminator: SIRTerminator::Return,
                        },
                    );
                    split.verify();
                    let function = lower_execution_unit(&split, &layout, four_state);
                    // Only inspect Slice, excluding the final partial output store.
                    let mut prefix = split.clone();
                    prefix
                        .blocks
                        .get_mut(&SirBlockId(1))
                        .unwrap()
                        .instructions
                        .truncate(1);
                    let sliced = lower_execution_unit(&prefix, &layout, four_state);
                    assert_eq!(sliced.blocks.len(), function.blocks.len());
                    assert!(sliced.blocks[1].insts.iter().all(|inst| !matches!(
                        inst,
                        MInst::Load {
                            base: BaseReg::SimState,
                            ..
                        }
                    )));
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
                {
                    let jit = JitCode::new(&emitted.code).unwrap();
                    let mut state = vec![0u8; 128];
                    // Capture unknown bits too, then overwrite them with known data.
                    if four_state {
                        let plane = input_width.div_ceil(8);
                        state[plane..plane * 2].fill(0xff);
                    }
                    assert_eq!(unsafe { jit.call(&mut state) }, 0);
                    for bit in 0..width {
                        assert_eq!((state[64 + bit / 8] >> (bit % 8)) & 1, 0);
                        if four_state {
                            assert_eq!(
                                (state[64 + width.div_ceil(8) + bit / 8] >> (bit % 8)) & 1,
                                1
                            );
                        }
                    }
                }
            }
        }
    }
}
