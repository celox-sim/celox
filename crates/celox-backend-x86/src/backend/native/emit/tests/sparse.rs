use super::*;

fn check_sparse_worklist_clobbers(features: X86Features) {
    const INPUT: usize = 0;
    const OUTPUT: usize = 8;
    const ACTIVE_BITS: usize = 16;

    let mut vregs = VRegAllocator::new();
    let live_through = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient()]);
    func.target_features = features;
    let descriptor = func.intern_constant_table(vec![0; SparseCommitDescriptor::WORDS]);
    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::Load {
        dst: live_through,
        base: BaseReg::SimState,
        offset: INPUT as i32,
        size: OpSize::S64,
    });
    entry.push(MInst::SparseCommitWorklist {
        descriptor_table: descriptor,
        active_bits_offset: ACTIVE_BITS as i32,
        active_capacity: 1,
    });
    entry.push(MInst::Jump { target: BlockId(1) });
    let mut exit = MBlock::new(BlockId(1));
    exit.push(MInst::Store {
        base: BaseReg::SimState,
        offset: OUTPUT as i32,
        src: live_through,
        size: OpSize::S64,
    });
    exit.push(MInst::Return);
    func.push_block(entry);
    func.push_block(exit);

    mir_legalize::legalize(&mut func);
    mir_opt::optimize(&mut func);
    let allocation = regalloc::run_regalloc(&mut func).unwrap();
    assert!(
        allocation.spill_frame_size >= 8,
        "a value live through an all-GPR clobber needs a stack home"
    );
    let emitted = emit(&func, &allocation.assignment, allocation.spill_frame_size).unwrap();

    let mut decoder = Decoder::new(64, &emitted.code, DecoderOptions::NONE);
    let mut pushes = Vec::new();
    let mut pops = Vec::new();
    while decoder.can_decode() {
        let instruction = decoder.decode();
        match instruction.mnemonic() {
            Mnemonic::Push => pushes.push(instruction.op0_register()),
            Mnemonic::Pop => pops.push(instruction.op0_register()),
            _ => {}
        }
    }
    assert!(pushes.is_empty(), "the JIT arena keeps RSP invariant");
    assert!(pops.is_empty(), "the JIT arena keeps RSP invariant");

    let jit = JitCode::new(&emitted.code).unwrap();
    let mut state = [0u8; 32];
    state[INPUT..INPUT + 8].copy_from_slice(&0x0123_4567_89ab_cdefu64.to_le_bytes());
    assert_eq!(unsafe { jit.call(&mut state) }, 0);
    assert_eq!(
        u64::from_le_bytes(state[OUTPUT..OUTPUT + 8].try_into().unwrap()),
        0x0123_4567_89ab_cdef
    );
}

#[test]
fn sparse_mark_active_is_register_free_and_preserves_live_values() {
    const OUTPUT: usize = 0;
    const INPUT: usize = 8;
    const ACTIVE_BITS: usize = 16;
    const ACTIVE_INDEX: u32 = 65;

    let mut vregs = VRegAllocator::new();
    let live_value = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient()]);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::Load {
        dst: live_value,
        base: BaseReg::SimState,
        offset: INPUT as i32,
        size: OpSize::S64,
    });
    block.push(MInst::SparseMarkActive {
        active_index: ACTIVE_INDEX,
        active_bits_offset: ACTIVE_BITS as i32,
        active_capacity: 70,
    });
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: OUTPUT as i32,
        src: live_value,
        size: OpSize::S64,
    });
    block.push(MInst::Return);
    func.push_block(block);

    mir_legalize::legalize(&mut func);
    mir_opt::optimize(&mut func);
    let allocation = regalloc::run_regalloc(&mut func).unwrap();
    let emitted = emit(&func, &allocation.assignment, allocation.spill_frame_size).unwrap();

    let mut decoder = Decoder::new(64, &emitted.code, DecoderOptions::NONE);
    let mut bit_sets = 0;
    while decoder.can_decode() {
        let instruction = decoder.decode();
        bit_sets += usize::from(instruction.mnemonic() == Mnemonic::Bts);
        assert!(
            !matches!(instruction.mnemonic(), Mnemonic::Push | Mnemonic::Pop)
                || instruction.op0_register() != Register::RAX,
            "SparseMarkActive emitted a hidden RAX save/restore: {instruction}"
        );
    }
    assert_eq!(bit_sets, 1);

    let jit = JitCode::new(&emitted.code).unwrap();
    let mut state = [0u8; 64];
    state[INPUT..INPUT + 8].copy_from_slice(&7u64.to_le_bytes());
    assert_eq!(unsafe { jit.call(&mut state) }, 0);
    assert_eq!(
        u64::from_le_bytes(state[OUTPUT..OUTPUT + 8].try_into().unwrap()),
        7
    );
    assert_eq!(
        u64::from_le_bytes(state[ACTIVE_BITS..ACTIVE_BITS + 8].try_into().unwrap()),
        0
    );
    assert_eq!(
        u64::from_le_bytes(state[ACTIVE_BITS + 8..ACTIVE_BITS + 16].try_into().unwrap()),
        1 << (ACTIVE_INDEX % 64)
    );
}

