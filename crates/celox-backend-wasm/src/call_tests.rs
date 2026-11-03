//! Function units called through `SIRInstruction::Call`, embedded in the
//! modules that call them, run on hand-built SIR: a recursive function, a
//! wide four-state value through a frame, and a callee's error.

use std::sync::Arc;

use celox_design::StateObjectId;
use celox_state_layout::{LayoutInput, LayoutSource, MemoryLayoutMode, StateObjectLayout};
use num_bigint::BigUint;

use celox_design::{FRAME_REGION, InstanceId, STABLE_REGION};
use celox_sir::{
    BasicBlock, BinaryOp, BlockId, ExecutionUnit, FrameLayout, FrameSlot, FunctionId, FunctionUnit,
    RegisterId, RegisterType, SIRInstruction, SIROffset, SIRTerminator, SIRValue,
};
use wasmtime::{Engine, Linker, Memory, Module as WasmtimeModule, Store};

use crate::{AbsoluteAddr, HashMap, HashSet, MemoryLayout, RegionedAbsoluteAddr};

/// The stack pointer, which call sites move around their calls.
const SP: u32 = 0;
/// Where the caller stores the call's result.
const OUT: u32 = 1;
/// Where the caller stores a register that lives across the call.
const KEEP: u32 = 2;
/// Where the caller stores after the call.
const MARK: u32 = 3;
/// A 100-bit four-state result.
const WIDE: u32 = 4;
/// A 200-bit four-state result, of two output slots.
const PAIR: u32 = 5;
/// The first frame slot address, in `FRAME_REGION`. A frame slot may share
/// its object identity with a state object, so the slots reuse the state's.
const SLOT: u32 = 0;

fn object(id: u32) -> AbsoluteAddr {
    AbsoluteAddr {
        instance_id: InstanceId(0),
        var_id: StateObjectId(id),
    }
}

fn state(id: u32) -> RegionedAbsoluteAddr {
    RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, object(id))
}

fn slot(id: u32) -> RegionedAbsoluteAddr {
    RegionedAbsoluteAddr::from_absolute_addr(FRAME_REGION, object(SLOT + id))
}

struct Objects;
impl LayoutSource<AbsoluteAddr> for Objects {
    fn layout_input(&self, _: MemoryLayoutMode) -> LayoutInput<AbsoluteAddr> {
        LayoutInput {
            state_objects: [
                (SP, 32, false),
                (OUT, 32, false),
                (KEEP, 32, false),
                (MARK, 8, false),
                (WIDE, 100, true),
                (PAIR, 200, true),
            ]
            .into_iter()
            .map(|(id, width, is_4state)| StateObjectLayout {
                address: object(id),
                width,
                is_4state,
            })
            .collect(),
            working_addresses: vec![],
            sparse_addresses: vec![],
            unpacked_arrays: HashMap::default(),
            requirements: Default::default(),
            ff_referenced_addresses: HashSet::default(),
            num_events: 0,
            runtime_event_sites: vec![],
            lane_writers: Default::default(),
            access_rank: Default::default(),
            call_stack: Some(celox_sir::STACK_SIZE),
        }
    }
}

fn reg(id: usize) -> RegisterId {
    RegisterId(id)
}

fn bit(width: usize) -> RegisterType {
    RegisterType::Bit {
        width,
        signed: false,
    }
}

fn block(
    id: usize,
    instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
    terminator: SIRTerminator,
) -> (BlockId, BasicBlock<RegionedAbsoluteAddr>) {
    (
        BlockId(id),
        BasicBlock {
            id: BlockId(id),
            params: vec![],
            instructions,
            terminator,
        },
    )
}

fn store(
    addr: RegionedAbsoluteAddr,
    bits: usize,
    src: usize,
) -> SIRInstruction<RegionedAbsoluteAddr> {
    SIRInstruction::Store(addr, SIROffset::Static(0), bits, reg(src), vec![], vec![])
}

fn load(
    dst: usize,
    addr: RegionedAbsoluteAddr,
    bits: usize,
) -> SIRInstruction<RegionedAbsoluteAddr> {
    SIRInstruction::Load(reg(dst), addr, SIROffset::Static(0), bits)
}

