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

/// Diagnostics that strict mode promotes to errors. Each has a defined
/// fallback, so the default severity is `warn`: the design still compiles
/// with the fallback semantics and the diagnostic stays visible.
/// IEEE 1800-2023 9.2.2.2 likewise asks tools to warn, not fail, when
/// latched behavior is inferred in `always_comb`.
///
/// `shared_always_temporary` is deliberately absent: it is not a
/// diagnostic severity at all. The `[compatibility]` opt-in of the same
/// name selects per-block temporaries, which changes simulation
/// results, so it is controlled by that boolean, never by a level.
pub(crate) const STRICT_CODES: [&str; 7] = [
    "mixed_timescale",
    "duplicate_module",
    "interface_array_query",
    "interface_array_parameter",
    "unpacked_port_default",
    "multiple_always_ff_drivers",
    "inferred_latch",
];

/// Overrides keyed by the code shown in a compilation diagnostic.
/// Unlisted diagnostics retain their default severity. Only explicitly
/// recoverable errors can be relaxed.
///
/// By default every recoverable diagnostic is a warning. With
/// [`strict`](Self::strict) enabled, the codes in `STRICT_CODES` default
/// to errors, which restores the pre-strict-mode behavior. An explicit
/// per-code override always wins over strict mode.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct DiagnosticPolicy {
    /// Promote the recoverable diagnostics to errors by default.
    pub strict: bool,
    /// Compatibility opt-in: treat a variable written by several
    /// `always @*` blocks as a temporary of each block. Not a diagnostic
    /// level; when enabled, the deviation is reported as a warning.
    pub shared_always_temporary: bool,
    levels: BTreeMap<String, DiagnosticLevel>,
}

impl serde::Serialize for DiagnosticPolicy {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.levels.serialize(serializer)
    }
}

impl<'de> serde::Deserialize<'de> for DiagnosticPolicy {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self {
            strict: false,
            shared_always_temporary: false,
            levels: BTreeMap::deserialize(deserializer)?,
        })
    }
}

impl DiagnosticPolicy {
    /// Set a per-code override. Later assignments replace earlier ones.
    pub fn set(&mut self, code: impl Into<String>, level: DiagnosticLevel) {
        self.levels.insert(code.into(), level);
    }

    /// The explicit per-code overrides, without the strict-mode default.
    pub fn overrides(&self) -> &BTreeMap<String, DiagnosticLevel> {
        &self.levels
    }

    /// Enable or disable strict mode, returning the updated policy.
    pub fn strict(mut self, strict: bool) -> Self {
        self.strict = strict;
        self
    }

    /// Merge `other` over this policy: its per-code overrides win.
    /// Strict mode is left unchanged; callers apply it separately.
    pub fn merge(&mut self, other: &Self) {
        self.levels.extend(
            other
                .levels
                .iter()
                .map(|(code, level)| (code.clone(), *level)),
        );
    }

    /// Enable or disable shared `always @*` temporaries (the
    /// `[compatibility]` opt-in), returning the updated policy.
    pub fn with_shared_always_temporary(mut self, enable: bool) -> Self {
        self.shared_always_temporary = enable;
        self
    }

    /// Supply the legacy latch opt-in only when no explicit diagnostic setting exists.
    /// Explicit per-code policy wins regardless of builder setter order.
    pub fn with_inferred_latches(mut self, allow: bool) -> Self {
        if allow {
            self.levels
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
    /// A root `strict = true` in the same file enables strict mode.
    pub fn from_file(path: &Path) -> Result<Self, String> {
        #[derive(serde::Deserialize, Default)]
        struct Config {
            #[serde(default)]
            diagnostics: DiagnosticPolicy,
            strict: Option<bool>,
        }
        let text = std::fs::read_to_string(path)
            .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
        toml::from_str::<Config>(&text)
            .map(|config| config.diagnostics.strict(config.strict.unwrap_or(false)))
            .map_err(|error| format!("failed to parse {}: {error}", path.display()))
    }

    pub(crate) fn level(&self, code: &str) -> DiagnosticLevel {
        // `shared_always_temporary` is a compatibility boolean, not a
        // diagnostic level: a level entry for it has no effect.
        if code == "shared_always_temporary" {
            return if self.shared_always_temporary {
                DiagnosticLevel::Warn
            } else {
                DiagnosticLevel::Error
            };
        }
        self.levels
            .get(code)
            .copied()
            .unwrap_or(if self.strict && STRICT_CODES.contains(&code) {
                DiagnosticLevel::Error
            } else {
                DiagnosticLevel::Warn
            })
    }

    pub(crate) fn sv_options(
        &self,
        timescale: Option<crate::SvTimescale>,
        legacy_empty_reductions: bool,
    ) -> celox_frontend_sv::AnalysisOptions {
        celox_frontend_sv::AnalysisOptions {
            allow_mixed_timescales: self.level("mixed_timescale") != DiagnosticLevel::Error,
            first_duplicate_module: self.level("duplicate_module") != DiagnosticLevel::Error,
            shared_always_temporaries: self.level("shared_always_temporary")
                != DiagnosticLevel::Error,
            default_timescale: timescale.map(|scale| (scale.unit, scale.precision)),
            legacy_empty_reductions,
            ..Default::default()
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
