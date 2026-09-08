use super::*;

#[test]
fn overwritten_stores_preserve_intermediate_reads_and_partial_updates() {
    for old_size in [OpSize::S8, OpSize::S16, OpSize::S32, OpSize::S64] {
        for new_size in [OpSize::S8, OpSize::S16, OpSize::S32, OpSize::S64] {
            for delta in [0, 1, 4, 7] {
                for observe in [false, true] {
                    let mut block = MBlock::new(BlockId(0));
                    block.insts = vec![
                        MInst::Load {
                            dst: VReg(0),
                            base: BaseReg::SimState,
                            offset: 0,
                            size: OpSize::S64,
                        },
                        MInst::Load {
                            dst: VReg(1),
                            base: BaseReg::SimState,
                            offset: 8,
                            size: OpSize::S64,
                        },
                        MInst::Store {
                            base: BaseReg::SimState,
                            offset: 16,
                            src: VReg(0),
                            size: old_size,
                        },
                    ];
                    if observe {
                        block.push(MInst::Load {
                            dst: VReg(2),
                            base: BaseReg::SimState,
                            offset: 16,
                            size: OpSize::S64,
                        });
                        block.push(MInst::Store {
                            base: BaseReg::SimState,
                            offset: 48,
                            src: VReg(2),
                            size: OpSize::S64,
                        });
                    }
                    block.push(MInst::Store {
                        base: BaseReg::SimState,
                        offset: 16 + delta,
                        src: VReg(1),
                        size: new_size,
                    });
                    block.push(MInst::Return);
                    let (jit, mut state) = compile(MFunction::new(vec![block], vec![]), 56);
                    state[..56].fill(0xa5);
                    let old = 0x0123_4567_89ab_cdef_u64.to_le_bytes();
                    let new = 0xfedc_ba98_7654_3210_u64.to_le_bytes();
                    state[..8].copy_from_slice(&old);
                    state[8..16].copy_from_slice(&new);
                    let mut expected = state[..56].to_vec();
                    expected[16..16 + usize::from(old_size.bytes())]
                        .copy_from_slice(&old[..usize::from(old_size.bytes())]);
                    if observe {
                        expected.copy_within(16..24, 48);
                    }
                    let start = 16 + delta as usize;
                    expected[start..start + usize::from(new_size.bytes())]
                        .copy_from_slice(&new[..usize::from(new_size.bytes())]);
                    assert_eq!(unsafe { (jit.fn_ptr)(state.as_mut_ptr()) }, 0);
                    assert_eq!(
                        &state[..56],
                        expected,
                        "{old_size:?} {new_size:?} {delta} {observe}"
                    );
                }
            }
        }
    }
}

#[test]
fn forwarded_state_subranges_truncate_stored_values_and_preserve_padding() {
    for store_size in [OpSize::S8, OpSize::S16, OpSize::S32, OpSize::S64] {
        for load_size in [OpSize::S8, OpSize::S16, OpSize::S32, OpSize::S64] {
            if load_size.bytes() > store_size.bytes() {
                continue;
            }
            for delta in 0..=store_size.bytes() - load_size.bytes() {
                let mut block = MBlock::new(BlockId(0));
                block.insts = vec![
                    MInst::Load {
                        dst: VReg(0),
                        base: BaseReg::SimState,
                        offset: 0,
                        size: OpSize::S64,
                    },
                    MInst::Store {
                        base: BaseReg::SimState,
                        offset: 19,
                        src: VReg(0),
                        size: store_size,
                    },
                    MInst::Load {
                        dst: VReg(1),
                        base: BaseReg::SimState,
                        offset: 19 + i32::from(delta),
                        size: load_size,
                    },
                    MInst::Store {
                        base: BaseReg::SimState,
                        offset: 48,
                        src: VReg(1),
                        size: OpSize::S64,
                    },
                    MInst::Load {
                        dst: VReg(2),
                        base: BaseReg::SimState,
                        offset: 19,
                        size: OpSize::S64,
                    },
                    MInst::AndImm {
                        dst: VReg(3),
                        src: VReg(2),
                        imm: 7,
                    },
                    MInst::Store {
                        base: BaseReg::SimState,
                        offset: 56,
                        src: VReg(3),
                        size: OpSize::S64,
                    },
                    MInst::Return,
                ];
                let (jit, mut state) = compile(MFunction::new(vec![block], vec![]), 64);
                for input in [
                    0_u64,
                    u64::MAX,
                    0x0123_4567_89ab_cdef,
                    0xfedc_ba98_7654_3210,
                ] {
                    state[..64].fill(0xa5);
                    state[..8].copy_from_slice(&input.to_le_bytes());
                    let mut expected = state[..64].to_vec();
                    expected[19..19 + usize::from(store_size.bytes())]
                        .copy_from_slice(&input.to_le_bytes()[..usize::from(store_size.bytes())]);
                    let mut loaded = [0; 8];
                    loaded[..usize::from(load_size.bytes())].copy_from_slice(
                        &expected
                            [19 + usize::from(delta)..19 + usize::from(delta + load_size.bytes())],
                    );
                    expected[48..56].copy_from_slice(&loaded);
                    expected[56..64].copy_from_slice(&(input & 7).to_le_bytes());
                    assert_eq!(unsafe { (jit.fn_ptr)(state.as_mut_ptr()) }, 0);
                    assert_eq!(
                        &state[..64],
                        expected,
                        "{store_size:?} {load_size:?} {delta}"
                    );
                }
            }
        }
    }
}

