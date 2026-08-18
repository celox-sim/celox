use std::{collections::BTreeSet, fmt};

use celox_design::{
    InitialStateValue, ModuleId, RegionedAbsoluteAddrBase, RegionedVarAddrBase, RuntimeErrorInfo,
    RuntimeEventSite, TriggerSet, VariableMetadata,
};
use celox_sir::ExecutionUnit;
use celox_slt::{CombObserver, FfAccessSummary, GlueBlockBase, LogicPath, SLTNodeArena};

use crate::{HashMap, HashSet, SourceLocation, SourceVarId, VariableKind};

pub type SymbolicRegionedAddr = RegionedVarAddrBase<SourceVarId>;
pub type SymbolicGlueAddr = celox_slt::GlueAddrBase<SourceVarId>;
pub type SymbolicGlueBlock = GlueBlockBase<SourceVarId>;

/// Source-independent variable metadata consumed by symbolic assembly.
#[derive(Clone, Debug)]
pub struct SymbolicVariable {
    pub path: Vec<String>,
    pub kind: VariableKind,
    pub signed: bool,
    pub metadata: VariableMetadata,
    pub packed_dims: Vec<usize>,
    pub source: Option<SourceLocation>,
    pub module_affiliated: bool,
}

#[derive(Clone)]
pub struct SimModule {
    pub name: String,
    pub variables: HashMap<SourceVarId, SymbolicVariable>,
    pub ff_access_summaries:
        HashMap<TriggerSet<SourceVarId>, FfAccessSummary<SymbolicRegionedAddr>>,
    pub eval_only_ff_blocks: HashMap<TriggerSet<SourceVarId>, ExecutionUnit<SymbolicRegionedAddr>>,
    pub apply_ff_blocks: HashMap<TriggerSet<SourceVarId>, ExecutionUnit<SymbolicRegionedAddr>>,
    pub eval_apply_ff_blocks: HashMap<TriggerSet<SourceVarId>, ExecutionUnit<SymbolicRegionedAddr>>,
    pub glue_blocks: HashMap<String, Vec<SymbolicGlueBlock>>,
    pub indexed_instance_names: HashSet<String>,
    pub comb_blocks: Vec<LogicPath<SourceVarId>>,
    pub comb_observers: Vec<CombObserver<SourceVarId>>,
    pub runtime_errors: HashMap<i64, RuntimeErrorInfo<SourceVarId>>,
    pub runtime_event_sites: Vec<RuntimeEventSite>,
    pub initial_memory_values: Vec<InitialStateValue<SourceVarId>>,
    pub comb_boundaries: HashMap<SourceVarId, BTreeSet<usize>>,
    pub arena: SLTNodeArena<SourceVarId>,
    pub reset_clock_map: HashMap<SourceVarId, SourceVarId>,
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
            .field("indexed_instance_names", &self.indexed_instance_names)
            .field("comb_blocks", &self.comb_blocks)
            .field("comb_boundaries", &self.comb_boundaries)
            .field("arena", &self.arena)
            .field("reset_clock_map", &self.reset_clock_map)
            .finish()
    }
}

#[derive(Clone)]
pub struct ExternalModule {
    pub sim_module: SimModule,
    pub port_order: Vec<SourceVarId>,
    pub unresolved_instances: Vec<String>,
}

#[derive(Clone, Default)]
pub struct ExternalHierarchy {
    pub modules: HashMap<ModuleId, ExternalModule>,
    pub roots: HashMap<String, ModuleId>,
}

pub struct SymbolicRtl {
    pub modules: HashMap<ModuleId, SimModule>,
    pub module_names: HashMap<ModuleId, String>,
    pub root_id: ModuleId,
}

#[derive(Clone)]
pub struct RelocationModule {
    pub eval_apply_ff_blocks:
        HashMap<TriggerSet<SourceVarId>, ExecutionUnit<RegionedAbsoluteAddrBase<SourceVarId>>>,
    pub eval_only_ff_blocks:
        HashMap<TriggerSet<SourceVarId>, ExecutionUnit<RegionedAbsoluteAddrBase<SourceVarId>>>,
    pub apply_ff_blocks:
        HashMap<TriggerSet<SourceVarId>, ExecutionUnit<RegionedAbsoluteAddrBase<SourceVarId>>>,
    pub comb_blocks: Vec<LogicPath<crate::SourceAddr>>,
    pub comb_observers: Vec<CombObserver<crate::SourceAddr>>,
}

impl fmt::Debug for RelocationModule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RelocationModule")
            .field("eval_apply_ff_blocks", &self.eval_apply_ff_blocks)
            .field("eval_only_ff_blocks", &self.eval_only_ff_blocks)
            .field("apply_ff_blocks", &self.apply_ff_blocks)
            .field("comb_blocks", &self.comb_blocks)
            .field("comb_observers", &self.comb_observers)
            .finish()
    }
}
