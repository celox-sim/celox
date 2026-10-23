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

fn simple_unit(value: u8, add: bool) -> ExecutionUnit<RegionedAbsoluteAddr> {
    let mut instructions = vec![SIRInstruction::Imm(RegisterId(0), SIRValue::new(value))];
    let result = if add {
        instructions.push(SIRInstruction::Load(
            RegisterId(1),
            region(),
            SIROffset::Static(0),
            WIDTH,
        ));
        instructions.push(SIRInstruction::Binary(
            RegisterId(2),
            RegisterId(1),
            BinaryOp::Add,
            RegisterId(0),
        ));
        RegisterId(2)
    } else {
        RegisterId(0)
    };
    instructions.push(SIRInstruction::Store(
        region(),
        SIROffset::Static(0),
        WIDTH,
        result,
        vec![],
        vec![],
    ));
    ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [(
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: vec![],
                instructions,
                terminator: SIRTerminator::Return,
            },
        )]
        .into_iter()
        .collect(),
        register_map: (0..3)
            .map(|i| (RegisterId(i), RegisterType::Logic { width: WIDTH }))
            .collect(),
    }
}

// Explicit block parameters force SinglePass to allocate distinct phi spill
// slots even though the peak live set is only 128 values. Wide parameters also
// exercise the cost model's expansion into individual 64-bit CLIF values.
fn phi_chain() -> ExecutionUnit<RegionedAbsoluteAddr> {
    let mut blocks = HashMap::default();
    let mut register_map = HashMap::default();
    let mut next = 0;
    let mut new_reg = || {
        let reg = RegisterId(next);
        next += 1;
        register_map.insert(reg, RegisterType::Logic { width: WIDTH });
        reg
    };
    let input = new_reg();
    let mut instructions = vec![SIRInstruction::Load(
        input,
        region(),
        SIROffset::Static(0),
        WIDTH,
    )];
    let mut previous = Vec::new();
    for i in 0..128u32 {
        let constant = new_reg();
        let result = new_reg();
        instructions.push(SIRInstruction::Imm(constant, SIRValue::new(i)));
        instructions.push(SIRInstruction::Binary(
            result,
            input,
            BinaryOp::Add,
            constant,
        ));
        previous.push(result);
    }
    blocks.insert(
        BlockId(0),
        BasicBlock {
            id: BlockId(0),
            params: vec![],
            instructions,
            terminator: SIRTerminator::Jump(BlockId(1), previous),
        },
    );
    for b in 1..80 {
        let params: Vec<_> = (0..128).map(|_| new_reg()).collect();
        let (instructions, terminator) = if b == 79 {
            (
                params
                    .iter()
                    .map(|&reg| {
                        SIRInstruction::Store(
                            region(),
                            SIROffset::Static(0),
                            WIDTH,
                            reg,
                            vec![],
                            vec![],
                        )
                    })
                    .collect(),
                SIRTerminator::Return,
            )
        } else {
            (vec![], SIRTerminator::Jump(BlockId(b + 1), params.clone()))
        };
        blocks.insert(
            BlockId(b),
            BasicBlock {
                id: BlockId(b),
                params,
                instructions,
                terminator,
            },
        );
    }
    ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks,
        register_map,
    }
}

#[test]
fn wide_phi_spills_are_split_and_all_execution_units_run_in_order() {
    for four_state in [false, true] {
        let units = [
            simple_unit(17, false),
            phi_chain(),
            simple_unit(1, true),
            phi_chain(),
            simple_unit(1, true),
        ];
        let plan = tail_call_split::split_if_needed_spilled(&units, four_state)
            .expect("phi spill budget must trigger splitting");
        assert_eq!(plan.entry_chunks.len(), units.len());
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
        let entry = jit
            .compile_spilled_chunks(&plan, None, None, None)
            .expect("every generated chunk must fit the actual frame limit");
        let run: unsafe extern "C" fn(*mut u8) -> u64 = unsafe { std::mem::transmute(entry) };
        let mut memory = vec![0u8; layout.merged_total_size];
        assert_eq!(unsafe { run(memory.as_mut_ptr()) }, 0);
        let offset = layout.offsets[&address()];
        assert_eq!(
            u64::from_le_bytes(memory[offset..offset + 8].try_into().unwrap()),
            273
        );
        assert!(
            memory[offset + 8..offset + WIDTH / 8]
                .iter()
                .all(|&byte| byte == 0)
        );
        if four_state {
            assert!(
                memory[offset + WIDTH / 8..offset + WIDTH / 4]
                    .iter()
                    .all(|&byte| byte == 0)
            );
        }
    }
}

#[test]
fn error_from_split_execution_unit_skips_later_units() {
    for four_state in [false, true] {
        let mut chain = phi_chain();
        chain.blocks.get_mut(&BlockId(79)).unwrap().terminator = SIRTerminator::Error(13);
        let units = [simple_unit(17, false), chain, simple_unit(1, true)];
        let plan = tail_call_split::split_if_needed_spilled(&units, four_state).unwrap();
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
        let mut memory = vec![0u8; layout.merged_total_size];
        assert_eq!(unsafe { run(memory.as_mut_ptr()) }, 13);
        let offset = layout.offsets[&address()];
        assert_eq!(
            u64::from_le_bytes(memory[offset..offset + 8].try_into().unwrap()),
            144
        );
    }
}
