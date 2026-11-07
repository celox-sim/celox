//! Partitioned combinational settle (`RuntimeSchema::settle_partitions`):
//! the flag layout, and the flag stores of the units outside the settle.
//!
//! A partition of the settle runs only when its flag is set. The settle's
//! own partitions set the flags of the readers of what they change; every
//! other writer of state the settle reads or writes sets them here, at each
//! store or commit that changes the state, and host writes set them through
//! [`celox_state_layout::SettleLayout`].
use crate::ir::{
    ExecutionUnit, LaidOutProgram, RegionedAbsoluteAddr, RegisterId, RegisterType, SIRInstruction,
    SIROffset, STABLE_REGION,
};
use celox_design::WORKING_REGION;
use celox_sir::native_memory::NativeMemoryOp;
use celox_sir::{BinaryOp, BlockId, SIRTerminator, SIRValue};
use celox_state_layout::{SettleLayout, SettleObject};
use std::collections::BTreeMap;

mod cone;
mod prune;

#[cfg(feature = "host-runtime")]
pub(crate) use cone::clock_cone;
pub(crate) use prune::prune_stale_marks;

/// The flag layout of `program`'s partitioned settle, if it has one.
pub(crate) fn layout(
    program: &crate::ir::OptimizedSir,
    layout: &crate::backend::MemoryLayout,
) -> Option<SettleLayout> {
    let settle = program.runtime.runtime_schema.settle_partitions.as_ref()?;
    let planes = |address| {
        if layout.four_state && layout.is_4states.get(address).copied().unwrap_or(false) {
            2
        } else {
            1
        }
    };
    let mut objects: Vec<SettleObject> = Vec::new();
    for (address, partitions) in &settle.marks {
        let Some(&start) = layout.offsets.get(address) else {
            continue;
        };
        let end = start + layout.plane_size(address) * planes(address);
        objects.push(SettleObject {
            start,
            end,
            partitions: partitions.clone(),
        });
    }
    // Aliases share storage: one entry per home, with every reader.
    objects.sort_unstable_by_key(|object| (object.start, object.end));
    let mut merged: Vec<SettleObject> = Vec::new();
    for object in objects {
        match merged.last_mut() {
            Some(last) if last.start == object.start => {
                last.end = last.end.max(object.end);
                last.partitions.extend(object.partitions);
                last.partitions.sort_unstable();
                last.partitions.dedup();
            }
            _ => merged.push(object),
        }
    }
    let mut settle_layout = SettleLayout::new(
        layout.offsets[&settle.flags],
        settle.partitions as usize,
        merged,
    );
    settle_layout.wait_flags = settle.waits as usize;
    Some(settle_layout)
}

/// Run a settle through `run` and cross-check it ([`SettleLayout::check`]):
/// run it again from the same state with every partition flagged, and panic
/// at the first byte of design state (below `state_end`) the two runs leave
/// differently. The state continues from the first run.
// The backends that cross-check: native and Cranelift.
#[cfg(any(
    feature = "cranelift",
    all(
        feature = "host-runtime",
        any(
            target_arch = "x86_64",
            feature = "arm64-codegen",
            target_arch = "aarch64"
        )
    )
))]
#[cold]
pub(crate) fn run_checked<B, E>(
    backend: &mut B,
    settle: &SettleLayout,
    state_end: usize,
    memory: impl Fn(&mut B) -> &mut [u8],
    mut run: impl FnMut(&mut B) -> Result<(), E>,
) -> Result<(), E> {
    let flags = settle.flags();
    let before = memory(backend).to_vec();
    run(backend)?;
    let partial = memory(backend).to_vec();
    let state = memory(backend);
    state.copy_from_slice(&before);
    state[flags.start..flags.start + settle.partitions].fill(1);
    run(backend)?;
    let full = memory(backend);
    if let Some(offset) = (celox_state_layout::STATE_HEADER_SIZE..state_end)
        .find(|offset| full[*offset] != partial[*offset] && !flags.contains(offset))
    {
        let object = settle
            .objects
            .iter()
            .find(|object| (object.start..object.end).contains(&offset));
        let dirty: Vec<usize> = (0..settle.partitions)
            .filter(|&partition| before[flags.start + partition] != 0)
            .collect();
        panic!(
            "partitioned settle: state byte {offset} is {} after the partitions that ran, {} \
             after all ({}; flagged at entry: {dirty:?} of {})",
            partial[offset],
            full[offset],
            object.map_or_else(
                || "not read or written by the settle".to_string(),
                |object| format!(
                    "object {}..{} of partitions {:?}",
                    object.start, object.end, object.partitions
                )
            ),
            settle.partitions
        );
    }
    full.copy_from_slice(&partial);
    Ok(())
}

/// A kernel write of a data home takes the clock cone when the partitions
/// it dirties, closed over publishing, are at most this share of all.
const CONE_SOURCE_SHARE: usize = 8;

