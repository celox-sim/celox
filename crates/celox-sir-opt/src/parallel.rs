//! Concurrency plans for lane-partitioned kernels.
//!
//! A partitioned kernel is a list of execution units in a valid sequential
//! order, each placed in a lane. This module derives the happens-before
//! relation from the units' final memory effects on the physical state layout
//! and cuts every lane's units into tasks with minimal cross-lane waits.
//!
//! Accesses are exact physical bit ranges, so aliased objects and
//! overlapping fields are compared by their storage rather than by name.
//! Generated code may widen a partial store into a read-modify-write of
//! neighbouring bits and bytes. Such a widened store writes back the unchanged
//! bits it read, which is invisible to a concurrent reader, but it would lose
//! a concurrent write. Every write therefore also claims the bytes its code
//! generator may store (its *envelope*, see [`CodegenFootprint`]), and writes
//! with overlapping envelopes are ordered, while reads of untouched bits
//! stay independent. Runtime buffers shared by every lane (the event ring,
//! comb-capture flags, and trigger bytes) are serialized as whole resources.
//! Waveform notification bytes need no ordering: generated code only ever
//! sets them to one with plain byte stores.

use celox_analysis::dependence::MemoryDependencyTracker;
use celox_analysis::lanes::{
    LaneError, TaskFormationOptions, form_lane_tasks_separated, form_lane_tasks_with_options,
    verify_lane_tasks,
};
use celox_analysis::memory::{MemoryEffect, MemoryLocation};
use celox_state_layout::MemoryLayout;

pub use celox_analysis::lanes::{LaneTask, LaneWait};

use crate::ir::{
    AbsoluteAddr, ExecutionUnit, LaneUnit, RegionedAbsoluteAddr, SIRInstruction, SIROffset,
    SPARSE_WORKING_REGION, STABLE_REGION,
};

/// Storage domains compared by the dependency analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Storage {
    /// The simulation state image, addressed by physical bit offset.
    ///
    /// Generated code never changes bits outside an access, even when it
    /// stores a wider envelope, so reads and writes are compared by bit.
    /// Location lengths in this storage count bits.
    StateBits,
    /// Read-modify-write envelopes of writes, by physical byte offset.
    WriteEnvelope,
    /// The runtime-event ring, which has a single-producer append protocol.
    RuntimeEvents,
    /// Per-site comb-capture enable bytes.
    CaptureFlags,
    /// Packed event trigger bytes, updated with byte read-modify-writes.
    Triggers,
    /// External functions, which need not be thread-safe and are called in
    /// program order.
    ExternCalls,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParallelPlanError {
    #[error("lane-partitioned kernel has an invalid lane or dependency: {0:?}")]
    Lanes(LaneError),
    #[error("lane-partitioned kernel violates dependency {predecessor} -> {unit}")]
    Unordered { predecessor: usize, unit: usize },
}

/// Code generator whose widened writes a concurrency plan must respect.
///
/// Both generators write a static field with read-modify-write accesses
/// that start at the field's first byte and may end past its last byte. A
/// dynamically addressed write stays within its object plus a few bytes,
/// and is planned as a write of its whole home with a 16-byte overhang.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodegenFootprint {
    /// x86-64 and AArch64 native code. A static field of `n` bytes is
    /// written in parts of at most eight bytes; the last part is rounded up
    /// to 1, 2, 4, or 8 bytes. Element-strided arrays with padding are
    /// written per element, and a part may run past the end of its element
    /// slot; arrays without padding are written like any other field.
    Native,
    /// Cranelift code. A static field written from a register of at most 64
    /// bits within one 64-bit word is rounded up to 1, 2, 4, or 8 bytes;
    /// any other static write is a sequence of 8-byte words. Commits choose
    /// the same way by the width of the committed object. Cranelift only
    /// compiles packed layouts.
    Cranelift,
}

/// The kind of a write, as far as it selects generated code.
#[derive(Debug, Clone, Copy)]
enum WriteKind {
    /// A store from a register of the given width.
    Store { source_width: usize },
    /// A copy between two homes of an object of the given width.
    Commit { object_width: usize },
}

