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
/// The top-level modules of `sources`: the modules that no source
/// instantiates (IEEE 1800-2023 23.3.1), in name order. Instantiations in
/// generate branches count whether or not they are taken.
pub fn top_level_modules(
    sources: &[(&str, &std::path::Path)],
    first_modules: bool,
) -> Result<Vec<String>, AnalyzerError> {
    let parsed = parse_sources(sources, first_modules)?;
    let referenced: fxhash::FxHashSet<_> = parsed
        .iter()
        .flat_map(|source| source.component_references())
        .collect();
    // Declarations and references name a module alike, `\\foo ` as `foo`
    // (IEEE 1800-2023 5.6.1).
    let mut tops: Vec<_> = parsed
        .iter()
        .flat_map(|source| source.module_names())
        .map(|name| celox_sv_analyzer::normalize_identifier(name))
        .filter(|name| !referenced.contains(name))
        .collect();
    tops.sort();
    tops.dedup();
    Ok(tops)
}

/// The name and the source of a module that instantiates each of `tops`
/// under its own name (IEEE 1800-2023 23.3.1), with the time unit and
/// precision of those tops (see
/// [`celox_sv_analyzer::generated_time_declarations_with_options`]): the
/// root of a design of several top-level modules, which stands for `$root`.
/// Its name is an escaped identifier no module of `sources` declares; the
/// analysis that elaborates it is told it is generated
/// ([`AnalysisOptions::generated_root`]), never by its name.
pub fn top_level_root_source(
    sources: &[(&str, &std::path::Path)],
    tops: &[&str],
    options: AnalysisOptions,
) -> Result<(String, String), AnalyzerError> {
    const BASE: &str = "\\$root";
    let declared = if sources.iter().any(|(code, _)| code.contains(BASE)) {
        component_names(sources, options.first_duplicate_module)?
    } else {
        fxhash::FxHashSet::default()
    };
    let name = std::iter::once(BASE.to_string())
        .chain((1..).map(|index| format!("{BASE}_{index}")))
        .find(|name| !declared.contains(name))
        .expect("an unbounded sequence of names");
    let time =
        celox_sv_analyzer::generated_time_declarations_with_options(sources, Some(tops), options)?;
    let mut source = format!("{time}module {name} ;\n");
    // Each top by its escaped spelling, which names the same module as its
    // simple one (IEEE 1800-2023 5.6.1) and is also a name a keyword can be
    // (`\\module `); an escaped name keeps its own.
    for top in tops {
        let spelling = if top.starts_with('\\') {
            top.to_string()
        } else {
            format!("\\{top}")
        };
        source.push_str(&format!("  {spelling} {spelling} ();\n"));
    }
    source.push_str("endmodule\n");
    Ok((name, source))
}

/// The tops that the [`top_level_root_source`] named `root` among `sources`
/// instantiates.
fn generated_root_tops(sources: &[(&str, &std::path::Path)], root: &str) -> Vec<String> {
    let header = format!("module {root} ;\n");
    sources
        .iter()
        .find_map(|(code, _)| code.split_once(header.as_str()).map(|(_, body)| body))
        .map(|body| {
            body.lines()
                .take_while(|line| *line != "endmodule")
                .filter_map(|line| line.split_whitespace().next())
                .map(celox_sv_analyzer::normalize_identifier)
                .collect()
        })
        .unwrap_or_default()
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
