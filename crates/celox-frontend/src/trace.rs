use celox_design::ModuleId;
use celox_sir::ExecutionUnit;
use celox_slt::{LogicPath, SLTNodeArena};

use crate::{AbsoluteAddr, HashMap, RegionedAbsoluteAddr, SimModule};

/// Frontend-owned trace switches. Backend and optimizer trace options are
/// intentionally absent from this contract.
#[derive(Debug, Clone, Copy, Default)]
pub struct FrontendTraceOptions {
    pub phase_timing: bool,
    pub sim_modules: bool,
    pub pre_atomized_comb_blocks: bool,
    pub atomized_comb_blocks: bool,
    pub flattened_comb_blocks: bool,
    pub scheduled_units: bool,
}

/// Optional diagnostics produced while `SymbolicRtl` is consumed.
#[derive(Debug, Clone, Default)]
pub struct FrontendTrace {
    pub sim_modules: Option<HashMap<ModuleId, SimModule>>,
    pub pre_atomized_comb_blocks:
        Option<(Vec<LogicPath<AbsoluteAddr>>, SLTNodeArena<AbsoluteAddr>)>,
    pub atomized_comb_blocks: Option<(Vec<LogicPath<AbsoluteAddr>>, SLTNodeArena<AbsoluteAddr>)>,
    pub flattened_comb_blocks: Option<(Vec<LogicPath<AbsoluteAddr>>, SLTNodeArena<AbsoluteAddr>)>,
    pub scheduled_units: Option<Vec<ExecutionUnit<RegionedAbsoluteAddr>>>,
}
