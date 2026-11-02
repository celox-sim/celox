//! Function units called through `SIRInstruction::Call` on AArch64, built
//! from hand-written SIR as the x86-64 and Cranelift call tests are. Every
//! host compiles them and checks the emitted code; AArch64 hosts also run
//! them: a recursive function with a register live across the call,
//! recursion whose activations spill and make extern calls, wide four-state
//! values through a frame, a callee's error, a slot at the very end of the
//! stack, and the guards' traps.

use std::sync::Arc;

use celox_design::{FRAME_REGION, InstanceId, StateObjectId};
use celox_sir::{FrameLayout, FrameSlot, FunctionId, FunctionUnit};
use celox_state_layout::{LayoutInput, LayoutSource, MemoryLayoutMode, StateObjectLayout};
use num_bigint::BigUint;

use super::*;
use crate::{
    AbsoluteAddr, BasicBlock, BinaryOp, ExecutionUnit, FunctionCalls, MemoryLayout,
    RegionedAbsoluteAddr, RegisterId, RegisterType, SIRInstruction, SIROffset, SIRTerminator,
    SIRValue, STABLE_REGION,
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
            unpacked_arrays: crate::HashMap::default(),
            requirements: Default::default(),
            ff_referenced_addresses: crate::HashSet::default(),
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
) -> (crate::BlockId, BasicBlock<RegionedAbsoluteAddr>) {
    (
        crate::BlockId(id),
        BasicBlock {
            id: crate::BlockId(id),
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

fn imm(dst: usize, value: u64) -> SIRInstruction<RegionedAbsoluteAddr> {
    SIRInstruction::Imm(reg(dst), SIRValue::new(value))
}

fn binary(
    dst: usize,
    lhs: usize,
    op: BinaryOp,
    rhs: usize,
) -> SIRInstruction<RegionedAbsoluteAddr> {
    SIRInstruction::Binary(reg(dst), reg(lhs), op, reg(rhs))
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

/// A frame of `count` 32-bit two-state slots.
fn words(count: u32) -> FrameLayout<RegionedAbsoluteAddr> {
    FrameLayout {
        size: count as usize * 4,
        slots: (0..count)
            .map(|index| FrameSlot {
                addr: slot(index),
                offset: index as usize * 4,
                width: 32,
                four_state: false,
                handle: false,
            })
            .collect(),
    }
}

/// `fact(n)`: its frame holds `n` and the result, 32 bits each, and it calls
/// itself in a frame at the stack pointer, as an out-of-line call site does.
/// `n` keeps its value across the call. With `fail`, `fact(1)` raises error
/// 7 instead of returning.
fn factorial(fail: bool) -> FunctionUnit<RegionedAbsoluteAddr> {
    let unit = ExecutionUnit {
        entry_block_id: crate::BlockId(0),
        blocks: [
            block(
                0,
                vec![
                    load(0, slot(0), 32),
                    imm(1, 2),
                    binary(2, 0, BinaryOp::LtU, 1),
                ],
                SIRTerminator::Branch {
                    cond: reg(2),
                    true_block: (crate::BlockId(1), vec![]),
                    false_block: (crate::BlockId(2), vec![]),
                },
            ),
            block(
                1,
                vec![imm(3, 1), store(slot(1), 32, 3)],
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
                    imm(5, 8),
                    binary(6, 4, BinaryOp::Add, 5),
                    store(state(SP), 32, 6),
                    imm(7, 1),
                    binary(8, 0, BinaryOp::Sub, 7),
                    call(4, vec![(0, reg(8))], vec![1], 9),
                    store(state(SP), 32, 4),
                    binary(10, 0, BinaryOp::Mul, 9),
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
        frame: words(2),
    }
}

/// A unit that calls `fact(6)` in a frame at the stack pointer, stores the
/// result to `OUT`, a register defined before the call to `KEEP`, and then
/// sets `MARK`.
fn caller() -> ExecutionUnit<RegionedAbsoluteAddr> {
    caller_of(6)
}

fn caller_of(argument: u64) -> ExecutionUnit<RegionedAbsoluteAddr> {
    let unit = ExecutionUnit {
        entry_block_id: crate::BlockId(0),
        blocks: [block(
            0,
            vec![
                imm(5, 0x1234),
                load(0, state(SP), 32),
                imm(1, 8),
                binary(2, 0, BinaryOp::Add, 1),
                store(state(SP), 32, 2),
                imm(3, argument),
                call(0, vec![(0, reg(3))], vec![1], 4),
                store(state(SP), 32, 0),
                store(state(OUT), 32, 4),
                store(state(KEEP), 32, 5),
                imm(6, 1),
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
        entry_block_id: crate::BlockId(0),
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
        entry_block_id: crate::BlockId(0),
        blocks: [block(
            0,
            vec![
                imm(0, 16),
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

fn layout(four_state: bool) -> MemoryLayout {
    MemoryLayout::build(&Objects, four_state, MemoryLayoutMode::Packed)
}

/// `mov x29, x1`, with which a function unit takes its frame's address.
const TAKE_FRAME: u32 = 0xaa01_03fd;
/// `brk #1`, where a failed call guard stops.
const CALL_TRAP: u32 = 0xd420_0020;

fn words_of(code: &[u8]) -> Vec<u32> {
    code.as_chunks::<4>()
        .0
        .iter()
        .map(|bytes| u32::from_le_bytes(*bytes))
        .collect()
}

/// The code of a program's function units and one calling unit.
#[cfg_attr(not(target_arch = "aarch64"), allow(dead_code))]
struct Compiled {
    functions: Vec<EmitResult>,
    caller: EmitResult,
    first_slot: u32,
}

fn compile(
    functions: &[FunctionUnit<RegionedAbsoluteAddr>],
    unit: &ExecutionUnit<RegionedAbsoluteAddr>,
    layout: &MemoryLayout,
    externs: usize,
) -> Compiled {
    let frames = functions
        .iter()
        .map(|function| function.frame.clone())
        .collect::<Vec<_>>();
    let calls = |function| FunctionCalls {
        frames: &frames,
        first_slot: externs as u32,
        function,
    };
    let semantic = layout.merged_total_size + layout.triggered_bits_total_size;
    let functions = functions
        .iter()
        .enumerate()
        .map(|(index, function)| {
            let emitted = emit_prepared_eu_with_functions(
                &function.unit,
                layout,
                layout.four_state,
                "function",
                false,
                None,
                Some(calls(Some(FunctionId(index as u32)))),
                None,
                || false,
            )
            .unwrap();
            // A function unit's arena is on the machine stack, and it takes
            // the frame its caller passes.
            assert_eq!(emitted.required_state_size as usize, semantic);
            assert!(words_of(&emitted.code[..emitted.text_size]).contains(&TAKE_FRAME));
            emitted
        })
        .collect();
    let caller = emit_prepared_eu_with_functions(
        unit,
        layout,
        layout.four_state,
        "caller",
        false,
        None,
        Some(calls(None)),
        None,
        || false,
    )
    .unwrap();
    Compiled {
        functions,
        caller,
        first_slot: externs as u32,
    }
}

impl Compiled {
    fn calls_guarded(&self) -> bool {
        words_of(&self.caller.code[..self.caller.text_size]).contains(&CALL_TRAP)
    }
}

#[cfg(target_arch = "aarch64")]
mod run {
    use super::*;
    use crate::jit_mem::JitCode;

    /// A compiled program loaded into executable memory, with the extern
    /// address table its calls use: the externs first, then the functions.
    pub(super) struct Program {
        _code: Vec<JitCode>,
        pub(super) table: Vec<usize>,
        entry: unsafe extern "C" fn(*mut u8) -> i64,
        pub(super) state_size: usize,
    }

    impl Program {
        pub(super) fn load(compiled: &Compiled, externs: &[usize]) -> Self {
            assert_eq!(compiled.first_slot as usize, externs.len());
            let mut table = externs.to_vec();
            let mut code = Vec::new();
            let mut state_size = compiled.caller.required_state_size as usize;
            for function in &compiled.functions {
                let jit = JitCode::new(&function.code).unwrap();
                table.push(jit.fn_ptr as usize);
                state_size = state_size.max(function.required_state_size as usize);
                code.push(jit);
            }
            let jit = JitCode::new(&compiled.caller.code).unwrap();
            let entry = jit.fn_ptr;
            code.push(jit);
            Self {
                _code: code,
                table,
                entry,
                state_size,
            }
        }

        /// # Safety
        ///
        /// `memory` must hold at least `self.state_size` writable bytes.
        pub(super) unsafe fn run_on(&self, memory: *mut u8) -> i64 {
            // SAFETY: the header word holds the table's address, which
            // outlives the run, and the caller provides the complete image.
            unsafe {
                memory
                    .add(celox_state_layout::STATE_HEADER_EXTERN_FUNCTIONS_ADDR_OFFSET)
                    .cast::<u64>()
                    .write_unaligned(self.table.as_ptr() as u64);
                (self.entry)(memory)
            }
        }

        pub(super) fn run_with(&self, prepare: impl FnOnce(&mut [u8])) -> (i64, Vec<u8>) {
            let mut words = vec![0u64; self.state_size.div_ceil(8) + 1];
            // SAFETY: the words are the program's complete state image.
            let memory = unsafe {
                std::slice::from_raw_parts_mut(words.as_mut_ptr().cast::<u8>(), words.len() * 8)
            };
            prepare(memory);
            // SAFETY: `memory` holds the program's complete state image.
            let status = unsafe { self.run_on(memory.as_mut_ptr()) };
            (status, memory.to_vec())
        }

        pub(super) fn run(&self) -> (i64, Vec<u8>) {
            self.run_with(|_| {})
        }
    }

    /// Run `program` in a child process and return the signal that ended it.
    #[cfg(target_os = "linux")]
    pub(super) fn signal_of(program: &Program, prepare: impl FnOnce(&mut [u8])) -> Option<i32> {
        let mut words = vec![0u64; program.state_size.div_ceil(8) + 1];
        // SAFETY: the words are the program's complete state image.
        let memory = unsafe {
            std::slice::from_raw_parts_mut(words.as_mut_ptr().cast::<u8>(), words.len() * 8)
        };
        prepare(memory);
        // SAFETY: the child only runs the generated code on its copy of the
        // image and exits without unwinding or touching the parent's threads.
        unsafe {
            let child = libc::fork();
            assert!(child >= 0, "fork failed");
            if child == 0 {
                program.run_on(memory.as_mut_ptr());
                libc::_exit(0);
            }
            let mut status = 0;
            assert_eq!(libc::waitpid(child, &mut status, 0), child);
            libc::WIFSIGNALED(status).then(|| libc::WTERMSIG(status))
        }
    }
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

#[cfg_attr(not(target_arch = "aarch64"), allow(dead_code))]
fn check_factorial(layout: &MemoryLayout, status: i64, memory: &[u8]) {
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
        let compiled = compile(&[factorial(false)], &caller(), &layout, 0);
        assert!(compiled.calls_guarded());
        #[cfg(target_arch = "aarch64")]
        {
            let (status, memory) = run::Program::load(&compiled, &[]).run();
            check_factorial(&layout, status, &memory);
        }
    }
}

#[test]
fn a_callee_error_ends_the_caller() {
    for four_state in [false, true] {
        let layout = layout(four_state);
        let compiled = compile(&[factorial(true)], &caller(), &layout, 0);
        #[cfg(target_arch = "aarch64")]
        {
            let (status, memory) = run::Program::load(&compiled, &[]).run();
            assert_eq!(status, 7);
            // The caller's stores after the call do not happen.
            assert_eq!(word(&layout, &memory, MARK), 0);
            assert_eq!(word(&layout, &memory, OUT), 0);
            assert_eq!(depth(&layout, &memory), 0);
        }
        let _ = compiled;
    }
}

#[test]
fn passes_wide_four_state_values_through_a_frame() {
    let value = wide_value();
    for four_state in [false, true] {
        let layout = layout(four_state);
        let compiled = compile(&[identity()], &wide_caller(&value), &layout, 0);
        #[cfg(target_arch = "aarch64")]
        {
            let (status, memory) = run::Program::load(&compiled, &[]).run();
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
                let mask = value.mask.clone();
                assert_eq!(read(&memory, wide + plane, plane), mask);
                assert_eq!(
                    read(&memory, pair + pair_plane, pair_plane),
                    &mask | (&mask << 100u32)
                );
            }
        }
        let _ = compiled;
    }
}

/// How many values `spilling` keeps live across its recursive call.
const LIVE: u64 = 24;

/// `sum(n)`: zero when `n` is zero, else `sum(n - 1)` plus `LIVE` distinct
/// values derived from `n`, all computed before the recursive call and used
/// after it, so that every activation spills them.
fn spilling() -> FunctionUnit<RegionedAbsoluteAddr> {
    // Registers: 0 = n, 1 = zero, 2 = n == 0, 3 = sp, 4 = 8, 5 = sp + 8,
    // 6 = 1, 7 = n - 1, 8 = result of the call, 10.. = the live values and
    // their sums.
    let live = |index: u64| 10 + 2 * index as usize;
    let mut body = vec![
        load(3, state(SP), 32),
        imm(4, 8),
        binary(5, 3, BinaryOp::Add, 4),
        store(state(SP), 32, 5),
        imm(6, 1),
        binary(7, 0, BinaryOp::Sub, 6),
    ];
    for index in 0..LIVE {
        // value = n * (index + 3) + index, from a constant in the register
        // after it.
        body.push(imm(live(index) + 1, index + 3));
        body.push(binary(live(index), 0, BinaryOp::Mul, live(index) + 1));
    }
    body.push(call(3, vec![(0, reg(7))], vec![1], 8));
    body.push(store(state(SP), 32, 3));
    let mut total = 8;
    let mut next = live(LIVE);
    for index in 0..LIVE {
        body.push(binary(next, total, BinaryOp::Add, live(index)));
        body.push(imm(next + 1, index));
        body.push(binary(next + 2, next, BinaryOp::Add, next + 1));
        total = next + 2;
        next += 3;
    }
    body.push(store(slot(1), 32, total));
    let registers = next;
    let unit = ExecutionUnit {
        entry_block_id: crate::BlockId(0),
        blocks: [
            block(
                0,
                vec![
                    load(0, slot(0), 32),
                    imm(1, 0),
                    binary(2, 0, BinaryOp::Eq, 1),
                ],
                SIRTerminator::Branch {
                    cond: reg(2),
                    true_block: (crate::BlockId(1), vec![]),
                    false_block: (crate::BlockId(2), vec![]),
                },
            ),
            block(1, vec![store(slot(1), 32, 1)], SIRTerminator::Return),
            block(2, body, SIRTerminator::Return),
        ]
        .into_iter()
        .collect(),
        register_map: (0..registers)
            .map(|id| (reg(id), bit(if id == 2 { 1 } else { 32 })))
            .collect(),
    };
    unit.verify();
    FunctionUnit {
        unit,
        effects: Arc::default(),
        frame: words(2),
    }
}

fn expected_sum(n: u64) -> u64 {
    (1..=n)
        .map(|n| (0..LIVE).map(|index| n * (index + 3) + index).sum::<u64>())
        .sum::<u64>()
        & 0xffff_ffff
}

#[test]
fn recursive_activations_keep_their_own_spills() {
    for four_state in [false, true] {
        let layout = layout(four_state);
        let compiled = compile(&[spilling()], &caller_of(9), &layout, 0);
        #[cfg(target_arch = "aarch64")]
        {
            let (status, memory) = run::Program::load(&compiled, &[]).run();
            assert_eq!(status, 0);
            assert_eq!(word(&layout, &memory, OUT), expected_sum(9));
            assert_eq!(word(&layout, &memory, KEEP), 0x1234);
            assert_eq!(word(&layout, &memory, SP), 0);
            assert_eq!(depth(&layout, &memory), 0);
        }
        let _ = (compiled, expected_sum(0));
    }
}

/// Returns `x * 3 + 1`.
#[cfg_attr(not(target_arch = "aarch64"), allow(dead_code))]
extern "C" fn triple(x: u64) -> u64 {
    x.wrapping_mul(3).wrapping_add(1)
}

/// `ext(n)`: zero when `n` is zero, else `ext(n - 1) + triple(n) + n`, with
/// extern function 0 as `triple`, called after the recursive call so that
/// each activation's extern call uses its own argument area while its
/// callers' are live.
fn extern_calling() -> FunctionUnit<RegionedAbsoluteAddr> {
    let unit = ExecutionUnit {
        entry_block_id: crate::BlockId(0),
        blocks: [
            block(
                0,
                vec![
                    load(0, slot(0), 32),
                    imm(1, 0),
                    binary(2, 0, BinaryOp::Eq, 1),
                ],
                SIRTerminator::Branch {
                    cond: reg(2),
                    true_block: (crate::BlockId(1), vec![]),
                    false_block: (crate::BlockId(2), vec![]),
                },
            ),
            block(1, vec![store(slot(1), 32, 1)], SIRTerminator::Return),
            block(
                2,
                vec![
                    load(3, state(SP), 32),
                    imm(4, 8),
                    binary(5, 3, BinaryOp::Add, 4),
                    store(state(SP), 32, 5),
                    imm(6, 1),
                    binary(7, 0, BinaryOp::Sub, 6),
                    call(3, vec![(0, reg(7))], vec![1], 8),
                    store(state(SP), 32, 3),
                    SIRInstruction::ExternCall {
                        dst: Some(reg(9)),
                        func: 0,
                        args: vec![reg(0)],
                    },
                    binary(10, 8, BinaryOp::Add, 9),
                    binary(11, 10, BinaryOp::Add, 0),
                    store(slot(1), 32, 11),
                ],
                SIRTerminator::Return,
            ),
        ]
        .into_iter()
        .collect(),
        register_map: (0..=11)
            .map(|id| (reg(id), bit(if id == 2 { 1 } else { 32 })))
            .collect(),
    };
    unit.verify();
    FunctionUnit {
        unit,
        effects: Arc::default(),
        frame: words(2),
    }
}

#[test]
fn recursive_activations_make_extern_calls() {
    for four_state in [false, true] {
        let layout = layout(four_state);
        let compiled = compile(&[extern_calling()], &caller_of(10), &layout, 1);
        #[cfg(target_arch = "aarch64")]
        {
            let program = run::Program::load(&compiled, &[triple as *const () as usize]);
            let (status, memory) = program.run();
            assert_eq!(status, 0);
            let expected = (1..=10u64).map(|n| n * 3 + 1 + n).sum::<u64>();
            assert_eq!(word(&layout, &memory, OUT), expected);
            assert_eq!(word(&layout, &memory, KEEP), 0x1234);
            assert_eq!(depth(&layout, &memory), 0);
        }
        let _ = compiled;
    }
}

/// A unit that does nothing.
fn empty_unit() -> ExecutionUnit<RegionedAbsoluteAddr> {
    ExecutionUnit {
        entry_block_id: crate::BlockId(0),
        blocks: [block(0, vec![], SIRTerminator::Return)]
            .into_iter()
            .collect(),
        register_map: Default::default(),
    }
}

/// `last(x)` returns bits 1..100 of its 100-bit slot 1, which ends its frame,
/// and an entry function unit, with an empty frame, that calls it in the
/// frame that ends the stack with `value`.
fn last_of_the_stack(value: &BigUint) -> [FunctionUnit<RegionedAbsoluteAddr>; 2] {
    let function = FunctionUnit {
        unit: ExecutionUnit {
            entry_block_id: crate::BlockId(0),
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
    let caller = ExecutionUnit {
        entry_block_id: crate::BlockId(0),
        blocks: [block(
            0,
            vec![
                imm(0, (celox_sir::STACK_SIZE - 26) as u64),
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
    // An ordinary caller keeps its arena after the state, which would
    // separate the stack from the end of the image; a function unit's arena
    // is on the machine stack.
    let entry = FunctionUnit {
        unit: caller,
        frame: FrameLayout {
            size: 0,
            slots: vec![],
        },
        effects: Default::default(),
    };
    [function, entry]
}

#[test]
fn reads_a_slot_at_the_end_of_the_stack_within_the_image() {
    let value = (BigUint::from(1u8) << 99u32) | BigUint::from(0b1011u8);
    let layout = layout(false);
    let stack = layout.call_stack.unwrap();
    assert!(
        layout.merged_total_size
            <= stack.base_offset + stack.size + celox_state_layout::CALL_STACK_GUARD + 8,
        "the state image ends just after the stack's guard"
    );
    let compiled = compile(&last_of_the_stack(&value), &empty_unit(), &layout, 0);
    #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
    // SAFETY: the mapping is private to this test and unmapped below; entry
    // 1 is a function unit, `(state, frame) -> status`, owned by the program.
    unsafe {
        let program = run::Program::load(&compiled, &[]);
        let entry = std::mem::transmute::<usize, unsafe extern "C" fn(*mut u8, *mut u8) -> i64>(
            program.table[1],
        );
        // The function units need no state past the call stack's guard.
        let size = layout.merged_total_size;
        let page = libc::sysconf(libc::_SC_PAGESIZE) as usize;
        let mapped = size.div_ceil(page) * page;
        let map = libc::mmap(
            std::ptr::null_mut(),
            mapped + page,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        );
        assert_ne!(map, libc::MAP_FAILED);
        let guard = (map as *mut u8).add(mapped);
        assert_eq!(libc::mprotect(guard.cast(), page, libc::PROT_NONE), 0);
        let memory = guard.sub(size);
        memory
            .add(celox_state_layout::STATE_HEADER_EXTERN_FUNCTIONS_ADDR_OFFSET)
            .cast::<u64>()
            .write_unaligned(program.table.as_ptr() as u64);
        let status = entry(memory, memory.add(stack.base_offset));
        let image = std::slice::from_raw_parts(memory, size).to_vec();
        libc::munmap(map, mapped + page);
        assert_eq!(status, 0);
        let wide = layout.offsets[&object(WIDE)];
        assert_eq!(
            read(&image, wide, 13) & ((BigUint::from(1u8) << 99u32) - 1u8),
            &value >> 1u32
        );
    }
    let _ = compiled;
}

#[test]
fn a_frame_outside_the_stack_traps() {
    let layout = layout(false);
    let compiled = compile(&[factorial(false)], &caller(), &layout, 0);
    #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
    {
        let program = run::Program::load(&compiled, &[]);
        let pointer = layout.offsets[&object(SP)];
        let at = |frame: usize| {
            move |memory: &mut [u8]| {
                memory[pointer..pointer + 4].copy_from_slice(&(frame as u32).to_le_bytes());
            }
        };
        // fact(6) runs in six 8-byte frames from the stack pointer on; the
        // last one may end the stack, and one byte further it does not fit.
        let (status, memory) = program.run_with(at(celox_sir::STACK_SIZE - 48));
        assert_eq!(status, 0);
        assert_eq!(word(&layout, &memory, OUT), 720);
        for frame in [
            celox_sir::STACK_SIZE - 47,
            celox_sir::STACK_SIZE - 7,
            u32::MAX as usize,
        ] {
            assert_eq!(run::signal_of(&program, at(frame)), Some(libc::SIGTRAP));
        }
    }
    let _ = compiled;
}

#[test]
fn calls_nested_too_deep_trap() {
    let layout = layout(false);
    let compiled = compile(&[factorial(false)], &caller(), &layout, 0);
    #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
    {
        let program = run::Program::load(&compiled, &[]);
        let offset = layout.call_stack.unwrap().depth_offset;
        // fact(6) nests six calls deep.
        for (depth, signal) in [
            (celox_sir::MAX_CALL_DEPTH as u64 - 5, Some(libc::SIGTRAP)),
            (celox_sir::MAX_CALL_DEPTH as u64 - 6, None),
        ] {
            assert_eq!(
                run::signal_of(&program, |memory| {
                    memory[offset..offset + 8].copy_from_slice(&depth.to_le_bytes());
                }),
                signal,
                "from depth {depth}"
            );
        }
    }
    let _ = compiled;
}

#[test]
fn units_without_calls_compile_alike_with_functions_present() {
    // Compiling with the program's functions leaves the code of a unit that
    // calls none of them unchanged.
    let unit = ExecutionUnit {
        entry_block_id: crate::BlockId(0),
        blocks: [block(
            0,
            vec![load(0, state(SP), 32), store(state(OUT), 32, 0)],
            SIRTerminator::Return,
        )]
        .into_iter()
        .collect(),
        register_map: [(reg(0), bit(32))].into_iter().collect(),
    };
    let layout = layout(true);
    let frames = [factorial(false).frame];
    let code = |functions| {
        emit_prepared_eu_with_functions(
            &unit,
            &layout,
            true,
            "unit",
            false,
            None,
            functions,
            None,
            || false,
        )
        .unwrap()
        .code
    };
    assert_eq!(
        code(None),
        code(Some(FunctionCalls {
            frames: &frames,
            first_slot: 0,
            function: None,
        }))
    );
}
