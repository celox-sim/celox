use veryl_analyzer::ir::{Module, Variable};

use crate::ParserError;

/// Resolve the total storage size of a variable.
pub fn resolve_total_width(module: &Module, variable: &Variable) -> Result<usize, ParserError> {
    let width = variable.total_width().ok_or_else(|| {
        ParserError::unresolved_width(module, variable, variable.r#type.to_string())
    })?;
    let array = variable.r#type.total_array().ok_or_else(|| {
        ParserError::unresolved_width(module, variable, variable.r#type.to_string())
    })?;
    Ok(width * array)
}

/// Resolve every dimension in an array/width shape.
pub fn resolve_dims(
    module: &Module,
    variable: &Variable,
    shape: &[Option<usize>],
    kind: &str,
) -> Result<Vec<usize>, ParserError> {
    shape
        .iter()
        .map(|dimension| {
            dimension.ok_or_else(|| {
                ParserError::unresolved_width(
                    module,
                    variable,
                    format!("{kind} dimension in {}", variable.r#type),
                )
            })
        })
        .collect()
}