impl CodegenFootprint {
    /// Bytes, relative to the field's first byte, that a static write of
    /// `width` bits at bit `shift` of its first byte may store.
    fn static_write_span(self, shift: usize, width: usize, kind: WriteKind) -> usize {
        let width = width.max(1);
        let bytes = (shift + width).div_ceil(8);
        let words = (shift + width).div_ceil(64) * 8;
        match self {
            Self::Native => {
                let tail = (bytes - 1) % 8 + 1;
                bytes + tail.next_power_of_two() - tail
            }
            Self::Cranelift => match kind {
                WriteKind::Store { source_width } if source_width <= 64 && shift + width <= 64 => {
                    bytes.next_power_of_two()
                }
                WriteKind::Commit { object_width } if object_width <= 64 => {
                    bytes.next_power_of_two()
                }
                WriteKind::Commit { .. } if shift == 0 && width.is_multiple_of(8) => bytes,
                _ => words,
            },
        }
    }
}

/// One physical part of a static access: its first byte relative to the
/// plane base, the bit shift within that byte, and its width.
#[derive(Debug, Clone, Copy)]
struct StaticPart {
    byte: usize,
    shift: usize,
    width: usize,
}

/// Parts beyond which a static access to a strided array is treated as an
/// access to the whole home.
const MAX_STRIDED_PARTS: usize = 64;

struct UnitEffects<'a> {
    layout: &'a MemoryLayout<AbsoluteAddr>,
    footprint: CodegenFootprint,
    reads: Vec<MemoryEffect<Storage>>,
    writes: Vec<MemoryEffect<Storage>>,
}

const fn align_up(value: usize, alignment: usize) -> usize {
    value.div_ceil(alignment) * alignment
}

/// Bytes past an object that a dynamically addressed write may touch.
const DYNAMIC_WRITE_OVERHANG: usize = 16;

impl<'a> UnitEffects<'a> {
    fn new(layout: &'a MemoryLayout<AbsoluteAddr>, footprint: CodegenFootprint) -> Self {
        Self {
            layout,
            footprint,
            reads: Vec::new(),
            writes: Vec::new(),
        }
    }

    fn planes(&self) -> usize {
        if self.layout.four_state { 2 } else { 1 }
    }

    fn region_base(&self, address: &RegionedAbsoluteAddr) -> Option<usize> {
        let object = address.absolute_addr();
        match address.region {
            STABLE_REGION => self.layout.offsets.get(&object).copied(),
            SPARSE_WORKING_REGION => self
                .layout
                .sparse_offsets
                .get(&object)
                .map(|offset| self.layout.sparse_base_offset + offset),
            _ => self
                .layout
                .working_offsets
                .get(&object)
                .map(|offset| self.layout.working_base_offset + offset),
        }
    }

    /// State bytes `[offset, offset + byte_len)`.
    fn range(offset: usize, byte_len: usize) -> MemoryEffect<Storage> {
        Self::bit_range(offset * 8, byte_len * 8)
    }

    /// State bits `[offset, offset + bit_len)`.
    fn bit_range(offset: usize, bit_len: usize) -> MemoryEffect<Storage> {
        MemoryEffect::Exact(MemoryLocation {
            object: Storage::StateBits,
            offset: offset as i64,
            byte_len: bit_len,
        })
    }

    fn envelope_range(offset: usize, byte_len: usize) -> MemoryEffect<Storage> {
        MemoryEffect::Exact(MemoryLocation {
            object: Storage::WriteEnvelope,
            offset: offset as i64,
            byte_len,
        })
    }

    /// Physical parts of a static access, relative to the plane base, or
    /// `None` when the access is treated as covering its whole home.
    fn static_parts(
        &self,
        address: &RegionedAbsoluteAddr,
        offset: &SIROffset,
        width: usize,
    ) -> Option<Vec<StaticPart>> {
        if address.region == SPARSE_WORKING_REGION {
            return None;
        }
        let bit_offset = offset.constant_bit_offset()?;
        let width = width.max(1);
        // Without padding the elements are contiguous, and code generators
        // write the access as one field, rounding only its last part.
        let Some(array) = self
            .layout
            .unpacked_arrays
            .get(&address.absolute_addr())
            .filter(|array| array.element_stride * 8 != array.element_width)
        else {
            return Some(vec![StaticPart {
                byte: bit_offset / 8,
                shift: bit_offset % 8,
                width,
            }]);
        };
        // Element-strided storage: each element starts its own slot.
        let element_width = array.element_width.max(1);
        let first = bit_offset / element_width;
        let last = (bit_offset + width - 1) / element_width;
        if last >= array.element_count || last - first >= MAX_STRIDED_PARTS {
            return None;
        }
        Some(
            (first..=last)
                .map(|element| {
                    let element_start = element * element_width;
                    let low = bit_offset.max(element_start) - element_start;
                    let high =
                        (bit_offset + width).min(element_start + element_width) - element_start;
                    StaticPart {
                        byte: element * array.element_stride + low / 8,
                        shift: low % 8,
                        width: high - low,
                    }
                })
                .collect(),
        )
    }

