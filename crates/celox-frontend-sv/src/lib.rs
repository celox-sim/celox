//! SystemVerilog frontend adapter for Celox.

mod lowering;

pub use celox_sv_analyzer::AnalyzerError;
pub use celox_sv_analyzer::ModuleInterfaces;
pub use celox_sv_analyzer::ir as analyzer_ir;
pub use celox_sv_analyzer::typecheck as analyzer_typecheck;
pub use lowering::{ForeignModuleResolver, prepare_external_hierarchy_with_foreign_resolver};
pub use lowering::{SvModuleRequest, SvParameterValue};

pub fn module_interfaces(
    sources: &[(&str, &std::path::Path)],
) -> Result<ModuleInterfaces, AnalyzerError> {
    let mut interfaces = ModuleInterfaces::default();
    for (code, path) in sources {
        let parsed = celox_sv_analyzer::ParsedSource::parse(code, path)?;
        interfaces.extend(parsed.module_interfaces().clone());
    }
    Ok(interfaces)
}

pub fn component_references(
    sources: &[(&str, &std::path::Path)],
) -> Result<fxhash::FxHashSet<String>, AnalyzerError> {
    let mut references = fxhash::FxHashSet::default();
    for (code, path) in sources {
        references
            .extend(celox_sv_analyzer::ParsedSource::parse(code, path)?.component_references());
    }
    Ok(references)
}
pub fn component_names(
    sources: &[(&str, &std::path::Path)],
) -> Result<fxhash::FxHashSet<String>, AnalyzerError> {
    let mut names = fxhash::FxHashSet::default();
    for (code, path) in sources {
        names.extend(celox_sv_analyzer::ParsedSource::parse(code, path)?.component_names());
    }
    Ok(names)
}

pub use lowering::{
    FrontendError, prepare_external_hierarchy, prepare_external_hierarchy_with_foreign,
    schedule_sources,
};
pub use lowering::{InterfaceSignature, interface_signatures, interface_signatures_for};
