use crate::HashMap;
use crate::ir::{AbsoluteAddr, ExecutionUnit, RegionedAbsoluteAddr, SimModule};
use crate::logic_tree::{LogicPath, SLTNodeArena};
use veryl_parser::resource_table::StrId;
mod output;
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
    pub output_to_stdout: bool,
}

#[derive(Debug, Clone, Default)]
pub struct CompilationTrace {
    pub sim_modules: Option<HashMap<StrId, SimModule>>,
    pub pre_atomized_comb_blocks:
        Option<(Vec<LogicPath<AbsoluteAddr>>, SLTNodeArena<AbsoluteAddr>)>,
    pub atomized_comb_blocks: Option<(Vec<LogicPath<AbsoluteAddr>>, SLTNodeArena<AbsoluteAddr>)>,
    pub flattened_comb_blocks: Option<(Vec<LogicPath<AbsoluteAddr>>, SLTNodeArena<AbsoluteAddr>)>,
    pub scheduled_units: Option<Vec<ExecutionUnit<RegionedAbsoluteAddr>>>,
    pub pre_optimized_sir: Option<crate::ir::Program>,
    pub post_optimized_sir: Option<crate::ir::Program>,
    pub analyzer_ir: Option<String>,
    pub pre_optimized_clif: Option<String>,
    pub post_optimized_clif: Option<String>,
    pub native: Option<String>,
}

pub struct CompilationTraceResult {
    pub res: Result<crate::simulator::Simulator, crate::simulator::SimulatorError>,
    pub trace: CompilationTrace,
}

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
