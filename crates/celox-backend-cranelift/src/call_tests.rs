//! Function units called through `SIRInstruction::Call`, compiled by hand-built
//! SIR: a recursive function, a wide four-state value through a frame, a
//! callee's error, and calls in split chunks.

use std::sync::Arc;

use celox_design::StateObjectId;
use celox_state_layout::{LayoutInput, LayoutSource, MemoryLayoutMode, StateObjectLayout};
use num_bigint::BigUint;

use crate::{
    AbsoluteAddr, BasicBlock, BinaryOp, BlockId, CompileOptions, CraneliftOptions, ExecutionUnit,
    FRAME_REGION, FrameLayout, FrameSlot, FunctionId, FunctionUnit, HashMap, HashSet, InstanceId,
    JitEngine, MemoryLayout, RegionedAbsoluteAddr, RegisterId, RegisterType, SIRInstruction,
    SIROffset, SIRTerminator, SIRValue, STABLE_REGION,
};

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
            call_stack: Some(crate::STACK_SIZE),
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
pub(crate) fn factorial(fail: bool) -> FunctionUnit<RegionedAbsoluteAddr> {
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

/// A unit that calls `fact(6)` in a frame at the stack pointer, stores the
/// result to `OUT`, a register defined before the call to `KEEP`, and then
/// sets `MARK`.
pub(crate) fn caller() -> ExecutionUnit<RegionedAbsoluteAddr> {
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
                SIRInstruction::Imm(reg(3), SIRValue::new(6u8)),
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
pub(crate) fn identity() -> FunctionUnit<RegionedAbsoluteAddr> {
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
pub(crate) fn wide_caller(value: &SIRValue) -> ExecutionUnit<RegionedAbsoluteAddr> {
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

pub(crate) fn wide_value() -> SIRValue {
    SIRValue::new_four_state(
        (BigUint::from(1u8) << 99u32) | (BigUint::from(0xabcdu32) << 40u32) | BigUint::from(5u8),
        (BigUint::from(1u8) << 70u32) | BigUint::from(4u8),
    )
}

fn options(four_state: bool) -> CompileOptions {
    CompileOptions {
        four_state,
        emit_triggers: false,
        cranelift: CraneliftOptions::fast_compile(),
    }
}

fn layout(four_state: bool) -> MemoryLayout {
    MemoryLayout::build(&Objects, four_state, MemoryLayoutMode::Packed)
}

/// Run compiled entry `entry` on a fresh state image and return its status
/// and the image.
fn run(entry: *const u8, layout: &MemoryLayout) -> (u64, Vec<u8>) {
    // The engine owns the code for the whole test; its ABI is (memory) -> u64.
    let run: unsafe extern "C" fn(*mut u8) -> u64 = unsafe { std::mem::transmute(entry) };
    let mut memory = vec![0u8; layout.merged_total_size];
    let status = unsafe { run(memory.as_mut_ptr()) };
    (status, memory)
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

fn check_factorial(layout: &MemoryLayout, status: u64, memory: &[u8]) {
    assert_eq!(status, 0);
    assert_eq!(word(layout, memory, OUT), 720);
    assert_eq!(word(layout, memory, KEEP), 0x1234);
    assert_eq!(word(layout, memory, SP), 0);
    assert_eq!(word(layout, memory, MARK), 1);
    assert_eq!(depth(layout, memory), 0);
}

#[test]
fn calls_a_recursive_function() {
    for four_state in [false, true] {
        let layout = layout(four_state);
        let mut jit = JitEngine::new(layout.clone(), &options(four_state)).unwrap();
        jit.compile_functions(&[factorial(false)]).unwrap();
        let entry = jit.compile_units(&[caller()], None, None, None).unwrap();
        let (status, memory) = run(entry, &layout);
        check_factorial(&layout, status, &memory);
    }
}

#[test]
fn a_callee_error_ends_the_caller() {
    for four_state in [false, true] {
        let layout = layout(four_state);
        let mut jit = JitEngine::new(layout.clone(), &options(four_state)).unwrap();
        jit.compile_functions(&[factorial(true)]).unwrap();
        let entry = jit.compile_units(&[caller()], None, None, None).unwrap();
        let (status, memory) = run(entry, &layout);
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
        let mut jit = JitEngine::new(layout.clone(), &options(four_state)).unwrap();
        jit.compile_functions(&[identity()]).unwrap();
        let entry = jit
            .compile_units(&[wide_caller(&value)], None, None, None)
            .unwrap();
        let (status, memory) = run(entry, &layout);
        assert_eq!(status, 0);
        let mask = if four_state {
            value.mask.clone()
        } else {
            BigUint::default()
        };
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
            assert_eq!(read(&memory, wide + plane, plane), mask);
            assert_eq!(
                read(&memory, pair + pair_plane, pair_plane),
                &mask | (&mask << 100u32)
            );
        }
    }
}

/// Run compiled entry `entry` on a state image that ends where an
/// inaccessible page starts, so an access past the image faults.
#[cfg(unix)]
fn run_at_page_end(entry: *const u8, layout: &MemoryLayout) -> (u64, Vec<u8>) {
    // SAFETY: the mapping is private to this test and unmapped below; the
    // engine owns the code for the whole test, whose ABI is (memory) -> u64.
    unsafe {
        let page = libc::sysconf(libc::_SC_PAGESIZE) as usize;
        let size = layout.merged_total_size.div_ceil(page) * page;
        let map = libc::mmap(
            std::ptr::null_mut(),
            size + page,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        );
        assert_ne!(map, libc::MAP_FAILED);
        let guard = (map as *mut u8).add(size);
        assert_eq!(libc::mprotect(guard.cast(), page, libc::PROT_NONE), 0);
        let memory = guard.sub(layout.merged_total_size);
        let run: unsafe extern "C" fn(*mut u8) -> u64 = std::mem::transmute(entry);
        let status = run(memory);
        let image = std::slice::from_raw_parts(memory, layout.merged_total_size).to_vec();
        libc::munmap(map, size + page);
        (status, image)
    }
}

#[cfg(unix)]
#[test]
fn reads_a_slot_at_the_end_of_the_stack_within_the_image() {
    // `last(x)` returns bits 1..100 of its 100-bit slot 1, which ends its
    // frame; the frame ends the stack, and the image ends after its guard.
    let function = FunctionUnit {
        unit: ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [block(
                0,
                vec![
                    SIRInstruction::Load(reg(0), slot(1), SIROffset::Static(1), 99),
                    store(slot(0), 99, 0),
                ],
                SIRTerminator::Return,
            )]
            .into_iter()
            .collect(),
            register_map: [(reg(0), bit(99))].into_iter().collect(),
        },
        frame: FrameLayout {
            size: 26,
            slots: vec![
                FrameSlot {
                    addr: slot(0),
                    offset: 0,
                    width: 99,
                    four_state: false,
                    handle: false,
                },
                FrameSlot {
                    addr: slot(1),
                    offset: 13,
                    width: 100,
                    four_state: false,
                    handle: false,
                },
            ],
        },
        effects: Default::default(),
    };
    let value = (BigUint::from(1u8) << 99u32) | BigUint::from(0b1011u8);
    let caller = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [block(
            0,
            vec![
                SIRInstruction::Imm(reg(0), SIRValue::new((crate::STACK_SIZE - 26) as u64)),
                SIRInstruction::Imm(reg(1), SIRValue::new(value.clone())),
                call(0, vec![(1, reg(1))], vec![0], 2),
                store(state(WIDE), 99, 2),
            ],
            SIRTerminator::Return,
        )]
        .into_iter()
        .collect(),
        register_map: [(reg(0), bit(32)), (reg(1), bit(100)), (reg(2), bit(99))]
            .into_iter()
            .collect(),
    };
    let layout = layout(false);
    let stack = layout.call_stack.unwrap();
    assert!(
        layout.merged_total_size
            <= stack.base_offset + stack.size + celox_state_layout::CALL_STACK_GUARD + 8,
        "the image ends just after the stack's guard"
    );
    let mut jit = JitEngine::new(layout.clone(), &options(false)).unwrap();
    jit.compile_functions(&[function]).unwrap();
    let entry = jit.compile_units(&[caller], None, None, None).unwrap();
    let (status, memory) = run_at_page_end(entry, &layout);
    assert_eq!(status, 0);
    let wide = layout.offsets[&object(WIDE)];
    assert_eq!(
        read(&memory, wide, 13) & ((BigUint::from(1u8) << 99u32) - 1u8),
        &value >> 1u32
    );
}

#[test]
fn calls_from_split_chunks() {
    for four_state in [false, true] {
        let unit = caller();
        // Split after every store, so the call runs in a chunk that receives
        // the stack pointer and the kept register from the one before: the
        // threshold is the cost of the costliest run of instructions between
        // stores.
        let mut threshold = 0;
        let mut cost = 0;
        for instruction in &unit.blocks[&BlockId(0)].instructions {
            cost +=
                crate::cost_model::estimate_clif_cost(instruction, &unit.register_map, four_state);
            if matches!(instruction, SIRInstruction::Store(..)) {
                threshold = threshold.max(cost);
                cost = 0;
            }
        }
        let threshold = threshold.max(cost);
        let chunks = crate::tail_call_split::split_with_threshold(
            std::slice::from_ref(&unit),
            four_state,
            threshold,
            threshold,
        )
        .expect("a split");
        assert!(chunks.len() > 2);
        let calling = chunks
            .iter()
            .position(|chunk| {
                chunk.units.iter().any(|unit| {
                    unit.blocks.values().any(|block| {
                        block
                            .instructions
                            .iter()
                            .any(|instruction| matches!(instruction, SIRInstruction::Call { .. }))
                    })
                })
            })
            .unwrap();
        assert!(calling > 0 && !chunks[calling].incoming_live_regs.is_empty());
        let layout = layout(four_state);
        let mut jit = JitEngine::new(layout.clone(), &options(four_state)).unwrap();
        jit.compile_functions(&[factorial(false)]).unwrap();
        let entry = jit.compile_chunks(&chunks, None, None, None).unwrap();
        let (status, memory) = run(entry, &layout);
        check_factorial(&layout, status, &memory);

        let plan = crate::tail_call_split::split_multi_block_with_threshold(
            std::slice::from_ref(&unit),
            four_state,
            threshold,
            threshold,
        )
        .expect("a spilled split");
        assert!(plan.chunks.len() > 1);
        let layout = self::layout(four_state).with_backend_scratch(plan.scratch_bytes);
        let mut jit = JitEngine::new(layout.clone(), &options(four_state)).unwrap();
        jit.compile_functions(&[factorial(false)]).unwrap();
        let entry = jit.compile_spilled_chunks(&plan, None, None, None).unwrap();
        let (status, memory) = run(entry, &layout);
        check_factorial(&layout, status, &memory);
    }
}

#[test]
fn units_without_calls_compile_alike_with_functions_present() {
    // Compiling the functions leaves the code of a unit that calls none of
    // them unchanged.
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
    let clif = |functions: &[FunctionUnit<RegionedAbsoluteAddr>]| {
        let mut jit = JitEngine::new(layout(true), &options(true)).unwrap();
        jit.compile_functions(functions).unwrap();
        let mut clif = String::new();
        jit.compile_units(std::slice::from_ref(&unit), None, Some(&mut clif), None)
            .unwrap();
        clif
    };
    let plain = clif(&[]);
    assert_eq!(plain, clif(&[factorial(false)]));
    assert!(!plain.contains("call"), "{plain}");
}
