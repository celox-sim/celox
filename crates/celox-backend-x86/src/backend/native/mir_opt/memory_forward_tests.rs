use super::*;
use crate::native::{emit, jit_mem::JitCode, mir_legalize, regalloc};

#[test]
fn indexed_dead_stores_preserve_intervening_observers() {
    for size in [OpSize::S8, OpSize::S16, OpSize::S32, OpSize::S64] {
        for bounded in [false, true] {
            for effect in 0..12 {
                let mut vregs = VRegAllocator::new();
                for _ in 0..6 {
                    vregs.alloc();
                }
                let mut function = MFunction::new(vregs, vec![SpillDesc::transient(); 6]);
                let mut block = MBlock::new(BlockId(0));
                for (dst, offset) in [(VReg(0), 0), (VReg(1), 8), (VReg(2), 16)] {
                    block.push(MInst::Load {
                        dst,
                        base: BaseReg::SimState,
                        offset,
                        size: OpSize::S64,
                    });
                }
                block.push(MInst::LoadImm {
                    dst: VReg(3),
                    value: 0,
                });
                let envelope = bounded.then(|| MemoryAliasRange::new(64, 32).unwrap());
                block.push(MInst::StoreIndexed {
                    base: BaseReg::SimState,
                    offset: 64,
                    index: VReg(1),
                    src: VReg(0),
                    size,
                    alias_range: if effect == 11 {
                        MemoryAliasRange::new(64, 40)
                    } else {
                        envelope
                    },
                });
                match effect {
                    1 | 2 => block.push(MInst::Load {
                        dst: VReg(4),
                        base: BaseReg::SimState,
                        offset: if effect == 1 { 96 } else { 64 },
                        size: OpSize::S64,
                    }),
                    3..=5 => block.push(MInst::LoadIndexed {
                        dst: VReg(4),
                        base: BaseReg::SimState,
                        offset: if effect == 3 { 96 } else { 64 },
                        index: VReg(1),
                        scale: 1,
                        size,
                        alias_range: match effect {
                            3 => MemoryAliasRange::new(96, 32),
                            4 => envelope,
                            _ => None,
                        },
                    }),
                    6 => block.push(MInst::OrStoreIndexed {
                        base: BaseReg::SimState,
                        offset: 64,
                        index: VReg(1),
                        src: VReg(0),
                        size,
                        alias_range: envelope,
                    }),
                    7 => block.push(MInst::MemCopy {
                        dst_offset: 128,
                        src_offset: 64,
                        byte_len: 32,
                    }),
                    8 => block.push(MInst::MemFill {
                        dst_offset: 64,
                        byte_len: 32,
                        value: 0xa5,
                    }),
                    _ => {}
                }
                if (1..=5).contains(&effect) {
                    block.push(MInst::Store {
                        base: BaseReg::SimState,
                        offset: 32,
                        src: VReg(4),
                        size: OpSize::S64,
                    });
                }
                block.push(MInst::StoreIndexed {
                    base: BaseReg::SimState,
                    offset: 64,
                    index: if effect == 9 { VReg(2) } else { VReg(1) },
                    src: VReg(3),
                    size: if effect == 10 && size != OpSize::S8 {
                        OpSize::S8
                    } else {
                        size
                    },
                    alias_range: envelope,
                });
                block.push(MInst::Return);
                function.push_block(block);
                let mut optimized = function.clone();
                eliminate_redundant_local_stores(&mut optimized);
                dead_code_eliminate(&mut optimized);
                optimized.verify();
                let removed = matches!(effect, 0 | 8)
                    || bounded && matches!(effect, 1 | 3)
                    || effect == 10 && size == OpSize::S8;
                assert_eq!(
                    optimized.blocks[0]
                        .insts
                        .iter()
                        .filter(|inst| matches!(inst, MInst::StoreIndexed { .. }))
                        .count(),
                    if removed { 1 } else { 2 },
                    "size={size:?} bounded={bounded} effect={effect}"
                );
                let compile = |mut function: MFunction| {
                    mir_legalize::legalize(&mut function);
                    let allocation = regalloc::run_regalloc(&mut function).unwrap();
                    let emitted = emit::emit(
                        &function,
                        &allocation.assignment,
                        allocation.spill_frame_size,
                    )
                    .unwrap();
                    (
                        JitCode::new(&emitted.code).unwrap(),
                        emitted.required_state_size.max(160) as usize,
                    )
                };
                let (before, before_size) = compile(function);
                let (after, after_size) = compile(optimized);
                for input in [0u64, 1, 0x0123_4567_89ab_cdef, u64::MAX] {
                    for index in [0u64, 1, 8, 16] {
                        let mut left = vec![0x69u8; before_size.max(after_size)];
                        left[..8].copy_from_slice(&input.to_le_bytes());
                        left[8..16].copy_from_slice(&index.to_le_bytes());
                        left[16..24].copy_from_slice(&(16 - index).to_le_bytes());
                        let mut right = left.clone();
                        assert_eq!(unsafe { before.call(&mut left) }, 0);
                        assert_eq!(unsafe { after.call(&mut right) }, 0);
                        assert_eq!(
                            &left[..160],
                            &right[..160],
                            "size={size:?} bounded={bounded} effect={effect} input={input:#x} index={index}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn live_partial_forwarding_preserves_bytes_across_widths_and_aliases() {
    for wide in [OpSize::S16, OpSize::S32, OpSize::S64] {
        for narrow in [OpSize::S8, OpSize::S16, OpSize::S32] {
            if narrow.bytes() >= wide.bytes() {
                continue;
            }
            for displacement in 0..=wide.bytes() - narrow.bytes() {
                for effect in 0..5 {
                    let mut vregs = VRegAllocator::new();
                    for _ in 0..5 {
                        vregs.alloc();
                    }
                    let mut function = MFunction::new(vregs, vec![SpillDesc::transient(); 5]);
                    let mut block = MBlock::new(BlockId(0));
                    block.push(MInst::Load {
                        dst: VReg(0),
                        base: BaseReg::SimState,
                        offset: 0,
                        size: OpSize::S64,
                    });
                    block.push(MInst::LoadImm {
                        dst: VReg(1),
                        value: 0,
                    });
                    block.push(MInst::Store {
                        base: BaseReg::SimState,
                        offset: 64 + displacement as i32,
                        src: VReg(0),
                        size: narrow,
                    });
                    // Reading the bytes before the final observation must not
                    // make a forwarded store disappear from observable state.
                    block.push(MInst::Load {
                        dst: VReg(2),
                        base: BaseReg::SimState,
                        offset: 64,
                        size: wide,
                    });
                    let (offset, alias_range) = match effect {
                        1 | 4 => (80, MemoryAliasRange::new(80, 8)),
                        _ => (64, None),
                    };
                    match effect {
                        1 | 2 => block.push(MInst::MemFill {
                            dst_offset: offset,
                            byte_len: 1,
                            value: 0xa5,
                        }),
                        3 | 4 => block.push(MInst::StoreIndexed {
                            base: BaseReg::SimState,
                            offset,
                            index: VReg(1),
                            src: VReg(0),
                            size: OpSize::S8,
                            alias_range,
                        }),
                        _ => {}
                    }
                    block.push(MInst::Load {
                        dst: VReg(3),
                        base: BaseReg::SimState,
                        offset: 64,
                        size: wide,
                    });
                    for (source, offset) in [(VReg(2), 16), (VReg(3), 24)] {
                        block.push(MInst::Store {
                            base: BaseReg::SimState,
                            offset,
                            src: source,
                            size: OpSize::S64,
                        });
                    }
                    block.push(MInst::Return);
                    function.push_block(block);
                    forward_live_partial_stores(&mut function);
                    known_bits::fold(&mut function);
                    for _ in 0..2 {
                        known_bits::fold_demanded(&mut function);
                        algebraic_simplify(&mut function);
                        copy_propagate(&mut function);
                        dead_code_eliminate(&mut function);
                    }
                    function.verify();
                    mir_legalize::legalize(&mut function);
                    let allocation = regalloc::run_regalloc(&mut function).unwrap();
                    let emitted = emit::emit(
                        &function,
                        &allocation.assignment,
                        allocation.spill_frame_size,
                    )
                    .unwrap();
                    let jit = JitCode::new(&emitted.code).unwrap();
                    for input in [0u64, 1, 0x0123_4567_89ab_cdef, u64::MAX] {
                        let mut state = vec![0x69u8; emitted.required_state_size.max(96) as usize];
                        state[..8].copy_from_slice(&input.to_le_bytes());
                        let mut expected = state.clone();
                        let start = 64 + displacement as usize;
                        expected[start..start + narrow.bytes() as usize]
                            .copy_from_slice(&input.to_le_bytes()[..narrow.bytes() as usize]);
                        let observed = |state: &[u8]| {
                            let mut bytes = [0u8; 8];
                            bytes[..wide.bytes() as usize]
                                .copy_from_slice(&state[64..64 + wide.bytes() as usize]);
                            bytes
                        };
                        let first = observed(&expected);
                        match effect {
                            1 | 2 => expected[offset as usize] = 0xa5,
                            3 | 4 => expected[offset as usize] = input as u8,
                            _ => {}
                        }
                        let second = observed(&expected);
                        expected[16..24].copy_from_slice(&first);
                        expected[24..32].copy_from_slice(&second);
                        assert_eq!(unsafe { jit.call(&mut state) }, 0);
                        assert_eq!(
                            &state[..96],
                            &expected[..96],
                            "wide={wide:?} narrow={narrow:?} displacement={displacement} effect={effect} input={input:#x}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn local_forwarding_preserves_narrow_values_and_physical_write_effects() {
    for size in [OpSize::S8, OpSize::S16, OpSize::S32, OpSize::S64] {
        for effect in 0..5 {
            let mut vregs = VRegAllocator::new();
            for _ in 0..5 {
                vregs.alloc();
            }
            let mut function = MFunction::new(vregs, vec![SpillDesc::transient(); 5]);
            let mut block = MBlock::new(BlockId(0));
            block.push(MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 0,
                size: OpSize::S64,
            });
            block.push(MInst::LoadImm {
                dst: VReg(1),
                value: 0x9876_dead_bacd_a321,
            });
            block.push(MInst::LoadImm {
                dst: VReg(2),
                value: 0,
            });
            block.push(MInst::Store {
                base: BaseReg::SimState,
                offset: 8,
                src: VReg(0),
                size,
            });
            let observer = match effect {
                0 => MInst::LoadIndexed {
                    dst: VReg(4),
                    base: BaseReg::SimState,
                    offset: 24,
                    index: VReg(2),
                    scale: 1,
                    size: OpSize::S64,
                    alias_range: MemoryAliasRange::new(24, 8),
                },
                1..=3 => MInst::StoreIndexed {
                    base: BaseReg::SimState,
                    offset: if effect == 1 { 24 } else { 8 },
                    index: VReg(2),
                    src: VReg(1),
                    size,
                    alias_range: match effect {
                        1 => MemoryAliasRange::new(24, 8),
                        2 => MemoryAliasRange::new(8, 8),
                        _ => None,
                    },
                },
                _ => MInst::MemFill {
                    dst_offset: 8,
                    byte_len: 8,
                    value: 0xa5,
                },
            };
            block.push(observer);
            block.push(MInst::Load {
                dst: VReg(3),
                base: BaseReg::SimState,
                offset: 8,
                size,
            });
            block.push(MInst::Store {
                base: BaseReg::SimState,
                offset: 32,
                src: VReg(3),
                size: OpSize::S64,
            });
            if effect == 0 {
                block.push(MInst::Store {
                    base: BaseReg::SimState,
                    offset: 40,
                    src: VReg(4),
                    size: OpSize::S64,
                });
            }
            block.push(MInst::Return);
            function.push_block(block);
            forward_local_store_loads(&mut function);
            function.verify();
            assert_eq!(
                function.blocks[0]
                    .insts
                    .iter()
                    .any(|inst| matches!(inst, MInst::Load { dst: VReg(3), .. })),
                effect >= 2
            );
            mir_legalize::legalize(&mut function);
            let allocation = regalloc::run_regalloc(&mut function).unwrap();
            let code = emit::emit(
                &function,
                &allocation.assignment,
                allocation.spill_frame_size,
            )
            .unwrap();
            let jit = JitCode::new(&code.code).unwrap();
            for input in [0u64, 1, 0x1234_5678_9abc_def0, u64::MAX] {
                let mut state = vec![0u8; code.required_state_size.max(48) as usize];
                state[..8].copy_from_slice(&input.to_le_bytes());
                assert_eq!(unsafe { jit.call(&mut state) }, 0);
                let expected = match effect {
                    0 | 1 => input,
                    2 | 3 => 0x9876_dead_bacd_a321,
                    _ => 0xa5a5_a5a5_a5a5_a5a5,
                } & machine_width_mask(size);
                assert_eq!(
                    u64::from_le_bytes(state[32..40].try_into().unwrap()),
                    expected,
                    "size={size:?}, effect={effect}, input={input:#x}"
                );
            }
        }
    }
}
