use celox_backend_x86::{
    native::{emit, jit_mem::JitCode, scalar_pipeline},
    *,
};
use std::sync::Arc;

fn sir_fixture(
    lanes: usize,
    element_width: usize,
    word_width: usize,
) -> ExecutionUnit<RegionedAbsoluteAddr> {
    let r = RegisterId;
    let key_width = lanes.trailing_zeros() as usize;
    let bit = |width| RegisterType::Bit {
        width,
        signed: false,
    };
    let index_type = RegisterType::Bit {
        width: 32,
        signed: true,
    };
    let value_type = RegisterType::Logic { width: word_width };
    let register_map = (0..31)
        .map(|id| {
            let ty = match id {
                0 | 9 | 16 => bit(key_width),
                3 | 5 | 7 | 12 | 27 => bit(key_width + 1),
                4 | 6 | 13 | 29 => index_type.clone(),
                8 | 10 | 19 => bit(64),
                15 => bit(32),
                17 => RegisterType::Logic { width: 1 },
                18 | 28 => bit(1),
                11 | 20 | 21 => bit(word_width),
                _ => value_type.clone(),
            };
            (r(id), ty)
        })
        .collect();
    let entry = BasicBlock {
        id: BlockId(0),
        params: vec![r(0), r(1), r(2)],
        instructions: vec![
            SIRInstruction::Imm(r(3), SIRValue::new(lanes as u64)),
            SIRInstruction::Imm(r(4), SIRValue::new(0u8)),
            SIRInstruction::Imm(r(5), SIRValue::new(1u8)),
            SIRInstruction::Imm(r(6), SIRValue::new(1u8)),
            SIRInstruction::Imm(r(7), SIRValue::new(0u8)),
            SIRInstruction::Imm(r(8), SIRValue::new(0u8)),
            SIRInstruction::Imm(r(9), SIRValue::new((lanes - 1) as u64)),
            SIRInstruction::Imm(r(10), SIRValue::new(element_width as u64)),
        ],
        terminator: SIRTerminator::Jump(BlockId(1), vec![r(3), r(4), r(2)]),
    };
    let scan = BasicBlock {
        id: BlockId(1),
        params: vec![r(12), r(13), r(14)],
        instructions: vec![
            SIRInstruction::Imm(r(11), SIRValue::new((1u64 << element_width) - 1)),
            SIRInstruction::Binary(r(15), r(13), BinaryOp::Shr, r(8)),
            SIRInstruction::Binary(r(16), r(15), BinaryOp::And, r(9)),
            SIRInstruction::Binary(r(17), r(0), BinaryOp::Eq, r(16)),
            SIRInstruction::Unary(r(18), UnaryOp::ToTwoState, r(17)),
            SIRInstruction::Binary(r(19), r(13), BinaryOp::Mul, r(10)),
            SIRInstruction::Binary(r(20), r(11), BinaryOp::Shl, r(19)),
            SIRInstruction::Unary(r(21), UnaryOp::BitNot, r(20)),
            SIRInstruction::Binary(r(22), r(14), BinaryOp::And, r(21)),
            SIRInstruction::Binary(r(23), r(1), BinaryOp::Shl, r(19)),
            SIRInstruction::Binary(r(24), r(23), BinaryOp::And, r(20)),
            SIRInstruction::Binary(r(25), r(22), BinaryOp::Or, r(24)),
            SIRInstruction::Mux(r(26), r(18), r(25), r(14)),
            SIRInstruction::Binary(r(27), r(12), BinaryOp::Sub, r(5)),
            SIRInstruction::Binary(r(28), r(27), BinaryOp::Ne, r(7)),
            SIRInstruction::Binary(r(29), r(13), BinaryOp::Add, r(6)),
        ],
        terminator: SIRTerminator::Branch {
            cond: r(28),
            true_block: (BlockId(1), vec![r(27), r(29), r(26)]),
            false_block: (BlockId(2), vec![r(26)]),
        },
    };
    let exit = BasicBlock {
        id: BlockId(2),
        params: vec![r(30)],
        instructions: vec![SIRInstruction::RuntimeEvent {
            site_id: 0,
            args: vec![r(30)],
        }],
        terminator: SIRTerminator::Return,
    };
    ExecutionUnit {
        entry_block_id: BlockId(0),
        register_map,
        blocks: [entry, scan, exit].into_iter().map(|b| (b.id, b)).collect(),
    }
}

