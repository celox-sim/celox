use crate::{HashMap, ir::RegisterType};

use veryl_analyzer::ir::{Module, VarId};
use veryl_parser::resource_table::StrId;

pub struct ModuleRegistry<'a> {
    // Map from module name (StrId) to its interface definition
    pub modules: HashMap<StrId, &'a Module>,
}

impl<'a> ModuleRegistry<'a> {
    /// Get the bit width of a specific port of a specific module
    pub fn get_port_type(&self, module_name: StrId, port_id: &VarId) -> RegisterType {
        let module = self
            .modules
            .get(&module_name)
            .expect("Module definition not found");

        // Look up type information from variables and calculate bit width
        let var = module
            .variables
            .get(port_id)
            .expect("Port ID not found in child module");

        let ty = &var.r#type;
        if ty.is_2state() {
            RegisterType::Bit {
                width: ty.total_width().unwrap(),
                signed: ty.signed,
            }
        } else {
            RegisterType::Logic {
                width: ty.total_width().unwrap(),
            }
        }
    }
}
