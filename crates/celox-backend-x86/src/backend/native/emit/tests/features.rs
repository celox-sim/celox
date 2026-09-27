use super::*;

#[test]
fn empty_function_does_not_require_unused_host_isa_features() {
    let mut function = MFunction::new(VRegAllocator::new(), Vec::new());
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::Return);
    function.push_block(block);

    let emitted = emit(&function, &AssignmentMap::default(), 0).unwrap();

    assert_eq!(
        emitted.required_image_features
            & (IMAGE_FEATURE_BMI2 | IMAGE_FEATURE_AVX | IMAGE_FEATURE_POPCNT),
        0
    );
}

#[test]
fn spilled_pack_scratch_uses_baseline_move_without_avx_requirement() {
    let mut vregs = VRegAllocator::new();
    let low = vregs.alloc();
    let high = vregs.alloc();
    let mut function = MFunction::new(vregs, vec![SpillDesc::transient(); 2]);
    function.target_features = X86Features::for_test_with_avx(false, true);
    let scratch = function.alloc_x86_vec();
    let destination = function.alloc_x86_vec();
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::LoadImm { dst: low, value: 1 });
    block.push(MInst::LoadImm {
        dst: high,
        value: 2,
    });
    block.push(MInst::X86Simd(X86SimdInst::Scratch128 { dst: scratch }));
    block.push(MInst::X86Simd(X86SimdInst::Pack128 {
        dst: destination,
        low,
        high,
        scratch: Some(scratch),
    }));
    block.push(MInst::Return);
    function.push_block(block);
    function.verify();

    let mut assignment = AssignmentMap::default();
    assignment.set(low, PhysReg::RAX);
    assignment.set(high, PhysReg::RBX);
    assignment.set_x86_vector(scratch, X86VectorLocation::Stack(0));
    assignment.set_x86_vector(destination, X86VectorLocation::Register(X86PhysVec(0)));

    let emitted = emit(&function, &assignment, 16).unwrap();
    assert_eq!(emitted.required_image_features & IMAGE_FEATURE_AVX, 0);

    let mut decoder = Decoder::new(64, &emitted.code[..emitted.text_size], DecoderOptions::NONE);
    let mut instructions = Vec::new();
    while decoder.can_decode() {
        instructions.push(decoder.decode());
    }
    assert!(
        instructions
            .iter()
            .any(|instruction| instruction.mnemonic() == Mnemonic::Movdqu)
    );
    assert!(
        !instructions
            .iter()
            .any(|instruction| instruction.mnemonic() == Mnemonic::Vmovdqu)
    );
}

#[test]
fn popcnt_instruction_records_its_image_requirement() {
    let mut vregs = VRegAllocator::new();
    let src = vregs.alloc();
    let dst = vregs.alloc();
    let mut function = MFunction::new(vregs, vec![SpillDesc::transient(); 2]);
    function.target_features = X86Features::for_test(false);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::LoadImm { dst: src, value: 7 });
    block.push(MInst::Popcnt { dst, src });
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 0,
        src: dst,
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

    assert_ne!(emitted.required_image_features & IMAGE_FEATURE_POPCNT, 0);
}

#[test]
fn vector_binary_ops_execute_both_lanes_without_gpr_roundtrips() {
    for op in [
        X86SimdBinaryOp::And,
        X86SimdBinaryOp::Or,
        X86SimdBinaryOp::Xor,
    ] {
        let mut function = MFunction::new(VRegAllocator::new(), Vec::new());
        let lhs = function.alloc_x86_vec();
        let rhs = function.alloc_x86_vec();
        let result = function.alloc_x86_vec();
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::X86Simd(X86SimdInst::Load128 {
            dst: lhs,
            base: BaseReg::SimState,
            offset: 0,
        }));
        block.push(MInst::X86Simd(X86SimdInst::Load128 {
            dst: rhs,
            base: BaseReg::SimState,
            offset: 16,
        }));
        block.push(MInst::X86Simd(X86SimdInst::Binary128 {
            op,
            dst: result,
            lhs,
            rhs,
        }));
        block.push(MInst::X86Simd(X86SimdInst::Store128 {
            base: BaseReg::SimState,
            offset: 32,
            src: result,
        }));
        block.push(MInst::Return);
        function.push_block(block);
        function.verify();

        let mut assignment = AssignmentMap::default();
        assignment.set_x86_vector(lhs, X86VectorLocation::Register(X86PhysVec(0)));
        assignment.set_x86_vector(rhs, X86VectorLocation::Register(X86PhysVec(1)));
        assignment.set_x86_vector(result, X86VectorLocation::Register(X86PhysVec(2)));
        let emitted = emit(&function, &assignment, 0).unwrap();
        let jit = JitCode::new(&emitted.code).unwrap();
        let lhs_lanes: [u64; 2] = [0xf0f0_aaaa_5555_0f0f, 0x0123_4567_89ab_cdef];
        let rhs_lanes: [u64; 2] = [0x3333_ffff_0000_cccc, 0xfedc_ba98_7654_3210];
        let mut state = [0u8; 48];
        for (lane, value) in lhs_lanes.into_iter().enumerate() {
            state[lane * 8..lane * 8 + 8].copy_from_slice(&value.to_le_bytes());
        }
        for (lane, value) in rhs_lanes.into_iter().enumerate() {
            let offset = 16 + lane * 8;
            state[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        }

        assert_eq!(unsafe { jit.call(&mut state) }, 0);
        for lane in 0..2 {
            let offset = 32 + lane * 8;
            let actual = u64::from_le_bytes(state[offset..offset + 8].try_into().unwrap());
            let expected = match op {
                X86SimdBinaryOp::And => lhs_lanes[lane] & rhs_lanes[lane],
                X86SimdBinaryOp::Or => lhs_lanes[lane] | rhs_lanes[lane],
                X86SimdBinaryOp::Xor => lhs_lanes[lane] ^ rhs_lanes[lane],
            };
            assert_eq!(actual, expected, "{op:?}");
        }
    }
}
