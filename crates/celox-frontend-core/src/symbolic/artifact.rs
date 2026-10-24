use std::{collections::BTreeSet, fmt};

use celox_design::{
    CounterSlots, ExternFunction, InitialStateValue, ModuleId, NbaPosition, NbaSite, ProcessSlots,
    RegionedAbsoluteAddrBase, RegionedVarAddrBase, RuntimeErrorInfo, RuntimeEventSite, TriggerSet,
    VarAtomBase, VariableMetadata, WaitSite,
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
    /// Declared unpacked bounds, in storage order; empty uses zero-based dimensions.
    pub unpacked_ranges: Vec<(i128, i128)>,
    pub source: Option<SourceLocation>,
    pub module_affiliated: bool,
}

/// One independently evaluated part of an FF trigger group.
#[derive(Clone, Debug)]
pub struct FfPart<A> {
    /// Seeds and next-state stores of the part's targets.
    pub evaluate: ExecutionUnit<A>,
    /// Publication of the part's staged targets.
    pub apply: ExecutionUnit<A>,
}

impl<A> FfPart<A> {
    pub fn map_units<B>(&self, map: impl Fn(&ExecutionUnit<A>) -> ExecutionUnit<B>) -> FfPart<B> {
        FfPart {
            evaluate: map(&self.evaluate),
            apply: map(&self.apply),
        }
    }
}

/// One resumable process of a module, lowered to a single kernel unit.
///
/// The kernel may only access stable state. Its control slots are module
/// variables, so every instance of the module owns a separate copy.
#[derive(Clone, Debug)]
pub struct SymbolicProcess {
    pub kernel: ExecutionUnit<SymbolicRegionedAddr>,
    pub slots: ProcessSlots<SourceVarId>,
    pub origin: celox_design::ProcessOrigin,
}

/// The commit of a trigger group's results, split by region.
#[derive(Clone, Debug)]
pub struct SplitCommit<A> {
    /// Commits of the results of blocking assignments.
    pub active: ExecutionUnit<A>,
    /// Commits of the results of nonblocking assignments.
    pub nba: ExecutionUnit<A>,
}

/// A combinational process that `comb_blocks` holds both as symbolic
/// definitions and as an imperative unit. The symbolic form computes only
/// the final value of each written variable; the unit performs every store,
/// so the counters of a watched variable see the intermediate values
/// (IEEE 1800-2023 4.3, 9.4.2). Assembly keeps the unit for an instance in
/// which a variable of `candidates` is counted, and the symbolic
/// definitions otherwise.
#[derive(Clone, Debug)]
pub struct CombAlternative {
    /// The `process` of the unit's paths.
    pub process: SourceVarId,
    /// The ranges the symbolic definitions write; other drivers may write
    /// the other bits of the same variables.
    pub targets: Vec<VarAtomBase<SourceVarId>>,
    /// The variables the unit may store twice in one execution.
    pub candidates: Vec<SourceVarId>,
}

