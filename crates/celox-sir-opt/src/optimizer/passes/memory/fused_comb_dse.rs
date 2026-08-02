use crate::ir::cfg::SirCfg;
use crate::ir::*;
use crate::{HashMap, HashSet, OptimizationError};

use super::state_ssa::{MemoryAccessKind, MemoryVersionId, StateSsa};

#[derive(Clone, Copy)]
struct StaticRange {
    addr: RegionedAbsoluteAddr,
    start: usize,
    width: usize,
}

impl StaticRange {
    fn overlaps(self, other: Self) -> bool {
        self.addr == other.addr
            && self.start < other.start.saturating_add(other.width)
            && other.start < self.start.saturating_add(self.width)
    }
}

fn mark_reaching(state: &StateSsa, version: MemoryVersionId, live: &mut [bool]) {
    if live[version.0] {
        return;
    }
    live[version.0] = true;
    match &state.accesses[version.0].kind {
        MemoryAccessKind::Kill { reaching } => mark_reaching(state, *reaching, live),
        MemoryAccessKind::Phi { incoming } => {
            for (_, version) in incoming {
                mark_reaching(state, *version, live);
            }
        }
        MemoryAccessKind::LiveOnEntry
        | MemoryAccessKind::Use { .. }
        | MemoryAccessKind::Def { .. } => {}
    }
}

/// Remove stable-state Stores in the combinational prefix which are not
/// observed by the FF sink.
///
/// A combined comb/FF call does not publish settled combinational state on
/// return: the clock transition makes that state dirty.  Consequently a comb
/// Store is required only when its exact StateSSA version reaches a load in
/// the FF suffix (or another read before it is overwritten).  Stores carrying
/// runtime effects and accesses with imprecise aliases remain explicit.
pub(in crate::optimizer) fn eliminate(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    provenance: &crate::ir::SirMergeProvenance,
    first_ff_unit: usize,
) -> Result<usize, OptimizationError> {
    if first_ff_unit == 0 || first_ff_unit >= provenance.unit_entries.len() {
        return Err(OptimizationError::invalid_input(
            "fused comb dead-store elimination",
            format!(
                "invalid comb/FF phase cut: first_ff_unit={first_ff_unit} units={}",
                provenance.unit_entries.len()
            ),
        ));
    }
    eliminate_candidates(
        eu,
        |block| {
            provenance
                .block_units
                .get(&block)
                .is_some_and(|unit| *unit < first_ff_unit)
        },
        &[],
    )
}

/// Shared comb/FF lowering may publish acyclic FF updates directly to STABLE.
/// Those exact ranges are semantic state updates. Every other ordinary STABLE
/// Store is a comb publication and can use the StateSSA liveness test without
/// an artificial source-EU boundary.
pub(in crate::optimizer) fn eliminate_shared(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    direct_ff_writes: &[VarAtomBase<RegionedAbsoluteAddr>],
) -> Result<usize, OptimizationError> {
    eliminate_candidates(eu, |_| true, direct_ff_writes)
}

