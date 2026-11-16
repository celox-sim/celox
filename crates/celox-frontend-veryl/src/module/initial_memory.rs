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

/// Validate a `$readmemh` destination and describe the load, whose file is
/// read when it runs (IEEE 1800-2023 21.4).
///
/// `index_values` resolves the destination's constant index prefix, if any.
pub(crate) fn readmem_load(
    module: &veryl_analyzer::ir::Module,
    filename_arg: &SystemFunctionInput,
    output: &[AssignDestination],
    radix: u32,
    index_values: impl FnOnce(&AssignDestination) -> Option<Vec<usize>>,
) -> Result<(VarId, celox_design::MemoryFileLoad), ParserError> {
    let destination = readmem_destination(module, output, index_values)?;
    let load = memory_file_load(
        filename_arg,
        radix,
        destination.element_width,
        destination.start,
        destination.depth,
    )?;
    Ok((destination.var_id, load))
}

/// The load of file `filename_arg` into `depth` words of `element_width`
/// bits from position `start`.
pub(crate) fn memory_file_load(
    filename_arg: &SystemFunctionInput,
    radix: u32,
    element_width: usize,
    start: usize,
    depth: usize,
) -> Result<celox_design::MemoryFileLoad, ParserError> {
    let Some(file) = static_string_expr(&filename_arg.0) else {
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
    let token = &filename_arg.0.comptime().token;
    let base = (!std::path::Path::new(&file).is_absolute())
        .then(|| readmem_base(token))
        .flatten();
    let location = token
        .beg
        .source
        .get_path()
        .and_then(veryl_parser::resource_table::get_path_value)
        .map(|path| celox_design::ReportLocation {
            file: path.to_string_lossy().into_owned(),
            line: token.beg.line as usize,
        });
    Ok(celox_design::MemoryFileLoad {
        radix,
        file,
        base,
        base_key: None,
        element_width,
        addressing: celox_design::MemoryFileAddressing::Offset { start, depth },
        location,
    })
}

/// The array a `$readmemh` loads and the words it may load.
struct ReadmemDestination {
    var_id: VarId,
    element_width: usize,
    /// The position of the first word.
    start: usize,
    depth: usize,
}

fn readmem_destination(
    module: &veryl_analyzer::ir::Module,
    output: &[AssignDestination],
    index_values: impl FnOnce(&AssignDestination) -> Option<Vec<usize>>,
) -> Result<ReadmemDestination, ParserError> {
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
    Ok(ReadmemDestination {
        var_id: dst.id,
        element_width,
        start: start_addr,
        depth,
    })
}

/// The directory a relative memory file name of the call at `token` is
/// looked up in first: that of its source file, as an absolute path.
fn readmem_base(token: &veryl_parser::token_range::TokenRange) -> Option<String> {
    let source = token.beg.source.to_string();
    let parent = std::path::Path::new(&source).parent()?;
    if parent.as_os_str().is_empty() {
        return None;
    }
    let base = if parent.is_absolute() {
        parent.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(parent)
    };
    Some(base.to_string_lossy().into_owned())
}

pub(super) fn static_string_expr(expr: &Expression) -> Option<String> {
    if !expr.comptime().r#type.is_string() {
        return None;
    }
    let value = expr.comptime().get_value().ok()?;
    byte_value_to_string(value)
}
