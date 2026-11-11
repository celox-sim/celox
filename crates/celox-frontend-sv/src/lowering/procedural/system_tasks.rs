//! System task statements: how a task reports, and its message template
//! and arguments.

use super::*;

/// How a system task statement reports.
pub(in crate::lowering) enum SystemTaskKind {
    /// `$display` and `$write`, with the radix of their `b`, `o` and `h`
    /// variants (`d` otherwise).
    Print(RuntimeEventKind, char),
    /// `$finish` and `$stop`, with their event kind.
    Control(RuntimeEventKind),
    /// `$error`, `$warning`, `$info`: a message, then execution continues.
    Message(Severity),
    /// `$fatal`: a message, then the simulation ends with an error.
    Fatal,
    /// Veryl's assertion calls: eagerly evaluate every argument, then report
    /// only if the condition fails. These are not standard SV assertions.
    Assertion(RuntimeEventKind),
}

impl SystemTaskKind {
    pub(in crate::lowering) fn is_fatal(&self) -> bool {
        self.event_kind().is_fatal()
    }

    pub(in crate::lowering) fn event_kind(&self) -> RuntimeEventKind {
        match *self {
            Self::Print(kind, _) | Self::Control(kind) | Self::Assertion(kind) => kind,
            Self::Message(severity) => RuntimeEventKind::Report(severity),
            Self::Fatal => RuntimeEventKind::Report(Severity::Fatal),
        }
    }
}

/// Where a report is written, from the span of its task call.
pub(in crate::lowering) fn report_location(
    site: Option<&sv::ir::SourceSpan>,
) -> Option<ReportLocation> {
    site.map(|site| ReportLocation {
        file: site.logical_path.display().to_string(),
        line: site.line,
    })
}

/// How a task reports, for the tasks that report: `$readmemh`/`$readmemb`
/// load memory instead, the analyzer rewrites `$printtimescale` into a
/// `$display`, and `$exit` outside a program block does nothing.
pub(in crate::lowering) fn system_task_kind(task: sv::ir::SystemTask) -> Option<SystemTaskKind> {
    use sv::ir::SystemTask;
    Some(match task {
        SystemTask::Display(radix) => {
            SystemTaskKind::Print(RuntimeEventKind::Display, radix.letter())
        }
        SystemTask::Write(radix) => SystemTaskKind::Print(RuntimeEventKind::Write, radix.letter()),
        SystemTask::Finish => SystemTaskKind::Control(RuntimeEventKind::Finish),
        SystemTask::Stop => SystemTaskKind::Control(RuntimeEventKind::Stop),
        SystemTask::Severity(severity) => SystemTaskKind::Message(match severity {
            sv::ir::Severity::Error => Severity::Error,
            sv::ir::Severity::Warning => Severity::Warning,
            sv::ir::Severity::Info => Severity::Info,
        }),
        SystemTask::Fatal => SystemTaskKind::Fatal,
        SystemTask::Assert => SystemTaskKind::Assertion(RuntimeEventKind::Report(Severity::Fatal)),
        SystemTask::AssertContinue => {
            SystemTaskKind::Assertion(RuntimeEventKind::Report(Severity::Error))
        }
        SystemTask::Monitor(_)
        | SystemTask::Fmonitor(_)
        | SystemTask::MonitorOn
        | SystemTask::MonitorOff
        | SystemTask::Strobe(_)
        | SystemTask::Fstrobe(_)
        | SystemTask::Readmem(_)
        | SystemTask::Writemem(_)
        | SystemTask::Fclose
        | SystemTask::Fflush
        | SystemTask::Fdisplay(_)
        | SystemTask::Fwrite(_)
        | SystemTask::Printtimescale
        | SystemTask::Timeformat
        | SystemTask::SetCoverageDbName
        | SystemTask::LoadCoverageDb
        | SystemTask::AssertOn
        | SystemTask::AssertOff
        | SystemTask::Exit
        | SystemTask::Vcd(_) => return None,
    })
}

/// How many arguments the conversions of `template` consume, as the runtime
/// renders them.
pub(super) fn template_arguments(template: &str) -> usize {
    let mut chars = template.chars().peekable();
    let mut count = 0;
    while let Some(ch) = chars.next() {
        if ch != '%' {
            continue;
        }
        while chars
            .next_if(|ch| matches!(ch, '-' | '+' | ' ' | '#' | '0'))
            .is_some()
        {}
        while chars.next_if(char::is_ascii_digit).is_some() {}
        if chars.next_if_eq(&'.').is_some() {
            while chars.next_if(char::is_ascii_digit).is_some() {}
        }
        if let Some(spec) = chars.next()
            && "bBoOhHxXdDiIcCsStTeEfFgGpP".contains(spec)
        {
            count += 1;
        }
    }
    count
}