#[test]
fn sparse_mark_active_shares_a_fallthrough_block_label() {
    const ACTIVE_BITS: usize = 8;
    const ACTIVE_INDEX: u32 = 3;

    let mut func = MFunction::new(VRegAllocator::new(), Vec::new());
    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::SparseMarkActive {
        active_index: ACTIVE_INDEX,
        active_bits_offset: ACTIVE_BITS as i32,
        active_capacity: 4,
    });
    entry.push(MInst::Jump { target: BlockId(1) });
    let mut exit = MBlock::new(BlockId(1));
    exit.push(MInst::Return);
    func.push_block(entry);
    func.push_block(exit);

    mir_legalize::legalize(&mut func);
    mir_opt::optimize(&mut func);
    let allocation = regalloc::run_regalloc(&mut func).unwrap();
    let emitted = emit(&func, &allocation.assignment, allocation.spill_frame_size).unwrap();
    let jit = JitCode::new(&emitted.code).unwrap();
    let mut state = [0u8; 64];

    assert_eq!(unsafe { jit.call(&mut state) }, 0);
    assert_eq!(
        u64::from_le_bytes(state[ACTIVE_BITS..ACTIVE_BITS + 8].try_into().unwrap()),
        1 << ACTIVE_INDEX
    );
}

#[test]
fn sparse_commit_clobbers_preserve_live_values_without_hidden_pushes() {
    const STABLE: usize = 0;
    const SPARSE: usize = 16;
    const DIRTY: usize = 32;
    const SUMMARY: usize = 40;
    const OLD_OUTPUT: usize = 48;

    let mut vregs = VRegAllocator::new();
    let old_stable = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient()]);
    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::Load {
        dst: old_stable,
        base: BaseReg::SimState,
        offset: STABLE as i32,
        size: OpSize::S64,
    });
    entry.push(MInst::SparseCommit {
        src_offset: SPARSE as i32,
        dst_offset: STABLE as i32,
        byte_size: 8,
        dirty_words_offset: DIRTY as i32,
        dirty_word_count: 1,
        summary_words_offset: SUMMARY as i32,
        summary_word_count: 1,
        four_state: false,
    });
    entry.push(MInst::Jump { target: BlockId(1) });
    let mut exit = MBlock::new(BlockId(1));
    exit.push(MInst::Store {
        base: BaseReg::SimState,
        offset: OLD_OUTPUT as i32,
        src: old_stable,
        size: OpSize::S64,
    });
    exit.push(MInst::Return);
    func.push_block(entry);
    func.push_block(exit);

    mir_legalize::legalize(&mut func);
    mir_opt::optimize(&mut func);
    let allocation = regalloc::run_regalloc(&mut func).unwrap();
    let emitted = emit(&func, &allocation.assignment, allocation.spill_frame_size).unwrap();

    let hidden_scratch = [
        Register::RAX,
        Register::RCX,
        Register::RDX,
        Register::RSI,
        Register::RDI,
        Register::R8,
        Register::R9,
    ];
    let mut decoder = Decoder::new(64, &emitted.code, DecoderOptions::NONE);
    while decoder.can_decode() {
        let instruction = decoder.decode();
        if matches!(instruction.mnemonic(), Mnemonic::Push | Mnemonic::Pop) {
            assert!(
                !hidden_scratch.contains(&instruction.op0_register()),
                "SparseCommit emitted a hidden scratch save/restore: {instruction}"
            );
        }
    }

    let old = 0x0123_4567_89ab_cdefu64;
    let new = 0xfedc_ba98_7654_3210u64;
    let jit = JitCode::new(&emitted.code).unwrap();
    let mut state = [0u8; 64];
    state[STABLE..STABLE + 8].copy_from_slice(&old.to_le_bytes());
    state[SPARSE..SPARSE + 8].copy_from_slice(&new.to_le_bytes());
    state[DIRTY..DIRTY + 8].copy_from_slice(&1u64.to_le_bytes());
    state[SUMMARY..SUMMARY + 8].copy_from_slice(&1u64.to_le_bytes());

    assert_eq!(unsafe { jit.call(&mut state) }, 0);
    assert_eq!(
        u64::from_le_bytes(state[STABLE..STABLE + 8].try_into().unwrap()),
        new
    );
    assert_eq!(
        u64::from_le_bytes(state[OLD_OUTPUT..OLD_OUTPUT + 8].try_into().unwrap()),
        old
    );
    assert_eq!(
        u64::from_le_bytes(state[DIRTY..DIRTY + 8].try_into().unwrap()),
        0
    );
    assert_eq!(
        u64::from_le_bytes(state[SUMMARY..SUMMARY + 8].try_into().unwrap()),
        0
    );
}

