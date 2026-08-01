use std::{collections::BTreeSet, fmt};

use celox_design::{
    AbsoluteAddrBase, ElaboratedDesign, InitialStateValue, RegionedAbsoluteAddrBase,
    RegionedVarAddrBase, RuntimeErrorInfo, RuntimeEventSite, RuntimeSchema, TriggerSet,
    VarAtomBase,
};
use celox_sir::{ExecutionUnit, SIRInstruction, SirProgram};
use celox_slt::{
    CombObserver, FfAccessSummary, GlueBlockBase, LogicPath, NodeId, SLTNodeArena, SymbolicStore,
};
use veryl_analyzer::ir::{VarId, VarPath, Variable};
use veryl_parser::resource_table::StrId;

use crate::{HashMap, VerylFrontendLookup, VerylTestbenchSource};

type RegionedVarAddr = RegionedVarAddrBase<VarId>;
type GlueBlock = GlueBlockBase<VarId>;
type AbsoluteAddr = AbsoluteAddrBase<VarId>;
type RegionedAbsoluteAddr = RegionedAbsoluteAddrBase<VarId>;

#[derive(Clone)]
pub struct SimModule {
    pub name: StrId,
    pub variables: HashMap<VarId, Variable>,
    pub ff_access_summaries: HashMap<TriggerSet<VarId>, FfAccessSummary<RegionedVarAddr>>,
    pub eval_only_ff_blocks: HashMap<TriggerSet<VarId>, ExecutionUnit<RegionedVarAddr>>,
    pub apply_ff_blocks: HashMap<TriggerSet<VarId>, ExecutionUnit<RegionedVarAddr>>,
    pub eval_apply_ff_blocks: HashMap<TriggerSet<VarId>, ExecutionUnit<RegionedVarAddr>>,
    pub glue_blocks: HashMap<StrId, Vec<GlueBlock>>,
    pub comb_blocks: Vec<LogicPath<VarId>>,
    pub comb_observers: Vec<CombObserver<VarId>>,
    pub runtime_errors: HashMap<i64, RuntimeErrorInfo<VarId>>,
    pub runtime_event_sites: Vec<RuntimeEventSite>,
    pub initial_memory_values: Vec<InitialStateValue<VarId>>,
    pub comb_boundaries: HashMap<VarId, BTreeSet<usize>>,
    pub arena: SLTNodeArena<VarId>,
    pub store: SymbolicStore<VarId, NodeId>,
    /// Maps reset VarId to clock VarId, derived from FF declarations.
    pub reset_clock_map: HashMap<VarId, VarId>,
}

impl fmt::Debug for SimModule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SimModule")
            .field("name", &self.name)
            .field("variables", &"<omitted>")
            .field("ff_access_summaries", &self.ff_access_summaries)
            .field("eval_only_ff_blocks", &self.eval_only_ff_blocks)
            .field("apply_ff_blocks", &self.apply_ff_blocks)
            .field("eval_apply_ff_blocks", &self.eval_apply_ff_blocks)
            .field("glue_blocks", &self.glue_blocks)
            .field("comb_blocks", &self.comb_blocks)
            .field("comb_boundaries", &self.comb_boundaries)
            .field("arena", &self.arena)
            .field("store", &self.store)
            .field("reset_clock_map", &self.reset_clock_map)
            .finish()
    }
}

impl SimModule {
    pub fn find_var_id(&self, path: &VarPath) -> VarId {
        self.variables
            .iter()
            .find(|(_, variable)| &variable.path == path)
            .map(|(id, _)| *id)
            .unwrap_or_else(|| panic!("Variable '{path}' not found in module"))
    }
}

/// One module after source identities have been relocated into the flattened
/// instance namespace, but before the complete design is assembled.
#[derive(Clone)]
pub struct RelocationModule {
    pub eval_apply_ff_blocks: HashMap<TriggerSet<VarId>, ExecutionUnit<RegionedAbsoluteAddr>>,
    pub eval_only_ff_blocks: HashMap<TriggerSet<VarId>, ExecutionUnit<RegionedAbsoluteAddr>>,
    pub apply_ff_blocks: HashMap<TriggerSet<VarId>, ExecutionUnit<RegionedAbsoluteAddr>>,
    pub comb_blocks: Vec<LogicPath<AbsoluteAddr>>,
    pub comb_observers: Vec<CombObserver<AbsoluteAddr>>,
}

impl fmt::Debug for RelocationModule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut debug = f.debug_struct("RelocationModule");
        debug
            .field("eval_apply_ff_blocks", &self.eval_apply_ff_blocks)
            .field("eval_only_ff_blocks", &self.eval_only_ff_blocks)
            .field("apply_ff_blocks", &self.apply_ff_blocks)
            .field("comb_blocks", &self.comb_blocks)
            .field("comb_observers", &self.comb_observers)
            .finish()
    }
}

/// Source-independent SIR and design data produced after all SLT nodes have
/// been scheduled and lowered.
///
/// No `NodeId` or SLT arena may cross this boundary. Veryl identities retained
/// for diagnostics and public path lookup live only in `frontend_lookup`.
#[derive(Clone)]
pub struct ScheduledRtl {
    pub sir: SirProgram<AbsoluteAddr, RegionedAbsoluteAddr>,
    pub design: ElaboratedDesign<AbsoluteAddr>,
    pub frontend_lookup: VerylFrontendLookup,
    pub runtime_schema: RuntimeSchema<AbsoluteAddr>,
    pub testbench_source: VerylTestbenchSource,
}

impl fmt::Debug for ScheduledRtl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScheduledRtl")
            .field("sir", &self.sir)
            .field("design", &self.design)
            .field("frontend_lookup", &self.frontend_lookup)
            .field("runtime_schema", &self.runtime_schema)
            .field("testbench_source", &self.testbench_source)
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
    pub direct_ff_writes: HashMap<AbsoluteAddr, Vec<VarAtomBase<RegionedAbsoluteAddr>>>,
}

#[derive(Clone, Debug)]
pub struct ScheduledRtlOutput {
    pub scheduled: ScheduledRtl,
    pub fused_optimization_hints: FusedSirOptimizationHints,
}
