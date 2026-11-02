use super::*;

#[test]
fn packed_state_comparisons_capture_both_planes_and_ignore_padding() {
    for width in [1usize, 7, 8, 9, 15, 16, 17, 24, 31, 32] {
        for operation in [BinaryOp::EqCase, BinaryOp::NeCase] {
            for (slice_offset, slice_width) in [(0, width), (width / 2, width - width / 2)] {
                let plane = width.div_ceil(8);
                let logical = mask_for_width(width);
                let absolute = |index| AbsoluteAddr {
                    instance_id: InstanceId(0),
                    var_id: VarId(index),
                };
                let stable = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, absolute(0));
                let working =
                    RegionedAbsoluteAddr::from_absolute_addr(crate::WORKING_REGION, absolute(0));
                let flag = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, absolute(1));
                let captured = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, absolute(2));
                let mut layout = empty_layout();
                layout.four_state = true;
                for (address, offset, bits, four) in [
                    (absolute(0), 0, width, true),
                    (absolute(1), 32, 1, false),
                    (absolute(2), 48, width, true),
                ] {
                    layout.offsets.insert(address, offset);
                    layout.widths.insert(address, bits);
                    layout.is_4states.insert(address, four);
                }
                layout.working_offsets.insert(absolute(0), 0);
                layout.total_size = 96;
                layout.working_base_offset = 128;
                layout.sparse_base_offset = 256;
                layout.merged_total_size = 256;
                layout.triggered_bits_offset = 256;
                layout.scratch_base_offset = 256;
                let instructions = vec![
                    SIRInstruction::Load(RegisterId(0), stable, SIROffset::Static(0), width),
                    SIRInstruction::Commit(working, stable, SIROffset::Static(0), width, vec![]),
                    SIRInstruction::Load(RegisterId(1), stable, SIROffset::Static(0), width),
                    SIRInstruction::Slice(RegisterId(3), RegisterId(0), slice_offset, slice_width),
                    SIRInstruction::Slice(RegisterId(4), RegisterId(1), slice_offset, slice_width),
                    SIRInstruction::Imm(RegisterId(5), SIRValue::new(0u64)),
                    // Both compared values must retain their captured SSA contents.
                    SIRInstruction::Store(
                        stable,
                        SIROffset::Static(0),
                        width,
                        RegisterId(5),
                        vec![],
                        vec![],
                    ),
                    SIRInstruction::Binary(RegisterId(2), RegisterId(3), operation, RegisterId(4)),
                    SIRInstruction::Store(
                        flag,
                        SIROffset::Static(0),
                        1,
                        RegisterId(2),
                        vec![],
                        vec![],
                    ),
                    // Ordinary value/mask users must still see the same captured load.
                    SIRInstruction::Store(
                        captured,
                        SIROffset::Static(0),
                        width,
                        RegisterId(0),
                        vec![],
                        vec![],
                    ),
                ];
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
                    register_map: (0..6)
                        .map(|r| {
                            (
                                RegisterId(r),
                                if r == 2 {
                                    RegisterType::Bit {
                                        width: 1,
                                        signed: false,
                                    }
                                } else {
                                    RegisterType::Logic {
                                        width: if r == 3 || r == 4 { slice_width } else { width },
                                    }
                                },
                            )
                        })
                        .collect(),
                };
                unit.verify();
                let mut function = lower_execution_unit(&unit, &layout, true);
                function.verify();
                if plane.is_power_of_two() {
                    let packed = OpSize::from_bits(plane * 16).unwrap();
                    assert!(
                        function.blocks[0]
                            .insts
                            .iter()
                            .any(|i| matches!(i,MInst::Load {offset:0,size,..} if *size==packed))
                    );
                    assert!(
                        function.blocks[0]
                            .insts
                            .iter()
                            .any(|i| matches!(i,MInst::Store {offset:0,size,..} if *size==packed))
                    );
                    assert!(
                        !function.blocks[0]
                            .insts
                            .iter()
                            .any(|i| matches!(i, MInst::Xor { .. }))
                    );
                }
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
                let mut seed = 0x9e37_79b9_7f4a_7c15u64;
                for trial in 0..272 {
                    let mut random = || {
                        seed ^= seed << 13;
                        seed ^= seed >> 7;
                        seed ^= seed << 17;
                        seed
                    };
                    let (old_value, old_mask, new_value, new_mask) = if trial < 16 {
                        let old = trial / 4;
                        let new = trial % 4;
                        (
                            if old & 1 != 0 { logical } else { 0 },
                            if old & 2 != 0 { logical } else { 0 },
                            if new & 1 != 0 { logical } else { 0 },
                            if new & 2 != 0 { logical } else { 0 },
                        )
                    } else {
                        (
                            random() & logical,
                            random() & logical,
                            random() & logical,
                            random() & logical,
                        )
                    };
                    let mut state = vec![0xa5; 512];
                    for (offset, bits) in [
                        (0, old_value),
                        (plane, old_mask),
                        (128, new_value),
                        (128 + plane, new_mask),
                    ] {
                        // Deliberately unrelated padding in the two captured objects.
                        let raw = bits | (random() & !logical);
                        state[offset..offset + plane].copy_from_slice(&raw.to_le_bytes()[..plane]);
                    }
                    let source = state[128..128 + 2 * plane].to_vec();
                    assert_eq!(unsafe { jit.call(&mut state) }, 0);
                    let select = |v: u64| (v >> slice_offset) & mask_for_width(slice_width);
                    let equal = (select(old_value), select(old_mask))
                        == (select(new_value), select(new_mask));
                    assert_eq!(
                        state[32] & 1,
                        u8::from(if operation == BinaryOp::EqCase {
                            equal
                        } else {
                            !equal
                        })
                    );
                    assert_eq!(get_bits(&state, 48 * 8, width), old_value);
                    assert_eq!(get_bits(&state, (48 + plane) * 8, width), old_mask);
                    assert_eq!(&state[128..128 + 2 * plane], source);
                    assert!(state[2 * plane..32].iter().all(|&b| b == 0xa5));
                }
            }
        }
    }
}