#[test]
fn sparse_worklist_deduplicates_regions_and_commits_tail_bytes() {
    const BYTE_SIZE: usize = 13;
    const STABLE: usize = 0;
    const SPARSE: usize = 32;
    const DIRTY: usize = 64;
    const SUMMARY: usize = 72;
    const ACTIVE_BITS: usize = 80;

    let mut func = MFunction::new(VRegAllocator::new(), Vec::new());
    let descriptor = SparseCommitDescriptor {
        src_offset: SPARSE as u64,
        dst_offset: STABLE as u64,
        byte_size: BYTE_SIZE as u64,
        dirty_words_offset: DIRTY as u64,
        dirty_word_count: 1,
        summary_words_offset: SUMMARY as u64,
        summary_word_count: 1,
        four_state: 1,
    };
    let table = func.intern_constant_table(descriptor.words().to_vec());
    let mut block = MBlock::new(BlockId(0));
    for _ in 0..2 {
        block.push(MInst::SparseMarkActive {
            active_index: 0,
            active_bits_offset: ACTIVE_BITS as i32,
            active_capacity: 1,
        });
    }
    block.push(MInst::SparseCommitWorklist {
        descriptor_table: table,
        active_bits_offset: ACTIVE_BITS as i32,
        active_capacity: 1,
    });
    block.push(MInst::Return);
    func.push_block(block);

    let emitted = emit(&func, &AssignmentMap::default(), 0).unwrap();
    let jit = JitCode::new(&emitted.code).unwrap();
    let mut state = [0xa5u8; 128];
    state[STABLE..STABLE + BYTE_SIZE * 2].fill(0);
    for index in 0..BYTE_SIZE * 2 {
        state[SPARSE + index] = index as u8 ^ 0x6d;
    }
    state[DIRTY..DIRTY + 8].copy_from_slice(&3u64.to_le_bytes());
    state[SUMMARY..SUMMARY + 8].copy_from_slice(&1u64.to_le_bytes());
    state[ACTIVE_BITS..ACTIVE_BITS + 8].fill(0);
    let stable_sentinel = state[STABLE + BYTE_SIZE * 2];

    assert_eq!(unsafe { jit.call(&mut state) }, 0);
    assert_eq!(
        &state[STABLE..STABLE + BYTE_SIZE * 2],
        &state[SPARSE..SPARSE + BYTE_SIZE * 2]
    );
    assert_eq!(state[STABLE + BYTE_SIZE * 2], stable_sentinel);
    assert_eq!(
        u64::from_le_bytes(state[DIRTY..DIRTY + 8].try_into().unwrap()),
        0
    );
    assert_eq!(
        u64::from_le_bytes(state[SUMMARY..SUMMARY + 8].try_into().unwrap()),
        0
    );
    assert_eq!(
        u64::from_le_bytes(state[ACTIVE_BITS..ACTIVE_BITS + 8].try_into().unwrap()),
        0
    );
}

#[test]
fn sparse_worklist_clobbers_are_allocated_and_saved_once_per_function() {
    check_sparse_worklist_clobbers(X86Features::detect());
}

#[test]
fn sparse_worklist_clobbers_exclude_the_reserved_state_base() {
    check_sparse_worklist_clobbers(X86Features::for_test_with_state_base(
        false,
        StateBaseStrategy::R15,
    ));
}

