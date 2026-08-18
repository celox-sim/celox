use std::{collections::BTreeSet, fmt};

use celox_design::{
    AbsoluteAddrBase, InitialStateValue, ModuleId, RegionedAbsoluteAddrBase, RegionedVarAddrBase,
    RuntimeErrorInfo, RuntimeEventSite, TriggerSet,
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
    /// Source instance declarations that explicitly have an array dimension.
    pub indexed_instance_names: crate::HashSet<StrId>,
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
            .field("indexed_instance_names", &self.indexed_instance_names)
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

/// One source module lowered into the symbolic compatibility vocabulary.
#[derive(Clone)]
pub struct ExternalModule {
    pub metadata: veryl_analyzer::ir::Module,
    pub sim_module: SimModule,
    pub port_order: Vec<VarId>,
    pub unresolved_instances: Vec<StrId>,
}

/// A source-local module graph ready to be embedded in a mixed-language
/// symbolic design. Module IDs are remapped during hierarchy assembly.
#[derive(Clone, Default)]
pub struct ExternalHierarchy {
    pub modules: HashMap<ModuleId, ExternalModule>,
    pub roots: HashMap<StrId, ModuleId>,
}

/// Modules discovered from the selected top before hierarchy expansion.
///
/// This private compatibility artifact may retain analyzer references. It is
/// consumed before source-independent design and SIR artifacts leave the
/// frontend boundary.
pub struct SymbolicRtl<'a> {
    pub modules: HashMap<ModuleId, SimModule>,
    pub module_ir: HashMap<ModuleId, &'a veryl_analyzer::ir::Module>,
    pub module_names: HashMap<ModuleId, StrId>,
    pub root_id: ModuleId,
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
