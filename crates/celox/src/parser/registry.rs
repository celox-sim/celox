use crate::ir::RegisterType;
use crate::parser::{ParserError, resolve_total_width};

use veryl_analyzer::ir::{Module, VarId};

/// Get the register type of a specific port from a module definition.
pub fn get_port_type(
    module: &Module,
    port_id: &VarId,
) -> Result<RegisterType, ParserError> {
    let var = module.variables.get(port_id).ok_or_else(|| {
        ParserError::UnsupportedSimulatorParser {
            feature: "port lookup",
            detail: format!("port ID not found in child module: {}", module.name),
        }
    })?;

    let width = resolve_total_width(module, var)?;
    let ty = &var.r#type;
    if ty.is_2state() {
        Ok(RegisterType::Bit {
            width,
            signed: ty.signed,
        })
    } else {
        Ok(RegisterType::Logic { width })
    }
}