/// The state homes whose writes by a kernel dirty only the clock cone of a
/// partitioned settle ([`crate::settle::clock_cone`]): the event signals,
/// the time slots, and the settle objects the kernels store to whose
/// readers' closure under publishing is small ([`CONE_SOURCE_SHARE`]). The
/// marks of such a write set the scheduler's dirty byte to 2, "only cone
/// sources changed" (ORed, so that a write of anything else in the same
/// pass keeps the whole settle); the scheduler's plan seeds the cone with
/// the readers of the same homes. Computed once the settle is final and
/// kept in [`LaidOutProgram::cone_homes`].
pub(crate) fn cone_source_homes(program: &LaidOutProgram) -> crate::HashSet<usize> {
    let layout = program.layout();
    let schema = &program.runtime.runtime_schema;
    let home = |address: &crate::AbsoluteAddr| layout.offsets.get(address).copied();
    let mut homes: crate::HashSet<usize> = crate::edge_plans::event_addresses(program)
        .iter()
        .chain(
            schema
                .scheduler
                .iter()
                .flat_map(|scheduler| &scheduler.events),
        )
        .chain(&schema.time_slots)
        .filter_map(home)
        .collect();
    let (Some(settle), Some(partitioning)) = (layout.settle.as_ref(), &schema.settle_partitions)
    else {
        return homes;
    };
    let flags = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, partitioning.flags);
    let Some(marks) = cone::publish_marks(
        &program.sir.eval_comb,
        &flags,
        settle.partitions,
        &layout.offsets,
    ) else {
        return homes;
    };
    let mut written: Vec<usize> = Vec::new();
    for unit in program.sir.processes.iter().take(schema.processes.len()) {
        for instruction in unit.blocks.values().flat_map(|block| &block.instructions) {
            for write in instruction.state_writes().objects().into_iter().flatten() {
                if write.region == STABLE_REGION
                    && let Some(home) = home(&write.absolute_addr())
                    && !homes.contains(&home)
                {
                    written.push(home);
                }
            }
        }
    }
    written.sort_unstable();
    written.dedup();
    for home in written {
        let Ok(index) = settle
            .objects
            .binary_search_by_key(&home, |object| object.start)
        else {
            continue;
        };
        let seeds = settle.objects[index].partitions.iter().map(|&p| p as usize);
        let reached = cone::closure(&marks, seeds)
            .iter()
            .filter(|&&reached| reached)
            .count();
        if reached * CONE_SOURCE_SHARE <= settle.partitions {
            homes.insert(home);
        }
    }
    homes
}

/// Add the flag stores to every unit outside the settle.
pub(crate) fn instrument(program: &mut LaidOutProgram) {
    instrument_processes_from(program, 0, false);
    instrument_ff_units(program, false);
}

/// Add the flag stores to the processes from index `first` on, and to the
/// other units outside the settle when `all`.
pub(crate) fn instrument_processes_from(program: &mut LaidOutProgram, first: usize, all: bool) {
    instrument_units(program, first, all, false);
}

/// Add the flag stores to the FF units. With `wide_commits`, the units
/// commit runs of registers that are contiguous in both the working and the
/// stable region word by word, comparing a whole word before marking the
/// readers of the registers it holds (see [`Marker::wide_run`]); the words
/// are raw state accesses ([`NativeMemoryOp`]) that only a native backend
/// compiles, and that every analysis takes for an access of any state, so a
/// builder instruments the FF units after its dead-store elimination.
pub(crate) fn instrument_ff_units(program: &mut LaidOutProgram, wide_commits: bool) {
    let (sir, layout, runtime) = program.split_mut();
    let Some(mut marker) = Marker::new(layout, runtime) else {
        return;
    };
    // Four-state objects have a second plane elsewhere.
    marker.wide = wide_commits && !layout.four_state;
    if marker.wide {
        marker.collect_homes();
        // The frontend splits the commits of a trigger over many small
        // units: joined, they form the runs the words commit.
        for units in sir.apply_ffs.values_mut() {
            merge_straight_units(units);
        }
    }
    let mut stats = WideStats::default();
    for unit in sir
        .eval_apply_ffs
        .values_mut()
        .chain(sir.eval_comb_apply_ffs.values_mut())
        .chain(sir.eval_only_ffs.values_mut())
        .chain(sir.apply_ffs.values_mut())
        .flatten()
    {
        stats.add(&marker.instrument(unit));
    }
    if marker.wide {
        tracing::debug!(
            "[wide-commits] registers {} in {} words, {} group tests; {} registers commit alone",
            stats.registers,
            stats.words,
            stats.tests,
            stats.single
        );
    }
}

fn instrument_units(program: &mut LaidOutProgram, first: usize, all: bool, wide_commits: bool) {
    // The scheduler schema lists its events only once the scheduler is
    // attached, after the kernels are instrumented: take the cone sources
    // from the design, as the scheduler's plan does.
    let cone_homes = match &program.cone_homes {
        Some(homes) => homes.clone(),
        None => {
            let homes = cone_source_homes(program);
            program.cone_homes = Some(homes.clone());
            homes
        }
    };
    let (sir, layout, runtime) = program.split_mut();
    let Some(mut marker) = Marker::new(layout, runtime) else {
        return;
    };
    marker.clock_homes.extend(cone_homes);
    debug_assert!(
        !wide_commits,
        "wide commits are the FF units' (instrument_ff_units)"
    );
    if all {
        for unit in sir
            .eval_apply_ffs
            .values_mut()
            .chain(sir.eval_comb_apply_ffs.values_mut())
            .chain(sir.eval_only_ffs.values_mut())
            .chain(sir.apply_ffs.values_mut())
            .flatten()
        {
            marker.instrument(unit);
        }
    }
    let scheduler = runtime
        .runtime_schema
        .scheduler
        .as_ref()
        .and_then(|scheduler| scheduler.unit);
    for (index, unit) in sir.processes.iter_mut().enumerate().skip(first) {
        // Its inlined units already own their flags. The generator marks only
        // its new glue stores, so marking the whole unit would duplicate them.
        if scheduler != Some(index) {
            marker.instrument(unit);
        }
    }
}

/// The instructions of `unit` in order when it is a chain of blocks
/// without parameters joined by jumps that ends in a return (blocks the
/// chain does not reach are dead), `None` when it branches.
fn straight_instructions(
    unit: &ExecutionUnit<RegionedAbsoluteAddr>,
) -> Option<Vec<SIRInstruction<RegionedAbsoluteAddr>>> {
    let mut instructions = Vec::new();
    let mut block = unit.entry_block_id;
    for _ in 0..unit.blocks.len() {
        let current = unit.blocks.get(&block)?;
        if !current.params.is_empty() {
            return None;
        }
        instructions.extend(current.instructions.iter().cloned());
        match &current.terminator {
            SIRTerminator::Return => return Some(instructions),
            SIRTerminator::Jump(next, args) if args.is_empty() => block = *next,
            _ => return None,
        }
    }
    None
}

