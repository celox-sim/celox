//! Explicit observation and intervention contracts for compiled designs.

/// Hierarchical signal paths, including the top module (for example `Top.uut.state`).
/// Force targets are also live roots. Empty lists preserve ordinary optimization.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SignalAccessOptions {
    pub live_signals: Vec<String>,
    pub force_signals: Vec<String>,
}
