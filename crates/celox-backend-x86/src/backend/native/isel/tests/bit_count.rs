use super::*;

#[test]
fn native_bit_counts_cover_one_to_sixty_four_bits_and_zero() {
    for source_width in 1..=64 {
        let top = BigUint::from(1u8) << (source_width - 1);
        let edge_bits = if source_width == 1 {
            top
        } else {
            top | BigUint::from(1u8)
        };
        let edge_popcount = if source_width == 1 { 1 } else { 2 };
        assert_bit_counts(
            source_width,
            [
                (
                    BigUint::from(0u8),
                    0,
                    source_width as u64,
                    source_width as u64,
                ),
                (edge_bits, edge_popcount, 0, 0),
            ],
        );
    }

    assert_bit_counts(
        7,
        [
            (BigUint::from(0b001_0100u8), 2, 2, 2),
            (BigUint::from(0b100_0000u8), 1, 0, 6),
        ],
    );
    assert_bit_counts(
        64,
        [
            (BigUint::from(1u64), 1, 63, 0),
            (BigUint::from(1u64 << 63), 1, 0, 63),
            (BigUint::from(u64::MAX), 64, 0, 0),
        ],
    );
}

#[test]
fn native_bit_counts_cover_wide_and_partial_top_chunks() {
    let bit64 = BigUint::from(1u8) << 64usize;
    assert_bit_counts(
        65,
        [
            (BigUint::from(0u8), 0, 65, 65),
            (BigUint::from(1u8), 1, 64, 0),
            (bit64.clone(), 1, 0, 64),
            (bit64 | BigUint::from(1u8), 2, 0, 0),
        ],
    );

    let mixed = (BigUint::from(1u8) << 129usize)
        | (BigUint::from(1u8) << 64usize)
        | (BigUint::from(1u8) << 3usize);
    let middle = BigUint::from(1u8) << 64usize;
    assert_bit_counts(
        130,
        [
            (BigUint::from(0u8), 0, 130, 130),
            (mixed, 3, 0, 3),
            (middle, 1, 65, 64),
        ],
    );
}

#[test]
fn native_wide_bit_counts_produce_conservative_x_results() {
    let unknown = BigUint::from(1u8) << 64usize;
    for op in [
        UnaryOp::PopCount,
        UnaryOp::CountLeadingZeros,
        UnaryOp::CountTrailingZeros,
    ] {
        let compiled = compile_bit_count(op, 65, true);
        assert_eq!(
            compiled.run(&BigUint::from(0u8), &unknown),
            (0x7f, 0x7f),
            "{op}"
        );
    }
}

struct CompiledBitCount {
    jit: JitCode,
    state_size: usize,
    input_offset: usize,
    input_bytes: usize,
    input_mask_offset: Option<usize>,
    output_offset: usize,
    output_bytes: usize,
    output_mask_offset: Option<usize>,
}

impl CompiledBitCount {
    fn run(&self, value: &BigUint, mask: &BigUint) -> (u64, u64) {
        let mut state = vec![0u8; self.state_size];
        let value_bytes = value.to_bytes_le();
        let value_len = value_bytes.len().min(self.input_bytes);
        state[self.input_offset..self.input_offset + value_len]
            .copy_from_slice(&value_bytes[..value_len]);

        if let Some(input_mask_offset) = self.input_mask_offset {
            let mask_bytes = mask.to_bytes_le();
            let mask_len = mask_bytes.len().min(self.input_bytes);
            state[input_mask_offset..input_mask_offset + mask_len]
                .copy_from_slice(&mask_bytes[..mask_len]);
        }

        assert_eq!(unsafe { self.jit.call(&mut state) }, 0);

        let read_word = |offset: usize| {
            let mut bytes = [0u8; 8];
            let len = self.output_bytes.min(bytes.len());
            bytes[..len].copy_from_slice(&state[offset..offset + len]);
            u64::from_le_bytes(bytes)
        };
        let result = read_word(self.output_offset);
        let result_mask = self.output_mask_offset.map(read_word).unwrap_or(0);
        (result, result_mask)
    }
}

