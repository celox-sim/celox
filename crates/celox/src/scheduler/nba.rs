//! Native record allocation and FIFO operations. All pointers reference stable
//! pages owned by the invocation's arena; allocation is the only host request.
mod observations;
mod sites;
pub(super) mod writes;
use super::*;
use crate::simulator::native_io::{self, ArenaHeader, NbaHeader};
use celox_sir::native_memory::NativeMemoryOp as Memory;
pub(super) use sites::Sites;
use std::mem::offset_of;

const NEXT: usize = 0;
const DUE: usize = 1;
const SEQUENCE: usize = 6;
const IMPORTED: usize = 7;
const SITE: usize = 2;
const LSB: usize = 3;
const WIDTH: usize = 4;
const CLASS: usize = 5;
const VALUE: usize = 8;

#[derive(Clone, Copy)]
pub(super) struct Queue {
    pub service: u32,
}

impl Queue {
    fn header_load(self, g: &mut Generator<'_>, offset: usize) -> RegisterId {
        g.arena_header_load(offset_of!(ArenaHeader, nba) + offset)
    }
    fn header_store(self, g: &mut Generator<'_>, offset: usize, value: RegisterId) {
        g.arena_header_store(offset_of!(ArenaHeader, nba) + offset, value);
    }
    fn read(self, g: &mut Generator<'_>, pointer: RegisterId, word: usize) -> RegisterId {
        let offset = g.constant((word * 8) as u64, 64);
        g.native_load(Memory::ReadIndirect, vec![pointer, offset])
    }
    fn write(self, g: &mut Generator<'_>, pointer: RegisterId, word: usize, value: RegisterId) {
        let offset = g.constant((word * 8) as u64, 64);
        g.native_store(Memory::WriteIndirect, vec![pointer, offset, value]);
    }
    fn allocate(self, g: &mut Generator<'_>, class: usize) -> RegisterId {
        let offset = offset_of!(NbaHeader, free) + class * 8;
        let available = self.header_load(g, offset);
        let empty = g.equals(available, 0);
        g.if_bit(empty, 0, |g| {
            let class = g.constant(class as u64, 64);
            g.arena_service(self.service, native_io::ARENA_RESERVE_NBA, class);
        });
        let record = self.header_load(g, offset);
        let next = self.read(g, record, NEXT);
        self.header_store(g, offset, next);
        let zero = g.constant(0, 64);
        self.write(g, record, NEXT, zero);
        let class = g.constant(class as u64, 64);
        self.write(g, record, CLASS, class);
        record
    }
    fn release(self, g: &mut Generator<'_>, record: RegisterId, class: usize) {
        let offset = offset_of!(NbaHeader, free) + class * 8;
        let first = self.header_load(g, offset);
        self.write(g, record, NEXT, first);
        self.header_store(g, offset, record);
    }
    fn append(self, g: &mut Generator<'_>, record: RegisterId) {
        let tail = self.header_load(g, offset_of!(NbaHeader, tail));
        let empty = g.equals(tail, 0);
        g.if_bit(empty, 0, |g| {
            self.header_store(g, offset_of!(NbaHeader, head), record)
        });
        let present = g.equals(empty, 0);
        g.if_bit(present, 0, |g| self.write(g, tail, NEXT, record));
        let zero = g.constant(0, 64);
        self.write(g, record, NEXT, zero);
        self.header_store(g, offset_of!(NbaHeader, tail), record);
    }
    /// Caller proves the queue nonempty. The record remains live until release.
    fn pop(self, g: &mut Generator<'_>) -> RegisterId {
        let first = self.header_load(g, offset_of!(NbaHeader, head));
        let next = self.read(g, first, NEXT);
        self.header_store(g, offset_of!(NbaHeader, head), next);
        let empty = g.equals(next, 0);
        g.if_bit(empty, 0, |g| {
            let zero = g.constant(0, 64);
            self.header_store(g, offset_of!(NbaHeader, tail), zero);
        });
        first
    }
    fn earlier(self, g: &mut Generator<'_>, left: RegisterId, right: RegisterId) -> RegisterId {
        let ltime = self.read(g, left, DUE);
        let rtime = self.read(g, right, DUE);
        let before = g.binary(1, ltime, BinaryOp::LtU, rtime);
        let equal = g.binary(1, ltime, BinaryOp::Eq, rtime);
        let lseq = self.read(g, left, SEQUENCE);
        let rseq = self.read(g, right, SEQUENCE);
        let ordered = g.binary(1, lseq, BinaryOp::LtU, rseq);
        let tie = g.binary(1, equal, BinaryOp::And, ordered);
        g.binary(1, before, BinaryOp::Or, tie)
    }
    fn delay(self, g: &mut Generator<'_>, record: RegisterId) {
        let len = self.header_load(g, offset_of!(NbaHeader, delayed_len));
        let one = g.constant(1, 64);
        let next_len = g.binary(64, len, BinaryOp::Add, one);
        let capacity = self.header_load(g, offset_of!(NbaHeader, delayed_capacity));
        let grow = g.binary(1, next_len, BinaryOp::GtU, capacity);
        g.if_bit(grow, 0, |g| {
            g.arena_service(self.service, native_io::ARENA_RESERVE_DELAYED_NBA, next_len);
        });
        let heap = self.header_load(g, offset_of!(NbaHeader, delayed));
        self.header_store(g, offset_of!(NbaHeader, delayed_len), next_len);
        let index = g.b.alloc_bit(64, false);
        let position = g.b.alloc_bit(64, false);
        let header = g.b.new_block_with(vec![index]);
        let finish = g.b.new_block_with(vec![position]);
        let compare = g.b.new_block();
        let move_parent = g.b.new_block();
        g.b.seal_block(SIRTerminator::Jump(header, vec![len]));
        g.b.switch_to_block(header);
        let root = g.equals(index, 0);
        g.b.seal_block(SIRTerminator::Branch {
            cond: root,
            true_block: (finish, vec![index]),
            false_block: (compare, Vec::new()),
        });
        g.b.switch_to_block(compare);
        let parent = g.binary(64, index, BinaryOp::Sub, one);
        let parent = g.binary(64, parent, BinaryOp::Shr, one);
        let previous = g.arena_load(heap, parent);
        let earlier = self.earlier(g, record, previous);
        g.b.seal_block(SIRTerminator::Branch {
            cond: earlier,
            true_block: (move_parent, Vec::new()),
            false_block: (finish, vec![index]),
        });
        g.b.switch_to_block(move_parent);
        g.arena_store(heap, index, previous);
        g.b.seal_block(SIRTerminator::Jump(header, vec![parent]));
        g.b.switch_to_block(finish);
        g.arena_store(heap, position, record);
    }
    /// Remove the first delayed update. Caller proves delayed_len is nonzero.
    fn pop_delayed(self, g: &mut Generator<'_>) -> RegisterId {
        let heap = self.header_load(g, offset_of!(NbaHeader, delayed));
        let zero = g.constant(0, 64);
        let root = g.arena_load(heap, zero);
        let len = self.header_load(g, offset_of!(NbaHeader, delayed_len));
        let one = g.constant(1, 64);
        let len = g.binary(64, len, BinaryOp::Sub, one);
        let last = g.arena_load(heap, len);
        self.header_store(g, offset_of!(NbaHeader, delayed_len), len);
        let empty = g.equals(len, 0);
        let index = g.b.alloc_bit(64, false);
        let position = g.b.alloc_bit(64, false);
        let header = g.b.new_block_with(vec![index]);
        let finish = g.b.new_block_with(vec![position]);
        let compare = g.b.new_block();
        let descend = g.b.new_block();
        let done = g.b.new_block();
        g.b.seal_block(SIRTerminator::Branch {
            cond: empty,
            true_block: (done, Vec::new()),
            false_block: (header, vec![zero]),
        });
        g.b.switch_to_block(header);
        let twice = g.binary(64, index, BinaryOp::Shl, one);
        let left = g.binary(64, twice, BinaryOp::Add, one);
        let has_child = g.binary(1, left, BinaryOp::LtU, len);
        g.b.seal_block(SIRTerminator::Branch {
            cond: has_child,
            true_block: (compare, Vec::new()),
            false_block: (finish, vec![index]),
        });
        g.b.switch_to_block(compare);
        let right = g.binary(64, left, BinaryOp::Add, one);
        let has_right = g.binary(1, right, BinaryOp::LtU, len);
        let lvalue = g.arena_load(heap, left);
        // The old last slot remains in bounds, but may contain the removed
        // root. Select the left child when no live right child exists.
        let rvalue = g.arena_load(heap, right);
        let right_first = self.earlier(g, rvalue, lvalue);
        let right_first = g.binary(1, has_right, BinaryOp::And, right_first);
        let child = g.b.alloc_bit(64, false);
        let value = g.b.alloc_bit(64, false);
        g.b.emit(SIRInstruction::Mux(child, right_first, right, left));
        g.b.emit(SIRInstruction::Mux(value, right_first, rvalue, lvalue));
        let before_last = self.earlier(g, value, last);
        g.b.seal_block(SIRTerminator::Branch {
            cond: before_last,
            true_block: (descend, Vec::new()),
            false_block: (finish, vec![index]),
        });
        g.b.switch_to_block(descend);
        g.arena_store(heap, index, value);
        g.b.seal_block(SIRTerminator::Jump(header, vec![child]));
        g.b.switch_to_block(finish);
        g.arena_store(heap, position, last);
        g.b.seal_block(SIRTerminator::Jump(done, Vec::new()));
        g.b.switch_to_block(done);
        root
    }
}

#[cfg(all(test, target_arch = "x86_64", not(feature = "arm64-codegen")))]
mod tests;