/// Prepare each value that a `%t` of `template` converts. A system task
/// argument is self-determined (IEEE 1800-2023 11.6.1, 21.2.1), so the
/// operand of the analyzer's scaling `value * factor` keeps its own width
/// and signedness. A `string` value has no time value, and Celox would print
/// its storage handle: it is rejected, as Verilator rejects it, while Icarus
/// prints the conversion unformatted.
pub(in crate::lowering) fn prepare_time_operands(
    module: &ProcModule<'_>,
    template: Option<&str>,
    values: &mut [sv::ir::SystemTaskArg<sv::ir::Expr>],
) -> Result<(), sv::AnalyzerError> {
    let Some(template) = template else {
        return Ok(());
    };
    let mut chars = template.chars().peekable();
    let mut index = 0;
    while let Some(ch) = chars.next() {
        if ch != '%' {
            continue;
        }
        while chars
            .next_if(|ch| matches!(ch, '-' | '+' | ' ' | '#' | '0'))
            .is_some()
        {}
        while chars.next_if(char::is_ascii_digit).is_some() {}
        if chars.next_if_eq(&'.').is_some() {
            while chars.next_if(char::is_ascii_digit).is_some() {}
        }
        let Some(spec) = chars.next() else {
            break;
        };
        if !"bBoOhHxXdDiIcCsStTeEfFgGpP".contains(spec) {
            continue;
        }
        if matches!(spec, 't' | 'T')
            && let Some(sv::ir::SystemTaskArg::Expr(expr)) = values.get_mut(index)
        {
            if module.expr_is_string(expr) {
                return Err(unsupported("string operand of a %t conversion"));
            }
            // The analyzer scales a %t value as `value * factor`.
            if let sv::ir::Expr::Binary {
                left,
                op: sv::ir::BinaryOp::Mul,
                ..
            } = expr
            {
                if module.expr_is_string(left) {
                    return Err(unsupported("string operand of a %t conversion"));
                }
                if module.expr_float_precision(left).is_none()
                    && let Some(width) = sv_expr_natural_width(
                        &module.typed_calls(left),
                        module.variables,
                        module.name_to_id,
                        module.constants,
                        module.parameter_types,
                    )
                {
                    let signed = module.expr_signed(left);
                    **left = sv::ir::Expr::Resize {
                        expr: left.clone(),
                        width,
                        signed,
                    };
                }
            }
        }
        index += 1;
    }
    Ok(())
}

/// The message template and value arguments of a system task: a leading
/// string literal is the template. `$fatal` first takes a finish number.
/// A `$display` or `$write` argument without a conversion of its own is
/// displayed in the task's radix, directly after the previous one (IEEE
/// 1800-2023 21.2.1.1). A severity task's message has the syntax of
/// `$display` (20.10), so its arguments convert the same way in decimal.
pub(in crate::lowering) fn system_task_template(
    module: &ProcModule<'_>,
    kind: &SystemTaskKind,
    args: &[sv::ir::SystemTaskArg<sv::ir::Expr>],
) -> Result<(Option<String>, Vec<sv::ir::SystemTaskArg<sv::ir::Expr>>), sv::AnalyzerError> {
    let args = match kind {
        SystemTaskKind::Fatal if matches!(args.first(), Some(sv::ir::SystemTaskArg::Expr(_))) => {
            &args[1..]
        }
        SystemTaskKind::Assertion(_) => &args[1..],
        SystemTaskKind::Control(_) => &[][..],
        _ => args,
    };
    let (mut template, values) = match args.first() {
        Some(sv::ir::SystemTaskArg::Str(template)) => {
            let mut values = args[1..].to_vec();
            let template = template_text(&unescape(template)?, &mut values);
            (Some(template), values)
        }
        _ => (None, args.to_vec()),
    };
    if matches!(kind, SystemTaskKind::Assertion(_)) && template.is_none() {
        template = Some("assertion failed".to_string());
    }
    let radix = match kind {
        SystemTaskKind::Print(_, radix) => Some(*radix),
        SystemTaskKind::Message(_) | SystemTaskKind::Fatal => Some('d'),
        SystemTaskKind::Control(_) | SystemTaskKind::Assertion(_) => None,
    };
    if let Some(radix) = radix {
        let converted = template.as_deref().map_or(0, template_arguments);
        let rest = values.len().saturating_sub(converted);
        if rest > 0 {
            let mut text = template.unwrap_or_default();
            for value in &values[converted..] {
                text.push('%');
                if let sv::ir::SystemTaskArg::Expr(expr) = value
                    && module.expr_float_precision(expr).is_some()
                {
                    text.push_str("#g");
                } else {
                    let string = match value {
                        sv::ir::SystemTaskArg::Str(_) => true,
                        sv::ir::SystemTaskArg::Expr(expr) => module.expr_is_string(expr),
                        sv::ir::SystemTaskArg::Empty => false,
                    };
                    text.push(if string { 's' } else { radix });
                }
            }
            template = Some(text);
        }
    }
    Ok((template, values))
}

/// The bytes of a string literal's text, written as in the source, after
/// its escape sequences (IEEE 1800-2023 5.9.1).
pub(in crate::lowering) fn unescape(text: &str) -> Result<Vec<u8>, sv::AnalyzerError> {
    sv::typecheck::unescape_string_literal(text)
        .ok_or_else(|| unsupported(format!("string literal \"{text}\" with a malformed escape")))
}

/// The format `bytes` of a system task as text: a byte that is not part of
/// UTF-8 text, such as one an escape `\377` writes, becomes a `%c`
/// conversion of an argument inserted into `values` in its place.
fn template_text(bytes: &[u8], values: &mut Vec<sv::ir::SystemTaskArg<sv::ir::Expr>>) -> String {
    let mut text = String::with_capacity(bytes.len());
    for chunk in bytes.utf8_chunks() {
        text.push_str(chunk.valid());
        for byte in chunk.invalid() {
            let index = template_arguments(&text).min(values.len());
            values.insert(
                index,
                sv::ir::SystemTaskArg::Expr(sv::ir::Expr::Literal(format!("8'h{byte:02x}"))),
            );
            text.push_str("%c");
        }
    }
    text
}