#[derive(Clone)]
pub struct SimModule {
    /// A frontend's global tick precision; absent for scale-free frontends.
    pub time_precision: Option<celox_design::TimePrecision>,
    /// Local unit and precision; scale-free frontends use the 1ps default.
    pub time_scale: celox_design::TimeScale,
    pub name: String,
    pub variables: HashMap<SourceVarId, SymbolicVariable>,
    pub ff_access_summaries:
        HashMap<TriggerSet<SourceVarId>, FfAccessSummary<SymbolicRegionedAddr>>,
    pub eval_only_ff_blocks: HashMap<TriggerSet<SourceVarId>, ExecutionUnit<SymbolicRegionedAddr>>,
    pub apply_ff_blocks: HashMap<TriggerSet<SourceVarId>, ExecutionUnit<SymbolicRegionedAddr>>,
    pub eval_apply_ff_blocks: HashMap<TriggerSet<SourceVarId>, ExecutionUnit<SymbolicRegionedAddr>>,
    /// `apply_ff_blocks` split into the commit of the blocking results (the
    /// active region) and that of the nonblocking ones (the NBA region), for
    /// a domain whose blocking results another unit observes before the NBA
    /// region. Empty when the frontend does not distinguish them.
    pub split_commit_ff_blocks: HashMap<TriggerSet<SourceVarId>, SplitCommit<SymbolicRegionedAddr>>,
    /// Lane-partitioning alternative of `eval_only_ff_blocks` and
    /// `apply_ff_blocks`: a trigger group split into parts which may be
    /// evaluated concurrently. Empty unless several lanes were requested.
    pub parallel_ff_parts: HashMap<TriggerSet<SourceVarId>, Vec<FfPart<SymbolicRegionedAddr>>>,
    pub glue_blocks: HashMap<String, Vec<SymbolicGlueBlock>>,
    pub indexed_instance_names: HashSet<String>,
    /// The index of the first glue block of an instance name, when it is not
    /// 0: the lower bound of a SystemVerilog instance array such as `u[3:2]`.
    pub instance_index_bases: HashMap<String, usize>,
    pub comb_blocks: Vec<LogicPath<SourceVarId>>,
    pub comb_observers: Vec<CombObserver<SourceVarId>>,
    pub runtime_errors: HashMap<i64, RuntimeErrorInfo<SourceVarId>>,
    pub runtime_event_sites: Vec<RuntimeEventSite>,
    /// Extern functions the module calls; its `ExternCall`s index this list.
    pub extern_functions: Vec<ExternFunction>,
    pub initial_memory_values: Vec<InitialStateValue<SourceVarId>>,
    pub folded_initial_memory_values: Vec<InitialStateValue<SourceVarId>>,
    /// Static declaration state, before initial blocks or combinational activity.
    pub declaration_initial_memory_values: Vec<InitialStateValue<SourceVarId>>,
    pub comb_boundaries: HashMap<SourceVarId, BTreeSet<usize>>,
    pub arena: SLTNodeArena<SourceVarId>,
    pub reset_clock_map: HashMap<SourceVarId, SourceVarId>,
    /// Resumable processes in declaration order.
    pub processes: Vec<SymbolicProcess>,
    /// Variables of the module that denote a variable of a package (IEEE
    /// 1800-2023 26.2). Every instance of the module refers to that one
    /// object instead of a state object of its own.
    pub package_bindings: HashMap<SourceVarId, PackageBinding>,
    /// Counter objects of the signals that a unit of the module may store
    /// twice between two runtime checks (see
    /// `crate::watch::multi_store_candidates`). Assembly counts a signal's
    /// occurrences only if a flip-flop domain or a wait watches it.
    pub counter_slots: Vec<CounterSlots<SourceVarId>>,
    /// Objects that hold the occurrences of a flip-flop's repeated writes
    /// to a signal until their commit (see `crate::watch::PendingSlots`).
    /// Assembly uses them only for the signals it counts.
    pub pending_slots: Vec<crate::watch::PendingSlots<SourceVarId>>,
    /// Combinational processes lowered both ways; see [`CombAlternative`].
    pub comb_alternatives: Vec<CombAlternative>,
    /// Wait sites of the module's processes; `process` indexes
    /// [`Self::processes`].
    pub wait_sites: Vec<WaitSite<SourceVarId>>,
    /// Nonblocking sites of the module's processes; `process` indexes
    /// [`Self::processes`].
    pub nba_sites: Vec<NbaSite<SourceVarId>>,
    /// Nonblocking sites whose kernel has both a staged and a queued path.
    pub nba_switches: Vec<NbaSwitch<SourceVarId>>,
    /// Synchronous host operations of the module processes.
    pub host_request_sites: Vec<celox_design::HostRequestSite<SourceVarId>>,
    /// The variable through which the module's code reads the simulation
    /// time; see `RuntimeSchema::time_slots`.
    pub time_slot: Option<SourceVarId>,
    /// Variables and their sampled copies; see
    /// `RuntimeSchema::sample_shadows`.
    pub sample_shadows: Vec<(SourceVarId, SourceVarId)>,
}

/// A nonblocking site whose kernel has two paths, as
/// `ProcessKernelBuilder::nonblocking_switched` builds them. The staged path
/// stages the value in the site's `value` and sets its `pending` flag; the
/// queued path leaves the value in `queued` and returns to the runtime.
/// Assembly keeps one path: the staged one only if nothing else writes the
/// site's target and nothing can observe when the target is updated. For the
/// queued path it makes `queued` the site's value and `queued_position` its
/// position, and drops the flag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NbaSwitch<A> {
    /// The site's `value` object, which identifies the site.
    pub value: A,
    /// The one-bit variable whose load selects the path. It holds no state:
    /// assembly replaces the selection by a jump.
    pub switch: A,
    /// The value object of the queued path.
    pub queued: A,
    /// The position objects of the queued path, for a site with a run-time
    /// position.
    pub queued_position: Option<NbaPosition<A>>,
}

impl<A: Copy> NbaSwitch<A> {
    pub fn map<B>(&self, mut map: impl FnMut(&A) -> B) -> NbaSwitch<B> {
        NbaSwitch {
            value: map(&self.value),
            switch: map(&self.switch),
            queued: map(&self.queued),
            queued_position: self
                .queued_position
                .map(|position| position.map(|var| map(&var))),
        }
    }
}

/// The package variable a module variable denotes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PackageBinding {
    /// The module holding the package's variables; see
    /// [`SymbolicRtl::packages`].
    pub package: ModuleId,
    pub var_id: SourceVarId,
}

impl SimModule {
    /// The hierarchy index of glue block `position` of instance `name`.
    pub fn instance_index(&self, name: &str, position: usize) -> usize {
        self.instance_index_bases.get(name).copied().unwrap_or(0) + position
    }
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
            .field("instance_index_bases", &self.instance_index_bases)
            .field("comb_blocks", &self.comb_blocks)
            .field("comb_boundaries", &self.comb_boundaries)
            .field("arena", &self.arena)
            .field("reset_clock_map", &self.reset_clock_map)
            .field("processes", &self.processes)
            .field("package_bindings", &self.package_bindings)
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
    /// The modules of `modules` that hold package variables, by package
    /// name; see [`SymbolicRtl::packages`].
    pub packages: Vec<(String, ModuleId)>,
}

pub struct SymbolicRtl {
    pub modules: HashMap<ModuleId, SimModule>,
    pub module_names: HashMap<ModuleId, String>,
    pub root_id: ModuleId,
    /// Modules holding the variables of a package, by package name. Each is
    /// instantiated once, outside the instance hierarchy, and the
    /// [`SimModule::package_bindings`] of other modules refer to its
    /// variables.
    pub packages: Vec<(String, ModuleId)>,
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