fn call(
    frame: usize,
    args: Vec<(usize, RegisterId)>,
    outputs: Vec<usize>,
    dst: usize,
) -> SIRInstruction<RegionedAbsoluteAddr> {
    SIRInstruction::Call {
        function: FunctionId(0),
        frame: reg(frame),
        args,
        outputs,
        dst: Some(reg(dst)),
        effects: Arc::default(),
    }
}

/// `fact(n)`: its frame holds `n` and the result, 32 bits each, and it calls
/// itself in a frame at the stack pointer, as an out-of-line call site does.
/// `n` keeps its value across the call. With `fail`, `fact(1)` raises error
/// 7 instead of returning.
fn factorial(fail: bool) -> FunctionUnit<RegionedAbsoluteAddr> {
    let unit = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [
            block(
                0,
                vec![
                    load(0, slot(0), 32),
                    SIRInstruction::Imm(reg(1), SIRValue::new(2u8)),
                    SIRInstruction::Binary(reg(2), reg(0), BinaryOp::LtU, reg(1)),
                ],
                SIRTerminator::Branch {
                    cond: reg(2),
                    true_block: (BlockId(1), vec![]),
                    false_block: (BlockId(2), vec![]),
                },
            ),
            block(
                1,
                vec![
                    SIRInstruction::Imm(reg(3), SIRValue::new(1u8)),
                    store(slot(1), 32, 3),
                ],
                if fail {
                    SIRTerminator::Error(7)
                } else {
                    SIRTerminator::Return
                },
            ),
            block(
                2,
                vec![
                    load(4, state(SP), 32),
                    SIRInstruction::Imm(reg(5), SIRValue::new(8u8)),
                    SIRInstruction::Binary(reg(6), reg(4), BinaryOp::Add, reg(5)),
                    store(state(SP), 32, 6),
                    SIRInstruction::Imm(reg(7), SIRValue::new(1u8)),
                    SIRInstruction::Binary(reg(8), reg(0), BinaryOp::Sub, reg(7)),
                    call(4, vec![(0, reg(8))], vec![1], 9),
                    store(state(SP), 32, 4),
                    SIRInstruction::Binary(reg(10), reg(0), BinaryOp::Mul, reg(9)),
                    store(slot(1), 32, 10),
                ],
                SIRTerminator::Return,
            ),
        ]
        .into_iter()
        .collect(),
        register_map: (0..=10)
            .map(|id| (reg(id), bit(if id == 2 { 1 } else { 32 })))
            .collect(),
    };
    unit.verify();
    FunctionUnit {
        unit,
        effects: Arc::default(),
        frame: FrameLayout {
            size: 8,
            slots: (0..2)
                .map(|index| FrameSlot {
                    addr: slot(index),
                    offset: index as usize * 4,
                    width: 32,
                    four_state: false,
                    handle: false,
                })
                .collect(),
        },
    }
}

/// A unit that calls `fact(n)` in a frame at the stack pointer, stores the
/// result to `OUT`, a register defined before the call to `KEEP`, and then
/// sets `MARK`.
fn caller_of(n: u32) -> ExecutionUnit<RegionedAbsoluteAddr> {
    let unit = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [block(
            0,
            vec![
                SIRInstruction::Imm(reg(5), SIRValue::new(0x1234u16)),
                load(0, state(SP), 32),
                SIRInstruction::Imm(reg(1), SIRValue::new(8u8)),
                SIRInstruction::Binary(reg(2), reg(0), BinaryOp::Add, reg(1)),
                store(state(SP), 32, 2),
                SIRInstruction::Imm(reg(3), SIRValue::new(n)),
                call(0, vec![(0, reg(3))], vec![1], 4),
                store(state(SP), 32, 0),
                store(state(OUT), 32, 4),
                store(state(KEEP), 32, 5),
                SIRInstruction::Imm(reg(6), SIRValue::new(1u8)),
                store(state(MARK), 8, 6),
            ],
            SIRTerminator::Return,
        )]
        .into_iter()
        .collect(),
        register_map: [(0, 32), (1, 32), (2, 32), (3, 32), (4, 32), (5, 32), (6, 8)]
            .into_iter()
            .map(|(id, width)| (reg(id), bit(width)))
            .collect(),
    };
    unit.verify();
    unit
}

