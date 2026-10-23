use celox_backend_cranelift::*;
use celox_design::StateObjectId;
use celox_state_layout::{LayoutInput, LayoutSource, MemoryLayoutMode, StateObjectLayout};

const WIDTH: usize = 512;

fn address() -> AbsoluteAddr {
    AbsoluteAddr {
        instance_id: InstanceId(0),
        var_id: StateObjectId(0),
    }
}

fn region() -> RegionedAbsoluteAddr {
    RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, address())
}

struct Objects;
impl LayoutSource<AbsoluteAddr> for Objects {
    fn layout_input(&self, _: MemoryLayoutMode) -> LayoutInput<AbsoluteAddr> {
        LayoutInput {
            state_objects: vec![StateObjectLayout {
                address: address(),
                width: WIDTH,
                is_4state: true,
            }],
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

fn compute_chain() -> ExecutionUnit<RegionedAbsoluteAddr> {
    let mut register_map = HashMap::default();
    let mut instructions = vec![
        SIRInstruction::Load(RegisterId(0), region(), SIROffset::Static(0), WIDTH),
        SIRInstruction::Load(RegisterId(1), region(), SIROffset::Static(0), 1),
        SIRInstruction::Imm(RegisterId(2), SIRValue::new(0u8)),
        // Both the old wide value and the branch condition must survive this
        // write, subsequent arbitrary instruction cuts, and their tail calls.
        SIRInstruction::Store(
            region(),
            SIROffset::Static(0),
            WIDTH,
            RegisterId(2),
            vec![],
            vec![],
        ),
        SIRInstruction::Imm(RegisterId(3), SIRValue::new(1u8)),
        SIRInstruction::Imm(RegisterId(4), SIRValue::new(7u8)),
    ];
    let mut result = RegisterId(0);
    // A pure computation interval with no Store boundary. An even number of
    // XORs preserves the original payload, while four-state XOR normalizes Z.
    for index in 0..2048 {
        let next = RegisterId(index + 5);
        instructions.push(SIRInstruction::Binary(
            next,
            result,
            BinaryOp::Xor,
            RegisterId(3),
        ));
        result = next;
    }
    for index in 0..2053 {
        register_map.insert(
            RegisterId(index),
            RegisterType::Logic {
                width: if index == 1 { 1 } else { WIDTH },
            },
        );
    }
    let store = |id, value| BasicBlock {
        id: BlockId(id),
        params: vec![],
        instructions: vec![SIRInstruction::Store(
            region(),
            SIROffset::Static(0),
            WIDTH,
            value,
            vec![],
            vec![],
        )],
        terminator: SIRTerminator::Return,
    };
    ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [
            (
                BlockId(0),
                BasicBlock {
                    id: BlockId(0),
                    params: vec![],
                    instructions,
                    terminator: SIRTerminator::Branch {
                        cond: RegisterId(1),
                        true_block: (BlockId(1), vec![]),
                        false_block: (BlockId(2), vec![]),
                    },
                },
            ),
            (BlockId(1), store(1, result)),
            (BlockId(2), store(2, RegisterId(4))),
        ]
        .into_iter()
        .collect(),
        register_map,
    }
}

#[test]
fn pure_computation_cuts_preserve_old_loads_and_branch_inputs() {
    for four_state in [false, true] {
        let unit = compute_chain();
        let plan = tail_call_split::split_if_needed_spilled(&[unit], four_state)
            .expect("pure computation must be split without a Store boundary");
        assert!(plan.chunks.len() > 1);
        for chunk in &plan.chunks {
            assert!(
                cost_model::estimate_eu_value_count(&chunk.eu, four_state)
                    <= cost_model::FUNCTION_VALUE_THRESHOLD
            );
        }
        let layout = MemoryLayout::build(&Objects, four_state, MemoryLayoutMode::Packed)
            .with_backend_scratch(plan.scratch_bytes);
        let mut jit = JitEngine::new(
            layout.clone(),
            &CompileOptions {
                four_state,
                emit_triggers: false,
                cranelift: CraneliftOptions::fast_compile(),
            },
        )
        .unwrap();
        let entry = jit.compile_spilled_chunks(&plan, None, None, None).unwrap();
        let run: unsafe extern "C" fn(*mut u8) -> u64 = unsafe { std::mem::transmute(entry) };
        for initial in [0x35u8, 0x34] {
            let mut memory = vec![0u8; layout.merged_total_size];
            let offset = layout.offsets[&address()];
            memory[offset] = initial;
            if four_state {
                memory[offset + WIDTH / 8 + 1] = 0x80;
            }
            assert_eq!(unsafe { run(memory.as_mut_ptr()) }, 0);
            assert_eq!(memory[offset], if initial & 1 == 1 { initial } else { 7 });
            if four_state {
                let expected = if initial & 1 == 1 { 0x80 } else { 0 };
                assert_eq!(memory[offset + 1], expected);
                assert_eq!(memory[offset + WIDTH / 8 + 1], expected);
            }
        }
    }
}
