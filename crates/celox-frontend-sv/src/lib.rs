#![cfg_attr(
    test,
    allow(
        clippy::disallowed_macros,
        reason = "thread-local specialization observations exist only in unit tests"
    )
)]
//! SystemVerilog frontend adapter for Celox.

mod lowering;
mod warning;

pub use celox_sv_analyzer::AnalyzerError;
pub use celox_sv_analyzer::ModuleInterfaces;
pub use celox_sv_analyzer::ir as analyzer_ir;
pub use celox_sv_analyzer::typecheck as analyzer_typecheck;
pub use lowering::{ForeignModuleResolver, prepare_external_hierarchy_with_foreign_resolver};
pub use lowering::{SvModuleRequest, SvParameterValue};
pub use warning::{FrontendWarning, FrontendWarningKind, WarningLocation};

/// `sources` parsed, with only the first declaration of each module name
/// when `first_modules` (a relaxed `duplicate_module` diagnostic, see
/// [`celox_sv_analyzer::ParsedSource::parse_with_first_modules`]).
fn parse_sources(
    sources: &[(&str, &std::path::Path)],
    first_modules: bool,
) -> Result<Vec<celox_sv_analyzer::ParsedSource>, AnalyzerError> {
    let mut declared = fxhash::FxHashSet::default();
    sources
        .iter()
        .map(|(code, path)| {
            if first_modules {
                celox_sv_analyzer::ParsedSource::parse_with_first_modules(code, path, &mut declared)
            } else {
                celox_sv_analyzer::ParsedSource::parse(code, path)
            }
        })
        .collect()
}

pub fn module_interfaces(
    sources: &[(&str, &std::path::Path)],
    first_modules: bool,
) -> Result<ModuleInterfaces, AnalyzerError> {
    let mut interfaces = ModuleInterfaces::default();
    for parsed in parse_sources(sources, first_modules)? {
        interfaces.extend(parsed.module_interfaces().clone());
    }
    Ok(interfaces)
}

pub fn component_references(
    sources: &[(&str, &std::path::Path)],
    first_modules: bool,
) -> Result<fxhash::FxHashSet<String>, AnalyzerError> {
    let mut references = fxhash::FxHashSet::default();
    for parsed in parse_sources(sources, first_modules)? {
        references.extend(parsed.component_references());
    }
    Ok(references)
}
pub fn component_names(
    sources: &[(&str, &std::path::Path)],
    first_modules: bool,
) -> Result<fxhash::FxHashSet<String>, AnalyzerError> {
    let mut names = fxhash::FxHashSet::default();
    for parsed in parse_sources(sources, first_modules)? {
        names.extend(parsed.component_names());
    }
    Ok(names)
}

pub use lowering::{
    FrontendError, prepare_external_hierarchy, prepare_external_hierarchy_with_foreign,
    prepare_external_hierarchy_with_foreign_resolver_and_latch_policy, schedule_sources,
    schedule_sources_with_latch_policy,
};
pub use lowering::{InterfaceSignature, interface_signatures, interface_signatures_for};

pub use celox_sv_analyzer::AnalysisOptions;
pub use lowering::{
    interface_signatures_for_with_options,
    prepare_external_hierarchy_with_foreign_resolver_with_options, schedule_sources_with_options,
};

pub use lowering::{
    prepare_external_hierarchy_with_foreign_resolver_with_options_and_latch_policy,
    schedule_sources_with_force_signals, schedule_sources_with_force_signals_and_latch_policy,
    schedule_sources_with_parameter_expressions,
    schedule_sources_with_parameter_expressions_and_latch_policy,
};
