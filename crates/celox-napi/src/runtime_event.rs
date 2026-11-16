//! Runtime events and assertion sites as JavaScript receives them.

use super::*;

/// A runtime event of a simulation: `$display`, `$write`, a report of a
/// severity task, `$finish`, `$stop`, or events the design's event buffer
/// overwrote before they were drained.
#[cfg(not(target_arch = "wasm32"))]
#[napi(object)]
pub struct NapiRuntimeEvent {
    /// `"display"`, `"write"`, `"report"`, `"finish"`, `"stop"` or
    /// `"missed"`.
    pub kind: String,
    pub message: Option<Uint8Array>,
    /// `"info"`, `"warning"`, `"error"` or `"fatal"` for a report.
    pub severity: Option<String>,
    pub time: Option<f64>,
    pub scope: Option<String>,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub assertion: Option<NapiAssertionSite>,
    /// The number of missed events.
    pub count: Option<f64>,
}

/// The assertion whose action block issued a report.
#[cfg(not(target_arch = "wasm32"))]
#[napi(object)]
pub struct NapiAssertionSite {
    pub label: Option<String>,
    /// `"assert"`, `"assume"` or `"cover"`.
    pub directive: String,
    /// `"none"`, `"observed"` (`#0`) or `"final"`.
    pub deferral: String,
    /// `"pass"` or `"fail"`.
    pub outcome: String,
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn severity_name(severity: celox::Severity) -> &'static str {
    match severity {
        celox::Severity::Info => "info",
        celox::Severity::Warning => "warning",
        celox::Severity::Error => "error",
        celox::Severity::Fatal => "fatal",
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn parse_severity(name: &str) -> Result<celox::Severity> {
    Ok(match name {
        "info" => celox::Severity::Info,
        "warning" => celox::Severity::Warning,
        "error" => celox::Severity::Error,
        "fatal" => celox::Severity::Fatal,
        _ => {
            return Err(Error::from_reason(format!(
                "unknown severity `{name}`: expected info, warning, error or fatal"
            )));
        }
    })
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn napi_runtime_event(event: celox::RuntimeEvent) -> NapiRuntimeEvent {
    let mut out = NapiRuntimeEvent {
        kind: String::new(),
        message: None,
        severity: None,
        time: None,
        scope: None,
        file: None,
        line: None,
        assertion: None,
        count: None,
    };
    match event {
        celox::RuntimeEvent::Display { message } => {
            out.kind = "display".into();
            out.message = Some(message.into());
        }
        celox::RuntimeEvent::Write { message } => {
            out.kind = "write".into();
            out.message = Some(message.into());
        }
        celox::RuntimeEvent::Report(report) => {
            out.kind = "report".into();
            out.severity = Some(severity_name(report.severity).into());
            out.message = Some(report.message.into());
            out.time = report.time.map(|time| time as f64);
            out.scope = report.scope;
            if let Some(location) = report.location {
                out.file = Some(location.file);
                out.line = Some(location.line as u32);
            }
            out.assertion = report.assertion.map(|assertion| NapiAssertionSite {
                label: assertion.label,
                directive: match assertion.directive {
                    celox::AssertionDirective::Assert => "assert",
                    celox::AssertionDirective::Assume => "assume",
                    celox::AssertionDirective::Cover => "cover",
                }
                .into(),
                deferral: match assertion.deferral {
                    celox::AssertionDeferral::None => "none",
                    celox::AssertionDeferral::Observed => "observed",
                    celox::AssertionDeferral::Final => "final",
                }
                .into(),
                outcome: match assertion.outcome {
                    celox::AssertionOutcome::Pass => "pass",
                    celox::AssertionOutcome::Fail => "fail",
                }
                .into(),
            });
        }
        celox::RuntimeEvent::Finish => out.kind = "finish".into(),
        celox::RuntimeEvent::Stop => out.kind = "stop".into(),
        celox::RuntimeEvent::Missed { count } => {
            out.kind = "missed".into();
            out.count = Some(count as f64);
        }
    }
    out
}

/// The error of a run that stopped at a report of the `failOn` severity.
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn check_failure(sim: &celox::Simulation) -> Result<()> {
    match sim.failure() {
        Some(report) => Err(Error::from_reason(format!(
            "failOn: {}",
            String::from_utf8_lossy(&report.render())
        ))),
        None => Ok(()),
    }
}
