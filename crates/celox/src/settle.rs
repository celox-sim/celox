//! Partitioned combinational settle (`RuntimeSchema::settle_partitions`):
//! the flag layout, and the flag stores of the units outside the settle.
//!
//! A partition of the settle runs only when its flag is set. The settle's
//! own partitions set the flags of their readers; every other writer of
//! state the settle reads or writes sets them here, at each store or commit
//! to the state, and host writes set them through
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
    state[flags.start..flags.end - 1].fill(1);
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

    fn instrument(&self, unit: &mut ExecutionUnit<RegionedAbsoluteAddr>) {
        let mut next_register = unit
            .register_map
            .keys()
            .map(|register| register.0 + 1)
            .max()
            .unwrap_or(0);
        for block in unit.blocks.values_mut() {
            let mut one = None;
            let mut instructions = Vec::with_capacity(block.instructions.len());
            for instruction in std::mem::take(&mut block.instructions) {
                let readers = match &instruction {
                    SIRInstruction::Store(address, ..) => self.readers(address),
                    SIRInstruction::Commit(_, address, ..) => self.readers(address),
                    _ => Vec::new(),
                };
                instructions.push(instruction);
                if readers.is_empty() {
                    continue;
                }
                let one = *one.get_or_insert_with(|| {
                    let register = RegisterId(next_register);
                    next_register += 1;
                    unit.register_map.insert(
                        register,
                        RegisterType::Bit {
                            width: 8,
                            signed: false,
                        },
                    );
                    instructions.push(SIRInstruction::Imm(register, SIRValue::new(1u64)));
                    register
                });
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
            block.instructions = instructions;
        }
    }
}
