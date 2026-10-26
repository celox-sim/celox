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
    for unit in sir.processes.iter_mut().skip(first) {
        marker.instrument(unit);
    }
}

struct Marker<'a> {
    settle: &'a SettleLayout,
    layout: &'a crate::backend::MemoryLayout,
    flags: RegionedAbsoluteAddr,
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

    fn instrument(&self, unit: &mut ExecutionUnit<RegionedAbsoluteAddr>) {
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
        let block_ids: Vec<_> = unit.blocks.keys().copied().collect();
        for block_id in block_ids {
            let block = unit.blocks.get_mut(&block_id).expect("listed block");
            let old_instructions = std::mem::take(&mut block.instructions);
            let mut one = None;
            let mut instructions = Vec::with_capacity(old_instructions.len());
            for instruction in old_instructions {
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
                for partition in readers {
                    let flag = alloc(unit, byte.clone());
                    let marked = alloc(unit, byte.clone());
                    let offset = SIROffset::Static(partition as usize * 8);
                    instructions.push(SIRInstruction::Load(flag, self.flags, offset.clone(), 8));
                    instructions.push(SIRInstruction::Mux(marked, changed, one, flag));
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
