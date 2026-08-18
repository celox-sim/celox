use std::fmt;

use celox_design::{AbsoluteAddrBase, InstanceId, ModuleId, StateAddr, VariableMetadata};

use crate::{HashMap, HashSet};

pub type SourceAddr = AbsoluteAddrBase<SourceVarId>;

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
    /// arrays because that is the source-independent storage shape. The
    /// testbench adapter also needs packed shape for chained selects.
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
