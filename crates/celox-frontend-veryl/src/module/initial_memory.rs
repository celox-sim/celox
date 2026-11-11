//! Initial statements and memory images read from files.

use super::*;

pub(crate) fn visit_initial_statement(
    stmt: &Statement,
    context: &mut veryl_analyzer::Context,
    visit: &mut impl FnMut(
        &SystemFunctionInput,
        &SystemFunctionOutput,
        &mut veryl_analyzer::Context,
    ) -> Result<(), ParserError>,
) -> Result<(), ParserError> {
    // Native testbenches retain runtime loops in initial declarations. This
    // visitor only extracts memory initialization: do not execute their loop
    // ranges or evaluate their conditions when there is nothing to extract.
    // Keep unsupported statements reachable so existing diagnostics survive.
    if !has_initial_memory_effect(stmt) {
        return Ok(());
    }
    match stmt {
        Statement::SystemFunctionCall(call) => {
            if let SystemFunctionKind::Readmemh(filename, output) = &call.kind {
                visit(filename, output, context)?;
            }
            Ok(())
        }
        Statement::If(if_stmt) => {
            let cond = if_stmt
                .cond
                .clone()
                .eval_value(context)
                .and_then(|value| value.to_usize());
            let Some(cond) = cond else {
                return Ok(());
            };
            let branch = if cond != 0 {
                &if_stmt.true_side
            } else {
                &if_stmt.false_side
            };
            for stmt in branch {
                visit_initial_statement(stmt, context, visit)?;
            }
            Ok(())
        }
        Statement::For(for_stmt) => {
            let Some(iter) = for_stmt.range.eval_iter(context) else {
                return Ok(());
            };
            for i in iter {
                if let Some(var) = context.variables.get_mut(&for_stmt.var_id)
                    && let Some(total_width) = for_stmt.var_type.total_width()
                {
                    let val = Value::new(i as u64, total_width, for_stmt.var_type.signed);
                    var.set_value(&[], val, None);
                }
                for stmt in &for_stmt.body {
                    visit_initial_statement(stmt, context, visit)?;
                }
            }
            Ok(())
        }
        Statement::Null => Ok(()),
        Statement::Unsupported(token) => Err(ParserError::illegal_context(
            "initial statement",
            "only direct $readmemh calls are valid in simulator-lowered initial blocks",
            Some(token),
        )),
        _ => Ok(()),
    }
}

pub(super) fn has_initial_memory_effect(stmt: &Statement) -> bool {
    let mut pending = vec![stmt];
    while let Some(stmt) = pending.pop() {
        match stmt {
            Statement::SystemFunctionCall(call) => {
                if matches!(call.kind, SystemFunctionKind::Readmemh(..)) {
                    return true;
                }
            }
            Statement::Unsupported(_) => return true,
            Statement::If(stmt) => {
                pending.extend(&stmt.true_side);
                pending.extend(&stmt.false_side);
            }
            Statement::For(stmt) => pending.extend(&stmt.body),
            _ => {}
        }
    }
    false
}

/// The writes performed by `$readmemh(filename, destination)`.
pub(crate) struct ReadmemImage {
    pub var_id: VarId,
    pub runs: Vec<InitialMemoryWriteRun>,
}

