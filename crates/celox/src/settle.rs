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
use celox_sir::SIRValue;
use celox_state_layout::{SettleLayout, SettleObject};

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
    Some(SettleLayout::new(
        layout.offsets[&settle.flags],
        settle.partitions as usize,
        merged,
    ))
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

/// Add the flag stores to every unit outside the settle.
pub(crate) fn instrument(program: &mut LaidOutProgram) {
    instrument_processes_from(program, 0, true);
}

/// Add the flag stores to the processes from index `first` on, and to the
/// other units outside the settle when `all`.
pub(crate) fn instrument_processes_from(program: &mut LaidOutProgram, first: usize, all: bool) {
    let (sir, layout, runtime) = program.split_mut();
    let Some(marker) = Marker::new(layout, runtime) else {
        return;
    };
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
        })
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

    /// Mark `readers`, and the scheduler's settle as dirty, with the byte
    /// `one`, whatever the write before changed.
    fn mark_unconditionally(
        &self,
        instructions: &mut Vec<SIRInstruction<RegionedAbsoluteAddr>>,
        one: RegisterId,
        readers: Vec<u32>,
    ) {
        if let Some(dirty) = self.scheduler_dirty {
            instructions.push(SIRInstruction::Store(
                dirty,
                SIROffset::Static(0),
                8,
                one,
                Vec::new(),
                Vec::new(),
            ));
        }
        for partition in readers {
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

    fn instrument(&self, unit: &mut ExecutionUnit<RegionedAbsoluteAddr>) {
        self.instrument_blocks(unit, |_| true);
    }

    fn instrument_blocks(
        &self,
        unit: &mut ExecutionUnit<RegionedAbsoluteAddr>,
        include: impl Fn(celox_sir::BlockId) -> bool,
    ) {
        let mut next_register = unit
            .register_map
            .keys()
            .map(|register| register.0 + 1)
            .max()
            .unwrap_or(0);
        let mut alloc = |unit: &mut ExecutionUnit<RegionedAbsoluteAddr>, ty: RegisterType| {
            let register = RegisterId(next_register);
            next_register += 1;
            unit.register_map.insert(register, ty);
            register
        };
        let byte = RegisterType::Bit {
            width: 8,
            signed: false,
        };
        let block_ids: Vec<_> = unit
            .blocks
            .keys()
            .copied()
            .filter(|block| include(*block))
            .collect();
        for block_id in block_ids {
            let block = unit.blocks.get_mut(&block_id).expect("listed block");
            let old_instructions = std::mem::take(&mut block.instructions);
            let mut one = None;
            let mut instructions = Vec::with_capacity(old_instructions.len());
            for instruction in old_instructions {
                // A call may write any part of the objects its effects list,
                // and its callee's stores are not instrumented: it marks the
                // readers of all of them whenever it runs, as a store too
                // wide to compare does. The listed writes of a call that
                // reaches a native kernel are incomplete, but a native
                // kernel's own unit is instrumented (`instrument_unit`).
                if let Some(effects) = instruction.call_effects() {
                    let mut readers: Vec<u32> = effects
                        .writes
                        .iter()
                        .flat_map(|address| self.readers(address))
                        .collect();
                    readers.sort_unstable();
                    readers.dedup();
                    instructions.push(instruction);
                    if readers.is_empty() {
                        continue;
                    }
                    let one = *one.get_or_insert_with(|| {
                        let register = alloc(unit, byte.clone());
                        instructions.push(SIRInstruction::Imm(register, SIRValue::new(1u64)));
                        register
                    });
                    self.mark_unconditionally(&mut instructions, one, readers);
                    continue;
                }
                let target = match &instruction {
                    SIRInstruction::Store(address, offset, width, ..)
                    | SIRInstruction::Commit(_, address, offset, width, _) => {
                        Some((*address, offset.clone(), *width))
                    }
                    _ => None,
                };
                let Some((address, offset, width)) = target else {
                    instructions.push(instruction);
                    continue;
                };
                let readers = self.readers(&address);
                if readers.is_empty() {
                    instructions.push(instruction);
                    continue;
                }
                let one = *one.get_or_insert_with(|| {
                    let register = alloc(unit, byte.clone());
                    instructions.push(SIRInstruction::Imm(register, SIRValue::new(1u64)));
                    register
                });
                if width > MAX_COMPARED_WIDTH
                    || self.spans_padded_elements(&address, &offset, width)
                {
                    instructions.push(instruction);
                    self.mark_unconditionally(&mut instructions, one, readers);
                    continue;
                }
                // Set the readers' flags only when the store changes the
                // state, both planes compared.
                let old = alloc(unit, RegisterType::Logic { width });
                instructions.push(SIRInstruction::Load(old, address, offset.clone(), width));
                instructions.push(instruction);
                let new = alloc(unit, RegisterType::Logic { width });
                instructions.push(SIRInstruction::Load(new, address, offset, width));
                let changed = alloc(
                    unit,
                    RegisterType::Bit {
                        width: 1,
                        signed: false,
                    },
                );
                instructions.push(SIRInstruction::Binary(
                    changed,
                    old,
                    celox_sir::BinaryOp::NeCase,
                    new,
                ));
                if let Some(dirty) = self.scheduler_dirty {
                    let previous = alloc(unit, byte.clone());
                    let pending = alloc(unit, byte.clone());
                    instructions.push(SIRInstruction::Load(
                        previous,
                        dirty,
                        SIROffset::Static(0),
                        8,
                    ));
                    instructions.push(SIRInstruction::Binary(
                        pending,
                        previous,
                        celox_sir::BinaryOp::Or,
                        changed,
                    ));
                    instructions.push(SIRInstruction::Store(
                        dirty,
                        SIROffset::Static(0),
                        8,
                        pending,
                        Vec::new(),
                        Vec::new(),
                    ));
                }
                for partition in readers {
                    let flag = alloc(unit, byte.clone());
                    let marked = alloc(unit, byte.clone());
                    let offset = SIROffset::Static(partition as usize * 8);
                    instructions.push(SIRInstruction::Load(flag, self.flags, offset.clone(), 8));
                    // Flags are boolean bytes. OR preserves a notification
                    // already pending without selecting 1 separately for
                    // every reader of this same state-change predicate.
                    instructions.push(SIRInstruction::Binary(
                        marked,
                        flag,
                        celox_sir::BinaryOp::Or,
                        changed,
                    ));
                    instructions.push(SIRInstruction::Store(
                        self.flags,
                        offset,
                        8,
                        marked,
                        Vec::new(),
                        Vec::new(),
                    ));
                }
            }
            unit.blocks
                .get_mut(&block_id)
                .expect("listed block")
                .instructions = instructions;
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