/// Join the consecutive straight units of `units` (see
/// [`straight_instructions`]) into one unit each, in order: the units run
/// one after the other wherever they run, and the commits of a trigger,
/// which the frontend splits over many small units, then form the runs
/// the words commit.
fn merge_straight_units(units: &mut Vec<ExecutionUnit<RegionedAbsoluteAddr>>) {
    let straight: Vec<Option<Vec<SIRInstruction<RegionedAbsoluteAddr>>>> =
        units.iter().map(straight_instructions).collect();
    if straight.iter().filter(|unit| unit.is_some()).count() < 2 {
        return;
    }
    let mut merged: Vec<ExecutionUnit<RegionedAbsoluteAddr>> = Vec::new();
    let mut open: Option<ExecutionUnit<RegionedAbsoluteAddr>> = None;
    for (unit, instructions) in units.drain(..).zip(straight) {
        let Some(instructions) = instructions else {
            merged.extend(open.take());
            merged.push(unit);
            continue;
        };
        let joined = open.get_or_insert_with(|| {
            let entry = BlockId(0);
            let mut joined = ExecutionUnit {
                entry_block_id: entry,
                blocks: crate::HashMap::default(),
                register_map: crate::HashMap::default(),
            };
            insert_block(&mut joined, entry, Vec::new(), SIRTerminator::Return);
            joined
        });
        let base = joined
            .register_map
            .keys()
            .map(|register| register.0 + 1)
            .max()
            .unwrap_or(0);
        for (register, ty) in unit.register_map {
            joined
                .register_map
                .insert(RegisterId(register.0 + base), ty);
        }
        let block = joined
            .blocks
            .get_mut(&joined.entry_block_id)
            .expect("the joined unit has its entry block");
        for mut instruction in instructions {
            instruction.for_each_use_mut(|register| register.0 += base);
            if let Some(defined) = instruction.defined_register_mut() {
                defined.0 += base;
            }
            block.instructions.push(instruction);
        }
    }
    merged.extend(open);
    *units = merged;
}

#[cfg(feature = "host-runtime")]
/// Whether `instruction` is a raw state word of a wide commit
/// ([`Marker::emit_wide_run`]): the only raw state access a unit outside
/// the compiled scheduler makes, shadowed by typed commits of the registers
/// it touches.
pub(crate) fn is_wide_commit_access(instruction: &SIRInstruction<RegionedAbsoluteAddr>) -> bool {
    matches!(
        instruction,
        SIRInstruction::ExternCall { func, .. }
            if matches!(
                NativeMemoryOp::decode(*func),
                Some(NativeMemoryOp::ReadState | NativeMemoryOp::WriteState)
            )
    )
}

/// `unit` without the typed shadows of its wide commits: the commits that
/// directly follow a raw state write ([`Marker::emit_wide_run`]). The words
/// already copied those registers, so a unit that runs as compiled code
/// only needs the shadows for the analyses that plan it.
#[cfg(feature = "host-runtime")]
pub(crate) fn without_shadow_commits(
    unit: &ExecutionUnit<RegionedAbsoluteAddr>,
) -> std::borrow::Cow<'_, ExecutionUnit<RegionedAbsoluteAddr>> {
    let is_write = |instruction: &SIRInstruction<RegionedAbsoluteAddr>| {
        matches!(
            instruction,
            SIRInstruction::ExternCall { func, .. }
                if NativeMemoryOp::decode(*func) == Some(NativeMemoryOp::WriteState)
        )
    };
    let shadows = |instructions: &[SIRInstruction<RegionedAbsoluteAddr>]| {
        instructions
            .windows(2)
            .any(|pair| is_write(&pair[0]) && matches!(pair[1], SIRInstruction::Commit(..)))
    };
    if !unit
        .blocks
        .values()
        .any(|block| shadows(&block.instructions))
    {
        return std::borrow::Cow::Borrowed(unit);
    }
    let mut stripped = unit.clone();
    for block in stripped.blocks.values_mut() {
        let mut after_write = false;
        block.instructions.retain(|instruction| {
            let shadow = after_write && matches!(instruction, SIRInstruction::Commit(..));
            after_write = shadow || is_write(instruction);
            !shadow
        });
    }
    std::borrow::Cow::Owned(stripped)
}

/// Mark only stores introduced by the compiled scheduler itself. Inlined
/// kernel/FF units were already instrumented; the settle manages its own flags.
#[cfg(feature = "host-runtime")]
pub(crate) fn instrument_generated_glue(
    program: &LaidOutProgram,
    unit: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    blocks: &crate::HashSet<celox_sir::BlockId>,
) {
    if let Some(marker) = Marker::new(program.layout(), program.runtime()) {
        marker.instrument_blocks(unit, |block| blocks.contains(&block));
    }
}

/// Apply the same process instrumentation after a deferred native kernel is
/// expanded. Its reserved process entry contained no stores to instrument.
#[cfg(feature = "host-runtime")]
pub(crate) fn instrument_unit(
    program: &LaidOutProgram,
    unit: &mut ExecutionUnit<RegionedAbsoluteAddr>,
) {
    if let Some(marker) = Marker::new(program.layout(), program.runtime()) {
        marker.instrument(unit);
    }
}

struct Marker<'a> {
    settle: &'a SettleLayout,
    layout: &'a crate::backend::MemoryLayout,
    flags: RegionedAbsoluteAddr,
    /// Reuse each state-change predicate for the timed scheduler's settle
    /// guard, instead of comparing the instrumented writer a second time.
    scheduler_dirty: Option<RegionedAbsoluteAddr>,
    /// The homes of the cone sources ([`cone_source_homes`]): a write of
    /// one marks the scheduler's settle dirty with 2, "only cone sources
    /// changed", which runs the clock cone instead of the whole settle.
    clock_homes: crate::HashSet<usize>,
    /// Commit contiguous registers word by word ([`Self::wide_run`]).
    wide: bool,
    /// The bytes of every object in the stable and the working region,
    /// `(start, end)` sorted by start, for [`Self::owned`].
    homes: Vec<(usize, usize)>,
}