/// `id(x)`: copies its 100-bit four-state slot 0 to slot 1.
fn identity() -> FunctionUnit<RegionedAbsoluteAddr> {
    let unit = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [block(
            0,
            vec![load(0, slot(0), 100), store(slot(1), 100, 0)],
            SIRTerminator::Return,
        )]
        .into_iter()
        .collect(),
        register_map: [(reg(0), RegisterType::Logic { width: 100 })]
            .into_iter()
            .collect(),
    };
    FunctionUnit {
        unit,
        effects: Arc::default(),
        frame: FrameLayout {
            // The second slot starts within a byte of the first's mask plane.
            size: 52,
            slots: (0..2)
                .map(|index| FrameSlot {
                    addr: slot(index),
                    offset: index as usize * 26,
                    width: 100,
                    four_state: true,
                    handle: false,
                })
                .collect(),
        },
    }
}

/// A unit that passes `value` to `id` and stores the result to `WIDE`, and
/// the concatenation of `id`'s slots 1 and 0 to `PAIR`.
fn wide_caller(value: &SIRValue) -> ExecutionUnit<RegionedAbsoluteAddr> {
    let logic = |width| RegisterType::Logic { width };
    let unit = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [block(
            0,
            vec![
                SIRInstruction::Imm(reg(0), SIRValue::new(16u8)),
                SIRInstruction::Imm(reg(1), value.clone()),
                call(0, vec![(0, reg(1))], vec![1], 2),
                store(state(WIDE), 100, 2),
                call(0, vec![(0, reg(1))], vec![1, 0], 3),
                store(state(PAIR), 200, 3),
            ],
            SIRTerminator::Return,
        )]
        .into_iter()
        .collect(),
        register_map: [
            (reg(0), bit(32)),
            (reg(1), logic(100)),
            (reg(2), logic(100)),
            (reg(3), logic(200)),
        ]
        .into_iter()
        .collect(),
    };
    unit.verify();
    unit
}

fn wide_value() -> SIRValue {
    SIRValue::new_four_state(
        (BigUint::from(1u8) << 99u32) | (BigUint::from(0xabcdu32) << 40u32) | BigUint::from(5u8),
        (BigUint::from(1u8) << 70u32) | BigUint::from(4u8),
    )
}

fn caller() -> ExecutionUnit<RegionedAbsoluteAddr> {
    caller_of(6)
}

fn layout(four_state: bool) -> MemoryLayout {
    MemoryLayout::build(&Objects, four_state, MemoryLayoutMode::Packed)
}

/// Compile `unit` calling `function` and run it on a fresh state image;
/// return its status and the image.
fn run(
    unit: &ExecutionUnit<RegionedAbsoluteAddr>,
    function: FunctionUnit<RegionedAbsoluteAddr>,
    layout: &MemoryLayout,
    four_state: bool,
) -> (i64, Vec<u8>) {
    try_run(unit, function, layout, four_state, 0).expect("execute Wasm")
}

/// [`run`] with the stack pointer starting at `sp`, which fails when the
/// code traps.
fn try_run(
    unit: &ExecutionUnit<RegionedAbsoluteAddr>,
    function: FunctionUnit<RegionedAbsoluteAddr>,
    layout: &MemoryLayout,
    four_state: bool,
    sp: u32,
) -> wasmtime::Result<(i64, Vec<u8>)> {
    let wasm = crate::compile_units_calling(
        std::slice::from_ref(unit),
        &[function],
        layout,
        four_state,
        false,
    );
    let engine = Engine::default();
    let module = WasmtimeModule::new(&engine, &wasm.bytes).expect("compile Wasm module");
    let mut store = Store::new(&engine, ());
    let pages = layout.merged_total_size.div_ceil(65536) as u32;
    let memory = Memory::new(&mut store, wasmtime::MemoryType::new(pages, None))
        .expect("create Wasm memory");
    let mut linker = Linker::new(&engine);
    linker
        .define(&mut store, "env", "memory", memory)
        .expect("define Wasm memory");
    let instance = linker
        .instantiate(&mut store, &module)
        .expect("instantiate Wasm module");
    let run = instance
        .get_typed_func::<(), i64>(&mut store, "run")
        .expect("find Wasm entry point");
    let offset = layout.offsets[&object(SP)];
    memory.data_mut(&mut store)[offset..offset + 4].copy_from_slice(&sp.to_le_bytes());
    let status = run.call(&mut store, ())?;
    Ok((status, memory.data(&store).to_vec()))
}

fn read(memory: &[u8], offset: usize, bytes: usize) -> BigUint {
    BigUint::from_bytes_le(&memory[offset..offset + bytes])
}

