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
    let mut dimensions = Vec::with_capacity(shape.len());
    extend_resolved_dims(module, variable, shape, kind, &mut dimensions)?;
    Ok(dimensions)
}

pub(crate) fn extend_resolved_dims(
    module: &Module,
    variable: &Variable,
    shape: &[Option<usize>],
    kind: &str,
    dimensions: &mut Vec<usize>,
) -> Result<(), ParserError> {
    dimensions.reserve(shape.len());
    for dimension in shape {
        dimensions.push(dimension.ok_or_else(|| {
            ParserError::unresolved_width(
                module,
                variable,
                format!("{kind} dimension in {}", variable.r#type),
            )
        })?);
    }
    Ok(())
}
