use std::fmt;

use celox_design::{ElaboratedDesign, RegionedStateAddr, RuntimeSchema, StateAddr, VarAtomBase};
use celox_sir::{SIRInstruction, SirProgram};

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
        for (id, address) in self.design.events.ordered_events.iter().enumerate() {
            if let Some(metadata) = self.design.state_objects.get(address) {
                trigger_map.entry(*address).or_insert_with(Vec::new).push(
                    celox_design::TriggerIdWithKind {
                        kind: metadata.kind,
                        id,
                    },
                );
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
        {
            for block in unit.blocks.values_mut() {
                for instruction in &mut block.instructions {
                    let (address, triggers) = match instruction {
                        SIRInstruction::Store(address, _, _, _, triggers, _) => {
                            (address.absolute_addr(), triggers)
                        }
                        SIRInstruction::Commit(_, address, .., triggers) => {
                            (address.absolute_addr(), triggers)
                        }
                        _ => continue,
                    };
                    let canonical = events.canonical(address);
                    if let Some(event_triggers) = trigger_map.get(&canonical) {
                        *triggers = event_triggers.clone();
                    }
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
