//! Project configuration and the sources of a Veryl project.

use super::*;

/// Append extra source as a separate file entry if provided.
pub(super) fn append_extra_source(
    sources: &mut Vec<(String, std::path::PathBuf)>,
    extra: &Option<String>,
) {
    if let Some(extra) = extra {
        sources.push((extra.clone(), std::path::PathBuf::from("<extra>")));
    }
}

/// Configuration loaded from an optional `celox.toml` in the project root.
#[derive(serde::Deserialize, Default)]
#[allow(dead_code)]
pub(super) struct CeloxConfig {
    #[serde(default)]
    pub(super) diagnostics: celox::DiagnosticPolicy,
    /// Glob patterns (relative to project root) for `.veryl` files to exclude
    /// from compilation and type generation.
    #[serde(default)]
    pub(super) exclude: Vec<String>,
    #[serde(default)]
    pub(super) test: CeloxTestConfig,
    #[serde(default)]
    pub(super) simulation: CeloxSimulationConfig,
}

#[derive(serde::Deserialize, Default)]
pub(super) struct CeloxTestConfig {
    /// Additional source directories (relative to `celox.toml`) whose `.veryl`
    /// files are included when running simulations and generating type stubs.
    #[serde(default)]
    pub(super) sources: Vec<String>,
}

#[derive(serde::Deserialize, Default)]
#[allow(dead_code)]
pub(super) struct CeloxSimulationConfig {
    /// Default maximum steps for `waitUntil` / `waitForCycles`.
    /// Overridden by the per-call `maxSteps` option.
    pub(super) max_steps: Option<u32>,
}

/// Load `celox.toml` from the given project root (same directory as `Veryl.toml`).
/// Returns `None` if the file does not exist.
pub(super) fn load_celox_config(project_root: &std::path::Path) -> Result<CeloxConfig> {
    let path = project_root.join("celox.toml");
    if !path.exists() {
        return Ok(CeloxConfig::default());
    }
    let content = std::fs::read_to_string(&path)
        .map_err(|e| Error::from_reason(format!("Failed to read celox.toml: {e}")))?;
    toml::from_str(&content)
        .map_err(|e| Error::from_reason(format!("Failed to parse celox.toml: {e}")))
}

/// Build a `GlobSet` from the exclude patterns in the config.
/// Returns `None` if there are no exclude patterns.
pub(super) fn build_exclude_set(config: &CeloxConfig) -> Result<Option<globset::GlobSet>> {
    if config.exclude.is_empty() {
        return Ok(None);
    }
    let mut builder = globset::GlobSetBuilder::new();
    for pattern in &config.exclude {
        let glob = globset::GlobBuilder::new(pattern)
            .literal_separator(true)
            .build()
            .map_err(|e| Error::from_reason(format!("Invalid exclude pattern '{pattern}': {e}")))?;
        builder.add(glob);
    }
    let set = builder
        .build()
        .map_err(|e| Error::from_reason(format!("Failed to build exclude set: {e}")))?;
    Ok(Some(set))
}

/// Returns `true` if the path should be excluded based on the glob set.
/// The path is matched relative to `project_root`.
pub(super) fn is_excluded(
    path: &std::path::Path,
    project_root: &std::path::Path,
    exclude_set: &globset::GlobSet,
) -> bool {
    let relative = path.strip_prefix(project_root).unwrap_or(path);
    // Normalize to forward slashes for consistent matching
    let rel_str = relative.to_string_lossy().replace('\\', "/");
    exclude_set.is_match(&rel_str)
}

/// Collect all `.veryl` files from the extra test source directories declared in
/// `celox.toml` and add them as individual source entries.
pub(super) fn collect_test_sources(
    sources: &mut Vec<(String, std::path::PathBuf)>,
    project_root: &std::path::Path,
    config: &CeloxConfig,
) -> Result<()> {
    for dir in &config.test.sources {
        let dir_path = project_root.join(dir);
        if !dir_path.exists() {
            continue;
        }
        let entries = walkdir(&dir_path)?;
        for entry in entries {
            let content = std::fs::read_to_string(&entry)
                .map_err(|e| Error::from_reason(format!("{}: {e}", entry.display())))?;
            sources.push((content, entry));
        }
    }
    Ok(())
}

/// Recursively collect `.veryl` files under `dir`, sorted for determinism.
pub(super) fn walkdir(dir: &std::path::Path) -> Result<Vec<std::path::PathBuf>> {
    let mut files = Vec::new();
    let read = std::fs::read_dir(dir)
        .map_err(|e| Error::from_reason(format!("Cannot read directory {}: {e}", dir.display())))?;
    for entry in read {
        let entry = entry.map_err(|e| Error::from_reason(format!("Directory entry error: {e}")))?;
        let path = entry.path();
        if path.is_dir() {
            files.extend(walkdir(&path)?);
        } else if path.extension().is_some_and(|ext| ext == "veryl") {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

/// Load a Veryl project's source files and metadata from a directory.
///
/// Searches upward from `project_path` for `Veryl.toml`, gathers all `.veryl`
/// source files, and returns the per-file sources, project metadata, and
/// the parsed `celox.toml` configuration.
pub(super) fn load_project_sources(
    project_path: &str,
) -> Result<(Vec<(String, std::path::PathBuf)>, Metadata, CeloxConfig)> {
    let toml_path = Metadata::search_from(project_path)
        .map_err(|e| Error::from_reason(format!("Could not find Veryl.toml: {e}")))?;
    let mut metadata = Metadata::load(&toml_path)
        .map_err(|e| Error::from_reason(format!("Failed to load Veryl.toml: {e}")))?;
    let paths = metadata
        .paths::<&str>(&[], false, false)
        .map_err(|e| Error::from_reason(format!("Failed to gather sources: {e}")))?;
    let mut sources = Vec::new();
    for p in paths.iter().filter(|path| !path.example) {
        let content = std::fs::read_to_string(&p.src)
            .map_err(|e| Error::from_reason(format!("{}: {e}", p.src.display())))?;
        sources.push((content, p.src.clone()));
    }
    let project_root = toml_path.parent().unwrap_or(&toml_path);
    let celox_cfg = load_celox_config(project_root)?;
    collect_test_sources(&mut sources, project_root, &celox_cfg)?;
    if let Some(exclude_set) = build_exclude_set(&celox_cfg)? {
        sources.retain(|(_, path)| !is_excluded(path, project_root, &exclude_set));
    }
    Ok((sources, metadata, celox_cfg))
}

/// Format compilation warnings as a JSON array of strings.
///
/// Uses `render_diagnostic` to include source location and span information,
/// matching the format used for error messages.
pub(super) fn format_warnings_json(warnings: &[celox::CompilationWarning]) -> String {
    let msgs: Vec<String> = warnings
        .iter()
        .map(|w| celox::render_diagnostic(w))
        .collect();
    serde_json::to_string(&msgs).unwrap_or_else(|_| "[]".to_string())
}