#[test]
fn sparse_worklist_mixed_regions_preserve_dirty_chunks_and_inactive_values() {
    // Cross active-word boundaries and the specialization budget, with
    // every scalar copy width, two/four-state planes, and larger regions.
    for capacity in [1usize, 66, 130, 300] {
        let active_offset = capacity * 128;
        let mut state = vec![0xa5u8; active_offset + capacity.div_ceil(64) * 8];
        state[active_offset..].fill(0);
        let mut table = Vec::new();
        let mut expected = state.clone();
        for index in 0..capacity {
            let base = index * 128;
            let width = if index % 9 == 8 { 13 } else { index % 9 + 1 };
            let planes = if index % 2 == 0 { 2 } else { 1 };
            let active = index % 3 != 1;
            let dirty = if index % 11 == 0 {
                0u64
            } else {
                2 | u64::from(index % 5 != 0)
            };
            let summary = u64::from(index % 7 != 0);
            table.extend(
                SparseCommitDescriptor {
                    src_offset: (base + 32) as u64,
                    dst_offset: base as u64,
                    byte_size: width as u64,
                    dirty_words_offset: (base + 64) as u64,
                    dirty_word_count: 1,
                    summary_words_offset: (base + 72) as u64,
                    summary_word_count: 1,
                    four_state: (planes - 1) as u64,
                }
                .words(),
            );
            for byte in 0..width * planes {
                state[base + 32 + byte] = (index ^ byte) as u8;
            }
            state[base + 64..base + 72].copy_from_slice(&dirty.to_le_bytes());
            state[base + 72..base + 80].copy_from_slice(&summary.to_le_bytes());
            if active {
                state[active_offset + index / 8] |= 1 << (index % 8);
            }
            expected[base..base + 128].copy_from_slice(&state[base..base + 128]);
            if !active {
                continue;
            }
            expected[base + 72..base + 80].fill(0);
            if width <= 8 || summary != 0 {
                expected[base + 64..base + 72].fill(0);
                for plane in 0..planes {
                    for byte in 0..width {
                        if (width <= 8 && dirty != 0)
                            || (width > 8 && dirty & (1 << (byte / 8)) != 0)
                        {
                            expected[base + plane * width + byte] =
                                state[base + 32 + plane * width + byte];
                        }
                    }
                }
            }
        }
        // A checkpoint may contain set padding bits; they cannot visit a
        // descriptor beyond the table, and must still be cleared.
        if !capacity.is_multiple_of(64) {
            *state.last_mut().unwrap() |= 0x80;
        }
        let mut func = MFunction::new(VRegAllocator::new(), Vec::new());
        let descriptor_table = func.intern_constant_table(table);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::SparseCommitWorklist {
            descriptor_table,
            active_bits_offset: active_offset as i32,
            active_capacity: capacity,
        });
        block.push(MInst::Return);
        func.push_block(block);
        let emitted = emit(&func, &AssignmentMap::default(), 0).unwrap();
        let jit = JitCode::new(&emitted.code).unwrap();
        assert_eq!(unsafe { jit.call(&mut state) }, 0);
        assert_eq!(state, expected, "capacity {capacity}");
        assert_eq!(unsafe { jit.call(&mut state) }, 0);
        assert_eq!(
            state, expected,
            "inactive second commit, capacity {capacity}"
        );
    }
}

#[test]
fn sparse_worklist_keeps_order_when_regions_alias() {
    // The large region produces the small region's source. Reordering the
    // small copy ahead of the large one would observe the old value.
    let mut func = MFunction::new(VRegAllocator::new(), Vec::new());
    let mut table = SparseCommitDescriptor {
        src_offset: 0,
        dst_offset: 16,
        byte_size: 16,
        dirty_words_offset: 64,
        dirty_word_count: 1,
        summary_words_offset: 72,
        summary_word_count: 1,
        four_state: 0,
    }
    .words()
    .to_vec();
    table.extend(
        SparseCommitDescriptor {
            src_offset: 16,
            dst_offset: 32,
            byte_size: 8,
            dirty_words_offset: 80,
            dirty_word_count: 1,
            summary_words_offset: 88,
            summary_word_count: 1,
            four_state: 0,
        }
        .words(),
    );
    let descriptor_table = func.intern_constant_table(table);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::SparseCommitWorklist {
        descriptor_table,
        active_bits_offset: 96,
        active_capacity: 2,
    });
    block.push(MInst::Return);
    func.push_block(block);
    let emitted = emit(&func, &AssignmentMap::default(), 0).unwrap();
    let jit = JitCode::new(&emitted.code).unwrap();
    let mut state = [0u8; 104];
    state[..8].copy_from_slice(&0x1234_5678_9abc_def0u64.to_le_bytes());
    for offset in [64, 72, 80, 88] {
        state[offset] = 1;
    }
    state[96] = 3;
    assert_eq!(unsafe { jit.call(&mut state) }, 0);
    assert_eq!(&state[32..40], &state[..8]);
    assert!(state[64..].iter().all(|&byte| byte == 0));
}

