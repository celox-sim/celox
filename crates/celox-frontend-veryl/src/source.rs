use std::fmt;

use celox_design::{InstanceId, ModuleId};
use veryl_analyzer::ir::{Expression, Function, Statement, VarId, VarPath};

use crate::{
    FrontendLookup, FusedSirOptimizationHints, HashMap, ScheduledRtl, ScheduledRtlOutput,
    SourceVarId,
};

pub type RegionedVarAddr = celox_design::RegionedVarAddrBase<VarId>;
pub type AbsoluteAddr = celox_design::AbsoluteAddrBase<VarId>;
pub type RegionedAbsoluteAddr = celox_design::RegionedAbsoluteAddrBase<VarId>;
pub type GlueAddr = celox_slt::GlueAddrBase<VarId>;
pub type GlueBlock = celox_slt::GlueBlockBase<VarId>;
pub type ModuleInitialMemoryValue = celox_design::InitialStateValue<VarId>;

pub(crate) fn function_call_arg<'a, T>(args: &'a [(VarPath, T)], path: &VarPath) -> Option<&'a T> {
    args.iter()
        .find_map(|(candidate, value)| (candidate == path).then_some(value))
}

pub(crate) fn function_call_has_arg<T>(args: &[(VarPath, T)], path: &VarPath) -> bool {
    args.iter().any(|(candidate, _)| candidate == path)
}

/// Compiler-only bridge from Veryl analyzer IDs into neutral frontend IDs.
/// This is carried by the Veryl testbench source and discarded after frontend
/// bytecode compilation.
#[derive(Clone, Default)]
pub struct VerylIdMap {
    pub module_variables: HashMap<ModuleId, HashMap<VarId, SourceVarId>>,
}

impl VerylIdMap {
    pub fn source_var(&self, module: ModuleId, var: VarId) -> Option<SourceVarId> {
        self.module_variables.get(&module)?.get(&var).copied()
    }

    pub fn instance_var(
        &self,
        lookup: &FrontendLookup,
        instance: InstanceId,
        var: VarId,
    ) -> Option<SourceVarId> {
        let module = *lookup.instance_module.get(&instance)?;
        self.source_var(module, var)
    }
}

/// Veryl-owned source input for frontend testbench lowering.
///
/// This artifact is intentionally separate from semantic design/runtime
/// schemas. It is consumed by the testbench compiler and must not be inspected
/// by SIR optimization, layout, or backend code generation.
#[derive(Clone, Default)]
pub struct VerylTestbenchSource {
    pub id_map: VerylIdMap,
    pub initial_statements: Option<Vec<Statement>>,
    pub functions: HashMap<VarId, Function>,
    pub components: Vec<celox_testbench::TestbenchComponent>,
    pub component_bindings: Vec<VerylComponentBinding>,
    pub component_libraries: Vec<celox_testbench::ComponentLibrary>,
    pub component_file_base: Option<std::path::PathBuf>,
}

impl VerylTestbenchSource {
    pub fn is_empty(&self) -> bool {
        self.initial_statements.is_none()
            && self.functions.is_empty()
            && self.components.is_empty()
            && self.component_bindings.is_empty()
            && self.component_libraries.is_empty()
            && self.component_file_base.is_none()
    }
}

#[derive(Clone)]
pub struct VerylComponentBinding {
    pub instance: String,
    pub parent_instance: InstanceId,
    pub functions: HashMap<VarId, Function>,
    pub connections: Vec<VerylComponentConnectionBinding>,
}

#[derive(Clone)]
pub struct VerylComponentConnectionBinding {
    pub port: String,
    pub input: Option<Expression>,
    pub input_target: Option<VerylComponentInputBinding>,
    pub output: Option<veryl_analyzer::ir::AssignDestination>,
    pub event: Option<VerylComponentEventBinding>,
}

#[derive(Clone)]
pub enum VerylComponentInputBinding {
    Root {
        id: VarId,
        index: veryl_analyzer::ir::VarIndex,
        select: veryl_analyzer::ir::VarSelect,
    },
    Hierarchical(Box<veryl_analyzer::ir::HierVarRef>),
}

#[derive(Clone)]
pub enum VerylComponentEventBinding {
    Root(VarId),
    Hierarchical(Box<veryl_analyzer::ir::HierVarRef>),
}

impl fmt::Debug for VerylTestbenchSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VerylTestbenchSource")
            .field(
                "initial_statements",
                &self.initial_statements.as_ref().map(Vec::len),
            )
            .field("functions", &self.functions.len())
            .field("components", &self.components.len())
            .field("component_bindings", &self.component_bindings.len())
            .field("component_libraries", &self.component_libraries.len())
            .finish()
    }
}

/// Veryl compiler output before its source-owned testbench syntax is consumed.
#[derive(Clone, Debug)]
pub struct VerylScheduledRtlOutput {
    pub scheduled: ScheduledRtl,
    pub fused_optimization_hints: FusedSirOptimizationHints,
    pub testbench_source: VerylTestbenchSource,
}

impl VerylScheduledRtlOutput {
    /// Drop the empty Veryl sidecar used by pure SystemVerilog compilation.
    pub fn into_shared(self) -> ScheduledRtlOutput {
        debug_assert!(self.testbench_source.is_empty());
        ScheduledRtlOutput {
            scheduled: self.scheduled,
            fused_optimization_hints: self.fused_optimization_hints,
        }
    }
}
