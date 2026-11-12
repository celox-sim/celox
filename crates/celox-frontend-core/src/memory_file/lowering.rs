//! The SIR of a `$readmemh` or `$readmemb` that reads its file when it
//! executes (IEEE 1800-2023 21.4): the heap's memory-file reader loads the
//! file (see `celox_heap::memory_files`), and the code stores each word it
//! gives with an ordinary store, so the words the file does not give keep
//! their values.

use celox_design::MemoryFileLoad;
use celox_heap::ops;
use celox_sir::extern_abi::{HEAP_HELPER_ARGUMENTS, HEAP_HELPER_ID};
use celox_sir::{
    BinaryOp, RegisterId, SIRBuilder, SIRInstruction, SIRTerminator, SIRValue, UnaryOp,
};
use num_bigint::BigUint;

/// The messages of a load that ran, as string handles: the empty string
/// when it has none.
pub struct MemoryFileMessages {
    pub warning: RegisterId,
    pub error: RegisterId,
}

/// Whether string `message`, a 64-bit handle of [`MemoryFileMessages`], is
/// not empty: a register of one bit.
pub fn message_present<A>(b: &mut SIRBuilder<A>, message: RegisterId) -> RegisterId {
    let empty = constant64(b, celox_heap::EMPTY_STRING.into());
    let present = b.alloc_bit(1, false);
    b.emit(SIRInstruction::Binary(
        present,
        message,
        BinaryOp::Ne,
        empty,
    ));
    present
}

/// A call of the heap helper with operation `code` and `operands`.
pub fn heap_call<A>(b: &mut SIRBuilder<A>, code: u64, operands: &[RegisterId]) -> RegisterId {
    let mut args = vec![constant64(b, code)];
    args.extend_from_slice(operands);
    while args.len() < HEAP_HELPER_ARGUMENTS {
        args.push(constant64(b, 0));
    }
    let dst = b.alloc_bit(64, false);
    b.emit(SIRInstruction::ExternCall {
        dst: Some(dst),
        func: HEAP_HELPER_ID,
        args,
    });
    dst
}

pub fn constant64<A>(b: &mut SIRBuilder<A>, value: u64) -> RegisterId {
    let register = b.alloc_bit(64, false);
    b.emit(SIRInstruction::Imm(
        register,
        SIRValue::new(BigUint::from(value)),
    ));
    register
}

/// The handle of the interned string `bytes`, built when the code runs.
pub fn frozen_bytes<A>(b: &mut SIRBuilder<A>, bytes: &[u8]) -> RegisterId {
    let mut buffer = heap_call(b, ops::NEW, &[]);
    for chunk in bytes.chunks(8) {
        let value = chunk
            .iter()
            .fold(0u64, |value, byte| (value << 8) | u64::from(*byte));
        let value = constant64(b, value);
        let count = constant64(b, chunk.len() as u64);
        buffer = heap_call(b, ops::APPEND_BYTES, &[buffer, value, count]);
    }
    heap_call(b, ops::FREEZE, &[buffer])
}

