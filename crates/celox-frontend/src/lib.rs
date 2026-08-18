//! HDL frontend construction and shared symbolic assembly for Celox.
//!
//! Veryl lowering lives in this crate alongside the shared symbolic assembly
//! used by every HDL adapter. SystemVerilog syntax and semantic
//! analysis remain isolated in `celox-sv-analyzer`; the optional
//! [`systemverilog`] module only adapts analyzed SV into the shared assembly
//! model. Semantic design and backend phases must not depend on source-language
//! identities retained here for diagnostics and public path lookup.

pub mod bitaccess;
pub mod bitslicer;
pub mod case;
mod config;
pub mod context_width;
mod design_assembly;
mod dynamic_for_check;
mod error;
pub mod ff;
pub mod flattening;
mod global_ff;
pub mod hierarchy;
pub mod logic_tree;
pub mod loop_provenance;
pub mod module;
mod module_artifact;
pub mod registry;
#[cfg(feature = "systemverilog")]
pub mod systemverilog;
mod testbench;
mod trace;
mod types;

pub use config::BuildConfig;
pub use design_assembly::schedule_symbolic_rtl;
pub use dynamic_for_check::{check_dynamic_for_bounds, check_elaborated_dynamic_for_bounds};
pub use error::{FrontendDiagnostic, LoweringPhase, ParserError, SourceLocation};
pub use global_ff::{
    FfClockRecipe, FfRuntimeRelocation, SharedClockLowering, build_ff_clock_recipes,
};
pub use hierarchy::{
    ExternalHierarchy, ExternalModule, SymbolicRtl, parse_ir, parse_ir_with_external_hierarchy,
    parse_ir_with_loop_provenance,
};
pub use module_artifact::{
    FusedSirOptimizationHints, RelocationModule, ScheduledRtl, ScheduledRtlOutput, SimModule,
};
pub use testbench::{collect_testbench_observability, compile_semantic_testbench};
pub use trace::{FrontendTrace, FrontendTraceOptions};
pub use types::{resolve_dims, resolve_total_width};

use celox_design::{InstanceId, ModuleId, StateAddr, VariableMetadata};
use fxhash::{FxHashMap as HashMap, FxHashSet as HashSet};
use std::fmt;
use veryl_analyzer::ir::{Expression, Function, Statement, VarId, VarPath};

pub type RegionedVarAddr = celox_design::RegionedVarAddrBase<VarId>;
pub type AbsoluteAddr = celox_design::AbsoluteAddrBase<VarId>;
pub type SourceAddr = celox_design::AbsoluteAddrBase<SourceVarId>;
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

/// Frontend-local identity of a source variable within one module.
///
/// This is deliberately distinct from every parser or analyzer's variable ID.
/// A frontend projects its native IDs into this namespace before constructing
/// source lookup metadata retained by the runtime.
#[derive(Debug, Clone, Copy, Default, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct SourceVarId(pub u32);

impl fmt::Display for SourceVarId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "var{}", self.0)
    }
}

/// Source-language-independent role of a frontend variable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VariableKind {
    Parameter,
    Constant,
    Input,
    Output,
    Inout,
    Variable,
    Let,
}

impl VariableKind {
    pub const fn is_port(self) -> bool {
        matches!(self, Self::Input | Self::Output | Self::Inout)
    }

    pub const fn description(self) -> &'static str {
        match self {
            Self::Parameter => "parameter",
            Self::Constant => "constant",
            Self::Input => "input",
            Self::Output => "output",
            Self::Inout => "inout",
            Self::Variable => "variable",
            Self::Let => "let-bounded variable",
        }
    }
}

#[derive(Clone)]
pub struct VariableInfo {
    pub id: SourceVarId,
    pub path: Vec<String>,
    pub var_kind: VariableKind,
    pub signed: bool,
    pub metadata: VariableMetadata,
    /// Per-dimension sizes for the packed shape of the variable.
    ///
    /// `VariableMetadata::array_dims` deliberately only describes unpacked
    /// arrays because that is the source-independent storage shape.  The
    /// testbench frontend also needs the packed shape to lower a chained
    /// select such as `logic<4, 4> m; m[2][1]` to one linear bit offset.
    pub packed_dims: Vec<usize>,
}

impl std::ops::Deref for VariableInfo {
    type Target = VariableMetadata;

    fn deref(&self) -> &Self::Target {
        &self.metadata
    }
}

impl fmt::Debug for VariableInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VariableInfo")
            .field("width", &self.width)
            .field("id", &self.id)
            .field("is_4state", &self.is_4state)
            .field("signed", &self.signed)
            .field("kind", &self.kind)
            .field("type_kind", &self.type_kind)
            .finish()
    }
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct InstancePath(pub Vec<(String, usize)>);

