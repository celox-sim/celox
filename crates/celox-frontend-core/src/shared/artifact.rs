use std::fmt;

use celox_design::{ElaboratedDesign, RegionedStateAddr, RuntimeSchema, StateAddr, VarAtomBase};
use celox_sir::{RegisterId, SIRInstruction, SirProgram};

use crate::{FrontendLookup, HashMap};

/// Source-independent SIR and design data produced after all SLT nodes have
/// been scheduled and lowered.
///
/// No `NodeId`, SLT arena, or source-language-owned testbench syntax may cross
/// this boundary. `frontend_lookup` contains only neutral source IDs and owned
/// strings.
#[derive(Clone)]
pub struct ScheduledRtl {
    pub sir: SirProgram<StateAddr, RegionedStateAddr>,
    pub design: ElaboratedDesign<StateAddr>,
    pub frontend_lookup: FrontendLookup,
    pub runtime_schema: RuntimeSchema<StateAddr>,
}

impl fmt::Debug for ScheduledRtl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScheduledRtl")
            .field("sir", &self.sir)
            .field("design", &self.design)
            .field("frontend_lookup", &self.frontend_lookup)
            .field("runtime_schema", &self.runtime_schema)
            .finish()
    }
}

impl ScheduledRtl {
    /// Attach runtime event IDs after fused SIR pre-optimization has removed
    /// disposable state publications.
    pub fn inject_triggers(&mut self) {
        let mut trigger_map = HashMap::default();
        let kinds = self.design.event_kinds();
        for (id, address) in self.design.events.ordered_events.iter().enumerate() {
            if let Some(kind) = kinds.get(address) {
                trigger_map
                    .entry(*address)
                    .or_insert_with(Vec::new)
                    .push(celox_design::TriggerIdWithKind { kind: *kind, id });
            }
        }

        let events = &self.design.events;
        for unit in self
            .sir
            .eval_apply_ffs
            .values_mut()
            .flatten()
            .chain(self.sir.eval_comb_apply_ffs.values_mut().flatten())
            .chain(self.sir.eval_only_ffs.values_mut().flatten())
            .chain(self.sir.apply_ffs.values_mut().flatten())
            .chain(self.sir.eval_comb.iter_mut())
            .chain(
                self.sir
                    .parallel
                    .iter_mut()
                    .flat_map(|parallel| parallel.units_mut().map(|unit| &mut unit.unit)),
            )
        {
            // The address each register loads, to tell a copy of an alias
            // from a copy of the event's canonical signal.
            let loaded: HashMap<RegisterId, StateAddr> = unit
                .blocks
                .values()
                .flat_map(|block| &block.instructions)
                .filter_map(|instruction| match instruction {
                    SIRInstruction::Load(register, address, _, _) => {
                        Some((*register, address.absolute_addr()))
                    }
                    _ => None,
                })
                .collect();
            for block in unit.blocks.values_mut() {
                for instruction in &mut block.instructions {
                    let (address, source, triggers) = match instruction {
                        SIRInstruction::Store(address, _, _, source, triggers, _) => {
                            (address.absolute_addr(), Some(*source), triggers)
                        }
                        SIRInstruction::Commit(_, address, .., triggers) => {
                            (address.absolute_addr(), None, triggers)
                        }
                        _ => continue,
                    };
                    let canonical = events.canonical(address);
                    let Some(event_triggers) = trigger_map.get(&canonical) else {
                        continue;
                    };
                    // A copy of an alias of the event repeats what the copy
                    // of the canonical signal it descends from already
                    // reported in the same evaluation: the copies run in
                    // dependency order, and a partition reaches it in the
                    // same settle. Only the canonical signal's own copies
                    // carry the event, so the copies further out are plain
                    // copies, which may alias their source.
                    if address != canonical
                        && source
                            .and_then(|source| loaded.get(&source))
                            .is_some_and(|&loaded| {
                                loaded != canonical && events.canonical(loaded) == canonical
                            })
                    {
                        continue;
                    }
                    *triggers = event_triggers.clone();
                }
            }
        }
    }
}

/// Optimizer inputs derived while fused comb/FF scheduling still has exact
/// action provenance. These hints are kept beside, not inside, `SirProgram`.
#[derive(Clone, Debug, Default)]
pub struct FusedSirOptimizationHints {
    pub direct_ff_writes: HashMap<StateAddr, Vec<VarAtomBase<RegionedStateAddr>>>,
}

#[derive(Clone, Debug)]
pub struct ScheduledRtlOutput {
    pub scheduled: ScheduledRtl,
    pub fused_optimization_hints: FusedSirOptimizationHints,
}