    /// The bytes one write may store, including bytes a read-modify-write
    /// stores back unchanged.
    fn envelope(
        &self,
        address: &RegionedAbsoluteAddr,
        offset: &SIROffset,
        width: usize,
        kind: WriteKind,
        effects: &mut Vec<MemoryEffect<Storage>>,
    ) {
        let object = address.absolute_addr();
        let Some(base) = self
            .region_base(address)
            .filter(|_| self.layout.widths.contains_key(&object))
        else {
            effects.push(MemoryEffect::UnknownObject(Storage::WriteEnvelope));
            return;
        };
        let Some(parts) = self.static_parts(address, offset, width) else {
            self.whole_envelope(address, base, effects);
            return;
        };
        let plane = self.layout.plane_size(&object);
        for part in parts {
            let span = self
                .footprint
                .static_write_span(part.shift, part.width, kind);
            for plane_index in 0..self.planes() {
                effects.push(Self::envelope_range(
                    base + plane_index * plane + part.byte,
                    span,
                ));
            }
        }
    }

    /// Envelope of an access to anywhere in a home.
    fn whole_envelope(
        &self,
        address: &RegionedAbsoluteAddr,
        base: usize,
        effects: &mut Vec<MemoryEffect<Storage>>,
    ) {
        let object = address.absolute_addr();
        let plane = self.layout.plane_size(&object);
        let extent = if address.region == SPARSE_WORKING_REGION {
            (self.planes() - 1) * plane + align_up(plane, 8)
        } else {
            plane * self.planes()
        };
        effects.push(Self::envelope_range(base, extent + DYNAMIC_WRITE_OVERHANG));
        if address.region == SPARSE_WORKING_REGION
            && let Some(sparse) = self.layout.sparse_layouts.get(&object)
        {
            effects.push(Self::envelope_range(
                sparse.dirty_words_offset,
                sparse.dirty_word_count.max(1) * 8,
            ));
            effects.push(Self::envelope_range(
                sparse.summary_words_offset,
                sparse.summary_word_count.max(1) * 8,
            ));
        }
    }

    /// Every byte owned by one home, including sparse write metadata.
    fn home(&self, address: &RegionedAbsoluteAddr, effects: &mut Vec<MemoryEffect<Storage>>) {
        let object = address.absolute_addr();
        let Some(base) = self
            .region_base(address)
            .filter(|_| self.layout.widths.contains_key(&object))
        else {
            effects.push(MemoryEffect::UnknownObject(Storage::StateBits));
            return;
        };
        let plane = self.layout.plane_size(&object);
        if address.region == SPARSE_WORKING_REGION {
            let extent = (self.planes() - 1) * plane + align_up(plane, 8);
            effects.push(Self::range(base, extent.max(1)));
            if let Some(sparse) = self.layout.sparse_layouts.get(&object) {
                effects.push(Self::range(
                    sparse.dirty_words_offset,
                    sparse.dirty_word_count.max(1) * 8,
                ));
                effects.push(Self::range(
                    sparse.summary_words_offset,
                    sparse.summary_word_count.max(1) * 8,
                ));
            }
            return;
        }
        effects.push(Self::range(base, (plane * self.planes()).max(1)));
    }

    /// The bits holding one logical bit range of an object.
    fn bits(
        &self,
        address: &RegionedAbsoluteAddr,
        offset: &SIROffset,
        width: usize,
        effects: &mut Vec<MemoryEffect<Storage>>,
    ) {
        let object = address.absolute_addr();
        let base = self
            .region_base(address)
            .filter(|_| self.layout.widths.contains_key(&object));
        let (Some(parts), Some(base)) = (self.static_parts(address, offset, width), base) else {
            self.home(address, effects);
            return;
        };
        let plane = self.layout.plane_size(&object) * 8;
        for part in parts {
            let first = (part.byte * 8 + part.shift).min(plane);
            let end =
                (part.byte * 8 + part.shift + part.width).clamp(first + 1, plane.max(first + 1));
            for plane_index in 0..self.planes() {
                effects.push(Self::bit_range(
                    base * 8 + plane_index * plane + first,
                    end - first,
                ));
            }
        }
    }