#[test]
fn sparse_worklist_fixed_dirty_word_handles_bit_63_and_partial_planes() {
    for bytes in [9usize, 13, 63, 64, 65, 504, 511, 512, 513] {
        for four_state in [false, true] {
            let planes = if four_state { 2 } else { 1 };
            let dirty_word_count = bytes.div_ceil(512);
            let mut state = vec![0xa5u8; 4144];
            for byte in 0..bytes * planes {
                state[2048 + byte] = (byte ^ (byte >> 8)) as u8;
            }
            let dirty = 1u64 | (1 << 31) | (1 << 63);
            state[4096..4104].copy_from_slice(&dirty.to_le_bytes());
            state[4104..4112].copy_from_slice(&1u64.to_le_bytes());
            state[4120..4128].copy_from_slice(&3u64.to_le_bytes());
            state[4128..4136].copy_from_slice(&1u64.to_le_bytes());
            let mut expected = state.clone();
            for plane in 0..planes {
                for byte in 0..bytes {
                    if [0, 31, 63, 64].contains(&(byte / 8)) {
                        expected[plane * bytes + byte] = state[2048 + plane * bytes + byte];
                    }
                }
            }
            expected[4096..4096 + dirty_word_count * 8].fill(0);
            expected[4120..4136].fill(0);
            let mut func = MFunction::new(VRegAllocator::new(), Vec::new());
            let descriptor_table = func.intern_constant_table(
                SparseCommitDescriptor {
                    src_offset: 2048,
                    dst_offset: 0,
                    byte_size: bytes as u64,
                    dirty_words_offset: 4096,
                    dirty_word_count: dirty_word_count as u64,
                    summary_words_offset: 4120,
                    summary_word_count: 1,
                    four_state: u64::from(four_state),
                }
                .words()
                .to_vec(),
            );
            let mut block = MBlock::new(BlockId(0));
            block.push(MInst::SparseCommitWorklist {
                descriptor_table,
                active_bits_offset: 4128,
                active_capacity: 1,
            });
            block.push(MInst::Return);
            func.push_block(block);
            let emitted = emit(&func, &AssignmentMap::default(), 0).unwrap();
            let jit = JitCode::new(&emitted.code).unwrap();
            assert_eq!(unsafe { jit.call(&mut state) }, 0);
            assert_eq!(state, expected, "bytes {bytes}, four_state {four_state}");
        }
    }
}

