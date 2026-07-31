use std::{collections::BTreeSet, fmt};

use celox_design::{
    InitialStateValue, RegionedVarAddrBase, RuntimeErrorInfo, RuntimeEventSite, TriggerSet,
};
use celox_sir::ExecutionUnit;
use celox_slt::{
    CombObserver, FfAccessSummary, GlueBlockBase, LogicPath, NodeId, SLTNodeArena, SymbolicStore,
};
use veryl_analyzer::ir::{VarId, VarPath, Variable};
use veryl_parser::resource_table::StrId;

use crate::HashMap;

type RegionedVarAddr = RegionedVarAddrBase<VarId>;
type GlueBlock = GlueBlockBase<VarId>;

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