    fn read(&mut self, address: &RegionedAbsoluteAddr, offset: &SIROffset, width: usize) {
        let mut effects = std::mem::take(&mut self.reads);
        self.bits(address, offset, width, &mut effects);
        self.reads = effects;
    }

    fn write(
        &mut self,
        address: &RegionedAbsoluteAddr,
        offset: &SIROffset,
        width: usize,
        kind: WriteKind,
    ) {
        let mut effects = std::mem::take(&mut self.writes);
        self.bits(address, offset, width, &mut effects);
        self.envelope(address, offset, width, kind, &mut effects);
        self.writes = effects;
    }

    fn write_home(&mut self, address: &RegionedAbsoluteAddr) {
        let mut effects = std::mem::take(&mut self.writes);
        self.home(address, &mut effects);
        match self.region_base(address) {
            Some(base) if self.layout.widths.contains_key(&address.absolute_addr()) => {
                self.whole_envelope(address, base, &mut effects);
            }
            _ => effects.push(MemoryEffect::UnknownObject(Storage::WriteEnvelope)),
        }
        self.writes = effects;
    }

    fn resource(&mut self, storage: Storage) {
        self.writes.push(MemoryEffect::UnknownObject(storage));
    }

    /// A call reads and writes the whole objects its effects list, and its
    /// frames on the one call stack, which every other call uses too.
    fn call(&mut self, effects: &celox_sir::CallEffects<RegionedAbsoluteAddr>) {
        if effects.kernel {
            // A native kernel inside can read and write anything.
            for storage in [
                Storage::StateBits,
                Storage::WriteEnvelope,
                Storage::RuntimeEvents,
                Storage::CaptureFlags,
                Storage::Triggers,
                Storage::ExternCalls,
            ] {
                self.reads.push(MemoryEffect::UnknownObject(storage));
                self.resource(storage);
            }
            return;
        }
        let mut reads = std::mem::take(&mut self.reads);
        for object in &effects.reads {
            self.home(object, &mut reads);
        }
        self.reads = reads;
        for object in &effects.writes {
            self.write_home(object);
        }
        match self.layout.call_stack {
            Some(stack) => {
                // The depth word is followed by the frames.
                let bytes = stack.base_offset + stack.size - stack.depth_offset;
                self.reads.push(Self::range(stack.depth_offset, bytes));
                self.writes.push(Self::range(stack.depth_offset, bytes));
                self.writes
                    .push(Self::envelope_range(stack.depth_offset, bytes));
            }
            None => {
                self.reads
                    .push(MemoryEffect::UnknownObject(Storage::StateBits));
                self.resource(Storage::StateBits);
            }
        }
        // Its host resources are its instruction's `host_effects`, as any
        // instruction's are (see `collect`).
    }

    /// The resources outside the state that `host` names.
    fn host(&mut self, host: celox_sir::HostEffects) {
        for (used, storage) in [
            (host.triggers, Storage::Triggers),
            (host.capture_flags, Storage::CaptureFlags),
            (host.runtime_events, Storage::RuntimeEvents),
            (host.extern_calls, Storage::ExternCalls),
        ] {
            if used {
                self.resource(storage);
            }
        }
    }

