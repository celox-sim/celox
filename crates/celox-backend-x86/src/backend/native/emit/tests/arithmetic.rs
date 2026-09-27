use super::*;

#[test]
fn truncation_masks_use_one_instruction_for_all_registers() {
    use crate::native::regalloc::assignment::ALLOCATABLE_REGS;
    use iced_x86::Code;

    for &src in ALLOCATABLE_REGS {
        for &dst in ALLOCATABLE_REGS {
            for (mask, expected) in [
                (0xff, Code::Movzx_r32_rm8),
                (0xffff, Code::Movzx_r32_rm16),
                (0xffff_ffff, Code::Mov_r32_rm32),
            ] {
                let mut asm = CodeAssembler::new(64).unwrap();
                emit_and_imm(&mut asm, dst, src, mask).unwrap();
                let code = asm.assemble(0).unwrap();
                let mut decoder = Decoder::new(64, &code, DecoderOptions::NONE);
                let instruction = decoder.decode();
                // Register MOV has two interchangeable direction encodings.
                if mask == 0xffff_ffff {
                    assert!(matches!(
                        instruction.code(),
                        Code::Mov_r32_rm32 | Code::Mov_rm32_r32
                    ));
                } else {
                    assert_eq!(instruction.code(), expected);
                }
                assert_eq!(
                    instruction.op0_register(),
                    Register::from(preg_to_reg32(dst))
                );
                assert_eq!(
                    instruction.op1_register(),
                    match mask {
                        0xff => Register::from(preg_to_reg8(src)),
                        0xffff => Register::from(preg_to_reg16(src)),
                        _ => Register::from(preg_to_reg32(src)),
                    }
                );
                assert!(!decoder.can_decode(), "{src:?} -> {dst:?}, mask={mask:#x}");
            }
        }
    }
}

#[test]
fn folded_truncation_loads_read_only_the_required_bytes() {
    use iced_x86::MemorySize;
    for (mask, mnemonic, size) in [
        (0xff, Mnemonic::Movzx, MemorySize::UInt8),
        (0xffff, Mnemonic::Movzx, MemorySize::UInt16),
        (0xffff_ffff, Mnemonic::Mov, MemorySize::UInt32),
    ] {
        let mut asm = CodeAssembler::new(64).unwrap();
        emit_and_memory_imm(&mut asm, PhysReg::R12, ptr(r15 + 24), mask).unwrap();
        let code = asm.assemble(0).unwrap();
        let mut decoder = Decoder::new(64, &code, DecoderOptions::NONE);
        let instruction = decoder.decode();
        assert_eq!(instruction.mnemonic(), mnemonic);
        assert_eq!(instruction.memory_size(), size);
        assert_eq!(instruction.memory_base(), Register::R15);
        assert_eq!(instruction.memory_displacement64(), 24);
        assert_eq!(instruction.op0_register(), Register::R12D);
        assert!(!decoder.can_decode());
    }
}