fn compile_bit_count(op: UnaryOp, source_width: usize, four_state: bool) -> CompiledBitCount {
    let expect_native_bsf = !four_state && matches!(op, UnaryOp::CountTrailingZeros);
    let result_width = op.result_width(source_width);
    let input_bytes = source_width.div_ceil(8);
    let output_bytes = result_width.div_ceil(8);
    let input_storage_bytes = input_bytes * if four_state { 2 } else { 1 };
    let output_offset = input_storage_bytes.next_multiple_of(8);
    let output_storage_bytes = output_bytes * if four_state { 2 } else { 1 };
    let state_size = (output_offset + output_storage_bytes).max(8);

    let input_var = VarId::default();
    let mut output_var = input_var;
    output_var.0 += 1;
    let input_abs = AbsoluteAddr {
        instance_id: InstanceId(0),
        var_id: input_var,
    };
    let output_abs = AbsoluteAddr {
        instance_id: InstanceId(0),
        var_id: output_var,
    };
    let input_addr = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, input_abs);
    let output_addr = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, output_abs);

    let source = RegisterId(0);
    let result = RegisterId(1);
    let register_type = |width| {
        if four_state {
            RegisterType::Logic { width }
        } else {
            RegisterType::Bit {
                width,
                signed: false,
            }
        }
    };
    let eu = ExecutionUnit {
        entry_block_id: SirBlockId(0),
        blocks: [(
            SirBlockId(0),
            BasicBlock {
                id: SirBlockId(0),
                params: vec![],
                instructions: vec![
                    SIRInstruction::Load(source, input_addr, SIROffset::Static(0), source_width),
                    SIRInstruction::Unary(result, op, source),
                    SIRInstruction::Store(
                        output_addr,
                        SIROffset::Static(0),
                        result_width,
                        result,
                        vec![],
                        vec![],
                    ),
                ],
                terminator: SIRTerminator::Return,
            },
        )]
        .into_iter()
        .collect(),
        register_map: [
            (source, register_type(source_width)),
            (result, register_type(result_width)),
        ]
        .into_iter()
        .collect(),
    };
    eu.verify();

    let layout = MemoryLayout {
        trace: None,
        four_state,
        mode: MemoryLayoutMode::Packed,
        unpacked_arrays: HashMap::default(),
        offsets: [(input_abs, 0), (output_abs, output_offset)]
            .into_iter()
            .collect(),
        widths: [(input_abs, source_width), (output_abs, result_width)]
            .into_iter()
            .collect(),
        is_4states: [(input_abs, four_state), (output_abs, four_state)]
            .into_iter()
            .collect(),
        total_size: state_size,
        working_offsets: HashMap::default(),
        working_base_offset: state_size,
        sparse_offsets: HashMap::default(),
        sparse_base_offset: state_size,
        sparse_layouts: HashMap::default(),
        sparse_active_bits_offset: state_size,
        sparse_active_capacity: 0,
        merged_total_size: state_size,
        triggered_bits_offset: state_size,
        triggered_bits_total_size: 0,
        scratch_base_offset: state_size,
        scratch_size: 0,
        runtime_event_capacity: 0,
        runtime_event_slot_size: 0,
        runtime_event_buffer_size: 0,
        runtime_event_site_layouts: vec![],
    };

    let mut function = lower_execution_unit(&eu, &layout, four_state);
    function.verify();
    mir_legalize::legalize(&mut function);
    function.verify();
    mir_opt::optimize(&mut function);
    function.verify();
    if expect_native_bsf {
        let mut instructions = function.blocks.iter().flat_map(|block| &block.insts);
        assert!(
            instructions
                .clone()
                .any(|inst| matches!(inst, MInst::Bsf { .. }))
        );
        assert!(!instructions.any(|inst| matches!(inst, MInst::Bsr { .. })));
    }
    let allocation = regalloc::run_regalloc(&mut function).unwrap();
    mir_opt::post_regalloc_peephole(&mut function, &allocation.assignment);
    function.verify();
    let emitted = emit::emit(
        &function,
        &allocation.assignment,
        allocation.spill_frame_size,
    )
    .unwrap();

    CompiledBitCount {
        jit: JitCode::new(&emitted.code).unwrap(),
        state_size,
        input_offset: 0,
        input_bytes,
        input_mask_offset: four_state.then_some(input_bytes),
        output_offset,
        output_bytes,
        output_mask_offset: four_state.then_some(output_offset + output_bytes),
    }
}

fn assert_bit_counts(
    source_width: usize,
    cases: impl IntoIterator<Item = (BigUint, u64, u64, u64)>,
) {
    let popcount = compile_bit_count(UnaryOp::PopCount, source_width, false);
    let leading = compile_bit_count(UnaryOp::CountLeadingZeros, source_width, false);
    let trailing = compile_bit_count(UnaryOp::CountTrailingZeros, source_width, false);
    for (value, expected_popcount, expected_leading, expected_trailing) in cases {
        assert_eq!(
            popcount.run(&value, &BigUint::from(0u8)),
            (expected_popcount, 0),
            "popcount width={source_width} value={value:#x}"
        );
        assert_eq!(
            leading.run(&value, &BigUint::from(0u8)),
            (expected_leading, 0),
            "clz width={source_width} value={value:#x}"
        );
        assert_eq!(
            trailing.run(&value, &BigUint::from(0u8)),
            (expected_trailing, 0),
            "ctz width={source_width} value={value:#x}"
        );
    }
}