/// Validate a `$readmemh` destination and read the file into write runs.
///
/// `index_values` resolves the destination's constant index prefix, if any.
pub(crate) fn readmem_image(
    module: &veryl_analyzer::ir::Module,
    filename_arg: &SystemFunctionInput,
    output: &[AssignDestination],
    dependencies: &crate::FileDependencyCollector,
    radix: u32,
    index_values: impl FnOnce(&AssignDestination) -> Option<Vec<usize>>,
) -> Result<ReadmemImage, ParserError> {
    let dst = match output {
        [dst] if dst.select.is_empty() && dst.select.1.is_none() => dst,
        [dst] => {
            return Err(ParserError::illegal_context(
                "$readmemh destination",
                "destination must be a whole unpacked array variable",
                Some(&dst.token),
            ));
        }
        _ => {
            return Err(ParserError::illegal_context(
                "$readmemh destination",
                "concatenated destinations are not supported",
                None,
            ));
        }
    };

    let var = &module.variables[&dst.id];
    let depth = var
        .r#type
        .total_array()
        .ok_or_else(|| ParserError::unresolved_width(module, var, var.r#type.to_string()))?;
    let start_addr = if dst.index.0.is_empty() {
        0
    } else {
        let Some(indices) = index_values(dst) else {
            return Err(ParserError::illegal_context(
                "$readmemh destination index",
                "destination index must be compile-time constant",
                Some(&dst.token),
            ));
        };
        let Some(index) = var.r#type.array.calc_index(&indices) else {
            return Err(ParserError::illegal_context(
                "$readmemh destination index",
                format!("destination index {indices:?} is out of range"),
                Some(&dst.token),
            ));
        };
        index
    };
    if depth <= 1 {
        return Err(ParserError::illegal_context(
            "$readmemh destination",
            "destination must be an unpacked array",
            Some(&dst.token),
        ));
    }

    let total_width = resolve_total_width(module, var)?;
    let element_width = total_width / depth;
    if element_width == 0 || element_width * depth != total_width {
        return Err(ParserError::unresolved_width(
            module,
            var,
            var.r#type.to_string(),
        ));
    }

    let InitialMemoryData::Writes(runs) = read_memory_file(
        dependencies,
        filename_arg,
        radix,
        element_width,
        start_addr,
        depth,
        &dst.token,
    )?
    else {
        unreachable!("memory files are read as write runs");
    };
    Ok(ReadmemImage {
        var_id: dst.id,
        runs,
    })
}

pub(crate) fn read_memory_file(
    dependencies: &crate::FileDependencyCollector,
    filename_arg: &SystemFunctionInput,
    radix: u32,
    element_width: usize,
    start_addr: usize,
    depth: usize,
    destination_token: &veryl_parser::token_range::TokenRange,
) -> Result<InitialMemoryData, ParserError> {
    let task = if radix == 2 { "$readmemb" } else { "$readmemh" };
    let Some(filename) = static_string_expr(&filename_arg.0) else {
        return Err(ParserError::illegal_context(
            if radix == 2 {
                "$readmemb filename expression"
            } else {
                "$readmemh filename expression"
            },
            "filename must be a compile-time string",
            Some(&filename_arg.0.comptime().token),
        ));
    };
    let path = resolve_readmem_path(&filename, &filename_arg.0.comptime().token, dependencies);
    let timing = readmem_timing_enabled();
    let total_start = timing.then(Instant::now);
    if timing {
        tracing::debug!(
            "[readmem-timing] start file={} depth={} element_width={} start_addr={} radix={}",
            path.display(),
            depth,
            element_width,
            start_addr,
            radix
        );
    }

    let read_start = timing.then(Instant::now);
    let content = std::fs::read_to_string(&path).map_err(|err| {
        ParserError::memory_file(
            format!("{task}: failed to read {}: {err}", path.display()),
            Some(&filename_arg.0.comptime().token),
        )
    })?;
    if let Some(start) = read_start {
        tracing::debug!(
            "[readmem-timing] read file={} bytes={} elapsed={:?}",
            path.display(),
            content.len(),
            start.elapsed()
        );
    }

    let parse_start = timing.then(Instant::now);
    let writes = celox_frontend_core::memory_file::parse_memory_write_runs(
        &content,
        radix,
        element_width,
        start_addr,
        depth,
    )
    .map_err(|error| {
        ParserError::memory_file(
            format!("{task}: {}", error.message),
            error.at_destination.then_some(destination_token),
        )
    })?;
    if let Some(start) = parse_start {
        tracing::debug!(
            "[readmem-timing] parse file={} words={} runs={} elapsed={:?}",
            path.display(),
            writes.words,
            writes.runs.len(),
            start.elapsed()
        );
    }
    if let Some(start) = total_start {
        tracing::debug!(
            "[readmem-timing] done file={} elapsed={:?}",
            path.display(),
            start.elapsed()
        );
    }

    Ok(InitialMemoryData::Writes(writes.runs))
}

pub(super) fn resolve_readmem_path(
    filename: &str,
    token: &veryl_parser::token_range::TokenRange,
    dependencies: &crate::FileDependencyCollector,
) -> std::path::PathBuf {
    let source_path = token.beg.source.to_string();
    let source_path = (!source_path.is_empty()).then(|| std::path::Path::new(&source_path));
    let cwd = std::env::current_dir().ok();
    resolve_readmem_path_with_fallback(filename, source_path, cwd.as_deref(), dependencies)
}

pub(super) fn static_string_expr(expr: &Expression) -> Option<String> {
    if !expr.comptime().r#type.is_string() {
        return None;
    }
    let value = expr.comptime().get_value().ok()?;
    byte_value_to_string(value)
}

pub(super) fn readmem_timing_enabled() -> bool {
    tracing::enabled!(tracing::Level::DEBUG)
}

pub(super) fn resolve_readmem_path_with_fallback(
    filename: &str,
    source_path: Option<&std::path::Path>,
    cwd: Option<&std::path::Path>,
    dependencies: &crate::FileDependencyCollector,
) -> std::path::PathBuf {
    let path = std::path::PathBuf::from(filename);
    if path.is_absolute() {
        dependencies.record(&path, true);
        return path;
    }

    let source_relative = source_path
        .and_then(std::path::Path::parent)
        .map(|parent| parent.join(&path))
        .unwrap_or_else(|| path.clone());
    dependencies.record(&source_relative, false);
    if source_relative.exists() {
        return source_relative;
    }

    if let Some(cwd) = cwd {
        let cwd_relative = cwd.join(&path);
        dependencies.record(&cwd_relative, false);
        if cwd_relative.exists() {
            return cwd_relative;
        }
    }

    source_relative
}