/// One register of a run of contiguous whole-object commits.
struct WideObject {
    /// Its commit, kept after the wide commit as the typed shadow of the raw
    /// words ([`without_shadow_commits`]).
    commit: SIRInstruction<RegionedAbsoluteAddr>,
    /// Byte offset of its stable home in the state image.
    stable: usize,
    /// Byte offset of its working home in the state image.
    working: usize,
    bytes: usize,
    /// The partitions reading it.
    readers: Vec<u32>,
}

/// How a write marks the scheduler's dirty byte
/// ([`Marker::mark_unconditionally`]).
#[derive(Clone, Copy)]
enum DirtyMark {
    /// Store 1.
    Data,
    /// OR 2 into the byte, through the registers `old` and `ored`.
    Cone {
        two: RegisterId,
        old: RegisterId,
        ored: RegisterId,
    },
}

/// The open segment of a block being instrumented: the block is split at
/// every compared store.
struct Segment {
    /// The block the next instructions go to.
    current: BlockId,
    /// The byte 1 of the segment chain's marks, once defined.
    one: Option<RegisterId>,
    /// The byte 2 of the chain's clock-only dirty marks, once defined.
    two: Option<RegisterId>,
    instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
}

impl Segment {
    fn one(
        &mut self,
        unit: &mut ExecutionUnit<RegionedAbsoluteAddr>,
        alloc: &mut Allocator,
    ) -> RegisterId {
        *self.one.get_or_insert_with(|| {
            let register = alloc.register(unit, byte_type());
            self.instructions
                .push(SIRInstruction::Imm(register, SIRValue::new(1u64)));
            register
        })
    }

    fn two(
        &mut self,
        unit: &mut ExecutionUnit<RegionedAbsoluteAddr>,
        alloc: &mut Allocator,
    ) -> RegisterId {
        *self.two.get_or_insert_with(|| {
            let register = alloc.register(unit, byte_type());
            self.instructions
                .push(SIRInstruction::Imm(register, SIRValue::new(2u64)));
            register
        })
    }

    /// End the segment with a branch on `cond` to `taken`, and open the
    /// block the other arm continues in, which is returned.
    fn split(
        &mut self,
        unit: &mut ExecutionUnit<RegionedAbsoluteAddr>,
        alloc: &mut Allocator,
        cond: RegisterId,
        taken: BlockId,
    ) -> BlockId {
        let rest = alloc.block();
        let block = unit.blocks.get_mut(&self.current).expect("split block");
        block.instructions = std::mem::take(&mut self.instructions);
        block.terminator = SIRTerminator::Branch {
            cond,
            true_block: (taken, Vec::new()),
            false_block: (rest, Vec::new()),
        };
        insert_block(unit, rest, Vec::new(), SIRTerminator::Return);
        self.current = rest;
        rest
    }
}

/// What the wide commits of the instrumented units cover.
#[derive(Default)]
struct WideStats {
    /// Registers committed by words.
    registers: usize,
    words: usize,
    /// Reader-group tests in the mark blocks of the words.
    tests: usize,
    /// Registers committed one by one.
    single: usize,
}

impl WideStats {
    fn add(&mut self, other: &Self) {
        self.registers += other.registers;
        self.words += other.words;
        self.tests += other.tests;
        self.single += other.single;
    }
}

/// Allocates the registers and blocks an instrumented unit grows by.
struct Allocator {
    next_register: usize,
    next_block: usize,
}

impl Allocator {
    fn new(unit: &ExecutionUnit<RegionedAbsoluteAddr>) -> Self {
        Self {
            next_register: unit
                .register_map
                .keys()
                .map(|register| register.0 + 1)
                .max()
                .unwrap_or(0),
            next_block: unit
                .blocks
                .keys()
                .map(|block| block.0 + 1)
                .max()
                .unwrap_or(0),
        }
    }

    fn register(
        &mut self,
        unit: &mut ExecutionUnit<RegionedAbsoluteAddr>,
        ty: RegisterType,
    ) -> RegisterId {
        let register = RegisterId(self.next_register);
        self.next_register += 1;
        unit.register_map.insert(register, ty);
        register
    }

    fn block(&mut self) -> BlockId {
        let block = BlockId(self.next_block);
        self.next_block += 1;
        block
    }
}

fn byte_type() -> RegisterType {
    RegisterType::Bit {
        width: 8,
        signed: false,
    }
}

fn word_type() -> RegisterType {
    RegisterType::Bit {
        width: 64,
        signed: false,
    }
}

fn bit_type() -> RegisterType {
    RegisterType::Bit {
        width: 1,
        signed: false,
    }
}

/// The mask of `bytes` bytes from byte `first` of a word.
fn byte_mask(first: usize, bytes: usize) -> u64 {
    let mask = if bytes >= 8 {
        u64::MAX
    } else {
        (1u64 << (bytes * 8)) - 1
    };
    mask << (first * 8)
}

/// Insert a block ending in `terminator` into `unit`.
fn insert_block(
    unit: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    id: BlockId,
    instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
    terminator: SIRTerminator,
) {
    unit.blocks.insert(
        id,
        celox_sir::BasicBlock {
            id,
            params: Vec::new(),
            instructions,
            terminator,
        },
    );
}

