//! Errors and diagnostics as JavaScript receives them.

#[cfg(not(target_arch = "wasm32"))]
use super::*;

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn runtime_errors_by_name(
    program: &celox::RuntimeProgram,
) -> HashMap<i64, (String, Vec<String>)> {
    program
        .runtime_schema
        .runtime_errors
        .iter()
        .map(|(&code, info)| {
            (
                code,
                (
                    info.message.clone(),
                    info.signals
                        .iter()
                        .map(|addr| program.get_path(addr))
                        .collect(),
                ),
            )
        })
        .collect()
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn napi_runtime_error(
    runtime_errors: &HashMap<i64, (String, Vec<String>)>,
    err: celox::RuntimeErrorCode,
) -> Error {
    match err {
        celox::RuntimeErrorCode::DetectedTrueLoopCode(code) => {
            if let Some((message, signals)) = runtime_errors.get(&code) {
                if message == "Detected True Loop" {
                    Error::from_reason(format!(
                        "{}",
                        celox::RuntimeErrorCode::DetectedTrueLoopAt {
                            signals: signals.clone(),
                        }
                    ))
                } else {
                    Error::from_reason(format!(
                        "{}",
                        celox::RuntimeErrorCode::Runtime {
                            message: message.clone(),
                            signals: signals.clone(),
                        }
                    ))
                }
            } else {
                Error::from_reason(format!("{}", celox::RuntimeErrorCode::DetectedTrueLoop))
            }
        }
        other => Error::from_reason(format!("{}", other)),
    }
}

/// Validate a raw numeric event ID before it reaches an unchecked backend lookup.
///
/// Standard Celox metadata assigns IDs by enumerating the backend event table, so
/// valid IDs are dense in `0..event_count`. Numeric N-API entry points must keep
/// this check in front of every backend call that indexes by event ID.
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn validate_event_id(event_id: u32, event_count: usize) -> Result<()> {
    if (event_id as usize) < event_count {
        return Ok(());
    }

    Err(Error::from_reason(
        celox::RuntimeErrorCode::NotAnEvent(format!("event_id={event_id}")).to_string(),
    ))
}

/// Convert an `AnalyzerError` to structured `JsonDiagnostic`s.
/// `all_sources` maps path → source content for offset → line:col conversion.
pub(super) fn analyzer_error_to_diagnostics(
    err: &veryl_analyzer::AnalyzerError,
    all_sources: &[(String, String)],
    is_error: bool,
) -> Vec<celox_ts_gen::JsonDiagnostic> {
    use miette::Diagnostic as _;

    let severity = if is_error {
        celox_ts_gen::DiagnosticSeverity::Error
    } else {
        celox_ts_gen::DiagnosticSeverity::Warning
    };

    let message = format!("{err}");
    let help = err.help().map(|h| h.to_string());
    let url = err.url().map(|u| u.to_string());

    let labels: Vec<_> = err.labels().map(|l| l.collect()).unwrap_or_default();

    if labels.is_empty() {
        return vec![celox_ts_gen::JsonDiagnostic {
            severity,
            message,
            file: String::new(),
            line: 1,
            column: 1,
            end_line: None,
            end_column: None,
            help,
            url,
        }];
    }

    labels
        .into_iter()
        .map(|label| {
            let offset = label.offset();
            let len = label.len();

            // Find which source file this offset belongs to
            // AnalyzerError uses global offsets across concatenated sources,
            // but typically the error_location is within a single file.
            // Try each source to find line:col.
            let mut file = String::new();
            let mut line = 1usize;
            let mut col = 1usize;
            let mut end_line = None;
            let mut end_col = None;

            for (path, content) in all_sources {
                // miette offsets are per-file when Parser::parse is given the source
                let mut cur_line = 1;
                let mut cur_col = 1;
                let mut found = false;

                for (i, ch) in content.char_indices() {
                    if i == offset {
                        file = path.clone();
                        line = cur_line;
                        col = cur_col;
                        found = true;
                    }
                    if found && i == offset + len {
                        end_line = Some(cur_line);
                        end_col = Some(cur_col);
                        break;
                    }
                    if ch == '\n' {
                        cur_line += 1;
                        cur_col = 1;
                    } else {
                        cur_col += 1;
                    }
                }

                if found {
                    if end_line.is_none() {
                        end_line = Some(cur_line);
                        end_col = Some(cur_col);
                    }
                    break;
                }
            }

            celox_ts_gen::JsonDiagnostic {
                severity: severity.clone(),
                message: label.label().unwrap_or(&message).to_string(),
                file,
                line,
                column: col,
                end_line,
                end_column: end_col,
                help: help.clone(),
                url: url.clone(),
            }
        })
        .collect()
}

/// Format analyzer errors with accumulated warnings for gen_ts error messages.
pub(super) fn format_errors_with_warnings(
    pass_label: &str,
    errors: &[&veryl_analyzer::AnalyzerError],
    warnings: &[veryl_analyzer::AnalyzerError],
) -> String {
    let error_msgs: Vec<String> = errors
        .iter()
        .map(|e| celox::render_diagnostic(*e))
        .collect();
    let mut msg = format!("Errors in {pass_label}: {}", error_msgs.join("; "));
    if !warnings.is_empty() {
        let warning_msgs: Vec<String> = warnings
            .iter()
            .map(|w| celox::render_diagnostic(w))
            .collect();
        msg.push_str("\n\n--- warnings ---\n\n");
        msg.push_str(&warning_msgs.join("\n"));
    }
    msg
}
