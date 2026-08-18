use crate::HashMap;
use crate::ir::{ModuleId, SimModule};
use celox_frontend::{
    AbsoluteAddr as FrontendAbsoluteAddr, RegionedAbsoluteAddr as FrontendRegionedAbsoluteAddr,
};
use celox_slt::{LogicPath, SLTNodeArena};
mod output;

/// One native JIT block selected by an external profile.
///
/// Function names are the names used in the JIT perf map (for example
/// `eval_comb_apply_ff`), not just a block number.  Block numbers are only
/// unique within one emitted function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeProfileBlock {
    pub function: String,
    pub block: u32,
    pub samples: u64,
}

#[derive(Debug, Clone, Default)]
pub struct TraceOptions {
    pub sim_modules: bool,
    pub pre_atomized_comb_blocks: bool,
    pub atomized_comb_blocks: bool,
    pub flattened_comb_blocks: bool,
    pub scheduled_units: bool,
    pub pre_optimized_sir: bool,
    pub post_optimized_sir: bool,
    pub analyzer_ir: bool,
    pub pre_optimized_clif: bool,
    pub post_optimized_clif: bool,
    pub native: bool,
    pub mir: bool,
    pub native_profile_blocks: Vec<NativeProfileBlock>,
    pub output_to_stdout: bool,
}

#[derive(Debug, Clone, Default)]
pub struct CompilationTrace {
    pub sim_modules: Option<HashMap<ModuleId, SimModule>>,
    pub pre_atomized_comb_blocks: Option<(
        Vec<LogicPath<FrontendAbsoluteAddr>>,
        SLTNodeArena<FrontendAbsoluteAddr>,
    )>,
    pub atomized_comb_blocks: Option<(
        Vec<LogicPath<FrontendAbsoluteAddr>>,
        SLTNodeArena<FrontendAbsoluteAddr>,
    )>,
    pub flattened_comb_blocks: Option<(
        Vec<LogicPath<FrontendAbsoluteAddr>>,
        SLTNodeArena<FrontendAbsoluteAddr>,
    )>,
    pub scheduled_units: Option<Vec<celox_sir::ExecutionUnit<FrontendRegionedAbsoluteAddr>>>,
    pub pre_optimized_sir: Option<crate::ir::UnoptimizedSir>,
    pub post_optimized_sir: Option<crate::ir::OptimizedSir>,
    /// SIR after native EU merging, StateSSA promotion, and merged-chain
    /// cleanup, captured from the exact functions passed to instruction
    /// selection.
    pub native_optimized_sir: Option<String>,
    pub analyzer_ir: Option<String>,
    pub pre_optimized_clif: Option<String>,
    pub post_optimized_clif: Option<String>,
    pub native: Option<String>,
    pub mir: Option<String>,
    /// Sparse FF/comb dependency projection captured before merged-chain
    /// rewrites, with exact source-EU and StateSSA provenance.
    pub reactive_event_graph: Option<String>,
    /// Analysis of profile-selected native state accesses captured from the
    /// exact merged SIR used by instruction selection.
    pub native_state_layout: Option<String>,
}

impl TraceOptions {
    pub(crate) fn frontend(
        &self,
        diagnostics: &crate::RuntimeDiagnostics,
    ) -> celox_frontend::FrontendTraceOptions {
        celox_frontend::FrontendTraceOptions {
            phase_timing: diagnostics.phase_timing,
            sim_modules: self.sim_modules,
            pre_atomized_comb_blocks: self.pre_atomized_comb_blocks,
            atomized_comb_blocks: self.atomized_comb_blocks,
            flattened_comb_blocks: self.flattened_comb_blocks,
            scheduled_units: self.scheduled_units,
        }
    }
}

impl CompilationTrace {
    pub(crate) fn absorb_frontend(&mut self, trace: celox_frontend::FrontendTrace) {
        self.sim_modules = trace.sim_modules;
        self.pre_atomized_comb_blocks = trace.pre_atomized_comb_blocks;
        self.atomized_comb_blocks = trace.atomized_comb_blocks;
        self.flattened_comb_blocks = trace.flattened_comb_blocks;
        self.scheduled_units = trace.scheduled_units;
    }
}

#[cfg(feature = "host-runtime")]
pub struct CompilationTraceResult {
    pub res: Result<crate::simulator::Simulator, crate::simulator::SimulatorError>,
    pub trace: CompilationTrace,
}

#[cfg(feature = "host-runtime")]
impl CompilationTraceResult {
    pub fn expect(self, msg: &str) -> crate::simulator::Simulator {
        match self.res {
            Ok(sim) => sim,
            Err(err) => {
                self.trace.print();
                panic!("{}: {:?}", msg, err);
            }
        }
    }

    pub fn unwrap(self) -> crate::simulator::Simulator {
        match self.res {
            Ok(sim) => sim,
            Err(err) => {
                self.trace.print();
                panic!("{:?}", err);
            }
        }
    }
}
