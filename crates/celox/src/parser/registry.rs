use crate::{HashMap, ir::RegisterType};
use crate::parser::{ParserError, resolve_width};

use veryl_analyzer::ir::{Module, VarId};
use veryl_parser::resource_table::StrId;

pub struct ModuleRegistry<'a> {
    // Map from module name (StrId) to its interface definition
    pub modules: HashMap<StrId, &'a Module>,
}

impl<'a> ModuleRegistry<'a> {
    /// Get the bit width of a specific port of a specific module
    pub fn get_port_type(
        &self,
        module_name: StrId,
        port_id: &VarId,
    ) -> Result<RegisterType, ParserError> {
        let module = self.modules.get(&module_name).ok_or_else(|| {
            ParserError::UnsupportedSimulatorParser {
                feature: "module lookup",
                detail: format!("module definition not found: {module_name}"),
            }
        })?;

        // Look up type information from variables and calculate bit width
        let var = module.variables.get(port_id).ok_or_else(|| {
            ParserError::UnsupportedSimulatorParser {
                feature: "port lookup",
                detail: format!("port ID not found in child module: {module_name}"),
            }
        })?;

        let width = resolve_width(module, var)?;
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
}
