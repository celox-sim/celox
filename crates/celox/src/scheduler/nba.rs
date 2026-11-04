//! Native record allocation and FIFO operations. All pointers reference stable
//! pages owned by the invocation's arena; allocation is the only host request.
mod observations;
mod sites;
use super::*;
use crate::simulator::native_io::{self, ArenaHeader, NbaHeader};
use celox_sir::native_memory::NativeMemoryOp as Memory;
pub(super) use sites::Sites;
use std::mem::offset_of;

const NEXT: usize = 0;
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
}

#[cfg(all(
    test,
    feature = "systemverilog",
    target_arch = "x86_64",
    not(feature = "arm64-codegen")
))]
mod tests;
