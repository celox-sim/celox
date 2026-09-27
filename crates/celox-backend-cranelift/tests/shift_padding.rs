//! Consume a partial-width shift directly in a wider SIR operation. A store or
//! explicit cast between them would hide padding left in the physical chunks.
use celox_backend_cranelift::*;
use celox_design::StateObjectId;
use celox_state_layout::{LayoutInput, LayoutSource, MemoryLayoutMode, StateObjectLayout};
use num_bigint::BigUint;

struct Objects(Vec<StateObjectLayout<AbsoluteAddr>>);
impl LayoutSource<AbsoluteAddr> for Objects {
    fn layout_input(&self, _: MemoryLayoutMode) -> LayoutInput<AbsoluteAddr> {
        LayoutInput {
            state_objects: self.0.clone(),
            working_addresses: vec![],
            sparse_addresses: vec![],
            unpacked_arrays: HashMap::default(),
            requirements: Default::default(),
            ff_referenced_addresses: HashSet::default(),
            num_events: 0,
            runtime_event_sites: vec![],
        }
    }
}

fn check_shift_padding(source_width: usize, width: usize, op: BinaryOp) {
    let address = |id| AbsoluteAddr {
        instance_id: InstanceId(0),
        var_id: StateObjectId(id),
    };
    let region = |id| RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, address(id));
    let output_width = width.div_ceil(64) * 64;
    let layout = MemoryLayout::build(
        &Objects(
            [(0, source_width), (1, 128), (2, output_width)]
                .into_iter()
                .map(|(id, width)| StateObjectLayout {
                    address: address(id),
                    width,
                    is_4state: true,
                })
                .collect(),
        ),
        true,
        MemoryLayoutMode::Packed,
    );
    let unit = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [(
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: vec![],
                instructions: vec![
                    SIRInstruction::Load(
                        RegisterId(0),
                        region(0),
                        SIROffset::Static(0),
                        source_width,
                    ),
                    SIRInstruction::Load(RegisterId(1), region(1), SIROffset::Static(0), 128),
                    SIRInstruction::Binary(RegisterId(2), RegisterId(0), op, RegisterId(1)),
                    SIRInstruction::Imm(RegisterId(3), SIRValue::new(0u8)),
                    SIRInstruction::Binary(
                        RegisterId(4),
                        RegisterId(2),
                        BinaryOp::Or,
                        RegisterId(3),
                    ),
                    SIRInstruction::Store(
                        region(2),
                        SIROffset::Static(0),
                        output_width,
                        RegisterId(4),
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
            (0, source_width),
            (1, 128),
            (2, width),
            (3, output_width),
            (4, output_width),
        ]
        .into_iter()
        .map(|(id, width)| (RegisterId(id), RegisterType::Logic { width }))
        .collect(),
    };
    let mut jit = JitEngine::new(
        layout.clone(),
        &CompileOptions {
            four_state: true,
            emit_triggers: false,
            cranelift: Default::default(),
        },
    )
    .unwrap();
    let entry = jit.compile_units(&[unit], None, None, None).unwrap();
    // The JIT owns this function for the entire test; its ABI is (memory) -> u64.
    let run: unsafe extern "C" fn(*mut u8) -> u64 = unsafe { std::mem::transmute(entry) };
    let limit = (BigUint::from(1u8) << width) - 1u8;
    for bit in [0usize, 127] {
        let mask = BigUint::from(1u8) << bit;
        for (payload, count_mask) in [
            (BigUint::default(), mask.clone()),
            (mask.clone(), mask),
            (BigUint::default(), BigUint::default()),
        ] {
            let mut memory = vec![0u8; layout.merged_total_size];
            memory[layout.offsets[&address(0)]] = 1;
            for (offset, value) in [
                (layout.offsets[&address(1)], &payload),
                (
                    layout.offsets[&address(1)] + layout.plane_size(&address(1)),
                    &count_mask,
                ),
            ] {
                let bytes = value.to_bytes_le();
                memory[offset..offset + bytes.len()].copy_from_slice(&bytes);
            }
            assert_eq!(unsafe { run(memory.as_mut_ptr()) }, 0);
            let start = layout.offsets[&address(2)];
            let bytes = layout.plane_size(&address(2));
            let actual = (
                BigUint::from_bytes_le(&memory[start..start + bytes]),
                BigUint::from_bytes_le(&memory[start + bytes..start + 2 * bytes]),
            );
            let expected = if count_mask == BigUint::default() {
                (1u8.into(), 0u8.into())
            } else {
                (limit.clone(), limit.clone())
            };
            assert_eq!(
                actual, expected,
                "{op:?} source={source_width} width={width} bit={bit} payload={payload:x}"
            );
        }
    }
}

#[test]
fn register_shift_clears_padding_before_widening() {
    for width in [65, 129] {
        for op in [BinaryOp::Shl, BinaryOp::Shr, BinaryOp::Sar] {
            check_shift_padding(width, width, op);
        }
    }
}

#[test]
fn memory_shift_clears_padding_before_widening() {
    for width in [193, 257] {
        for op in [BinaryOp::Shl, BinaryOp::Shr, BinaryOp::Sar] {
            check_shift_padding(width, width, op);
        }
    }
}

#[test]
fn memory_shift_clears_padding_when_materialized_in_registers() {
    for width in [65, 129] {
        for op in [BinaryOp::Shl, BinaryOp::Shr, BinaryOp::Sar] {
            check_shift_padding(257, width, op);
        }
    }
}