#[test]
fn forwarded_loads_observe_overlapping_indexed_pointer_and_bulk_writes() {
    let writes = [
        MInst::Store {
            base: BaseReg::SimState,
            offset: 19,
            src: VReg(0),
            size: OpSize::S8,
        },
        MInst::StoreIndexed {
            base: BaseReg::SimState,
            offset: 16,
            index: VReg(1),
            src: VReg(0),
            size: OpSize::S8,
            alias_range: None,
        },
        MInst::OrStoreIndexed {
            base: BaseReg::SimState,
            offset: 16,
            index: VReg(1),
            src: VReg(0),
            size: OpSize::S8,
            alias_range: None,
        },
        MInst::StorePtr {
            ptr: VReg(2),
            offset: 3,
            src: VReg(0),
            size: OpSize::S8,
        },
        MInst::ReleaseStorePtr {
            ptr: VReg(2),
            offset: 3,
            src: VReg(0),
            size: OpSize::S8,
        },
        MInst::StorePtrIndexed {
            ptr: VReg(2),
            offset: 0,
            index: VReg(1),
            src: VReg(0),
            size: OpSize::S8,
        },
        MInst::ReleaseStorePtrIndexed {
            ptr: VReg(2),
            offset: 0,
            index: VReg(1),
            src: VReg(0),
            size: OpSize::S8,
        },
        MInst::MemFill {
            dst_offset: 19,
            byte_len: 1,
            value: 0x42,
        },
        MInst::MemCopy {
            src_offset: 0,
            dst_offset: 19,
            byte_len: 1,
        },
        MInst::Store {
            base: BaseReg::SimState,
            offset: 40,
            src: VReg(0),
            size: OpSize::S8,
        },
    ];
    for (case, write) in writes.into_iter().enumerate() {
        let mut block = MBlock::new(BlockId(0));
        block.insts = vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 0,
                size: OpSize::S64,
            },
            MInst::Load {
                dst: VReg(1),
                base: BaseReg::SimState,
                offset: 8,
                size: OpSize::S64,
            },
            MInst::Load {
                dst: VReg(2),
                base: BaseReg::SimState,
                offset: 24,
                size: OpSize::S64,
            },
            MInst::Load {
                dst: VReg(3),
                base: BaseReg::SimState,
                offset: 16,
                size: OpSize::S64,
            },
            write,
            MInst::Load {
                dst: VReg(4),
                base: BaseReg::SimState,
                offset: 16,
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 48,
                src: VReg(3),
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 56,
                src: VReg(4),
                size: OpSize::S64,
            },
            MInst::Return,
        ];
        let (jit, mut state) = compile(MFunction::new(vec![block], vec![]), 64);
        for input in [0_u64, 0x42, u64::MAX, 0xfedc_ba98_7654_3210] {
            state[..64].fill(0xa5);
            state[..8].copy_from_slice(&input.to_le_bytes());
            state[8..16].copy_from_slice(&3_u64.to_le_bytes());
            let pointer = unsafe { state.as_mut_ptr().add(16) } as u64;
            state[24..32].copy_from_slice(&pointer.to_le_bytes());
            let mut expected = state[..64].to_vec();
            expected.copy_within(16..24, 48);
            match case {
                2 => expected[19] |= input as u8,
                7 => expected[19] = 0x42,
                9 => expected[40] = input as u8,
                _ => expected[19] = input as u8,
            }
            expected.copy_within(16..24, 56);
            assert_eq!(unsafe { (jit.fn_ptr)(state.as_mut_ptr()) }, 0);
            assert_eq!(&state[..64], expected, "write case {case}");
        }
    }
}

#[test]
fn narrowed_indexed_bitfields_stay_within_the_original_load() {
    for (lsb, width) in [(0, 1), (7, 2), (8, 8), (15, 17), (8, 56), (63, 1)] {
        let mut block = MBlock::new(BlockId(0));
        block.insts = vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 0,
                size: OpSize::S64,
            },
            MInst::LoadIndexed {
                dst: VReg(1),
                base: BaseReg::SimState,
                offset: 8,
                index: VReg(0),
                scale: 2,
                size: OpSize::S64,
                alias_range: MemoryAliasRange::new(8, 24),
            },
            MInst::BitExtract {
                dst: VReg(2),
                src: VReg(1),
                lsb,
                width,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 32,
                src: VReg(2),
                size: OpSize::S64,
            },
            MInst::Return,
        ];
        let mut function = MFunction::new(vec![block], vec![]);
        crate::mir_opt::optimize(&mut function);
        let (offset, bytes) = function.blocks[0]
            .insts
            .iter()
            .find_map(|inst| match inst {
                MInst::LoadIndexed { offset, size, .. } => Some((*offset, size.bytes())),
                _ => None,
            })
            .unwrap();
        assert!(offset >= 8 && offset + i32::from(bytes) <= 16);
        let (jit, mut state) = compile(function, 40);
        for index in 0_u64..=8 {
            for input in [0_u64, u64::MAX, 0x0123_4567_89ab_cdef] {
                state[..40].fill(0xa5);
                state[..8].copy_from_slice(&index.to_le_bytes());
                let start = 8 + index as usize * 2;
                state[start..start + 8].copy_from_slice(&input.to_le_bytes());
                let mut expected = state[..40].to_vec();
                let extracted = (input >> lsb) & (u64::MAX >> (64 - width));
                expected[32..40].copy_from_slice(&extracted.to_le_bytes());
                assert_eq!(unsafe { (jit.fn_ptr)(state.as_mut_ptr()) }, 0);
                assert_eq!(&state[..40], expected, "{lsb} {width} {index}");
            }
        }
    }
}
