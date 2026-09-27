use super::*;

#[test]
fn memfill_executes_qwords_and_every_tail_width_without_touching_neighbors() {
    const START: usize = 5;
    const LEN: usize = 23;

    let mut func = MFunction::new(VRegAllocator::new(), vec![]);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::MemFill {
        dst_offset: START as i32,
        byte_len: LEN,
        value: 0x5a,
    });
    block.push(MInst::Return);
    func.push_block(block);

    let emitted = emit(&func, &AssignmentMap::default(), 0).unwrap();
    let jit = JitCode::new(&emitted.code).unwrap();
    let mut state = [0xa5u8; 40];

    assert_eq!(unsafe { jit.call(&mut state) }, 0);
    assert_eq!(&state[..START], &[0xa5; START]);
    assert_eq!(&state[START..START + LEN], &[0x5a; LEN]);
    assert_eq!(&state[START + LEN..], &[0xa5; 40 - START - LEN]);
}

#[test]
fn nonoverlapping_memcopy_executes_vector_body_and_tail_without_touching_neighbors() {
    const SRC: usize = 3;
    const DST: usize = 64;
    const LEN: usize = 37;

    let mut func = MFunction::new(VRegAllocator::new(), vec![]);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::MemCopy {
        src_offset: SRC as i32,
        dst_offset: DST as i32,
        byte_len: LEN,
    });
    block.push(MInst::Return);
    func.push_block(block);

    let emitted = emit(&func, &AssignmentMap::default(), 0).unwrap();
    let jit = JitCode::new(&emitted.code).unwrap();
    let mut state = [0xa5u8; 112];
    for (index, byte) in state[SRC..SRC + LEN].iter_mut().enumerate() {
        *byte = index as u8 ^ 0x6d;
    }
    let expected = state[SRC..SRC + LEN].to_vec();

    assert_eq!(unsafe { jit.call(&mut state) }, 0);
    assert_eq!(&state[DST..DST + LEN], expected);
    assert_eq!(state[DST - 1], 0xa5);
    assert_eq!(state[DST + LEN], 0xa5);
}

#[test]
fn overlapping_memcopy_executes_backward_without_corrupting_source() {
    const SRC: usize = 3;
    const DST: usize = 8;
    const LEN: usize = 37;

    let mut func = MFunction::new(VRegAllocator::new(), vec![]);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::MemCopy {
        src_offset: SRC as i32,
        dst_offset: DST as i32,
        byte_len: LEN,
    });
    block.push(MInst::Return);
    func.push_block(block);

    let emitted = emit(&func, &AssignmentMap::default(), 0).unwrap();
    let jit = JitCode::new(&emitted.code).unwrap();
    let mut state = [0xa5u8; 64];
    for (index, byte) in state.iter_mut().enumerate() {
        *byte = index as u8 ^ 0x6d;
    }
    let mut expected = state;
    expected.copy_within(SRC..SRC + LEN, DST);

    assert_eq!(unsafe { jit.call(&mut state) }, 0);
    assert_eq!(state, expected);
}

#[test]
fn overlapping_memcopy_executes_forward_without_corrupting_source() {
    const SRC: usize = 8;
    const DST: usize = 3;
    const LEN: usize = 37;

    let mut func = MFunction::new(VRegAllocator::new(), vec![]);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::MemCopy {
        src_offset: SRC as i32,
        dst_offset: DST as i32,
        byte_len: LEN,
    });
    block.push(MInst::Return);
    func.push_block(block);

    let emitted = emit(&func, &AssignmentMap::default(), 0).unwrap();
    let jit = JitCode::new(&emitted.code).unwrap();
    let mut state = [0xa5u8; 64];
    for (index, byte) in state.iter_mut().enumerate() {
        *byte = index as u8 ^ 0xb3;
    }
    let mut expected = state;
    expected.copy_within(SRC..SRC + LEN, DST);

    assert_eq!(unsafe { jit.call(&mut state) }, 0);
    assert_eq!(state, expected);
}

#[test]
fn rip_relative_constant_tables_execute_for_multiple_indexes() {
    let mut vregs = VRegAllocator::new();
    let index = vregs.alloc();
    let byte_index = vregs.alloc();
    let first_addr = vregs.alloc();
    let second_addr = vregs.alloc();
    let first_value = vregs.alloc();
    let second_value = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 6]);
    let first_values = vec![0x0123_4567_89ab_cdef, 0xfedc_ba98_7654_3210, 0, u64::MAX];
    let second_values = vec![11, 29, 47, 83];
    let first_table = func.intern_constant_table(first_values.clone());
    let second_table = func.intern_constant_table(second_values.clone());

    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::Load {
        dst: index,
        base: BaseReg::SimState,
        offset: 0,
        size: OpSize::S64,
    });
    block.push(MInst::ShlImm {
        dst: byte_index,
        src: index,
        imm: 3,
    });
    block.push(MInst::LoadConstantTableAddr {
        dst: first_addr,
        table: first_table,
    });
    block.push(MInst::LoadPtrIndexed {
        dst: first_value,
        ptr: first_addr,
        offset: 0,
        index: byte_index,
        size: OpSize::S64,
    });
    block.push(MInst::LoadConstantTableAddr {
        dst: second_addr,
        table: second_table,
    });
    block.push(MInst::LoadPtrIndexed {
        dst: second_value,
        ptr: second_addr,
        offset: 0,
        index: byte_index,
        size: OpSize::S64,
    });
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 8,
        src: first_value,
        size: OpSize::S64,
    });
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 16,
        src: second_value,
        size: OpSize::S64,
    });
    block.push(MInst::Return);
    func.push_block(block);

    mir_legalize::legalize(&mut func);
    mir_opt::optimize(&mut func);
    let allocation = regalloc::run_regalloc(&mut func).unwrap();
    let emitted = emit(&func, &allocation.assignment, allocation.spill_frame_size).unwrap();

    let trailing_table = second_values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect::<Vec<_>>();
    assert!(emitted.code.ends_with(&trailing_table));
    assert_eq!(
        emitted.code.len() - emitted.text_size,
        (first_values.len() + second_values.len()) * std::mem::size_of::<u64>()
    );
    assert!(emitted.text_size < emitted.code.len());

    let mut decoder = Decoder::new(64, &emitted.code[..emitted.text_size], DecoderOptions::NONE);
    let mut table_leas = 0;
    while decoder.can_decode() {
        let instruction = decoder.decode();
        if instruction.mnemonic() == Mnemonic::Lea && instruction.memory_base() == Register::RIP {
            table_leas += 1;
        }
        if instruction.mnemonic() == Mnemonic::Ret {
            break;
        }
    }
    assert_eq!(table_leas, 2);

    let jit = JitCode::new(&emitted.code).unwrap();
    for index_value in 0..first_values.len() {
        let mut state = [0u8; 24];
        state[0..8].copy_from_slice(&(index_value as u64).to_le_bytes());
        assert_eq!(unsafe { jit.call(&mut state) }, 0);
        assert_eq!(
            u64::from_le_bytes(state[8..16].try_into().unwrap()),
            first_values[index_value]
        );
        assert_eq!(
            u64::from_le_bytes(state[16..24].try_into().unwrap()),
            second_values[index_value]
        );
    }
}
