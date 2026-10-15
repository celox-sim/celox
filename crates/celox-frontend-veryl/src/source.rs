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
    /// Length of each initial block in `initial_statements`; empty means one block.
    pub initial_block_lengths: Vec<usize>,
    /// Instance owning these statements; None selects the design root.
    pub instance: Option<InstanceId>,
    /// Flat list of initial procedures in all child instances, in instance-path order.
    pub child_sources: Vec<VerylTestbenchSource>,
    /// Function temporaries need process-private storage across clock waits.
    pub function_locals: Vec<VarId>,
    pub functions: HashMap<VarId, Function>,
    /// Configured generator periods in this elaborated module, including clocks
    /// used only through reset.assert() (which carries no period in Veryl IR).
    pub clock_periods: HashMap<veryl_parser::resource_table::StrId, u64>,
    pub components: Vec<celox_testbench::TestbenchComponent>,
    pub component_bindings: Vec<VerylComponentBinding>,
    pub component_libraries: Vec<celox_testbench::ComponentLibrary>,
    pub component_file_base: Option<std::path::PathBuf>,
    /// The `initial` blocks were compiled into process kernels: the
    /// testbench program carries only the components.
    pub kernels: bool,
}

impl VerylTestbenchSource {
    pub(crate) fn base_instance(&self, lookup: &FrontendLookup) -> InstanceId {
        self.instance
            .unwrap_or_else(|| lookup.root_instance_and_module().unwrap().0)
    }

    pub(crate) fn sources(&self) -> impl Iterator<Item = &Self> {
        std::iter::once(self).chain(self.child_sources.iter())
    }

    pub(crate) fn initial_blocks(&self) -> Vec<&[Statement]> {
        let Some(statements) = &self.initial_statements else {
            return Vec::new();
        };
        if self.initial_block_lengths.is_empty() {
            return vec![statements];
        }
        let mut offset = 0;
        self.initial_block_lengths
            .iter()
            .map(|&length| {
                let start = offset;
                offset += length;
                &statements[start..offset]
            })
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.child_sources.iter().all(Self::is_empty)
            && self.initial_statements.is_none()
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
            .field("initial_block_lengths", &self.initial_block_lengths)
            .field("instance", &self.instance)
            .field("child_sources", &self.child_sources.len())
            .field("function_locals", &self.function_locals.len())
            .field("clock_periods", &self.clock_periods)
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
    /// Bound checks resolved in each process owner's elaborated instance scope.
    pub dynamic_for_diagnostics: Vec<crate::FrontendDiagnostic>,
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