    fn collect(&mut self, unit: &ExecutionUnit<RegionedAbsoluteAddr>) {
        for block in unit.blocks.values() {
            for instruction in &block.instructions {
                match instruction {
                    SIRInstruction::Load(_, address, offset, width) => {
                        self.read(address, offset, *width);
                    }
                    SIRInstruction::Store(address, offset, width, source, _, _) => {
                        if address.region == SPARSE_WORKING_REGION {
                            // A sparse store also updates its dirty metadata.
                            self.write_home(address);
                        } else {
                            let source_width = unit
                                .register_map
                                .get(source)
                                .map_or(usize::MAX, |register| register.width());
                            self.write(address, offset, *width, WriteKind::Store { source_width });
                        }
                    }
                    SIRInstruction::Commit(source, destination, offset, width, _) => {
                        self.read(source, offset, *width);
                        if source.region == SPARSE_WORKING_REGION {
                            // A sparse commit consumes and clears dirty bits,
                            // and copies only the chunks they select.
                            self.write_home(source);
                            self.write_home(destination);
                        } else {
                            let object_width = self
                                .layout
                                .widths
                                .get(&destination.absolute_addr())
                                .copied()
                                .unwrap_or(usize::MAX);
                            self.write(
                                destination,
                                offset,
                                *width,
                                WriteKind::Commit { object_width },
                            );
                        }
                    }
                    SIRInstruction::Call { effects, .. } => self.call(effects),
                    SIRInstruction::RuntimeEvent { .. }
                    | SIRInstruction::CombCaptureEvent { .. }
                    | SIRInstruction::CombCaptureEnableIfChanged { .. }
                    | SIRInstruction::ExternCall { .. }
                    | SIRInstruction::Imm(..)
                    | SIRInstruction::Binary(..)
                    | SIRInstruction::Unary(..)
                    | SIRInstruction::Concat(..)
                    | SIRInstruction::Slice(..)
                    | SIRInstruction::Mux(..) => {}
                }
                // What it does outside the state, from the one query that
                // call summaries are built from too.
                self.host(instruction.host_effects());
            }
        }
    }
}

/// Memory effects of one unit or compiled task on a physical layout.
#[derive(Debug, Clone)]
pub struct Footprint {
    reads: Vec<MemoryEffect<Storage>>,
    writes: Vec<MemoryEffect<Storage>>,
}

impl Footprint {
    /// A task without memory effects.
    pub fn empty() -> Self {
        Self {
            reads: Vec::new(),
            writes: Vec::new(),
        }
    }

    /// Effects of `unit` when compiled by `codegen` for `layout`.
    pub fn of_unit(
        unit: &ExecutionUnit<RegionedAbsoluteAddr>,
        layout: &MemoryLayout<AbsoluteAddr>,
        codegen: CodegenFootprint,
    ) -> Self {
        let mut effects = UnitEffects::new(layout, codegen);
        effects.collect(unit);
        Self {
            reads: effects.reads,
            writes: effects.writes,
        }
    }
}

/// Dependencies of every footprint on earlier footprints of one kernel.
fn footprint_dependencies(footprints: impl IntoIterator<Item = Footprint>) -> Vec<Vec<usize>> {
    let diagnostic = tracing::enabled!(target: "celox::parallel", tracing::Level::TRACE);
    let mut history: Vec<Footprint> = Vec::new();
    let mut tracker = MemoryDependencyTracker::<Storage, usize>::default();
    let mut predecessors = Vec::new();
    let mut dependencies = std::collections::BTreeSet::new();
    for (index, footprint) in footprints.into_iter().enumerate() {
        tracing::trace!(target: "celox::parallel", event = "footprint", unit = index,
            reads = ?footprint.reads, writes = ?footprint.writes);
        dependencies.clear();
        if diagnostic {
            history.push(footprint.clone());
        }
        tracker.add_event(index, footprint.reads, footprint.writes, &mut dependencies);
        if diagnostic {
            for &predecessor in &dependencies {
                let before = &history[predecessor];
                let after = &history[index];
                for (hazard, left, right) in [
                    ("RAW", &before.writes, &after.reads),
                    ("WAR", &before.reads, &after.writes),
                    ("WAW", &before.writes, &after.writes),
                ] {
                    // One exact conflicting pair explains each hazard edge.
                    // Exhaustively expanding the Cartesian product is quadratic
                    // for large fused kernels and obscures the scheduler decision.
                    if let Some((producer, consumer)) = left.iter().find_map(|&producer| {
                        right
                            .iter()
                            .copied()
                            .find(|&consumer| {
                                celox_analysis::memory::effects_may_alias(producer, consumer)
                            })
                            .map(|consumer| (producer, consumer))
                    }) {
                        tracing::trace!(target: "celox::parallel", event = "hazard", predecessor,
                            unit = index, hazard, representative = true, producer = ?producer, consumer = ?consumer);
                    }
                }
            }
        }
        predecessors.push(dependencies.iter().copied().collect());
        tracing::trace!(target: "celox::parallel", event = "physical_dependencies", unit = index,
            predecessors = ?predecessors[index]);
    }
    predecessors
}

/// Dependencies of every unit on earlier units of the same kernel.
pub fn unit_dependencies(
    units: &[&LaneUnit<RegionedAbsoluteAddr>],
    layout: &MemoryLayout<AbsoluteAddr>,
    codegen: CodegenFootprint,
) -> Vec<Vec<usize>> {
    footprint_dependencies(
        units
            .iter()
            .map(|unit| Footprint::of_unit(&unit.unit, layout, codegen)),
    )
}

