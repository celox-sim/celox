//! `ToPlanes` and `FromPlanes` move the value and mask words of a run-time
//! four-state value unchanged, X and Z bits included.
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
            lane_writers: Default::default(),
            access_rank: Default::default(),
        }
    }
}

fn check_planes(width: usize, four_state: bool) {
    let address = |id| AbsoluteAddr {
        instance_id: InstanceId(0),
        var_id: StateObjectId(id),
    };
    let region = |id| RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, address(id));
    // The source, its planes, and the value made back from them.
    let layout = MemoryLayout::build(
        &Objects(
            [(0, width, true), (1, 2 * width, false), (2, width, true)]
                .into_iter()
                .map(|(id, width, is_4state)| StateObjectLayout {
                    address: address(id),
                    width,
                    is_4state,
                })
                .collect(),
        ),
        four_state,
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
                    SIRInstruction::Load(RegisterId(0), region(0), SIROffset::Static(0), width),
                    SIRInstruction::Unary(RegisterId(1), UnaryOp::ToPlanes, RegisterId(0)),
                    SIRInstruction::Store(
                        region(1),
                        SIROffset::Static(0),
                        2 * width,
                        RegisterId(1),
                        vec![],
                        vec![],
                    ),
                    SIRInstruction::Unary(RegisterId(2), UnaryOp::FromPlanes, RegisterId(1)),
                    SIRInstruction::Store(
                        region(2),
                        SIROffset::Static(0),
                        width,
                        RegisterId(2),
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
            (RegisterId(0), RegisterType::Logic { width }),
            (
                RegisterId(1),
                RegisterType::Bit {
                    width: 2 * width,
                    signed: false,
                },
            ),
            (RegisterId(2), RegisterType::Logic { width }),
        ]
        .into_iter()
        .collect(),
    };
    unit.verify();
    let mut jit = JitEngine::new(
        layout.clone(),
        &CompileOptions {
            four_state,
            emit_triggers: false,
            cranelift: Default::default(),
        },
    )
    .unwrap();
    let entry = jit.compile_units(&[unit], None, None, None).unwrap();
    // The JIT owns this function for the entire test; its ABI is (memory) -> u64.
    let run: unsafe extern "C" fn(*mut u8) -> u64 = unsafe { std::mem::transmute(entry) };
    let all = (BigUint::from(1u8) << width) - 1u8;
    // X where both planes are set, Z where only the mask is.
    let value = BigUint::parse_bytes(b"f0e1d2c3b4a5968778695a4b3c2d1e0f", 16).unwrap() & &all;
    let mask = BigUint::parse_bytes(b"00ff00000000f00f8000000000000003", 16).unwrap() & &all;
    let mut memory = vec![0u8; layout.merged_total_size];
    let source = layout.offsets[&address(0)];
    let plane = layout.plane_size(&address(0));
    memory[source..source + value.to_bytes_le().len()].copy_from_slice(&value.to_bytes_le());
    if four_state {
        let start = source + plane;
        memory[start..start + mask.to_bytes_le().len()].copy_from_slice(&mask.to_bytes_le());
    }
    assert_eq!(unsafe { run(memory.as_mut_ptr()) }, 0);
    let read = |start: usize, bytes: usize| BigUint::from_bytes_le(&memory[start..start + bytes]);
    let mask = if four_state { mask } else { BigUint::default() };
    let case = format!("width={width} four_state={four_state}");
    let planes = layout.offsets[&address(1)];
    assert_eq!(
        read(planes, layout.plane_size(&address(1))),
        (&mask << width) | &value,
        "{case}"
    );
    let back = layout.offsets[&address(2)];
    assert_eq!(read(back, plane), value, "{case}");
    if four_state {
        assert_eq!(read(back + plane, plane), mask, "{case}");
    }
}

#[test]
fn planes_round_trip_run_time_four_state_values() {
    for width in [64, 128, 192] {
        for four_state in [false, true] {
            check_planes(width, four_state);
        }
    }
}
