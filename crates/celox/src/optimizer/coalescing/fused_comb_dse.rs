use crate::ir::cfg::SirCfg;
use crate::ir::*;
use crate::{HashMap, HashSet};

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

/// Eliminate stable-state publications from the combinational prefix of a
/// fused comb/FF function when their MemorySSA versions cannot reach a load.
///
/// The fused call marks combinational state dirty on return, so a prefix Store
/// is not an externally observable exit value.  Trigger/capture Stores,
/// dynamic aliases, invalid slot shapes, and FF-suffix Stores stay explicit.
pub(super) fn eliminate(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    provenance: &crate::ir::SirMergeProvenance,
    first_ff_unit: usize,
) -> Result<usize, String> {
    if first_ff_unit == 0 || first_ff_unit >= provenance.unit_entries.len() {
        return Err(format!(
            "invalid fused phase cut: first_ff_unit={first_ff_unit} units={}",
            provenance.unit_entries.len()
        ));
    }
    let cfg = SirCfg::analyze(eu).map_err(|error| error.to_string())?;
    let state =
        StateSsa::analyze(eu, &cfg, STABLE_REGION, None).map_err(|error| error.to_string())?;

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

    // An imprecise/overlapping slot cannot be rewritten from its exact
    // MemorySSA name. Keep every definition touching that slot.
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

    // If StateSSA rejected a shape, only delete it when no static or dynamic
    // read can alias it at all.
    let mut static_reads = HashMap::<RegionedAbsoluteAddr, Vec<StaticRange>>::default();
    let mut dynamic_reads = HashSet::<RegionedAbsoluteAddr>::default();
    for block in eu.blocks.values() {
        for instruction in &block.instructions {
            match instruction {
                SIRInstruction::Load(_, addr, SIROffset::Static(start), width)
                    if addr.region == STABLE_REGION =>
                {
                    static_reads.entry(*addr).or_default().push(StaticRange {
                        addr: *addr,
                        start: *start,
                        width: *width,
                    });
                }
                SIRInstruction::Load(_, addr, _, _) if addr.region == STABLE_REGION => {
                    dynamic_reads.insert(*addr);
                }
                SIRInstruction::Commit(source, _, SIROffset::Static(start), width, _)
                    if source.region == STABLE_REGION =>
                {
                    static_reads.entry(*source).or_default().push(StaticRange {
                        addr: *source,
                        start: *start,
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
        let Some(&unit) = provenance.block_units.get(&block_id) else {
            continue;
        };
        if unit >= first_ff_unit {
            continue;
        }
        for (instruction, operation) in block.instructions.iter().enumerate() {
            let SIRInstruction::Store(
                addr,
                SIROffset::Static(start),
                width,
                _,
                triggers,
                capture_sites,
            ) = operation
            else {
                continue;
            };
            if addr.region != STABLE_REGION
                || !triggers.is_empty()
                || !capture_sites.is_empty()
                || *width == 0
            {
                continue;
            }
            if let Some(accesses) = definitions.get(&(block_id, instruction)) {
                if accesses.iter().all(|&access| !live[access]) {
                    remove.insert((block_id, instruction));
                }
                continue;
            }
            let range = StaticRange {
                addr: *addr,
                start: *start,
                width: *width,
            };
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
    use veryl_analyzer::ir::VarId;

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

    #[test]
    fn keeps_only_comb_publications_reaching_the_ff_suffix() {
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
}
