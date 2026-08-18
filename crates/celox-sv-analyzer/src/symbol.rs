//! Symbol tables built during SystemVerilog analysis.

use fxhash::FxHashMap as HashMap;

use crate::{AnalyzerError, ast};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ModuleId(usize);

impl ModuleId {
    pub fn index(self) -> usize {
        self.0
    }
}

#[derive(Debug, Default)]
pub struct ModuleTable {
    names: HashMap<String, ModuleId>,
}

impl ModuleTable {
    pub fn insert(&mut self, module: &ast::Module) -> Result<ModuleId, AnalyzerError> {
        if self.names.contains_key(module.name()) {
            return Err(AnalyzerError::DuplicateModule {
                name: module.name().to_string(),
            });
        }

        let id = ModuleId(self.names.len());
        self.names.insert(module.name().to_string(), id);
        Ok(id)
    }

    pub fn get(&self, name: &str) -> Option<ModuleId> {
        self.names.get(name).copied()
    }

    pub fn len(&self) -> usize {
        self.names.len()
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }
}

#[derive(Debug, Default)]
pub struct PortTable {
    names: HashMap<String, usize>,
}

#[derive(Debug, Default)]
pub struct ParameterTable {
    names: HashMap<String, usize>,
}

impl ParameterTable {
    pub fn insert(
        &mut self,
        module: &ast::Module,
        parameter: &ast::Parameter,
    ) -> Result<usize, AnalyzerError> {
        if self.names.contains_key(parameter.name()) {
            return Err(AnalyzerError::DuplicateParameter {
                module: module.name().to_string(),
                name: parameter.name().to_string(),
            });
        }

        let id = self.names.len();
        self.names.insert(parameter.name().to_string(), id);
        Ok(id)
    }
}

impl PortTable {
    pub fn insert(
        &mut self,
        module: &ast::Module,
        port: &ast::Port,
    ) -> Result<usize, AnalyzerError> {
        if self.names.contains_key(port.name()) {
            return Err(AnalyzerError::DuplicatePort {
                module: module.name().to_string(),
                name: port.name().to_string(),
            });
        }

        let id = self.names.len();
        self.names.insert(port.name().to_string(), id);
        Ok(id)
    }
}