/// Run `load` when the code executes, reading the file string `file` (its
/// handle, or `load.file` when `None`), from address `range.0` toward
/// `range.1` when given as signed 64-bit registers. `cx` is the lowering
/// that `builder` gives the builder of, and `store(cx, position, word)`
/// stores each word the file gives at its position in the storage order of
/// the array; a word keeps its unknown bits when `four_state` is set and has
/// them as zero otherwise (IEEE 1800-2023 21.4.2). The block open when it is
/// called stays open after it, at the end of the load.
pub fn lower_memory_file_load<C, A>(
    cx: &mut C,
    builder: impl Fn(&mut C) -> &mut SIRBuilder<A>,
    load: &MemoryFileLoad,
    file: Option<RegisterId>,
    range: Option<(RegisterId, RegisterId)>,
    four_state: bool,
    mut store: impl FnMut(&mut C, RegisterId, RegisterId),
) -> MemoryFileMessages {
    let b = builder(cx);
    let descriptor = frozen_bytes(b, &super::encode_descriptor(load));
    let file = match file {
        Some(file) => file,
        None => frozen_bytes(b, load.file.as_bytes()),
    };
    if let Some((start, finish)) = range {
        heap_call(b, ops::MEMORY_FILE_RANGE, &[start, finish]);
    }
    let count = heap_call(b, ops::MEMORY_FILE_LOAD, &[descriptor, file]);
    let zero = constant64(b, 0);
    // for (k = 0; k < count; k++) store word k at its position.
    let head_index = b.alloc_bit(64, false);
    let body_index = b.alloc_bit(64, false);
    let head = b.new_block_with(vec![head_index]);
    let body = b.new_block_with(vec![body_index]);
    let done = b.new_block();
    b.seal_block(SIRTerminator::Jump(head, vec![zero]));
    b.switch_to_block(head);
    let more = b.alloc_bit(1, false);
    b.emit(SIRInstruction::Binary(
        more,
        head_index,
        BinaryOp::LtU,
        count,
    ));
    b.seal_block(SIRTerminator::Branch {
        cond: more,
        true_block: (body, vec![head_index]),
        false_block: (done, Vec::new()),
    });
    b.switch_to_block(body);
    let position = heap_call(b, ops::MEMORY_FILE_POSITION, &[body_index]);
    let width = load.element_width;
    let chunks = width.div_ceil(64);
    let plane = |b: &mut SIRBuilder<A>, parameter: u64| {
        let mut parts = Vec::with_capacity(chunks);
        for chunk in (0..chunks).rev() {
            let chunk = constant64(b, chunk as u64);
            parts.push(heap_call(
                b,
                ops::MEMORY_FILE_WORD | parameter << 8,
                &[body_index, chunk],
            ));
        }
        // The plane in whole 64-bit words.
        if parts.len() == 1 {
            parts[0]
        } else {
            let joined = b.alloc_bit(64 * chunks, false);
            b.emit(SIRInstruction::Concat(joined, parts));
            joined
        }
    };
    let words = plane(b, 0);
    let mask_words = plane(b, 1);
    let word = if four_state {
        // The planes as one register, the unknown plane above the value.
        let planes = b.alloc_bit(2 * 64 * chunks, false);
        b.emit(SIRInstruction::Concat(planes, vec![mask_words, words]));
        let padded = b.alloc_logic(64 * chunks);
        b.emit(SIRInstruction::Unary(padded, UnaryOp::FromPlanes, planes));
        let word = b.alloc_logic(width);
        b.emit(SIRInstruction::Slice(word, padded, 0, width));
        word
    } else {
        let slice = |b: &mut SIRBuilder<A>, words: RegisterId| {
            let plane = b.alloc_bit(width, false);
            b.emit(SIRInstruction::Slice(plane, words, 0, width));
            plane
        };
        let value = slice(b, words);
        let mask = slice(b, mask_words);
        let known = b.alloc_bit(width, false);
        b.emit(SIRInstruction::Unary(known, UnaryOp::BitNot, mask));
        let word = b.alloc_bit(width, false);
        b.emit(SIRInstruction::Binary(word, value, BinaryOp::And, known));
        word
    };
    store(cx, position, word);
    let b = builder(cx);
    let one = constant64(b, 1);
    let next = b.alloc_bit(64, false);
    b.emit(SIRInstruction::Binary(next, body_index, BinaryOp::Add, one));
    b.seal_block(SIRTerminator::Jump(head, vec![next]));
    b.switch_to_block(done);
    MemoryFileMessages {
        warning: heap_call(b, ops::MEMORY_FILE_MESSAGE, &[]),
        error: heap_call(b, ops::MEMORY_FILE_MESSAGE | 1 << 8, &[]),
    }
}
