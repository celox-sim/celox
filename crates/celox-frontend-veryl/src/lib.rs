//! Veryl-owned frontend artifacts.
//!
//! Types in this crate may retain Veryl source identities for diagnostics and
//! public path lookup. Semantic design and backend phases must not depend on
//! them.

pub mod bitaccess;
pub mod bitslicer;
pub mod case;
mod config;
pub mod context_width;
mod error;
pub mod logic_tree;
pub mod loop_provenance;
mod types;

pub use config::BuildConfig;
pub use error::{LoweringPhase, ParserError, SourceLocation};
pub use types::{resolve_dims, resolve_total_width};

use celox_design::{InstanceId, ModuleId, VariableMetadata};
use fxhash::{FxHashMap as HashMap, FxHashSet as HashSet};
use std::fmt;
use veryl_analyzer::ir::{Function, Statement, VarId, VarPath};
use veryl_parser::resource_table::StrId;

#[derive(Clone)]
pub struct VariableInfo {
    pub id: VarId,
    pub path: VarPath,
    pub var_kind: veryl_analyzer::ir::VarKind,
    pub metadata: VariableMetadata,
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
            .field("kind", &self.kind)
            .field("type_kind", &self.type_kind)
            .finish()
    }
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct InstancePath(pub Vec<(StrId, usize)>);

/// Veryl source identities retained by the facade for diagnostics and public
/// path lookup. Compiler phases after elaboration should consume flattened
/// `celox-design` identities instead.
#[derive(Clone, Default)]
pub struct VerylFrontendLookup {
    pub instance_ids: HashMap<InstancePath, InstanceId>,
    pub instance_module: HashMap<InstanceId, ModuleId>,
    pub module_variables: HashMap<ModuleId, HashMap<VarId, VariableInfo>>,
    /// Reverse index from source path to source variable ID. `None` marks a
    /// path that is ambiguous within the module.
    pub module_var_path_index: HashMap<ModuleId, HashMap<VarPath, Option<VarId>>>,
    pub module_names: HashMap<ModuleId, StrId>,
}

impl fmt::Debug for VerylFrontendLookup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VerylFrontendLookup")
            .field("instances", &self.instance_module.len())
            .field("modules", &self.module_variables.len())
            .finish_non_exhaustive()
    }
}

/// Veryl-owned source input for frontend testbench lowering.
///
/// This artifact is intentionally separate from semantic design/runtime
/// schemas.  It is consumed by the testbench compiler and must not be
/// inspected by SIR optimization, layout, or backend code generation.
#[derive(Clone, Default)]
pub struct VerylTestbenchSource {
    pub initial_statements: Option<Vec<Statement>>,
    pub functions: HashMap<VarId, Function>,
}

impl fmt::Debug for VerylTestbenchSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VerylTestbenchSource")
            .field(
                "initial_statements",
                &self.initial_statements.as_ref().map(Vec::len),
            )
            .field("functions", &self.functions.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_lookup_has_no_source_identities() {
        let lookup = VerylFrontendLookup::default();
        assert!(lookup.instance_ids.is_empty());
        assert!(lookup.instance_module.is_empty());
        assert!(lookup.module_variables.is_empty());
        assert!(lookup.module_var_path_index.is_empty());
        assert!(lookup.module_names.is_empty());
    }

    #[test]
    fn default_testbench_source_is_empty() {
        let source = VerylTestbenchSource::default();
        assert!(source.initial_statements.is_none());
        assert!(source.functions.is_empty());
    }
}