#[test]
fn immediate_masks_preserve_widths_aliases_and_folded_load_values() {
    for word32 in [false, true] {
        for folded_load in [false, true] {
            // Include the REX-only low-byte names BPL/SIL and extended GPRs.
            for source in [
                PhysReg::RAX,
                PhysReg::RBP,
                PhysReg::RSI,
                PhysReg::R8,
                PhysReg::R12,
            ] {
                for destination in [source, PhysReg::R11] {
                    for mask in [
                        0,
                        1,
                        0xff,
                        0xffff,
                        0x7fff_ffff,
                        0x8000_0000,
                        0xffff_ffff,
                        0xffff_ffff_8000_0000,
                        u64::MAX,
                    ] {
                        if word32 && mask > u64::from(u32::MAX) {
                            continue;
                        }
                        let mut vregs = VRegAllocator::new();
                        let input = vregs.alloc();
                        let output = vregs.alloc();
                        let mut function = MFunction::new(vregs, vec![SpillDesc::transient(); 2]);
                        function.target_features = X86Features::for_test(false);
                        let mut block = MBlock::new(BlockId(0));
                        block.push(MInst::Load {
                            dst: input,
                            base: BaseReg::SimState,
                            offset: 0,
                            size: OpSize::S64,
                        });
                        if !folded_load {
                            // A second use prevents the load/AND memory fold.
                            block.push(MInst::Store {
                                base: BaseReg::SimState,
                                offset: 16,
                                src: input,
                                size: OpSize::S64,
                            });
                        }
                        block.push(if word32 {
                            MInst::AndImm32 {
                                dst: output,
                                src: input,
                                imm: mask as u32,
                            }
                        } else {
                            MInst::AndImm {
                                dst: output,
                                src: input,
                                imm: mask,
                            }
                        });
                        block.push(MInst::Store {
                            base: BaseReg::SimState,
                            offset: 8,
                            src: output,
                            size: OpSize::S64,
                        });
                        block.push(MInst::Return);
                        function.push_block(block);
                        let mut assignment = AssignmentMap::default();
                        assignment.set(input, source);
                        assignment.set(output, destination);
                        let emitted = emit(&function, &assignment, 0).unwrap();
                        let jit = JitCode::new(&emitted.code).unwrap();
                        for value in [
                            0u64,
                            0xff,
                            0xffff,
                            0xffff_ffff,
                            1 << 32,
                            1 << 63,
                            0xfedc_ba98_7654_3210,
                            u64::MAX,
                        ] {
                            let mut state =
                                vec![0xffu8; emitted.required_state_size.max(24) as usize];
                            state[..8].copy_from_slice(&value.to_le_bytes());
                            assert_eq!(unsafe { jit.call(&mut state) }, 0);
                            assert_eq!(
                                u64::from_le_bytes(state[8..16].try_into().unwrap()),
                                value & mask,
                                "word32={word32}, folded={folded_load}, {source:?} -> {destination:?}, value={value:#x}, mask={mask:#x}"
                            );
                            if !folded_load {
                                assert_eq!(
                                    u64::from_le_bytes(state[16..24].try_into().unwrap()),
                                    value
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn immediate_multiplication_executes_with_aliases_and_folded_loads() {
    for word32 in [false, true] {
        for folded_load in [false, true] {
            for source in [PhysReg::RAX, PhysReg::RBP, PhysReg::R12] {
                for destination in [source, PhysReg::R11] {
                    for imm in [0, 1, -1, 2, 3, 4, 5, 7, 8, 9, 30, i32::MIN, i32::MAX] {
                        let mut vregs = VRegAllocator::new();
                        let input = vregs.alloc();
                        let output = vregs.alloc();
                        let mut function = MFunction::new(vregs, vec![SpillDesc::transient(); 2]);
                        function.target_features = X86Features::for_test(false);
                        let mut block = MBlock::new(BlockId(0));
                        block.push(MInst::Load {
                            dst: input,
                            base: BaseReg::SimState,
                            offset: 0,
                            size: OpSize::S64,
                        });
                        if !folded_load {
                            block.push(MInst::Store {
                                base: BaseReg::SimState,
                                offset: 16,
                                src: input,
                                size: OpSize::S64,
                            });
                        }
                        block.push(if word32 {
                            MInst::MulImm32 {
                                dst: output,
                                src: input,
                                imm,
                            }
                        } else {
                            MInst::MulImm {
                                dst: output,
                                src: input,
                                imm,
                            }
                        });
                        block.push(MInst::Store {
                            base: BaseReg::SimState,
                            offset: 8,
                            src: output,
                            size: OpSize::S64,
                        });
                        block.push(MInst::Return);
                        function.push_block(block);
                        let mut assignment = AssignmentMap::default();
                        assignment.set(input, source);
                        assignment.set(output, destination);
                        let emitted = emit(&function, &assignment, 0).unwrap();
                        let decoded = Decoder::new(64, &emitted.code, DecoderOptions::NONE)
                            .into_iter()
                            .collect::<Vec<_>>();
                        let instructions = decoded
                            .iter()
                            .filter(|i| i.mnemonic() == Mnemonic::Imul)
                            .collect::<Vec<_>>();
                        if !folded_load && matches!(imm, 2 | 3 | 4 | 5 | 8 | 9) {
                            assert!(instructions.is_empty());
                            let expected = if source == destination && matches!(imm, 2 | 4 | 8) {
                                Mnemonic::Shl
                            } else {
                                Mnemonic::Lea
                            };
                            assert!(
                                decoded
                                    .iter()
                                    .any(|instruction| instruction.mnemonic() == expected)
                            );
                        } else {
                            assert_eq!(instructions.len(), 1);
                            assert_eq!(instructions[0].op_count(), 3);
                            assert_eq!(
                                instructions[0].op1_kind() == iced_x86::OpKind::Memory,
                                folded_load
                            );
                        }
                        let jit = JitCode::new(&emitted.code).unwrap();
                        for value in [
                            0u64,
                            1,
                            0xffff_ffff,
                            1 << 32,
                            1 << 63,
                            0xfedc_ba98_7654_3210,
                            u64::MAX,
                        ] {
                            let mut state =
                                vec![0xffu8; emitted.required_state_size.max(24) as usize];
                            state[..8].copy_from_slice(&value.to_le_bytes());
                            assert_eq!(unsafe { jit.call(&mut state) }, 0);
                            let product = value.wrapping_mul(imm as u64);
                            let expected = if word32 {
                                u64::from(product as u32)
                            } else {
                                product
                            };
                            assert_eq!(
                                u64::from_le_bytes(state[8..16].try_into().unwrap()),
                                expected,
                                "word32={word32} folded={folded_load} {source:?}->{destination:?} value={value:#x} imm={imm}"
                            );
                            if !folded_load {
                                assert_eq!(
                                    u64::from_le_bytes(state[16..24].try_into().unwrap()),
                                    value
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn folded_state_load_comparisons_preserve_zero_extension_and_signedness() {
    for size in [OpSize::S8, OpSize::S16, OpSize::S32, OpSize::S64] {
        for kind in [
            CmpKind::Eq,
            CmpKind::Ne,
            CmpKind::LtU,
            CmpKind::LeU,
            CmpKind::GtU,
            CmpKind::GeU,
            CmpKind::LtS,
            CmpKind::LeS,
            CmpKind::GtS,
            CmpKind::GeS,
        ] {
            for imm in [-1, 0, 1, 127, 128, 255, 32767, 65535, i32::MAX] {
                let mut vregs = VRegAllocator::new();
                let input = vregs.alloc();
                let result = vregs.alloc();
                let mut function = MFunction::new(vregs, vec![SpillDesc::transient(); 2]);
                let mut block = MBlock::new(BlockId(0));
                block.push(MInst::Load {
                    dst: input,
                    base: BaseReg::SimState,
                    offset: 0,
                    size,
                });
                block.push(MInst::CmpImm {
                    dst: result,
                    lhs: input,
                    imm,
                    kind,
                });
                block.push(MInst::Store {
                    base: BaseReg::SimState,
                    offset: 8,
                    src: result,
                    size: OpSize::S64,
                });
                block.push(MInst::Return);
                function.push_block(block);
                let allocation = regalloc::run_regalloc(&mut function).unwrap();
                let emitted = emit(
                    &function,
                    &allocation.assignment,
                    allocation.spill_frame_size,
                )
                .unwrap();
                let jit = JitCode::new(&emitted.code).unwrap();
                for value in [
                    0,
                    1,
                    127,
                    128,
                    255,
                    256,
                    32767,
                    32768,
                    65535,
                    65536,
                    u32::MAX as u64,
                    1 << 63,
                    u64::MAX,
                ] {
                    let mut state = [0xa5u8; 16];
                    state[..8].copy_from_slice(&value.to_le_bytes());
                    let lhs = value & (u64::MAX >> (64 - size.bytes() * 8));
                    let rhs = imm as u64;
                    let expected = match kind {
                        CmpKind::Eq => lhs == rhs,
                        CmpKind::Ne => lhs != rhs,
                        CmpKind::LtU => lhs < rhs,
                        CmpKind::LeU => lhs <= rhs,
                        CmpKind::GtU => lhs > rhs,
                        CmpKind::GeU => lhs >= rhs,
                        CmpKind::LtS => (lhs as i64) < (rhs as i64),
                        CmpKind::LeS => (lhs as i64) <= (rhs as i64),
                        CmpKind::GtS => (lhs as i64) > (rhs as i64),
                        CmpKind::GeS => (lhs as i64) >= (rhs as i64),
                    };
                    assert_eq!(unsafe { jit.call(&mut state) }, 0);
                    assert_eq!(
                        u64::from_le_bytes(state[8..].try_into().unwrap()),
                        u64::from(expected),
                        "{size:?} {kind:?}: {value:#x} vs {imm}"
                    );
                }
                if size == OpSize::S64
                    || (imm >= 0 && (imm as u64) <= u64::MAX >> (64 - size.bytes() * 8))
                {
                    let decoder = Decoder::new(64, &emitted.code, DecoderOptions::NONE);
                    assert!(
                        decoder
                            .into_iter()
                            .any(|inst| inst.mnemonic() == Mnemonic::Cmp
                                && inst.op0_kind() == iced_x86::OpKind::Memory)
                    );
                }
            }
        }
    }
}

#[test]
fn packed_lane_eq_executes_for_byte_word_and_dword_slots() {
    const LANES: usize = 32;
    const SCALAR_OFFSET: usize = 128;
    const RESULT_OFFSET: usize = 136;
    const TARGET: u32 = 5;

    for (stride, bit_offset, field_width) in [(1usize, 0usize, 5usize), (2, 3, 7), (4, 9, 9)] {
        let mut vregs = VRegAllocator::new();
        let scalar = vregs.alloc();
        let result = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 2]);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::Load {
            dst: scalar,
            base: BaseReg::SimState,
            offset: SCALAR_OFFSET as i32,
            size: OpSize::S32,
        });
        block.push(MInst::PackedLaneCompare {
            dst: result,
            rhs: PackedLaneCompareRhs::Scalar(scalar),
            kind: CmpKind::Eq,
            offset: 0,
            lane_count: LANES as u8,
            element_stride: stride as u8,
            bit_offset: bit_offset as u8,
            field_width: field_width as u8,
            alias_range: MemoryAliasRange::new(0, LANES * stride),
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: RESULT_OFFSET as i32,
            src: result,
            size: OpSize::S64,
        });
        block.push(MInst::Return);
        func.push_block(block);

        mir_legalize::legalize(&mut func);
        mir_opt::optimize(&mut func);
        let allocation = regalloc::run_regalloc(&mut func).unwrap();
        let emitted = emit(&func, &allocation.assignment, allocation.spill_frame_size).unwrap();
        let jit = JitCode::new(&emitted.code).unwrap();
        let mut state = [0u8; 144];
        let mut expected = 0u64;
        for lane in 0..LANES {
            let matches = lane % 3 == 1 || lane == 31;
            let field = if matches {
                TARGET
            } else {
                (lane as u32 + 7) & ((1 << field_width) - 1)
            };
            let slot = (field << bit_offset) | (u32::MAX << (bit_offset + field_width));
            state[lane * stride..(lane + 1) * stride]
                .copy_from_slice(&slot.to_le_bytes()[..stride]);
            if matches || field == TARGET {
                expected |= 1u64 << lane;
            }
        }
        state[SCALAR_OFFSET..SCALAR_OFFSET + 4].copy_from_slice(&TARGET.to_le_bytes());

        assert_eq!(unsafe { jit.call(&mut state) }, 0);
        assert_eq!(
            u64::from_le_bytes(state[RESULT_OFFSET..RESULT_OFFSET + 8].try_into().unwrap()),
            expected,
            "stride={stride} bit_offset={bit_offset} field_width={field_width}"
        );
    }
}

#[test]
fn packed_lane_compare_executes_all_relations_for_scalar_and_memory_rhs() {
    const LANES: usize = 16;
    const RHS_OFFSET: usize = 64;
    const SCALAR_OFFSET: usize = 128;
    const RESULT_OFFSET: usize = 136;
    const KINDS: [CmpKind; 10] = [
        CmpKind::Eq,
        CmpKind::Ne,
        CmpKind::LtU,
        CmpKind::LtS,
        CmpKind::LeU,
        CmpKind::LeS,
        CmpKind::GtU,
        CmpKind::GtS,
        CmpKind::GeU,
        CmpKind::GeS,
    ];

    fn relation(kind: CmpKind, lhs: u32, rhs: u32, bits: usize) -> bool {
        let shift = 64 - bits;
        let lhs_signed = ((u64::from(lhs) << shift) as i64) >> shift;
        let rhs_signed = ((u64::from(rhs) << shift) as i64) >> shift;
        match kind {
            CmpKind::Eq => lhs == rhs,
            CmpKind::Ne => lhs != rhs,
            CmpKind::LtU => lhs < rhs,
            CmpKind::LtS => lhs_signed < rhs_signed,
            CmpKind::LeU => lhs <= rhs,
            CmpKind::LeS => lhs_signed <= rhs_signed,
            CmpKind::GtU => lhs > rhs,
            CmpKind::GtS => lhs_signed > rhs_signed,
            CmpKind::GeU => lhs >= rhs,
            CmpKind::GeS => lhs_signed >= rhs_signed,
        }
    }

    for stride in [1usize, 2, 4] {
        let bits = stride * 8;
        let mask = if bits == 32 {
            u32::MAX
        } else {
            (1u32 << bits) - 1
        };
        let scalar_value = (0x8181_8181 & mask).max(1);
        for kind in KINDS {
            for memory_rhs in [false, true] {
                let mut vregs = VRegAllocator::new();
                let scalar = vregs.alloc();
                let result = vregs.alloc();
                let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 2]);
                let mut block = MBlock::new(BlockId(0));
                if !memory_rhs {
                    block.push(MInst::Load {
                        dst: scalar,
                        base: BaseReg::SimState,
                        offset: SCALAR_OFFSET as i32,
                        size: OpSize::S32,
                    });
                }
                block.push(MInst::PackedLaneCompare {
                    dst: result,
                    rhs: if memory_rhs {
                        PackedLaneCompareRhs::Memory {
                            offset: RHS_OFFSET as i32,
                            alias_range: MemoryAliasRange::new(RHS_OFFSET as i32, LANES * stride),
                        }
                    } else {
                        PackedLaneCompareRhs::Scalar(scalar)
                    },
                    kind,
                    offset: 0,
                    lane_count: LANES as u8,
                    element_stride: stride as u8,
                    bit_offset: 0,
                    field_width: bits as u8,
                    alias_range: MemoryAliasRange::new(0, LANES * stride),
                });
                block.push(MInst::Store {
                    base: BaseReg::SimState,
                    offset: RESULT_OFFSET as i32,
                    src: result,
                    size: OpSize::S64,
                });
                block.push(MInst::Return);
                func.push_block(block);

                mir_legalize::legalize(&mut func);
                mir_opt::optimize(&mut func);
                let allocation = regalloc::run_regalloc(&mut func).unwrap();
                let emitted =
                    emit(&func, &allocation.assignment, allocation.spill_frame_size).unwrap();
                let jit = JitCode::new(&emitted.code).unwrap();
                let mut state = [0u8; 144];
                let mut expected = 0u64;
                for lane in 0..LANES {
                    let lhs = ((lane as u32).wrapping_mul(0x31) ^ (mask >> (lane % 5))) & mask;
                    let rhs = if memory_rhs {
                        ((15 - lane) as u32).wrapping_mul(0x27) ^ (1u32 << (bits - 1))
                    } else {
                        scalar_value
                    } & mask;
                    state[lane * stride..(lane + 1) * stride]
                        .copy_from_slice(&lhs.to_le_bytes()[..stride]);
                    if memory_rhs {
                        let start = RHS_OFFSET + lane * stride;
                        state[start..start + stride].copy_from_slice(&rhs.to_le_bytes()[..stride]);
                    }
                    if relation(kind, lhs, rhs, bits) {
                        expected |= 1u64 << lane;
                    }
                }
                state[SCALAR_OFFSET..SCALAR_OFFSET + 4]
                    .copy_from_slice(&scalar_value.to_le_bytes());

                assert_eq!(unsafe { jit.call(&mut state) }, 0);
                assert_eq!(
                    u64::from_le_bytes(state[RESULT_OFFSET..RESULT_OFFSET + 8].try_into().unwrap()),
                    expected,
                    "stride={stride} kind={kind:?} memory_rhs={memory_rhs}"
                );
            }
        }
    }
}

#[test]
fn packed_byte_affine_compare_executes_all_relations_and_wraps() {
    const BASE_OFFSET: usize = 0;
    const RHS_OFFSET: usize = 1;
    const RESULT_OFFSET: usize = 8;
    const KINDS: [CmpKind; 10] = [
        CmpKind::Eq,
        CmpKind::Ne,
        CmpKind::LtU,
        CmpKind::LtS,
        CmpKind::LeU,
        CmpKind::LeS,
        CmpKind::GtU,
        CmpKind::GtS,
        CmpKind::GeU,
        CmpKind::GeS,
    ];

    fn relation(kind: CmpKind, lhs: u8, rhs: u8) -> bool {
        match kind {
            CmpKind::Eq => lhs == rhs,
            CmpKind::Ne => lhs != rhs,
            CmpKind::LtU => lhs < rhs,
            CmpKind::LtS => (lhs as i8) < (rhs as i8),
            CmpKind::LeU => lhs <= rhs,
            CmpKind::LeS => (lhs as i8) <= (rhs as i8),
            CmpKind::GtU => lhs > rhs,
            CmpKind::GtS => (lhs as i8) > (rhs as i8),
            CmpKind::GeU => lhs >= rhs,
            CmpKind::GeS => (lhs as i8) >= (rhs as i8),
        }
    }

    for kind in KINDS {
        let mut vregs = VRegAllocator::new();
        let base = vregs.alloc();
        let rhs = vregs.alloc();
        let result = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 3]);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::Load {
            dst: base,
            base: BaseReg::SimState,
            offset: BASE_OFFSET as i32,
            size: OpSize::S8,
        });
        block.push(MInst::Load {
            dst: rhs,
            base: BaseReg::SimState,
            offset: RHS_OFFSET as i32,
            size: OpSize::S8,
        });
        block.push(MInst::PackedByteAffineCompare {
            dst: result,
            base,
            rhs,
            kind,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: RESULT_OFFSET as i32,
            src: result,
            size: OpSize::S64,
        });
        block.push(MInst::Return);
        func.push_block(block);

        mir_legalize::legalize(&mut func);
        mir_opt::optimize(&mut func);
        let allocation = regalloc::run_regalloc(&mut func).unwrap();
        let emitted = emit(&func, &allocation.assignment, allocation.spill_frame_size).unwrap();
        let jit = JitCode::new(&emitted.code).unwrap();
        for (base_value, rhs_value) in [(0u8, 7u8), (120, 128), (248, 3), (255, 255)] {
            let mut state = [0u8; 16];
            state[BASE_OFFSET] = base_value;
            state[RHS_OFFSET] = rhs_value;
            assert_eq!(unsafe { jit.call(&mut state) }, 0);
            let actual =
                u64::from_le_bytes(state[RESULT_OFFSET..RESULT_OFFSET + 8].try_into().unwrap());
            let expected = (0..16).fold(0u64, |mask, lane| {
                mask | (u64::from(relation(kind, base_value.wrapping_add(lane), rhs_value)) << lane)
            });
            assert_eq!(
                actual, expected,
                "kind={kind:?} base={base_value} rhs={rhs_value}"
            );
        }
    }
}

#[test]
fn lea_arithmetic_preserves_register_aliases_and_word_widths() {
    use crate::native::{features::X86Features, jit_mem::JitCode};
    for word32 in [false, true] {
        for destination in [PhysReg::RAX, PhysReg::RCX, PhysReg::RDX] {
            for immediate in [
                None,
                Some(i32::MIN),
                Some(-1),
                Some(0),
                Some(1),
                Some(i32::MAX),
            ] {
                if word32 && immediate.is_some() {
                    continue;
                }
                let mut vregs = VRegAllocator::new();
                for _ in 0..3 {
                    vregs.alloc();
                }
                let mut function = MFunction::new(vregs, vec![SpillDesc::transient(); 3]);
                function.target_features = X86Features::for_test(false);
                let mut block = MBlock::new(BlockId(0));
                block.push(MInst::Load {
                    dst: VReg(0),
                    base: BaseReg::SimState,
                    offset: 0,
                    size: OpSize::S64,
                });
                block.push(MInst::Load {
                    dst: VReg(1),
                    base: BaseReg::SimState,
                    offset: 8,
                    size: OpSize::S64,
                });
                block.push(MInst::Store {
                    base: BaseReg::SimState,
                    offset: 24,
                    src: VReg(1),
                    size: OpSize::S64,
                });
                block.push(if let Some(imm) = immediate {
                    MInst::SubImm {
                        dst: VReg(2),
                        src: VReg(0),
                        imm,
                    }
                } else if word32 {
                    MInst::Add32 {
                        dst: VReg(2),
                        lhs: VReg(0),
                        rhs: VReg(1),
                    }
                } else {
                    MInst::Add {
                        dst: VReg(2),
                        lhs: VReg(0),
                        rhs: VReg(1),
                    }
                });
                block.push(MInst::Store {
                    base: BaseReg::SimState,
                    offset: 16,
                    src: VReg(2),
                    size: OpSize::S64,
                });
                block.push(MInst::Return);
                function.push_block(block);
                let mut assignment = AssignmentMap::default();
                assignment.set(VReg(0), PhysReg::RAX);
                assignment.set(VReg(1), PhysReg::RCX);
                assignment.set(VReg(2), destination);
                let emitted = emit(&function, &assignment, 0).unwrap();
                let jit = JitCode::new(&emitted.code).unwrap();
                let values = [
                    0u64,
                    1,
                    0x7fff_ffff,
                    0x8000_0000,
                    0xffff_ffff,
                    1 << 32,
                    1 << 63,
                    u64::MAX,
                ];
                for a in values {
                    for b in values {
                        let mut state = vec![0u8; emitted.required_state_size.max(24) as usize];
                        state[..8].copy_from_slice(&a.to_le_bytes());
                        state[8..16].copy_from_slice(&b.to_le_bytes());
                        assert_eq!(unsafe { jit.call(&mut state) }, 0);
                        let expected = if let Some(imm) = immediate {
                            a.wrapping_sub(imm as u64)
                        } else if word32 {
                            u64::from((a as u32).wrapping_add(b as u32))
                        } else {
                            a.wrapping_add(b)
                        };
                        assert_eq!(
                            u64::from_le_bytes(state[16..24].try_into().unwrap()),
                            expected
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn bmi1_not_and_fusion_preserves_aliases_widths_and_image_requirements() {
    use crate::native::features::{IMAGE_FEATURE_AVX, IMAGE_FEATURE_BMI1};
    for bmi1 in [false, true] {
        for word32 in [false, true] {
            for shared in [false, true] {
                for destination in [PhysReg::RAX, PhysReg::RCX, PhysReg::RDX] {
                    let mut vregs = VRegAllocator::new();
                    for _ in 0..4 {
                        vregs.alloc();
                    }
                    let mut function = MFunction::new(vregs, vec![SpillDesc::transient(); 4]);
                    function.target_features = X86Features::for_test(false).with_bmi1(bmi1);
                    let mut block = MBlock::new(BlockId(0));
                    block.push(MInst::Load {
                        dst: VReg(0),
                        base: BaseReg::SimState,
                        offset: 0,
                        size: OpSize::S64,
                    });
                    block.push(MInst::Load {
                        dst: VReg(1),
                        base: BaseReg::SimState,
                        offset: 8,
                        size: OpSize::S64,
                    });
                    block.push(MInst::BitNot {
                        dst: VReg(2),
                        src: VReg(0),
                    });
                    block.push(if word32 {
                        MInst::And32 {
                            dst: VReg(3),
                            lhs: VReg(2),
                            rhs: VReg(1),
                        }
                    } else {
                        MInst::And {
                            dst: VReg(3),
                            lhs: VReg(2),
                            rhs: VReg(1),
                        }
                    });
                    block.push(MInst::Store {
                        base: BaseReg::SimState,
                        offset: 16,
                        src: VReg(3),
                        size: OpSize::S64,
                    });
                    if shared {
                        block.push(MInst::Store {
                            base: BaseReg::SimState,
                            offset: 24,
                            src: VReg(2),
                            size: OpSize::S64,
                        });
                    }
                    block.push(MInst::Return);
                    function.push_block(block);
                    let mut assignment = AssignmentMap::default();
                    for (value, register) in [PhysReg::RAX, PhysReg::RCX, PhysReg::R8, destination]
                        .into_iter()
                        .enumerate()
                    {
                        assignment.set(VReg(value as u32), register);
                    }
                    let emitted = emit(&function, &assignment, 0).unwrap();
                    let instructions = Decoder::new(64, &emitted.code, DecoderOptions::NONE)
                        .into_iter()
                        .collect::<Vec<_>>();
                    let fused = bmi1 && !shared;
                    assert_eq!(
                        instructions
                            .iter()
                            .any(|inst| inst.mnemonic() == Mnemonic::Andn),
                        fused
                    );
                    assert_eq!(
                        emitted.required_image_features & IMAGE_FEATURE_BMI1 != 0,
                        fused
                    );
                    assert_eq!(emitted.required_image_features & IMAGE_FEATURE_AVX, 0);
                    if bmi1 && !std::arch::is_x86_feature_detected!("bmi1") {
                        continue;
                    }
                    let jit = JitCode::new(&emitted.code).unwrap();
                    let values = [0u64, 1, 2, 1 << 31, 1 << 32, 1 << 63, u64::MAX];
                    for lhs in values {
                        for rhs in values {
                            let mut state = vec![0u8; emitted.required_state_size.max(32) as usize];
                            state[..8].copy_from_slice(&lhs.to_le_bytes());
                            state[8..16].copy_from_slice(&rhs.to_le_bytes());
                            assert_eq!(unsafe { jit.call(&mut state) }, 0);
                            let mask = if word32 {
                                u64::from(u32::MAX)
                            } else {
                                u64::MAX
                            };
                            assert_eq!(
                                u64::from_le_bytes(state[16..24].try_into().unwrap()),
                                !lhs & rhs & mask
                            );
                            if shared {
                                assert_eq!(
                                    u64::from_le_bytes(state[24..32].try_into().unwrap()),
                                    !lhs
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn scaled_lea_preserves_shift_overflow_and_register_aliases() {
    use crate::native::{features::X86Features, jit_mem::JitCode};
    for source in [PhysReg::RAX, PhysReg::R12] {
        for destination in [source, PhysReg::RCX] {
            for imm in 0..64u8 {
                let mut vregs = VRegAllocator::new();
                let input = vregs.alloc();
                let output = vregs.alloc();
                let mut function = MFunction::new(vregs, vec![SpillDesc::transient(); 2]);
                function.target_features = X86Features::for_test(false);
                let mut block = MBlock::new(BlockId(0));
                block.push(MInst::Load {
                    dst: input,
                    base: BaseReg::SimState,
                    offset: 0,
                    size: OpSize::S64,
                });
                // Keep the load separate from the shift's memory fold.
                block.push(MInst::Store {
                    base: BaseReg::SimState,
                    offset: 16,
                    src: input,
                    size: OpSize::S64,
                });
                block.push(MInst::ShlImm {
                    dst: output,
                    src: input,
                    imm,
                });
                block.push(MInst::Store {
                    base: BaseReg::SimState,
                    offset: 8,
                    src: output,
                    size: OpSize::S64,
                });
                block.push(MInst::Return);
                function.push_block(block);
                let mut assignment = AssignmentMap::default();
                assignment.set(input, source);
                assignment.set(output, destination);
                let emitted = emit(&function, &assignment, 0).unwrap();
                let jit = JitCode::new(&emitted.code).unwrap();
                for value in [0u64, 1, 0xffff_ffff, 1 << 32, 1 << 63, u64::MAX] {
                    let mut state = vec![0u8; emitted.required_state_size.max(24) as usize];
                    state[..8].copy_from_slice(&value.to_le_bytes());
                    assert_eq!(unsafe { jit.call(&mut state) }, 0);
                    assert_eq!(
                        u64::from_le_bytes(state[8..16].try_into().unwrap()),
                        value << imm
                    );
                    assert_eq!(u64::from_le_bytes(state[16..24].try_into().unwrap()), value);
                }
            }
        }
    }
}
