//! Memory files (IEEE 1800-2023 21.4): the words `$readmemh`/`$readmemb`
//! load, and the shape of the array a memory task reads or writes.

use super::*;

/// One word a memory file writes: its bits in the destination, value, and
/// unknown mask.
pub(in crate::lowering) struct MemoryWord {
    pub access: BitAccess,
    pub value: BigUint,
    pub mask: BigUint,
}

impl ProcModule<'_> {
    /// The words `$readmemh`/`$readmemb` (IEEE 1800-2023 21.4) write, with
    /// the values of its optional start and finish addresses: the file is
    /// read when the design is compiled.
    pub fn readmem(
        &mut self,
        format: sv::ir::MemoryFormat,
        args: &[sv::ir::SystemTaskArg<sv::ir::Expr>],
        start: Option<i128>,
        finish: Option<i128>,
        site: Option<&sv::ir::SourceSpan>,
    ) -> Result<(SourceVarId, Vec<MemoryWord>), sv::AnalyzerError> {
        let name = sv::ir::SystemTask::Readmem(format).name();
        let radix = format.base();
        let filename = match args.first() {
            Some(sv::ir::SystemTaskArg::Str(filename)) => String::from_utf8(unescape(filename)?)
                .map_err(|_| unsupported(format!("`{name}` with a file name that is not UTF-8")))?,
            Some(sv::ir::SystemTaskArg::Expr(sv::ir::Expr::Literal(value)))
                if let Some(filename) = literal_characters(value) =>
            {
                filename
            }
            _ => {
                return Err(unsupported(format!(
                    "`{name}` with a file name that is not a constant string"
                )));
            }
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
        let start = start.or(selected).unwrap_or(low);
        let finish = finish.unwrap_or(high);
        if start < low || start > high || finish < low || finish > high {
            return Err(sv::AnalyzerError::MemoryFile(format!(
                "{name}: address range [{start}:{finish}] is outside the destination range [{low}:{high}]"
            )));
        }
        let content = std::fs::read_to_string(&filename).map_err(|error| {
            sv::AnalyzerError::MemoryFile(format!("{name}: failed to read {filename}: {error}"))
        })?;
        let parsed = celox_frontend_core::memory_file::parse_memory_range(
            &content,
            radix,
            element_width,
            start,
            finish,
        )
        .map_err(|error| sv::AnalyzerError::MemoryFile(format!("{name}: {}", error.message)))?;
        let warning =
            parsed
                .warning
                .map(|detail| crate::FrontendWarningKind::MemoryFileWordCount {
                    task: name.to_string(),
                    file: filename.clone(),
                    detail,
                });
        let writes = parsed.writes;
        let word_mask = (BigUint::from(1u8) << element_width) - BigUint::from(1u8);
        let mut words = Vec::new();
        for run in writes.runs {
            let value = BigUint::from_bytes_le(&run.value_bytes);
            let mask = BigUint::from_bytes_le(&run.mask_bytes);
            for k in 0..run.bit_width / element_width {
                let index = start.min(finish) + (run.bit_offset / element_width + k) as i128;
                let position = usize::try_from(index.abs_diff(left)).unwrap_or(usize::MAX);
                let shift = k * element_width;
                let mut word_value = (&value >> shift) & &word_mask;
                let mut word_unknown = (&mask >> shift) & &word_mask;
                if !(self.four_state && variable.is_4state) {
                    word_value &= &word_mask ^ &word_unknown;
                    word_unknown = BigUint::zero();
                }
                words.push(MemoryWord {
                    access: BitAccess::new(
                        position * element_width,
                        (position + 1) * element_width - 1,
                    ),
                    value: word_value,
                    mask: word_unknown,
                });
            }
        }
        if let Some(warning) = warning {
            self.warn(warning, site);
        }
        Ok((id, words))
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
