//! A branch on the equality of operands wider than 64 bits takes the side
//! the comparison gives (#2102).
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

/// Branch on `a op b` for 128-bit `a` and `b`, storing 1 when it holds and 2
/// otherwise.
fn branch_on(op: BinaryOp, a: &BigUint, b: &BigUint) -> u8 {
    let address = |id| AbsoluteAddr {
        instance_id: InstanceId(0),
        var_id: StateObjectId(id),
    };
    let region = |id| RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, address(id));
    let layout = MemoryLayout::build(
        &Objects(
            [(0, 128), (1, 128), (2, 8)]
                .into_iter()
                .map(|(id, width)| StateObjectLayout {
                    address: address(id),
                    width,
                    is_4state: false,
                })
                .collect(),
        ),
        false,
        MemoryLayoutMode::Packed,
    );
    let store = |value: u8, register: usize| BasicBlock {
        id: BlockId(value as usize),
        params: vec![],
        instructions: vec![
            SIRInstruction::Imm(RegisterId(register), SIRValue::new(value)),
            SIRInstruction::Store(
                region(2),
                SIROffset::Static(0),
                8,
                RegisterId(register),
                vec![],
                vec![],
            ),
        ],
        terminator: SIRTerminator::Return,
    };
    let unit = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [
            (
                BlockId(0),
                BasicBlock {
                    id: BlockId(0),
                    params: vec![],
                    instructions: vec![
                        SIRInstruction::Load(RegisterId(0), region(0), SIROffset::Static(0), 128),
                        SIRInstruction::Load(RegisterId(1), region(1), SIROffset::Static(0), 128),
                        SIRInstruction::Binary(RegisterId(2), RegisterId(0), op, RegisterId(1)),
                    ],
                    terminator: SIRTerminator::Branch {
                        cond: RegisterId(2),
                        true_block: (BlockId(1), vec![]),
                        false_block: (BlockId(2), vec![]),
                    },
                },
            ),
            (BlockId(1), store(1, 3)),
            (BlockId(2), store(2, 4)),
        ]
        .into_iter()
        .collect(),
        register_map: [(0, 128), (1, 128), (2, 1), (3, 8), (4, 8)]
            .into_iter()
            .map(|(id, width)| {
                (
                    RegisterId(id),
                    RegisterType::Bit {
                        width,
                        signed: false,
                    },
                )
            })
            .collect(),
    };
    let mut jit = JitEngine::new(
        layout.clone(),
        &CompileOptions {
            four_state: false,
            emit_triggers: false,
            cranelift: Default::default(),
        },
    )
    .unwrap();
    let entry = jit.compile_units(&[unit], None, None, None).unwrap();
    // The JIT owns this function for the entire test; its ABI is (memory) -> u64.
    let run: unsafe extern "C" fn(*mut u8) -> u64 = unsafe { std::mem::transmute(entry) };
    let mut memory = vec![0u8; layout.merged_total_size];
    for (id, value) in [(0, a), (1, b)] {
        let offset = layout.offsets[&address(id)];
        let bytes = value.to_bytes_le();
        memory[offset..offset + bytes.len()].copy_from_slice(&bytes);
    }
    assert_eq!(unsafe { run(memory.as_mut_ptr()) }, 0);
    memory[layout.offsets[&address(2)]]
}

#[test]
fn branches_on_wide_equality_take_the_compared_side() {
    let high: BigUint = BigUint::from(1u8) << 100usize;
    for (a, b) in [
        (high.clone(), high.clone()),
        (high.clone(), &high + 1u8),
        (high.clone(), BigUint::from(0u8)),
        (BigUint::from(5u8), BigUint::from(5u8)),
    ] {
        let equal = a == b;
        let side = |holds: bool| if holds { 1 } else { 2 };
        assert_eq!(
            branch_on(BinaryOp::Eq, &a, &b),
            side(equal),
            "{a:x} == {b:x}"
        );
        assert_eq!(
            branch_on(BinaryOp::Ne, &a, &b),
            side(!equal),
            "{a:x} != {b:x}"
        );
    }
}
