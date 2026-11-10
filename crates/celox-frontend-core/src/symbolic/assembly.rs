use num_bigint::BigUint;
use std::collections::{BTreeMap, BTreeSet};

use crate::symbolic::artifact::{
    FfPart, HierarchicalStart, NbaSwitch, RelocationModule, SimModule, SplitCommit,
    SymbolicGlueAddr as GlueAddr, SymbolicRtl,
};
use crate::{
    FrontendLookup, FrontendTrace, FrontendTraceOptions, FusedSirOptimizationHints, HashMap,
    HashSet, InstancePath, ParallelScheduleOptions, ParserError, ScheduledRtl, ScheduledRtlOutput,
    SettlePartitionMode, SourceAddr, SourceLocation, SourceVarId, VariableInfo, flattening,
};
use celox_design::{
    BitAccess, CombDerivedClock, CounterKind, CounterSlots, DomainKind, ElaboratedDesign,
    EventTopology, EventWatch, ExternFunction, GateCounters, GateOp, HostRequestSite,
    InitialStateData, InitialStateValue, InstanceId, ModuleId, NbaSite, ProcessSlots,
    RegionedAbsoluteAddrBase, RegionedStateAddr, RuntimeCombObserver, RuntimeErrorInfo,
    RuntimeEventKind, RuntimeEventSite, RuntimeSchema, STABLE_REGION, StateAddr, StateObjectId,
    TriggerSet, VarAtomBase, VariableMetadata, WORKING_REGION, WaitSite,
};
use celox_sir::{
    BasicBlock, ExecutionUnit, LaneUnit, ParallelSirProgram, SIRInstruction, SIROffset,
    SIRTerminator, SirProgram,
};
use celox_slt::{
    CombObserver, FfAccessSummary, LogicPath, LogicPathId, LogicPathTarget, NodeId, SLTNodeArena,
    scheduler::{self, SchedulerError},
};

mod boundaries;
mod clock_analysis;
mod clock_domains;
mod comb_observers;
mod commits;
mod counters;
mod fused_ff;
mod hierarchy;
mod loops;
mod nba;
mod package_bindings;
mod pinned_nets;
mod relocation;
mod schedule;
mod scheduler_slots;
mod settle;
mod startup;
mod timing;
use boundaries::*;
use clock_analysis::*;
use clock_domains::*;
use comb_observers::*;
use commits::*;
use counters::*;
use fused_ff::*;
use hierarchy::*;
use loops::*;
use nba::*;
use package_bindings::*;
use pinned_nets::*;
use relocation::*;
use scheduler_slots::*;
use settle::*;
use startup::*;
use timing::*;

pub(crate) use package_bindings::PackageBindings;
pub use schedule::schedule_symbolic_rtl_reporting_loops;

type AbsoluteAddr = SourceAddr;
type RegionedAbsoluteAddr = RegionedAbsoluteAddrBase<SourceVarId>;

/// Per canonical event, the split commit of each of its apply units, in
/// their order; `None` for a unit whose frontend does not split it.
type SplitCommits = HashMap<AbsoluteAddr, Vec<Option<SplitCommit<RegionedAbsoluteAddr>>>>;
type RegionedVarAddr = celox_design::RegionedVarAddrBase<SourceVarId>;

/// Runtime IDs assigned while a module instance is relocated into the global
/// design. Source adapters use these IDs only when rebuilding an optimized FF
/// action inside the shared comb/FF scheduler.
#[derive(Clone, Debug)]
pub struct FfRuntimeRelocation {
    pub error_codes: HashMap<i64, i64>,
    pub event_site_base: u32,
}

/// Source-neutral description of one FF action offered to an adapter-specific
/// lowering implementation.
#[derive(Clone)]
pub struct FusedFfAction {
    pub id: usize,
    pub instance_id: InstanceId,
    pub module_id: ModuleId,
    pub trigger: TriggerSet<SourceVarId>,
    pub summary: FfAccessSummary<RegionedAbsoluteAddrBase<SourceVarId>>,
    pub runtime: FfRuntimeRelocation,
}

/// Adapter hook for source-aware FF lowering used by shared-clock scheduling,
/// with or without a preceding comb phase. The scheduler and all identities
/// crossing this trait remain source neutral.
/// The most events whose combined comb-then-FF ticks each get their own
/// program with a copy of the combinational program
/// ([`SirProgram::merge_comb_apply`]).
pub const MERGED_COMB_APPLY_EVENTS: usize = 4;

pub trait FusedFfLoweringFactory {
    fn create(
        &self,
        actions: Vec<FusedFfAction>,
    ) -> Result<
        Box<
            dyn scheduler::ClockFfLowering<
                    RegionedAbsoluteAddrBase<SourceVarId>,
                    Error = ParserError,
                > + '_,
        >,
        ParserError,
    >;
}

#[allow(clippy::too_many_arguments)]
pub fn schedule_symbolic_rtl(
    symbolic: SymbolicRtl,
    fused_ff_factory: Option<&dyn FusedFfLoweringFactory>,
    ignored_loops: &[(
        (Vec<(String, usize)>, Vec<String>),
        (Vec<(String, usize)>, Vec<String>),
    )],
    true_loops: &[(
        (Vec<(String, usize)>, Vec<String>),
        (Vec<(String, usize)>, Vec<String>),
        usize,
    )],
    four_state: bool,
    parallel: &ParallelScheduleOptions,
    trace_opts: &FrontendTraceOptions,
    trace: Option<&mut FrontendTrace>,
) -> Result<ScheduledRtlOutput, ParserError> {
    schedule_symbolic_rtl_reporting_loops(
        symbolic,
        fused_ff_factory,
        ignored_loops,
        true_loops,
        four_state,
        parallel,
        trace_opts,
        trace,
        &mut Vec::new(),
    )
}

/// A range a block of a combinational loop writes, in the module that
/// declares it.
pub type LoopMember = (ModuleId, VarAtomBase<SourceVarId>);

#[cfg(test)]
mod tests;

#[cfg(test)]
mod estimated_settle_budget_tests;