impl<'a> Marker<'a> {
    fn new(
        layout: &'a crate::backend::MemoryLayout,
        runtime: &'a crate::ir::RuntimeProgram,
    ) -> Option<Self> {
        let settle = layout.settle.as_ref()?;
        let schema = runtime.runtime_schema.settle_partitions.as_ref()?;
        Some(Self {
            settle,
            layout,
            flags: RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, schema.flags),
            scheduler_dirty: runtime.runtime_schema.scheduler.as_ref().map(|scheduler| {
                RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, scheduler.slots.dirty)
            }),
            clock_homes: runtime
                .runtime_schema
                .scheduler
                .iter()
                .flat_map(|scheduler| scheduler.events.iter())
                .chain(runtime.runtime_schema.time_slots.iter())
                .filter_map(|address| layout.offsets.get(address).copied())
                .collect(),
            wide: false,
            homes: Vec::new(),
        })
    }

    /// Fill [`Self::homes`], which only the wide commits need.
    fn collect_homes(&mut self) {
        let planes = |address| {
            if self.layout.four_state
                && self
                    .layout
                    .is_4states
                    .get(address)
                    .copied()
                    .unwrap_or(false)
            {
                2
            } else {
                1
            }
        };
        let mut homes: Vec<(usize, usize)> = self
            .layout
            .offsets
            .iter()
            .map(|(address, &start)| {
                (
                    start,
                    start + self.layout.plane_size(address) * planes(address),
                )
            })
            .chain(self.layout.working_offsets.iter().map(|(address, &start)| {
                let start = self.layout.working_base_offset + start;
                (
                    start,
                    start + self.layout.plane_size(address) * planes(address),
                )
            }))
            .filter(|(start, end)| start < end)
            .collect();
        homes.sort_unstable();
        homes.dedup();
        // Overlapping homes (aliases) must not hide each other from the
        // binary search: merge them. Touching homes stay apart.
        let mut merged: Vec<(usize, usize)> = Vec::with_capacity(homes.len());
        for (start, end) in homes {
            match merged.last_mut() {
                Some(last) if start < last.1 => last.1 = last.1.max(end),
                _ => merged.push((start, end)),
            }
        }
        self.homes = merged;
    }

    /// The partitions reading the stable state `address` names.
    fn readers(&self, address: &RegionedAbsoluteAddr) -> Vec<u32> {
        if address.region != STABLE_REGION {
            return Vec::new();
        }
        let Some(&start) = self.layout.offsets.get(&address.absolute_addr()) else {
            return Vec::new();
        };
        let index = self
            .settle
            .objects
            .partition_point(|object| object.start < start);
        match self.settle.objects.get(index) {
            Some(object) if object.start == start => object.partitions.clone(),
            _ => Vec::new(),
        }
    }

    /// Whether an access covers more than one element of an element-strided
    /// array whose elements are padded: a load of it would not gather the
    /// elements on every backend, so the store cannot be compared.
    fn spans_padded_elements(
        &self,
        address: &RegionedAbsoluteAddr,
        offset: &SIROffset,
        width: usize,
    ) -> bool {
        let Some(array) = self.layout.unpacked_arrays.get(&address.absolute_addr()) else {
            return false;
        };
        if array.element_stride * 8 == array.element_width {
            return false;
        }
        match offset.constant_bit_offset() {
            Some(start) => {
                start / array.element_width != (start + width.max(1) - 1) / array.element_width
            }
            None => width > array.element_width,
        }
    }

    /// Mark `readers` with the byte `one`, and the scheduler's settle as
    /// dirty, whatever the write before changed: with 1, or, for a write of
    /// a cone source, with 2 ORed into the byte, so that a pass writing
    /// anything else as well settles the whole design.
    fn mark_unconditionally(
        &self,
        instructions: &mut Vec<SIRInstruction<RegionedAbsoluteAddr>>,
        one: RegisterId,
        dirty_value: DirtyMark,
        readers: Vec<u32>,
    ) {
        if let Some(dirty) = self.scheduler_dirty {
            let value = match dirty_value {
                DirtyMark::Data => one,
                DirtyMark::Cone { two, old, ored } => {
                    instructions.push(SIRInstruction::Load(old, dirty, SIROffset::Static(0), 8));
                    instructions.push(SIRInstruction::Binary(ored, old, BinaryOp::Or, two));
                    ored
                }
            };
            instructions.push(SIRInstruction::Store(
                dirty,
                SIROffset::Static(0),
                8,
                value,
                Vec::new(),
                Vec::new(),
            ));
        }
        self.mark_flags(instructions, one, &readers);
    }

    /// The dirty mark of a write of `address`: of a cone source, with fresh
    /// registers for the OR ([`Self::mark_unconditionally`]).
    fn dirty_mark(
        &self,
        unit: &mut ExecutionUnit<RegionedAbsoluteAddr>,
        alloc: &mut Allocator,
        segment: &mut Segment,
        address: &RegionedAbsoluteAddr,
    ) -> DirtyMark {
        if self.is_clock_home(address) {
            DirtyMark::Cone {
                two: segment.two(unit, alloc),
                old: alloc.register(unit, byte_type()),
                ored: alloc.register(unit, byte_type()),
            }
        } else {
            DirtyMark::Data
        }
    }

    fn instrument(&self, unit: &mut ExecutionUnit<RegionedAbsoluteAddr>) -> WideStats {
        self.instrument_blocks(unit, |_| true)
    }

    fn instrument_blocks(
        &self,
        unit: &mut ExecutionUnit<RegionedAbsoluteAddr>,
        include: impl Fn(celox_sir::BlockId) -> bool,
    ) -> WideStats {
        let mut stats = WideStats::default();
        let mut alloc = Allocator::new(unit);
        let block_ids: Vec<_> = unit
            .blocks
            .keys()
            .copied()
            .filter(|block| include(*block))
            .collect();
        for block_id in block_ids {
            let block = unit.blocks.get_mut(&block_id).expect("listed block");
            let old_instructions = std::mem::take(&mut block.instructions);
            let terminator = std::mem::replace(&mut block.terminator, SIRTerminator::Return);
            // The block is split after each compared store that marks: the
            // marks run only when the store changed the state.
            let mut segment = Segment {
                current: block_id,
                one: None,
                two: None,
                instructions: Vec::with_capacity(old_instructions.len()),
            };
            let mut index = 0;
            while index < old_instructions.len() {
                if self.wide {
                    let run = self.wide_run(&old_instructions[index..]);
                    if run.len() >= 2 {
                        index += run.len();
                        for segment_objects in self.segments(run) {
                            if segment_objects.len() >= 2 {
                                stats.registers += segment_objects.len();
                                let (words, tests) = self.emit_wide_run(
                                    unit,
                                    &mut alloc,
                                    &mut segment,
                                    &segment_objects,
                                );
                                stats.words += words;
                                stats.tests += tests;
                            } else {
                                for object in segment_objects {
                                    stats.single += 1;
                                    self.emit_instruction(
                                        unit,
                                        &mut alloc,
                                        &mut segment,
                                        object.commit,
                                    );
                                }
                            }
                        }
                        continue;
                    }
                }
                if self.wide && matches!(old_instructions[index], SIRInstruction::Commit(..)) {
                    stats.single += 1;
                }
                self.emit_instruction(
                    unit,
                    &mut alloc,
                    &mut segment,
                    old_instructions[index].clone(),
                );
                index += 1;
            }
            let block = unit.blocks.get_mut(&segment.current).expect("last segment");
            block.instructions = segment.instructions;
            block.terminator = terminator;
        }
        stats
    }

    /// Append `instruction` to the open segment with the marks of the
    /// readers of what it writes, splitting the segment at a compared store.
    fn emit_instruction(
        &self,
        unit: &mut ExecutionUnit<RegionedAbsoluteAddr>,
        alloc: &mut Allocator,
        segment: &mut Segment,
        instruction: SIRInstruction<RegionedAbsoluteAddr>,
    ) {
        // A call may write any part of the objects its effects list, and its
        // callee's stores are not instrumented: it marks the readers of all
        // of them whenever it runs, as a store too wide to compare does. The
        // listed writes of a call that reaches a native kernel are
        // incomplete, but a native kernel's own unit is instrumented
        // (`instrument_unit`).
        if let Some(effects) = instruction.call_effects() {
            let mut readers: Vec<u32> = effects
                .writes
                .iter()
                .flat_map(|address| self.readers(address))
                .collect();
            readers.sort_unstable();
            readers.dedup();
            segment.instructions.push(instruction);
            if readers.is_empty() {
                return;
            }
            let one = segment.one(unit, alloc);
            self.mark_unconditionally(&mut segment.instructions, one, DirtyMark::Data, readers);
            return;
        }
        let target = match &instruction {
            SIRInstruction::Store(address, offset, width, ..)
            | SIRInstruction::Commit(_, address, offset, width, _) => {
                Some((*address, offset.clone(), *width))
            }
            _ => None,
        };
        let Some((address, offset, width)) = target else {
            segment.instructions.push(instruction);
            return;
        };
        let readers = self.readers(&address);
        if readers.is_empty() {
            segment.instructions.push(instruction);
            return;
        }
        let one = segment.one(unit, alloc);
        let dirty_value = self.dirty_mark(unit, alloc, segment, &address);
        if width > MAX_COMPARED_WIDTH || self.spans_padded_elements(&address, &offset, width) {
            segment.instructions.push(instruction);
            self.mark_unconditionally(&mut segment.instructions, one, dirty_value, readers);
            return;
        }
        // Set the readers' flags only when the store changes the state, both
        // planes compared.
        let old = alloc.register(unit, RegisterType::Logic { width });
        segment
            .instructions
            .push(SIRInstruction::Load(old, address, offset.clone(), width));
        segment.instructions.push(instruction);
        let new = alloc.register(unit, RegisterType::Logic { width });
        segment
            .instructions
            .push(SIRInstruction::Load(new, address, offset, width));
        let changed = alloc.register(unit, bit_type());
        segment
            .instructions
            .push(SIRInstruction::Binary(changed, old, BinaryOp::NeCase, new));
        // Most stores leave the state as it was: branch over the marks rather
        // than OR the predicate into every reader's flag.
        let mark = alloc.block();
        let rest = segment.split(unit, alloc, changed, mark);
        let mut marks = Vec::new();
        self.mark_unconditionally(&mut marks, one, dirty_value, readers);
        insert_block(unit, mark, marks, SIRTerminator::Jump(rest, Vec::new()));
    }

    /// Whether a write of `address` is a write of a cone source
    /// ([`cone_source_homes`]).
    fn is_clock_home(&self, address: &RegionedAbsoluteAddr) -> bool {
        address.region == STABLE_REGION
            && self
                .layout
                .offsets
                .get(&address.absolute_addr())
                .is_some_and(|home| self.clock_homes.contains(home))
    }

    /// The run of whole-object commits `instructions` starts with whose
    /// working homes lie at one distance from their stable homes, so that
    /// words of the two regions correspond. A register ends the run at a
    /// commit with triggers, a partial or dynamic one, an array with its own
    /// element layout, a copy-on-write (sparse) target, or a home at another
    /// distance.
    fn wide_run(&self, instructions: &[SIRInstruction<RegionedAbsoluteAddr>]) -> Vec<WideObject> {
        let mut run: Vec<WideObject> = Vec::new();
        for instruction in instructions {
            let SIRInstruction::Commit(source, destination, SIROffset::Static(0), width, triggers) =
                instruction
            else {
                break;
            };
            if !triggers.is_empty()
                || source.region != WORKING_REGION
                || destination.region != STABLE_REGION
                || source.absolute_addr() != destination.absolute_addr()
            {
                break;
            }
            let address = destination.absolute_addr();
            if self.layout.widths.get(&address) != Some(width)
                || self.layout.unpacked_arrays.contains_key(&address)
                || self.layout.sparse_offsets.contains_key(&address)
            {
                break;
            }
            let (Some(&stable), Some(&working)) = (
                self.layout.offsets.get(&address),
                self.layout.working_offsets.get(&address),
            ) else {
                break;
            };
            let working = self.layout.working_base_offset + working;
            let bytes = self.layout.plane_size(&address);
            if bytes == 0 {
                break;
            }
            if let Some(first) = run.first()
                && working.wrapping_sub(stable) != first.working.wrapping_sub(first.stable)
            {
                break;
            }
            run.push(WideObject {
                commit: instruction.clone(),
                stable,
                working,
                bytes,
                readers: self.readers(destination),
            });
        }
        run
    }

    /// `run` in home order, cut wherever the bytes between two neighbours
    /// belong to another object in either region: the words of a segment
    /// copy the bytes between its registers too, which must be padding.
    fn segments(&self, mut run: Vec<WideObject>) -> Vec<Vec<WideObject>> {
        run.sort_unstable_by_key(|object| object.stable);
        let mut segments: Vec<Vec<WideObject>> = Vec::new();
        for object in run {
            if let Some(segment) = segments.last_mut()
                && let Some(last) = segment.last()
                && !self.owned(last.stable + last.bytes, object.stable)
                && !self.owned(last.working + last.bytes, object.working)
            {
                segment.push(object);
            } else {
                segments.push(vec![object]);
            }
        }
        segments
    }

    /// Whether any object has bytes in `lo..hi` of the state image.
    fn owned(&self, lo: usize, hi: usize) -> bool {
        if lo >= hi {
            return false;
        }
        let index = self.homes.partition_point(|&(_, end)| end <= lo);
        self.homes.get(index).is_some_and(|&(start, _)| start < hi)
    }

    /// Commit `run` word by word: each word of the working homes replaces
    /// the word of the stable homes, and when the two differed in a register
    /// that partitions read, the readers of every such register whose bytes
    /// differ are marked. Registers with the same readers share a test.
    ///
    /// The raw words are opaque to every analysis, so the run's commits
    /// follow the last word as its typed shadow: they name the registers
    /// the words wrote (the scheduler plans with them) and copy what the
    /// words already copied. The compiled scheduler drops them
    /// ([`without_shadow_commits`]).
    fn emit_wide_run(
        &self,
        unit: &mut ExecutionUnit<RegionedAbsoluteAddr>,
        alloc: &mut Allocator,
        segment: &mut Segment,
        run: &[WideObject],
    ) -> (usize, usize) {
        let stable_base = run[0].stable;
        let working_base = run[0].working;
        let last = run.last().expect("a run has registers");
        let total = last.stable + last.bytes - stable_base;
        let mut offset = 0;
        let (mut words, mut tests) = (0, 0);
        while offset < total {
            words += 1;
            let len = (total - offset).min(8);
            let window = byte_mask(0, len);
            // The readers of each register with bytes in the window, with
            // the mask of those bytes.
            let mut groups: BTreeMap<&[u32], u64> = BTreeMap::new();
            for object in run {
                let position = object.stable - stable_base;
                let start = position.max(offset);
                let end = (position + object.bytes).min(offset + len);
                if start >= end || object.readers.is_empty() {
                    continue;
                }
                *groups.entry(object.readers.as_slice()).or_default() |=
                    byte_mask(start - offset, end - start);
            }
            let watched = groups.values().fold(0u64, |all, mask| all | mask);

            let working_address = alloc.register(unit, word_type());
            segment.instructions.push(SIRInstruction::Imm(
                working_address,
                SIRValue::new((working_base + offset) as u64),
            ));
            let working = alloc.register(unit, word_type());
            segment.instructions.push(SIRInstruction::ExternCall {
                dst: Some(working),
                func: NativeMemoryOp::ReadState.function(),
                args: vec![working_address],
            });
            let stable_address = alloc.register(unit, word_type());
            segment.instructions.push(SIRInstruction::Imm(
                stable_address,
                SIRValue::new((stable_base + offset) as u64),
            ));
            let stable = alloc.register(unit, word_type());
            segment.instructions.push(SIRInstruction::ExternCall {
                dst: Some(stable),
                func: NativeMemoryOp::ReadState.function(),
                args: vec![stable_address],
            });
            let mut differing = alloc.register(unit, word_type());
            segment.instructions.push(SIRInstruction::Binary(
                differing,
                working,
                BinaryOp::Xor,
                stable,
            ));
            let new = if len < 8 {
                // The bytes after the run keep their stable value.
                let mask = alloc.register(unit, word_type());
                segment
                    .instructions
                    .push(SIRInstruction::Imm(mask, SIRValue::new(window)));
                let masked = alloc.register(unit, word_type());
                segment.instructions.push(SIRInstruction::Binary(
                    masked,
                    differing,
                    BinaryOp::And,
                    mask,
                ));
                differing = masked;
                let new = alloc.register(unit, word_type());
                segment.instructions.push(SIRInstruction::Binary(
                    new,
                    stable,
                    BinaryOp::Xor,
                    differing,
                ));
                new
            } else {
                working
            };
            segment.instructions.push(SIRInstruction::ExternCall {
                dst: None,
                func: NativeMemoryOp::WriteState.function(),
                args: vec![stable_address, new],
            });
            offset += len;
            if offset == total {
                segment
                    .instructions
                    .extend(run.iter().map(|object| object.commit.clone()));
            }
            if groups.is_empty() {
                continue;
            }
            tests += groups.len();
            let zero = alloc.register(unit, word_type());
            segment
                .instructions
                .push(SIRInstruction::Imm(zero, SIRValue::new(0u64)));
            let tested = if watched == window {
                differing
            } else {
                let mask = alloc.register(unit, word_type());
                segment
                    .instructions
                    .push(SIRInstruction::Imm(mask, SIRValue::new(watched)));
                let tested = alloc.register(unit, word_type());
                segment.instructions.push(SIRInstruction::Binary(
                    tested,
                    differing,
                    BinaryOp::And,
                    mask,
                ));
                tested
            };
            let changed = alloc.register(unit, bit_type());
            segment
                .instructions
                .push(SIRInstruction::Binary(changed, tested, BinaryOp::Ne, zero));
            let mark = alloc.block();
            let rest = segment.split(unit, alloc, changed, mark);
            // The mark block: the dirty byte, then each group's flags, the
            // group's bytes tested first unless they are all the tested bytes.
            let mut block = mark;
            let mut body = Vec::new();
            let one = alloc.register(unit, byte_type());
            body.push(SIRInstruction::Imm(one, SIRValue::new(1u64)));
            if let Some(dirty) = self.scheduler_dirty {
                body.push(SIRInstruction::Store(
                    dirty,
                    SIROffset::Static(0),
                    8,
                    one,
                    Vec::new(),
                    Vec::new(),
                ));
            }
            for (readers, mask) in groups {
                if mask == watched {
                    self.mark_flags(&mut body, one, readers);
                    continue;
                }
                let group_mask = alloc.register(unit, word_type());
                body.push(SIRInstruction::Imm(group_mask, SIRValue::new(mask)));
                let group = alloc.register(unit, word_type());
                body.push(SIRInstruction::Binary(
                    group,
                    differing,
                    BinaryOp::And,
                    group_mask,
                ));
                let group_changed = alloc.register(unit, bit_type());
                body.push(SIRInstruction::Binary(
                    group_changed,
                    group,
                    BinaryOp::Ne,
                    zero,
                ));
                let set = alloc.block();
                let next = alloc.block();
                insert_block(
                    unit,
                    block,
                    std::mem::take(&mut body),
                    SIRTerminator::Branch {
                        cond: group_changed,
                        true_block: (set, Vec::new()),
                        false_block: (next, Vec::new()),
                    },
                );
                let mut flags = Vec::new();
                self.mark_flags(&mut flags, one, readers);
                insert_block(unit, set, flags, SIRTerminator::Jump(next, Vec::new()));
                block = next;
            }
            insert_block(unit, block, body, SIRTerminator::Jump(rest, Vec::new()));
        }
        (words, tests)
    }

    /// Set the flags of `readers` to the byte `one`.
    fn mark_flags(
        &self,
        instructions: &mut Vec<SIRInstruction<RegionedAbsoluteAddr>>,
        one: RegisterId,
        readers: &[u32],
    ) {
        for &partition in readers {
            instructions.push(SIRInstruction::Store(
                self.flags,
                SIROffset::Static(partition as usize * 8),
                8,
                one,
                Vec::new(),
                Vec::new(),
            ));
        }
    }
}

