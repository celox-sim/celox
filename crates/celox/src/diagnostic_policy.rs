//! Per-code compilation diagnostic severity. Runtime failures are unaffected.

use std::collections::BTreeMap;
use std::path::Path;

use miette::Diagnostic;

use crate::{CompilationWarning, SimulatorError, SimulatorErrorKind};

/// The action for a recoverable compilation diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiagnosticLevel {
    Allow,
    Warn,
    Error,
}

/// Overrides keyed by the code shown in a compilation diagnostic.
/// Unlisted diagnostics retain their default severity. Only explicitly
/// recoverable errors can be relaxed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct DiagnosticPolicy(pub BTreeMap<String, DiagnosticLevel>);

impl DiagnosticPolicy {
    /// Set a per-code override. Later assignments replace earlier ones.
    pub fn set(&mut self, code: impl Into<String>, level: DiagnosticLevel) {
        self.0.insert(code.into(), level);
    }

    /// Supply the legacy latch opt-in only when no explicit diagnostic setting exists.
    /// Explicit per-code policy wins regardless of builder setter order.
    pub fn with_inferred_latches(mut self, allow: bool) -> Self {
        if allow {
            self.0
                .entry("inferred_latch".to_string())
                .or_insert(DiagnosticLevel::Allow);
        }
        self
    }

    /// Parse the CLI spelling `CODE=allow|warn|error`.
    pub fn set_override(&mut self, value: &str) -> Result<(), String> {
        let (code, level) = value
            .split_once('=')
            .ok_or_else(|| "diagnostic override must be CODE=allow|warn|error".to_string())?;
        if code.is_empty()
            || !code
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return Err(format!("invalid diagnostic code `{code}`"));
        }
        let level = match level {
            "allow" => DiagnosticLevel::Allow,
            "warn" => DiagnosticLevel::Warn,
            "error" => DiagnosticLevel::Error,
            _ => {
                return Err(format!(
                    "invalid diagnostic level `{level}`; use allow, warn, or error"
                ));
            }
        };
        self.set(code, level);
        Ok(())
    }

    /// Read the diagnostics table of a celox.toml, ignoring other configuration.
    pub fn from_file(path: &Path) -> Result<Self, String> {
        #[derive(serde::Deserialize, Default)]
        struct Config {
            #[serde(default)]
            diagnostics: DiagnosticPolicy,
        }
        let text = std::fs::read_to_string(path)
            .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
        toml::from_str::<Config>(&text)
            .map(|config| config.diagnostics)
            .map_err(|error| format!("failed to parse {}: {error}", path.display()))
    }

    pub(crate) fn level(&self, code: &str) -> DiagnosticLevel {
        self.0.get(code).copied().unwrap_or(
            if matches!(
                code,
                "mixed_timescale"
                    | "unpacked_port_default"
                    | "multiple_always_ff_drivers"
                    | "inferred_latch"
            ) {
                DiagnosticLevel::Error
            } else {
                DiagnosticLevel::Warn
            },
        )
    }

    #[cfg(feature = "systemverilog")]
    pub(crate) fn sv_options(
        &self,
        timescale: Option<crate::SvTimescale>,
    ) -> celox_frontend_sv::AnalysisOptions {
        celox_frontend_sv::AnalysisOptions {
            allow_mixed_timescales: self.level("mixed_timescale") != DiagnosticLevel::Error,
            default_timescale: timescale.map(|scale| (scale.unit, scale.precision)),
        }
    }

    /// Suppress diagnostics or reject those configured as errors.
    pub fn apply(&self, warnings: &mut Vec<CompilationWarning>) -> Result<(), SimulatorError> {
        let mut retained = Vec::new();
        let mut errors = Vec::new();
        for warning in warnings.drain(..) {
            let code = warning
                .code()
                .map(|code| code.to_string())
                .unwrap_or_default();
            match self.level(&code) {
                DiagnosticLevel::Allow => (),
                DiagnosticLevel::Warn => retained.push(warning),
                DiagnosticLevel::Error => errors.push(warning),
            }
        }
        *warnings = retained;
        if errors.is_empty() {
            Ok(())
        } else {
            Err(
                SimulatorError::new(SimulatorErrorKind::DiagnosticPolicy(errors))
                    .with_warnings(std::mem::take(warnings)),
            )
        }
    }
}
