use std::collections::HashMap;
use std::sync::Arc;

use veryl_metadata::ComponentManifest;

/// A value crossing the boundary of an in-process component callback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InjectedValue {
    Bits {
        words: Vec<u64>,
        mask_xz: Vec<u64>,
        width: u32,
    },
    String(String),
    Unit,
}

/// The lifecycle operation requested from an injected component.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InjectedHook {
    Create,
    Init,
    Reset,
    Clock,
    Finish,
    Method {
        name: String,
        args: Vec<InjectedValue>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InjectedNamedValue {
    pub name: String,
    pub value: InjectedValue,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InjectedPort {
    pub name: String,
    pub direction: String,
    pub role: Option<String>,
    pub width: u32,
}

/// One synchronous call into an injected component implementation.
#[derive(Clone, Debug)]
pub struct InjectedCall {
    pub instance: String,
    pub hook: InjectedHook,
    pub inputs: Vec<InjectedNamedValue>,
    pub params: Vec<InjectedNamedValue>,
    pub ports: Vec<InjectedPort>,
    pub cycle: u64,
    pub time: u64,
    pub seed: u64,
    pub fired_clock: Option<String>,
    pub four_state: bool,
}

/// Effects returned by an injected callback. Outputs are staged using the
/// same component/NBA path as compiled native and Wasm components.
#[derive(Clone, Debug, Default)]
pub struct InjectedResult {
    pub outputs: Vec<InjectedNamedValue>,
    pub return_value: Option<InjectedValue>,
    pub failures: Vec<String>,
    pub logs: Vec<String>,
    pub finish: bool,
}

/// Synchronous component implementation supplied by an embedding runtime.
pub trait InjectedComponentHandler: Send + Sync + 'static {
    fn call(&self, call: InjectedCall) -> Result<InjectedResult, String>;
}

#[derive(Clone)]
pub(crate) struct InjectedComponentDefinition {
    pub manifest: ComponentManifest,
    pub kind: u32,
    pub handler: Arc<dyn InjectedComponentHandler>,
}

/// Component definitions injected by an embedding API for one simulator.
#[derive(Clone, Default)]
pub struct InjectedComponents {
    pub(crate) definitions: HashMap<String, InjectedComponentDefinition>,
}

impl InjectedComponents {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(
        &mut self,
        name: impl Into<String>,
        manifest_json: &str,
        handler: Arc<dyn InjectedComponentHandler>,
    ) -> Result<(), String> {
        let name = name.into();
        if name.is_empty() || name.contains("::") {
            return Err("injected component names must be non-empty identifiers".into());
        }
        let manifest = ComponentManifest::parse(manifest_json)
            .ok_or_else(|| format!("manifest of injected component `{name}` cannot be parsed"))?;
        let kind = match manifest.kind.as_deref() {
            Some("clocked") => veryl_component_sys::VRL_KIND_CLOCKED,
            Some("method_only") => veryl_component_sys::VRL_KIND_METHOD_ONLY,
            Some(kind) => return Err(format!("unsupported component kind `{kind}`")),
            None => {
                return Err(format!(
                    "manifest of injected component `{name}` has no kind"
                ));
            }
        };
        if self.definitions.contains_key(&name) {
            return Err(format!(
                "injected component `{name}` is defined more than once"
            ));
        }
        self.definitions.insert(
            name,
            InjectedComponentDefinition {
                manifest,
                kind,
                handler,
            },
        );
        Ok(())
    }

    pub(crate) fn get(&self, name: &str) -> Option<&InjectedComponentDefinition> {
        self.definitions.get(name)
    }

    pub(crate) fn manifests(&self) -> Vec<(String, ComponentManifest)> {
        self.definitions
            .iter()
            .map(|(name, definition)| (name.clone(), definition.manifest.clone()))
            .collect()
    }
}
