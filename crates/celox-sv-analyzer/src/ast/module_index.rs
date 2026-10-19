//! Native module scopes cached for repeated hierarchy specialization.

use super::*;

#[derive(Default)]
pub(crate) struct ModuleIndex {
    names: Vec<String>,
    locations: HashMap<String, Vec<SyntaxTree>>,
    pub interfaces: ModuleInterfaces,
}

impl ModuleIndex {
    pub(crate) fn new(tree: &SyntaxTree) -> Result<Self, AnalyzerError> {
        Self::build(tree, None)
    }

    pub(crate) fn from_declarations(
        tree: &SyntaxTree,
        declarations: &celox_sv_syntax::Declarations,
    ) -> Result<Self, AnalyzerError> {
        Self::build(tree, Some(declarations))
    }

    fn build(
        tree: &SyntaxTree,
        declarations: Option<&celox_sv_syntax::Declarations>,
    ) -> Result<Self, AnalyzerError> {
        let mut index = Self::default();
        let mut ansi = Vec::new();
        for handle in tree.root_scopes()? {
            let scoped = tree.scoped(handle)?;
            let unit = scoped.root_unit()?;
            if unit.kind != celox_sv_syntax::UnitKind::Module
                || unit.port_style == celox_sv_syntax::PortStyle::Wildcard
            {
                continue;
            }
            let name = scoped.root_name()?;
            index.names.push(name.clone());
            ansi.push(unit.port_style == celox_sv_syntax::PortStyle::Ansi);
            index
                .locations
                .entry(name.clone())
                .or_default()
                .push(scoped.clone());
            if declarations.is_none() && unit.port_style == celox_sv_syntax::PortStyle::Ansi {
                index.interfaces.insert(
                    name,
                    module_interface_from_scope(&AnalysisContext::unspecialized(&scoped))?,
                );
            }
        }
        if let Some(declarations) = declarations {
            use celox_sv_syntax::{PortKind, PortStyle, UnitKind};
            let units = declarations
                .all_units()
                .filter(|unit| {
                    unit.kind == UnitKind::Module
                        && !unit.external
                        && unit.port_style != PortStyle::Wildcard
                })
                .collect::<Vec<_>>();
            if units.len() != index.names.len()
                || units
                    .iter()
                    .zip(index.names.iter().zip(&ansi))
                    .any(|(unit, (name, ansi))| {
                        unit.name.spelling != *name || (unit.port_style == PortStyle::Ansi) != *ansi
                    })
            {
                return Err(AnalyzerError::Parse(
                    "native declaration index disagrees with the validated module bodies".into(),
                ));
            }
            index.names = units
                .iter()
                .map(|unit| unit.name.spelling.clone())
                .collect();
            for unit in units {
                if unit.port_style != PortStyle::Ansi {
                    continue;
                }
                let ports = unit
                    .ports
                    .iter()
                    .filter_map(|port| match &port.kind {
                        PortKind::Named(name) => Some(name.spelling.clone()),
                        _ => None,
                    })
                    .collect();
                let parameters = unit
                    .overridable_parameters()
                    // Preserve the legacy positional binding contract for body
                    // type parameters during this syntax-only source_tree.
                    .filter(|parameter| {
                        unit.parameters.is_some()
                            || matches!(
                                parameter.kind,
                                celox_sv_syntax::ParameterKind::Value { .. }
                            )
                    })
                    .map(|parameter| parameter.name.spelling.clone())
                    .collect();
                index.interfaces.insert(
                    unit.name.spelling.clone(),
                    ModuleInterface { ports, parameters },
                );
            }
        }
        Ok(index)
    }

    pub fn names(&self) -> &[String] {
        &self.names
    }

    pub(crate) fn scopes(&self, name: &str) -> &[SyntaxTree] {
        self.locations
            .get(name)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }
}
