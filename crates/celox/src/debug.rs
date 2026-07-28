use crate::HashMap;
use crate::ir::{AbsoluteAddr, ExecutionUnit, ModuleId, RegionedAbsoluteAddr, SimModule};
use crate::logic_tree::{LogicPath, SLTNodeArena};
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
    pub ff_air: bool,
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
    pub pre_atomized_comb_blocks:
        Option<(Vec<LogicPath<AbsoluteAddr>>, SLTNodeArena<AbsoluteAddr>)>,
    pub atomized_comb_blocks: Option<(Vec<LogicPath<AbsoluteAddr>>, SLTNodeArena<AbsoluteAddr>)>,
    pub flattened_comb_blocks: Option<(Vec<LogicPath<AbsoluteAddr>>, SLTNodeArena<AbsoluteAddr>)>,
    pub scheduled_units: Option<Vec<ExecutionUnit<RegionedAbsoluteAddr>>>,
    /// Complete FF AIR recorded before EventIR/SSA construction.
    pub ff_air: Option<String>,
    pub pre_optimized_sir: Option<crate::ir::Program>,
    pub post_optimized_sir: Option<crate::ir::Program>,
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

#[cfg(not(target_arch = "wasm32"))]
pub struct CompilationTraceResult {
    pub res: Result<crate::simulator::Simulator, crate::simulator::SimulatorError>,
    pub trace: CompilationTrace,
}

#[cfg(not(target_arch = "wasm32"))]
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