fn word(layout: &MemoryLayout, memory: &[u8], id: u32) -> u64 {
    let bytes = layout.plane_size(&object(id));
    read(memory, layout.offsets[&object(id)], bytes)
        .to_u64_digits()
        .first()
        .copied()
        .unwrap_or(0)
}

/// The call depth, which every call restores.
fn depth(layout: &MemoryLayout, memory: &[u8]) -> u64 {
    let offset = layout.call_stack.unwrap().depth_offset;
    u64::from_le_bytes(memory[offset..offset + 8].try_into().unwrap())
}

#[test]
fn calls_a_recursive_function() {
    for four_state in [false, true] {
        let layout = layout(four_state);
        let (status, memory) = run(&caller(), factorial(false), &layout, four_state);
        assert_eq!(status, 0);
        assert_eq!(word(&layout, &memory, OUT), 720);
        assert_eq!(word(&layout, &memory, KEEP), 0x1234);
        assert_eq!(word(&layout, &memory, SP), 0);
        assert_eq!(word(&layout, &memory, MARK), 1);
        assert_eq!(depth(&layout, &memory), 0);
    }
}

#[test]
fn a_callee_error_ends_the_caller() {
    for four_state in [false, true] {
        let layout = layout(four_state);
        let (status, memory) = run(&caller(), factorial(true), &layout, four_state);
        assert_eq!(status, 7);
        // The caller's stores after the call do not happen.
        assert_eq!(word(&layout, &memory, MARK), 0);
        assert_eq!(word(&layout, &memory, OUT), 0);
        assert_eq!(depth(&layout, &memory), 0);
    }
}

#[test]
fn passes_wide_four_state_values_through_a_frame() {
    let value = wide_value();
    for four_state in [false, true] {
        let layout = layout(four_state);
        let (status, memory) = run(&wide_caller(&value), identity(), &layout, four_state);
        assert_eq!(status, 0);
        let wide = layout.offsets[&object(WIDE)];
        let plane = layout.plane_size(&object(WIDE));
        assert_eq!(read(&memory, wide, plane), value.payload);
        let pair = layout.offsets[&object(PAIR)];
        let pair_plane = layout.plane_size(&object(PAIR));
        assert_eq!(
            read(&memory, pair, pair_plane),
            &value.payload | (&value.payload << 100u32)
        );
        if four_state {
            assert_eq!(read(&memory, wide + plane, plane), value.mask);
            assert_eq!(
                read(&memory, pair + pair_plane, pair_plane),
                &value.mask | (&value.mask << 100u32)
            );
        }
    }
}

#[test]
fn a_module_that_calls_nothing_embeds_no_function() {
    // The functions are embedded only where a unit calls them: a module whose
    // units call none is byte for byte the one built without functions.
    let unit = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [block(
            0,
            vec![load(0, state(SP), 32), store(state(OUT), 32, 0)],
            SIRTerminator::Return,
        )]
        .into_iter()
        .collect(),
        register_map: [(reg(0), bit(32))].into_iter().collect(),
    };
    for four_state in [false, true] {
        let layout = layout(four_state);
        let units = std::slice::from_ref(&unit);
        assert_eq!(
            crate::compile_units_calling(units, &[factorial(false)], &layout, four_state, true)
                .bytes,
            crate::compile_units(units, &layout, four_state, true).bytes
        );
    }
}

#[test]
fn traps_past_the_call_depth_and_the_stack() {
    let layout = layout(false);
    let depth = celox_sir::MAX_CALL_DEPTH as u32;
    // `fact(n)` nests `n` calls.
    let (status, memory) = try_run(&caller_of(depth), factorial(false), &layout, false, 0).unwrap();
    assert_eq!(status, 0);
    assert_eq!(word(&layout, &memory, MARK), 1);
    assert!(try_run(&caller_of(depth + 1), factorial(false), &layout, false, 0).is_err());
    // The last frame that fits ends the stack; one byte further does not.
    let last = (celox_sir::STACK_SIZE - 8) as u32;
    let (status, memory) = try_run(&caller_of(1), factorial(false), &layout, false, last).unwrap();
    assert_eq!((status, word(&layout, &memory, OUT)), (0, 1));
    assert!(try_run(&caller_of(1), factorial(false), &layout, false, last + 1).is_err());
}