/// The widest store compared before it marks the readers; wider ones mark
/// them on every store, like stores across padded array elements.
const MAX_COMPARED_WIDTH: usize = 4096;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{AbsoluteAddr, BasicBlock, BlockId};
    use celox_state_layout::SettleObject;

    /// A unit outside the partitioned settle marks the partitions that read
    /// what a call of it writes, after the call, and only those.
    #[test]
    fn calls_mark_the_partitions_reading_what_they_write() {
        let source = "module Top (a: input logic<8>, b: input logic<8>, f: input logic<8>, y: output logic<8>) { assign y = a + b + f; }";
        let (program, _) = crate::compile_to_sir(
            &[(source, std::path::Path::new("settle-calls.veryl"))],
            "Top",
            &[],
            &[],
            false,
            &Default::default(),
            None,
            None,
            None,
            None,
            &[],
            &Default::default(),
        )
        .unwrap();
        let [a, b, flags] = ["a", "b", "f"].map(|name| program.get_addr(&[], &[name]).unwrap());
        let program = program.into_laid_out(false);
        let layout = program.layout();
        // Partition 0 reads `a`; nothing reads `b`.
        let start = layout.offsets[&a];
        let settle = SettleLayout::new(
            layout.offsets[&flags],
            1,
            vec![SettleObject {
                start,
                end: start + 1,
                partitions: vec![0],
            }],
        );
        let flags = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, flags);
        let marker = Marker {
            settle: &settle,
            layout,
            flags,
            scheduler_dirty: None,
            clock_homes: crate::HashSet::default(),
            wide: false,
            homes: Vec::new(),
        };
        let marks = |writes: Vec<AbsoluteAddr>| {
            let call = SIRInstruction::Call {
                function: celox_sir::FunctionId(0),
                frame: RegisterId(0),
                args: Vec::new(),
                outputs: Vec::new(),
                dst: None,
                effects: std::sync::Arc::new(celox_sir::CallEffects {
                    writes: writes
                        .into_iter()
                        .map(|address| {
                            RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, address)
                        })
                        .collect(),
                    ..Default::default()
                }),
            };
            let mut unit = ExecutionUnit {
                entry_block_id: BlockId(0),
                blocks: [(
                    BlockId(0),
                    BasicBlock {
                        id: BlockId(0),
                        params: Vec::new(),
                        instructions: vec![
                            SIRInstruction::Imm(RegisterId(0), SIRValue::new(0u8)),
                            call,
                        ],
                        terminator: celox_sir::SIRTerminator::Return,
                    },
                )]
                .into_iter()
                .collect(),
                register_map: [(
                    RegisterId(0),
                    RegisterType::Bit {
                        width: 32,
                        signed: false,
                    },
                )]
                .into_iter()
                .collect(),
            };
            marker.instrument(&mut unit);
            let instructions = &unit.blocks[&BlockId(0)].instructions;
            let call = instructions
                .iter()
                .position(|instruction| instruction.call_effects().is_some())
                .unwrap();
            instructions[call..]
                .iter()
                .filter(|instruction| {
                    matches!(instruction, SIRInstruction::Store(address, SIROffset::Static(0), 8, ..)
                        if *address == flags)
                })
                .count()
        };
        assert_eq!(marks(vec![a]), 1);
        assert_eq!(marks(vec![b, a]), 1);
        assert_eq!(marks(vec![b]), 0);
        assert_eq!(marks(Vec::new()), 0);
    }
}
