use super::*;

#[test]
fn packed_bitwise_lanes_preserve_four_state_truth_tables() {
    for depth in [1, 2, 3, 18] {
        for lane_count in [1usize, 3] {
            for lane_width in [1usize, 7, 8, 16, 31, 63, 64]
                .into_iter()
                .filter(|width| width * lane_count <= 64)
            {
                let width = lane_width * lane_count;
                let bytes = width.div_ceil(8);
                let word_mask = mask_for_width(width);
                for operation in [
                    BinaryOp::And,
                    BinaryOp::Or,
                    BinaryOp::Xor,
                    BinaryOp::LogicAnd,
                    BinaryOp::LogicOr,
                ]
                .into_iter()
                .filter(|operation| {
                    lane_width == 1 || !matches!(operation, BinaryOp::LogicAnd | BinaryOp::LogicOr)
                }) {
                    let mut layout = empty_layout();
                    layout.four_state = true;
                    let addresses: Vec<_> = (0..4)
                        .map(|index| {
                            let absolute = AbsoluteAddr {
                                instance_id: InstanceId(0),
                                var_id: VarId(index as u32),
                            };
                            layout.offsets.insert(absolute, index * bytes * 2);
                            layout.widths.insert(absolute, width);
                            layout.is_4states.insert(absolute, true);
                            RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, absolute)
                        })
                        .collect();
                    layout.total_size = bytes * 8;
                    layout.working_base_offset = bytes * 8;
                    layout.sparse_base_offset = bytes * 8;
                    layout.merged_total_size = bytes * 8;
                    layout.triggered_bits_offset = bytes * 8;
                    layout.scratch_base_offset = bytes * 8;
                    // Depth three deliberately inserts a different operator;
                    // it must remain a normalization boundary. Deeper cases
                    // exercise homogeneous trees beyond the expansion limit.
                    let operation_at = |level| {
                        if depth == 3 && level == 1 {
                            match operation {
                                BinaryOp::And => BinaryOp::Or,
                                BinaryOp::LogicAnd => BinaryOp::LogicOr,
                                BinaryOp::Or => BinaryOp::And,
                                BinaryOp::LogicOr => BinaryOp::LogicAnd,
                                BinaryOp::Xor => BinaryOp::Or,
                                _ => unreachable!(),
                            }
                        } else {
                            operation
                        }
                    };
                    let mut instructions = vec![
                        SIRInstruction::Load(
                            RegisterId(0),
                            addresses[0],
                            SIROffset::Static(0),
                            width,
                        ),
                        SIRInstruction::Load(
                            RegisterId(1),
                            addresses[1],
                            SIROffset::Static(0),
                            width,
                        ),
                        SIRInstruction::Load(
                            RegisterId(14),
                            addresses[2],
                            SIROffset::Static(0),
                            width,
                        ),
                    ];
                    let mut types = HashMap::default();
                    for register in [0, 1, 11, 14] {
                        types.insert(RegisterId(register), RegisterType::Logic { width });
                    }
                    for lane in 0..lane_count {
                        let a = RegisterId(2 + lane * 3);
                        let b = RegisterId(3 + lane * 3);
                        let result = RegisterId(4 + lane * 3);
                        let c = RegisterId(15 + lane);
                        for register in [a, b, c, result] {
                            types.insert(register, RegisterType::Logic { width: lane_width });
                        }
                        instructions.push(SIRInstruction::Slice(
                            a,
                            RegisterId(0),
                            lane * lane_width,
                            lane_width,
                        ));
                        instructions.push(SIRInstruction::Slice(
                            b,
                            RegisterId(1),
                            lane * lane_width,
                            lane_width,
                        ));
                        instructions.push(SIRInstruction::Slice(
                            c,
                            RegisterId(14),
                            lane * lane_width,
                            lane_width,
                        ));
                        let mut previous = a;
                        for level in 0..depth {
                            let target = if level + 1 == depth {
                                result
                            } else {
                                RegisterId(100 + lane * 32 + level)
                            };
                            types.insert(target, RegisterType::Logic { width: lane_width });
                            instructions.push(SIRInstruction::Binary(
                                target,
                                previous,
                                operation_at(level),
                                if level % 2 == 0 { b } else { c },
                            ));
                            previous = target;
                        }
                    }
                    // Packed lanes must retain captured SSA values after the source
                    // homes are overwritten, even when register pressure causes spills.
                    types.insert(RegisterId(12), RegisterType::Logic { width });
                    instructions.push(SIRInstruction::Imm(RegisterId(12), SIRValue::new(0u8)));
                    for &address in &addresses[..3] {
                        instructions.push(SIRInstruction::Store(
                            address,
                            SIROffset::Static(0),
                            width,
                            RegisterId(12),
                            vec![],
                            vec![],
                        ));
                    }
                    instructions.push(SIRInstruction::Concat(
                        RegisterId(11),
                        (0..lane_count)
                            .rev()
                            .map(|lane| RegisterId(4 + lane * 3))
                            .collect(),
                    ));
                    instructions.push(SIRInstruction::Store(
                        addresses[3],
                        SIROffset::Static(0),
                        width,
                        RegisterId(11),
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
                        register_map: types,
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
                    let expand = |bits: u64| {
                        (0..lane_count).fold(0, |value, lane| {
                            value
                                | ((((bits >> lane) & 1) * mask_for_width(lane_width))
                                    << (lane * lane_width))
                        })
                    };
                    let mut random = 0x91a3_dc49_b210_570fu64;
                    for trial in 0..4352 {
                        let mut planes = [0u64; 6];
                        for (index, plane) in planes.iter_mut().enumerate() {
                            random ^= random << 13;
                            random ^= random >> 7;
                            random ^= random << 17;
                            *plane = if trial < 4096 {
                                {
                                    let bits = ((trial as u64) >> (index * 2)) & 3;
                                    expand(bits | (((bits ^ (bits >> 1)) & 1) << 2))
                                }
                            } else {
                                random & word_mask
                            };
                            set_bits(&mut state, index * bytes * 8, width, *plane);
                        }
                        let [mut av, mut am, bv, bm, cv, cm] = planes;
                        for level in 0..depth {
                            let (bv, bm) = if level % 2 == 0 { (bv, bm) } else { (cv, cm) };
                            let operation = operation_at(level);
                            let known_zero = match operation {
                                BinaryOp::And | BinaryOp::LogicAnd => (!am & !av) | (!bm & !bv),
                                BinaryOp::Or | BinaryOp::LogicOr => !am & !bm & !av & !bv,
                                BinaryOp::Xor => !am & !bm & !(av ^ bv),
                                _ => unreachable!(),
                            } & word_mask;
                            let known_one = match operation {
                                BinaryOp::And | BinaryOp::LogicAnd => !am & !bm & av & bv,
                                BinaryOp::Or | BinaryOp::LogicOr => (!am & av) | (!bm & bv),
                                BinaryOp::Xor => !am & !bm & (av ^ bv),
                                _ => unreachable!(),
                            } & word_mask;
                            av = !known_zero & word_mask;
                            am = !(known_zero | known_one) & word_mask;
                        }
                        assert_eq!(unsafe { jit.call(&mut state) }, 0);
                        let actual = (
                            get_bits(&state, bytes * 6 * 8, width),
                            get_bits(&state, bytes * 7 * 8, width),
                        );
                        assert_eq!(
                            actual,
                            (av, am),
                            "depth={depth} {operation:?} lane_width={lane_width} lane_count={lane_count} trial={trial}"
                        );
                    }
                }
            }
        }
    }
}