#[test]
fn sparse_worklist_fast_path_commits_single_chunk_four_state_tail() {
    const BYTE_SIZE: usize = 5;
    const STABLE: usize = 0;
    const SPARSE: usize = 16;
    const DIRTY: usize = 32;
    const SUMMARY: usize = 40;
    const ACTIVE_BITS: usize = 48;

    let mut func = MFunction::new(VRegAllocator::new(), Vec::new());
    let descriptor = SparseCommitDescriptor {
        src_offset: SPARSE as u64,
        dst_offset: STABLE as u64,
        byte_size: BYTE_SIZE as u64,
        dirty_words_offset: DIRTY as u64,
        dirty_word_count: 1,
        summary_words_offset: SUMMARY as u64,
        summary_word_count: 1,
        four_state: 1,
    };
    let table = func.intern_constant_table(descriptor.words().to_vec());
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::SparseMarkActive {
        active_index: 0,
        active_bits_offset: ACTIVE_BITS as i32,
        active_capacity: 1,
    });
    block.push(MInst::SparseCommitWorklist {
        descriptor_table: table,
        active_bits_offset: ACTIVE_BITS as i32,
        active_capacity: 1,
    });
    block.push(MInst::Return);
    func.push_block(block);

    let emitted = emit(&func, &AssignmentMap::default(), 0).unwrap();
    let jit = JitCode::new(&emitted.code).unwrap();
    let mut state = [0xa5u8; 80];
    state[STABLE..STABLE + BYTE_SIZE * 2].fill(0);
    for index in 0..BYTE_SIZE * 2 {
        state[SPARSE + index] = 0x31 + index as u8;
    }
    state[DIRTY..DIRTY + 8].copy_from_slice(&1u64.to_le_bytes());
    state[SUMMARY..SUMMARY + 8].copy_from_slice(&1u64.to_le_bytes());
    state[ACTIVE_BITS..ACTIVE_BITS + 8].fill(0);
    let sentinel = state[STABLE + BYTE_SIZE * 2];

    assert_eq!(unsafe { jit.call(&mut state) }, 0);
    assert_eq!(
        &state[STABLE..STABLE + BYTE_SIZE * 2],
        &state[SPARSE..SPARSE + BYTE_SIZE * 2]
    );
    assert_eq!(state[STABLE + BYTE_SIZE * 2], sentinel);
    assert_eq!(
        u64::from_le_bytes(state[DIRTY..DIRTY + 8].try_into().unwrap()),
        0
    );
    assert_eq!(
        u64::from_le_bytes(state[SUMMARY..SUMMARY + 8].try_into().unwrap()),
        0
    );
    assert_eq!(
        u64::from_le_bytes(state[ACTIVE_BITS..ACTIVE_BITS + 8].try_into().unwrap()),
        0
    );
}

#[test]
fn sparse_worklist_scans_later_bitmap_words_and_ignores_padding_bits() {
    const CAPACITY: usize = 66;
    const ACTIVE_INDEX: usize = 65;
    const STABLE: usize = 0;
    const SPARSE: usize = 8;
    const DIRTY: usize = 16;
    const SUMMARY: usize = 24;
    const ACTIVE_BITS: usize = 32;

    let descriptor = SparseCommitDescriptor {
        src_offset: SPARSE as u64,
        dst_offset: STABLE as u64,
        byte_size: 8,
        dirty_words_offset: DIRTY as u64,
        dirty_word_count: 1,
        summary_words_offset: SUMMARY as u64,
        summary_word_count: 1,
        four_state: 0,
    };
    let mut rows = vec![0; CAPACITY * SparseCommitDescriptor::WORDS];
    let row = ACTIVE_INDEX * SparseCommitDescriptor::WORDS;
    rows[row..row + SparseCommitDescriptor::WORDS].copy_from_slice(&descriptor.words());

    let mut func = MFunction::new(VRegAllocator::new(), Vec::new());
    let table = func.intern_constant_table(rows);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::SparseMarkActive {
        active_index: ACTIVE_INDEX as u32,
        active_bits_offset: ACTIVE_BITS as i32,
        active_capacity: CAPACITY,
    });
    block.push(MInst::SparseCommitWorklist {
        descriptor_table: table,
        active_bits_offset: ACTIVE_BITS as i32,
        active_capacity: CAPACITY,
    });
    block.push(MInst::Return);
    func.push_block(block);
    func.verify();

    let emitted = emit(&func, &AssignmentMap::default(), 0).unwrap();
    let jit = JitCode::new(&emitted.code).unwrap();
    let mut state = [0u8; 64];
    state[SPARSE..SPARSE + 8].copy_from_slice(&0xdead_beef_cafe_babeu64.to_le_bytes());
    state[DIRTY..DIRTY + 8].copy_from_slice(&1u64.to_le_bytes());
    state[SUMMARY..SUMMARY + 8].copy_from_slice(&1u64.to_le_bytes());
    // Bit 127 is padding outside CAPACITY and models a malformed restored
    // checkpoint. The generated mark adds valid bit 65 in the same word.
    state[ACTIVE_BITS + 8..ACTIVE_BITS + 16].copy_from_slice(&(1u64 << 63).to_le_bytes());

    assert_eq!(unsafe { jit.call(&mut state) }, 0);
    assert_eq!(
        u64::from_le_bytes(state[STABLE..STABLE + 8].try_into().unwrap()),
        0xdead_beef_cafe_babe
    );
    assert_eq!(
        u64::from_le_bytes(state[ACTIVE_BITS..ACTIVE_BITS + 8].try_into().unwrap()),
        0
    );
    assert_eq!(
        u64::from_le_bytes(state[ACTIVE_BITS + 8..ACTIVE_BITS + 16].try_into().unwrap()),
        0
    );
}