/// Sparse staging class of a unit: 1 when it stores sparse next-state data,
/// 2 when it commits sparse data. Native code generation commits every
/// active sparse object when one function contains both, so such units never
/// share a task. Only such a function marks and scans the shared sparse
/// "active" bitmap, so keeping the classes apart also keeps that bitmap out
/// of every lane task's footprint.
fn sparse_class(unit: &ExecutionUnit<RegionedAbsoluteAddr>) -> u8 {
    let mut class = 0;
    for instruction in unit.blocks.values().flat_map(|block| &block.instructions) {
        match instruction {
            SIRInstruction::Store(address, ..) if address.region == SPARSE_WORKING_REGION => {
                class |= 1;
            }
            SIRInstruction::Commit(source, ..) if source.region == SPARSE_WORKING_REGION => {
                class |= 2;
            }
            _ => {}
        }
    }
    class
}

/// Cut a partitioned kernel into lane tasks whose waits honour every memory
/// dependency between its units.
///
/// The tasks are returned in a valid sequential order. Each task's `items`
/// are indices into `units`.
pub fn plan_parallel_kernel(
    units: &[&LaneUnit<RegionedAbsoluteAddr>],
    layout: &MemoryLayout<AbsoluteAddr>,
    lanes: u32,
    codegen: CodegenFootprint,
) -> Result<Vec<LaneTask>, ParallelPlanError> {
    let predecessors = unit_dependencies(units, layout, codegen);
    let unit_lanes = units.iter().map(|unit| unit.lane).collect::<Vec<_>>();
    let classes = units
        .iter()
        .map(|unit| sparse_class(&unit.unit))
        .collect::<Vec<_>>();
    let publish_after = publication_boundaries(&unit_lanes, &predecessors);
    let tasks = form_lane_tasks_with_options(
        lanes,
        &unit_lanes,
        &predecessors,
        &classes,
        TaskFormationOptions {
            publish_after: &publish_after,
            ..TaskFormationOptions::default()
        },
    )
    .map_err(ParallelPlanError::Lanes)?;
    verify_lane_tasks(lanes, &tasks, &predecessors)
        .map_err(|(predecessor, unit)| ParallelPlanError::Unordered { predecessor, unit })?;
    Ok(tasks)
}

// A consumer cannot start before the latest producer on each other lane.
// Earlier publications only help when another consumer needs them sooner.
fn publication_boundaries(unit_lanes: &[u32], predecessors: &[Vec<usize>]) -> Vec<bool> {
    let mut publish = vec![false; unit_lanes.len()];
    let mut latest = std::collections::BTreeMap::<u32, usize>::new();
    for (consumer, dependencies) in predecessors.iter().enumerate() {
        latest.clear();
        for &producer in dependencies {
            let lane = unit_lanes[producer];
            if lane != unit_lanes[consumer] {
                latest
                    .entry(lane)
                    .and_modify(|old| *old = (*old).max(producer))
                    .or_insert(producer);
            }
        }
        for &producer in latest.values() {
            publish[producer] = true;
        }
    }
    publish
}

/// Waits of tasks whose code is final.
///
/// Code generation may rewrite a task's merged SIR (for example by
/// coalescing stores or redirecting staged writes), which can change the
/// bytes it writes. Given the footprint of every compiled task in task
/// order, this recomputes the waits so every task stays one task and every
/// dependency between the final effects is honoured.
pub fn plan_fixed_tasks(
    lanes: u32,
    task_lanes: &[u32],
    footprints: Vec<Footprint>,
) -> Result<Vec<LaneTask>, ParallelPlanError> {
    let predecessors = footprint_dependencies(footprints);
    // Class 3 never shares a task.
    let classes = vec![3; task_lanes.len()];
    let tasks = form_lane_tasks_separated(lanes, task_lanes, &predecessors, &classes)
        .map_err(ParallelPlanError::Lanes)?;
    verify_lane_tasks(lanes, &tasks, &predecessors)
        .map_err(|(predecessor, unit)| ParallelPlanError::Unordered { predecessor, unit })?;
    debug_assert_eq!(tasks.len(), task_lanes.len());
    Ok(tasks)
}

#[cfg(test)]
mod tests;
