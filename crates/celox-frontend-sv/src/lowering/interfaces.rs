//! Interface signatures, and the member paths of interface instances.

use super::*;

/// The type and modport contract of an SV interface, without translating its
/// executable body to another language.
pub struct InterfaceSignature {
    pub name: String,
    pub module: sv::ir::Module,
    pub modports: std::collections::BTreeMap<String, Vec<(String, String)>>,
    pub parameters: Vec<String>,
    pub driven: Vec<String>,
    pub has_behavior: bool,
    pub header_dependent_cast: bool,
    pub time_declarations: String,
}

pub fn interface_signatures(
    sources: &[(&str, &Path)],
) -> Result<Vec<InterfaceSignature>, FrontendError> {
    interface_signatures_for(sources, None, &mut Vec::new())
}

/// The signatures of the interfaces in `names`, or of every interface.
/// Lowering an interface for its signature reports its warnings in
/// `warnings`; lowering the design reports them again.
pub fn interface_signatures_for(
    sources: &[(&str, &Path)],
    names: Option<&HashSet<String>>,
    warnings: &mut Vec<crate::FrontendWarning>,
) -> Result<Vec<InterfaceSignature>, FrontendError> {
    interface_signatures_for_with_options(sources, names, warnings, sv::AnalysisOptions::default())
}

/// Probe interface signatures with explicit language compatibility options.
pub fn interface_signatures_for_with_options(
    sources: &[(&str, &Path)],
    names: Option<&HashSet<String>>,
    warnings: &mut Vec<crate::FrontendWarning>,
    analysis_options: sv::AnalysisOptions,
) -> Result<Vec<InterfaceSignature>, FrontendError> {
    let mut found = Vec::new();
    let signatures = interface_signatures_collecting(sources, names, &mut found, analysis_options);
    append_error_warnings(&signatures, &mut found);
    crate::warning::append_unique_warnings(sources, warnings, found);
    signatures
}

fn interface_signatures_collecting(
    sources: &[(&str, &Path)],
    names: Option<&HashSet<String>>,
    warnings: &mut Vec<crate::warning::PendingWarning>,
    analysis_options: sv::AnalysisOptions,
) -> Result<Vec<InterfaceSignature>, FrontendError> {
    let prototypes = sv::interface_prototypes_with_options(sources, names, analysis_options)?;
    if prototypes.is_empty() {
        return Ok(Vec::new());
    }
    prototypes
        .into_iter()
        .map(|prototype| {
            warnings.extend(crate::warning::analyzer_warnings(&prototype.warnings));
            let lowered = lower_module(
                std::borrow::Cow::Borrowed(&prototype.module),
                false,
                true,
                false,
                warnings,
                &[],
                LevelLists::Processes,
                &HashMap::default(),
                &HashMap::default(),
                SharedAlways::default(),
                None,
            )?;
            Ok(InterfaceSignature {
                name: prototype.name,
                module: lowered.source,
                modports: prototype.modports,
                parameters: prototype.parameters,
                driven: prototype.driven,
                has_behavior: prototype.has_behavior,
                header_dependent_cast: prototype.header_dependent_cast,
                time_declarations: prototype.time_declarations,
            })
        })
        .collect()
}

// Candidate indices only locate a declaration; source provenance decides
// whether this is an interface path. Authored '$' names stay intact.
pub(super) fn restore_interface_path(
    name: &str,
    paths: &HashMap<String, sv::InterfaceMemberPath>,
) -> Option<Vec<String>> {
    if let Some(path) = paths.get(name) {
        return path.restore(name);
    }
    for (separator, _) in name.match_indices('$') {
        let member = &name[separator + 1..];
        let mut handle = &name[..separator];
        while let Some(indexed) = handle.strip_suffix(']') {
            let Some((base, index)) = indexed.rsplit_once('[') else {
                break;
            };
            if index.parse::<i128>().is_err() {
                break;
            }
            let key = format!("{base}${member}");
            if let Some(path) = paths.get(&key).filter(|path| path.arrayed)
                && let Some(restored) = path.restore(name)
            {
                return Some(restored);
            }
            handle = base;
        }
    }
    None
}

pub(super) fn restore_interface_member_paths(module: &mut SimModule, source: &AnalyzedSvModule) {
    for variable in module.variables.values_mut() {
        variable.path = variable
            .path
            .iter()
            .flat_map(|segment| {
                restore_interface_path(segment, &source.interface_member_paths)
                    .unwrap_or_else(|| vec![segment.clone()])
            })
            .collect();
    }
}