/// Source-language-independent lookup retained for diagnostics and public paths.
/// Parser-native IDs are projected into [`SourceVarId`] before this artifact is
/// built and do not cross into runtime metadata.
#[derive(Clone, Default)]
pub struct FrontendLookup {
    pub instance_ids: HashMap<InstancePath, InstanceId>,
    pub instance_module: HashMap<InstanceId, ModuleId>,
    /// Elaborated children whose source-facing name requires an index.
    pub indexed_instances: HashSet<InstanceId>,
    pub module_variables: HashMap<ModuleId, HashMap<SourceVarId, VariableInfo>>,
    /// Reverse index from source path to source variable ID. `None` marks a
    /// path that is ambiguous within the module.
    pub module_var_path_index: HashMap<ModuleId, HashMap<Vec<String>, Option<SourceVarId>>>,
    pub module_names: HashMap<ModuleId, String>,
    /// Bidirectional boundary map between frontend source identities and the
    /// dense source-independent state identities consumed by later phases.
    pub source_to_state: HashMap<SourceAddr, StateAddr>,
    pub state_to_source: HashMap<StateAddr, SourceAddr>,
    /// Event aliases projected to the canonical runtime event domain.
    pub event_aliases: HashMap<StateAddr, StateAddr>,
}

impl fmt::Debug for FrontendLookup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FrontendLookup")
            .field("instances", &self.instance_module.len())
            .field("modules", &self.module_variables.len())
            .field("projected_state_objects", &self.source_to_state.len())
            .finish_non_exhaustive()
    }
}

impl FrontendLookup {
    pub fn instance_path_segments(&self, path: &InstancePath) -> Vec<String> {
        let mut prefix = Vec::with_capacity(path.0.len());
        path.0
            .iter()
            .map(|(name, index)| {
                prefix.push((name.clone(), *index));
                let instance = self.instance_ids.get(&InstancePath(prefix.clone()));
                if instance.is_some_and(|id| self.indexed_instances.contains(id)) {
                    format!("{name}[{index}]")
                } else {
                    name.clone()
                }
            })
            .collect()
    }

    pub fn root_instance_and_module(&self) -> Option<(InstanceId, ModuleId)> {
        let instance_id = *self.instance_ids.get(&InstancePath(Vec::new()))?;
        let module_id = *self.instance_module.get(&instance_id)?;
        Some((instance_id, module_id))
    }

    pub fn root_variable(&self, var_id: SourceVarId) -> Option<(StateAddr, &VariableInfo)> {
        let (instance_id, _) = self.root_instance_and_module()?;
        self.instance_variable(instance_id, var_id)
    }

    pub fn instance_variable(
        &self,
        instance_id: InstanceId,
        var_id: SourceVarId,
    ) -> Option<(StateAddr, &VariableInfo)> {
        let module_id = *self.instance_module.get(&instance_id)?;
        let info = self.module_variables.get(&module_id)?.get(&var_id)?;
        let address = self.state_address(&SourceAddr {
            instance_id,
            var_id,
        })?;
        Some((address, info))
    }

    pub fn root_named_variable(&self, name: &str) -> Option<(StateAddr, &VariableInfo)> {
        let (_, module_id) = self.root_instance_and_module()?;
        let var_id = self
            .module_var_path_index
            .get(&module_id)?
            .get(&vec![name.to_string()])
            .copied()
            .flatten()?;
        self.root_variable(var_id)
    }

    pub fn get_path(&self, address: &SourceAddr) -> String {
        let instance_path = self
            .instance_ids
            .iter()
            .find(|(_, id)| **id == address.instance_id)
            .map(|(path, _)| path);
        let module_id = self.instance_module.get(&address.instance_id).unwrap();
        let module_vars = self.module_variables.get(module_id).unwrap();
        let variable_path = module_vars
            .values()
            .find(|info| info.id == address.var_id)
            .map(|info| &info.path);

        let mut result = Vec::new();
        if let Some(instance_path) = instance_path {
            result.extend(self.instance_path_segments(instance_path));
        }
        if let Some(variable_path) = variable_path {
            result.extend(variable_path.iter().cloned());
        }
        result.join(".")
    }

    pub fn get_state_path(&self, address: &StateAddr) -> String {
        self.state_to_source
            .get(address)
            .map(|source| self.get_path(source))
            .unwrap_or_else(|| address.to_string())
    }

    pub fn source_address(&self, address: &StateAddr) -> Option<SourceAddr> {
        self.state_to_source.get(address).copied()
    }

    pub fn state_address(&self, address: &SourceAddr) -> Option<StateAddr> {
        self.source_to_state.get(address).copied()
    }
}

/// Compatibility alias for callers compiled against the Veryl-only frontend
/// facade. New code should use [`FrontendLookup`].
pub type VerylFrontendLookup = FrontendLookup;

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
/// schemas.  It is consumed by the testbench compiler and must not be
/// inspected by SIR optimization, layout, or backend code generation.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_lookup_has_no_source_identities() {
        let lookup = FrontendLookup::default();
        assert!(lookup.instance_ids.is_empty());
        assert!(lookup.instance_module.is_empty());
        assert!(lookup.module_variables.is_empty());
        assert!(lookup.module_var_path_index.is_empty());
        assert!(lookup.module_names.is_empty());
        assert!(lookup.source_to_state.is_empty());
        assert!(lookup.state_to_source.is_empty());
    }

    #[test]
    fn default_testbench_source_is_empty() {
        let source = VerylTestbenchSource::default();
        assert!(source.initial_statements.is_none());
        assert!(source.functions.is_empty());
        assert!(source.components.is_empty());
        assert!(source.component_libraries.is_empty());
        assert!(source.component_file_base.is_none());
    }
}