pub struct CompiledUpdate {
    pub jit: JitCode,
    pub state: Vec<u128>,
}

impl CompiledUpdate {
    pub fn bytes(&mut self) -> &mut [u8] {
        // u128 has no invalid bit patterns; the slice stays within this
        // allocation and preserves its 16-byte alignment for native scratch.
        unsafe {
            std::slice::from_raw_parts_mut(self.state.as_mut_ptr().cast(), self.state.len() * 16)
        }
    }

    pub fn execute(&mut self) -> i64 {
        // The function passed every SIR/MIR/allocation/emission verifier and
        // state includes its exact required native arena, allocated once.
        unsafe { (self.jit.fn_ptr)(self.state.as_mut_ptr().cast()) }
    }
}

pub fn compile(recover: bool) -> CompiledUpdate {
    let mut eu = sir_fixture(32, 6, 192);
    let addresses = (0..4)
        .map(|id| AbsoluteAddr {
            instance_id: InstanceId(0),
            var_id: celox_design::StateObjectId(id),
        })
        .collect::<Vec<_>>();
    let addr = |id| RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, addresses[id]);
    let entry = eu.blocks.get_mut(&BlockId(0)).unwrap();
    entry.params.clear();
    entry.instructions.splice(
        0..0,
        [
            SIRInstruction::Load(RegisterId(0), addr(0), SIROffset::Static(0), 5),
            SIRInstruction::Load(RegisterId(1), addr(1), SIROffset::Static(0), 192),
            SIRInstruction::Load(RegisterId(2), addr(2), SIROffset::Static(0), 192),
        ],
    );
    eu.blocks.get_mut(&BlockId(2)).unwrap().instructions = vec![SIRInstruction::Store(
        addr(3),
        SIROffset::Static(0),
        192,
        RegisterId(30),
        vec![],
        vec![],
    )];
    let layout = MemoryLayout {
        trace: None,
        four_state: false,
        mode: celox_state_layout::MemoryLayoutMode::Packed,
        offsets: [
            (addresses[0], 0),
            (addresses[1], 8),
            (addresses[2], 32),
            (addresses[3], 56),
        ]
        .into_iter()
        .collect(),
        widths: [
            (addresses[0], 5),
            (addresses[1], 192),
            (addresses[2], 192),
            (addresses[3], 192),
        ]
        .into_iter()
        .collect(),
        is_4states: addresses.iter().map(|&address| (address, false)).collect(),
        unpacked_arrays: HashMap::default(),
        total_size: 80,
        working_offsets: HashMap::default(),
        working_base_offset: 80,
        sparse_offsets: HashMap::default(),
        sparse_base_offset: 80,
        sparse_layouts: HashMap::default(),
        sparse_active_bits_offset: 80,
        sparse_active_capacity: 0,
        merged_total_size: 80,
        triggered_bits_offset: 80,
        triggered_bits_total_size: 0,
        scratch_base_offset: 80,
        scratch_size: 0,
        runtime_event_capacity: 0,
        runtime_event_slot_size: 0,
        runtime_event_buffer_size: 0,
        runtime_event_site_layouts: vec![],
    };
    eu.verify();
    celox_sir_opt::optimizer::optimize_merged_chain(
        &mut eu,
        Arc::new(HashMap::default()),
        |_, _, _| true,
        false,
        recover,
        &celox_sir_opt::SirDiagnostics::default(),
        || false,
    )
    .unwrap();
    eu.verify();
    assert_eq!(eu.blocks.contains_key(&BlockId(1)), !recover);
    let prepared =
        scalar_pipeline::prepare_scalar_eu(&eu, &layout, false, "packed-index-update").unwrap();
    let emitted = emit::emit(
        &prepared.function,
        &prepared.allocation,
        prepared.spill_frame_size,
    )
    .unwrap();
    CompiledUpdate {
        jit: JitCode::new(&emitted.code).unwrap(),
        state: vec![0; (emitted.required_state_size as usize).div_ceil(16)],
    }
}