fn eliminate_candidates(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    is_comb_block: impl Fn(BlockId) -> bool,
    protected_writes: &[VarAtomBase<RegionedAbsoluteAddr>],
) -> Result<usize, OptimizationError> {
    let cfg = SirCfg::analyze(eu).map_err(|error| {
        OptimizationError::control_flow("fused comb dead-store elimination", error)
    })?;
    let state = StateSsa::analyze(eu, &cfg, STABLE_REGION, None).map_err(|error| {
        OptimizationError::state_ssa("fused comb dead-store elimination", error)
    })?;

    let mut live = vec![false; state.accesses.len()];
    for access in &state.accesses {
        match &access.kind {
            MemoryAccessKind::Use { reaching, .. } => {
                mark_reaching(&state, *reaching, &mut live);
            }
            MemoryAccessKind::Def {
                observable: true, ..
            } => {
                live[access.id.0] = true;
            }
            _ => {}
        }
    }

    // If StateSSA cannot name a range precisely, retain all definitions for
    // that slot.  Deleting an imprecise state transition is never a fallback.
    for (slot, facts) in state.slots.iter().enumerate() {
        if !(facts.escapes || facts.has_kill || facts.has_effectful_store) {
            continue;
        }
        for access in &state.accesses {
            if access.slot == slot && !matches!(access.kind, MemoryAccessKind::Use { .. }) {
                live[access.id.0] = true;
            }
        }
    }

    let mut definitions = HashMap::<(BlockId, usize), Vec<usize>>::default();
    for access in &state.accesses {
        if let (Some(block), Some(instruction)) = (access.block, access.instruction)
            && !matches!(access.kind, MemoryAccessKind::Use { .. })
        {
            definitions
                .entry((block, instruction))
                .or_default()
                .push(access.id.0);
        }
    }

    // StateSSA intentionally rejects some access shapes.  Such a Store can
    // still be removed when no static or dynamic read in the combined
    // function can alias it.
    let mut static_reads = HashMap::<RegionedAbsoluteAddr, Vec<StaticRange>>::default();
    let mut dynamic_reads = HashSet::<RegionedAbsoluteAddr>::default();
    for block in eu.blocks.values() {
        for instruction in &block.instructions {
            match instruction {
                SIRInstruction::Load(_, addr, offset, width)
                    if addr.region == STABLE_REGION && offset.constant_bit_offset().is_some() =>
                {
                    let start = offset.constant_bit_offset().unwrap();
                    static_reads.entry(*addr).or_default().push(StaticRange {
                        addr: *addr,
                        start,
                        width: *width,
                    });
                }
                SIRInstruction::Load(_, addr, _, _) if addr.region == STABLE_REGION => {
                    dynamic_reads.insert(*addr);
                }
                SIRInstruction::Commit(source, _, offset, width, _)
                    if source.region == STABLE_REGION && offset.constant_bit_offset().is_some() =>
                {
                    let start = offset.constant_bit_offset().unwrap();
                    static_reads.entry(*source).or_default().push(StaticRange {
                        addr: *source,
                        start,
                        width: *width,
                    });
                }
                SIRInstruction::Commit(source, _, _, _, _) if source.region == STABLE_REGION => {
                    dynamic_reads.insert(*source);
                }
                _ => {}
            }
        }
    }

    let mut remove = HashSet::default();
    for (&block_id, block) in &eu.blocks {
        if !is_comb_block(block_id) {
            continue;
        }
        for (instruction, operation) in block.instructions.iter().enumerate() {
            let SIRInstruction::Store(addr, offset, width, _, triggers, capture_sites) = operation
            else {
                continue;
            };
            let Some(start) = offset.constant_bit_offset() else {
                continue;
            };
            if addr.region != STABLE_REGION
                || !triggers.is_empty()
                || !capture_sites.is_empty()
                || *width == 0
            {
                continue;
            }
            let range = StaticRange {
                addr: *addr,
                start,
                width: *width,
            };
            if protected_writes.iter().any(|protected| {
                range.overlaps(StaticRange {
                    addr: protected.id,
                    start: protected.access.lsb,
                    width: protected.access.msb - protected.access.lsb + 1,
                })
            }) {
                continue;
            }

            if let Some(accesses) = definitions.get(&(block_id, instruction)) {
                if accesses.iter().all(|&access| !live[access]) {
                    remove.insert((block_id, instruction));
                }
                continue;
            }

            let may_be_read = dynamic_reads.contains(addr)
                || static_reads
                    .get(addr)
                    .is_some_and(|reads| reads.iter().any(|read| range.overlaps(*read)));
            if !may_be_read {
                remove.insert((block_id, instruction));
            }
        }
    }

    let removed = remove.len();
    for (&block_id, block) in &mut eu.blocks {
        let mut instruction = 0usize;
        block.instructions.retain(|_| {
            let keep = !remove.contains(&(block_id, instruction));
            instruction += 1;
            keep
        });
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{InstanceId, RegisterType};
    use celox_design::StateObjectId as VarId;

    fn address(instance: usize) -> RegionedAbsoluteAddr {
        RegionedAbsoluteAddr {
            region: STABLE_REGION,
            instance_id: InstanceId(instance),
            var_id: VarId::default(),
        }
    }

    fn unit(
        instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
    ) -> ExecutionUnit<RegionedAbsoluteAddr> {
        ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [(
                BlockId(0),
                BasicBlock {
                    id: BlockId(0),
                    params: Vec::new(),
                    instructions,
                    terminator: SIRTerminator::Return,
                },
            )]
            .into_iter()
            .collect(),
            register_map: (0..4)
                .map(|index| {
                    (
                        RegisterId(index),
                        RegisterType::Bit {
                            width: 8,
                            signed: false,
                        },
                    )
                })
                .collect(),
        }
    }

    fn store(instance: usize, source: usize) -> SIRInstruction<RegionedAbsoluteAddr> {
        SIRInstruction::Store(
            address(instance),
            SIROffset::Static(0),
            8,
            RegisterId(source),
            Vec::new(),
            Vec::new(),
        )
    }

    fn packed_store(instance: usize, source: usize) -> SIRInstruction<RegionedAbsoluteAddr> {
        SIRInstruction::Store(
            address(instance),
            SIROffset::PackedElements {
                bit_offset: 0,
                element_width: 1,
            },
            8,
            RegisterId(source),
            Vec::new(),
            Vec::new(),
        )
    }

    #[test]
    fn keeps_only_comb_state_observed_by_the_ff_sink() {
        let comb = unit(vec![store(0, 0), store(1, 1)]);
        let ff = unit(vec![SIRInstruction::Load(
            RegisterId(0),
            address(1),
            SIROffset::Static(0),
            8,
        )]);
        let refs = [&comb, &ff];
        let (mut fused, provenance) = crate::ir::merge_sir_eu_refs_with_provenance(&refs);

        assert_eq!(eliminate(&mut fused, &provenance, 1).unwrap(), 1);
        let comb_entry = provenance.unit_entries[0];
        assert_eq!(fused.blocks[&comb_entry].instructions, vec![store(1, 1)]);
        let ff_entry = provenance.unit_entries[1];
        assert!(matches!(
            fused.blocks[&ff_entry].instructions.as_slice(),
            [SIRInstruction::Load(_, _, _, 8)]
        ));
    }

    #[test]
    fn shared_eu_needs_no_artificial_comb_ff_boundary() {
        let mut fused = unit(vec![
            store(0, 0),
            store(1, 1),
            SIRInstruction::Load(RegisterId(2), address(1), SIROffset::Static(0), 8),
        ]);

        assert_eq!(eliminate_shared(&mut fused, &[]).unwrap(), 1);
        assert_eq!(
            fused.blocks[&BlockId(0)].instructions,
            vec![
                store(1, 1),
                SIRInstruction::Load(RegisterId(2), address(1), SIROffset::Static(0), 8),
            ],
        );
    }

    #[test]
    fn removes_unobserved_packed_element_publication() {
        let mut fused = unit(vec![
            packed_store(0, 0),
            SIRInstruction::Load(RegisterId(2), address(1), SIROffset::Static(0), 8),
        ]);

        assert_eq!(eliminate_shared(&mut fused, &[]).unwrap(), 1);
        assert_eq!(
            fused.blocks[&BlockId(0)].instructions,
            vec![SIRInstruction::Load(
                RegisterId(2),
                address(1),
                SIROffset::Static(0),
                8,
            )],
        );
    }

    #[test]
    fn keeps_packed_element_publication_reaching_a_packed_load() {
        let packed = SIROffset::PackedElements {
            bit_offset: 0,
            element_width: 1,
        };
        let mut fused = unit(vec![
            packed_store(0, 0),
            SIRInstruction::Load(RegisterId(2), address(0), packed, 8),
        ]);

        assert_eq!(eliminate_shared(&mut fused, &[]).unwrap(), 0);
        assert!(matches!(
            fused.blocks[&BlockId(0)].instructions.as_slice(),
            [SIRInstruction::Store(..), SIRInstruction::Load(..)]
        ));
    }

    #[test]
    fn keeps_direct_ff_state_update_without_a_later_read() {
        let mut fused = unit(vec![store(0, 0), store(1, 1)]);
        let direct_ff_write = VarAtomBase::new(address(0), 0, 7);

        assert_eq!(eliminate_shared(&mut fused, &[direct_ff_write]).unwrap(), 1);
        assert_eq!(fused.blocks[&BlockId(0)].instructions, vec![store(0, 0)]);
    }
}
