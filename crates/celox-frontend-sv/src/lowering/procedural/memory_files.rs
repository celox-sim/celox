//! Memory files (IEEE 1800-2023 21.4): the words `$readmemh`/`$readmemb`
//! load, and the shape of the array a memory task reads or writes.

use super::*;

impl ProcModule<'_> {
    /// The load `$readmemh`/`$readmemb` (IEEE 1800-2023 21.4) makes, with
    /// the values of its optional start and finish addresses, and the
    /// variable it loads. The file is read when the load runs; a name that
    /// is not a constant string (see [`constant_file_name`]) is left empty
    /// for the code that runs it to give.
    pub fn readmem_load(
        &mut self,
        format: sv::ir::MemoryFormat,
        args: &[sv::ir::SystemTaskArg<sv::ir::Expr>],
        start: Option<i128>,
        finish: Option<i128>,
        site: Option<&sv::ir::SourceSpan>,
    ) -> Result<(SourceVarId, celox_design::MemoryFileLoad), sv::AnalyzerError> {
        let name = sv::ir::SystemTask::Readmem(format).name();
        let filename = match args.first() {
            Some(sv::ir::SystemTaskArg::Str(filename)) => String::from_utf8(unescape(filename)?)
                .map_err(|_| unsupported(format!("`{name}` with a file name that is not UTF-8")))?,
            Some(sv::ir::SystemTaskArg::Expr(sv::ir::Expr::Literal(value)))
                if let Some(filename) = literal_characters(value) =>
            {
                filename
            }
            // A name the task evaluates when it executes.
            _ => String::new(),
        };
        // Veryl also emits a constant element select, which loads from that
        // element on.
        let (destination, select_lsb) = match args.get(1) {
            Some(sv::ir::SystemTaskArg::Expr(sv::ir::Expr::Ident(destination))) => {
                (destination, None)
            }
            Some(sv::ir::SystemTaskArg::Expr(sv::ir::Expr::Select { expr, lsb, .. }))
                if let sv::ir::Expr::Ident(destination) = &**expr =>
            {
                let lsb = sv::typecheck::eval_const_expr_with_types(
                    lsb,
                    self.constants,
                    self.parameter_types,
                )
                .and_then(|lsb| usize::try_from(lsb).ok())
                .ok_or_else(|| {
                    unsupported(format!("`{name}` destination with a run-time index"))
                })?;
                (destination, Some(lsb))
            }
            _ => {
                return Err(unsupported(format!(
                    "`{name}` destination that is not an array"
                )));
            }
        };
        let id = self
            .id(destination)
            .ok_or_else(|| unsupported(format!("`{name}` destination `{destination}`")))?;
        let variable = self.var(id);
        let (Some(bounds), Some(element_width)) = (
            self.unpacked_bounds.get(&id),
            unpacked_element_width(variable),
        ) else {
            return Err(unsupported(format!(
                "`{name}` destination that is not an unpacked array"
            )));
        };
        // A single dimension is addressed by its index values; several are
        // addressed in their storage order.
        let depth = variable.width / element_width;
        let (left, right) = match bounds.as_slice() {
            [bound] => *bound,
            _ => (0, depth as i128 - 1),
        };
        let (low, high) = (left.min(right), left.max(right));
        let selected = select_lsb.map(|lsb| {
            let position = (lsb / element_width) as i128;
            if left <= right {
                left + position
            } else {
                left - position
            }
        });
        let load = celox_design::MemoryFileLoad {
            radix: format.base(),
            file: filename,
            base: None,
            element_width,
            addressing: celox_design::MemoryFileAddressing::Range {
                left,
                right,
                start: start.or(selected).unwrap_or(low),
                finish: finish.unwrap_or(high),
            },
            location: report_location(site),
        };
        Ok((id, load))
    }

    pub fn file_memory_shape(
        &self,
        id: SourceVarId,
    ) -> Result<(i128, i128, usize), sv::AnalyzerError> {
        let Some(bounds) = self.unpacked_bounds.get(&id) else {
            return Err(unsupported("$writemem requires a static unpacked array"));
        };
        let Some(width) = unpacked_element_width(self.var(id)) else {
            return Err(unsupported("$writemem packed element type"));
        };
        let (left, right) = match bounds.as_slice() {
            [bound] => *bound,
            _ => (0, (self.var(id).width / width) as i128 - 1),
        };
        Ok((left, right, width))
    }
}

/// The bounds of the array a SystemVerilog load writes and the addresses it
/// loads from and toward: `(left, right, start, finish)`.
pub(in crate::lowering) fn range_addresses(
    load: &celox_design::MemoryFileLoad,
) -> (i128, i128, i128, i128) {
    match load.addressing {
        celox_design::MemoryFileAddressing::Range {
            left,
            right,
            start,
            finish,
        } => (left, right, start, finish),
        // [`ProcModule::readmem_load`] addresses a range; an offset load
        // counts its positions from zero.
        celox_design::MemoryFileAddressing::Offset { start, depth } => {
            let last = depth as i128 - 1;
            (0, last, start as i128, last)
        }
    }
}

/// The variable `$readmemh`/`$readmemb` writes, if it is a task of these.
pub(in crate::lowering) fn readmem_destination(
    task: sv::ir::SystemTask,
    args: &[sv::ir::SystemTaskArg<sv::ir::Expr>],
) -> Option<&str> {
    if !matches!(task, sv::ir::SystemTask::Readmem(_)) {
        return None;
    }
    match args.get(1)? {
        sv::ir::SystemTaskArg::Expr(sv::ir::Expr::Ident(destination)) => Some(destination),
        sv::ir::SystemTaskArg::Expr(sv::ir::Expr::Select { expr, .. }) => match &**expr {
            sv::ir::Expr::Ident(destination) => Some(destination),
            _ => None,
        },
        _ => None,
    }
}

/// Whether the file name argument of `$readmemh`/`$readmemb` is a constant
/// string, whose file [`ProcModule::readmem`] reads when the design is
/// compiled.
pub(in crate::lowering) fn constant_file_name(
    arg: Option<&sv::ir::SystemTaskArg<sv::ir::Expr>>,
) -> bool {
    match arg {
        Some(sv::ir::SystemTaskArg::Str(_)) => true,
        Some(sv::ir::SystemTaskArg::Expr(sv::ir::Expr::Literal(value))) => {
            literal_characters(value).is_some()
        }
        _ => false,
    }
}

/// The characters of a constant integral value that holds a character
/// string, such as a `string` parameter substituted as its bytes: one per
/// byte, most significant first, without the zero bytes that only pad it
/// (IEEE 1800-2023 21.4, 6.16). `None` for a value with unknown bits.
pub(super) fn literal_characters(value: &str) -> Option<String> {
    let literal = sv::typecheck::parse_integral_literal(value)?;
    if literal.mask != BigUint::from(0u8) {
        return None;
    }
    let bytes = literal.value.to_bytes_be();
    String::from_utf8(bytes.into_iter().filter(|byte| *byte != 0).collect()).ok()
}
