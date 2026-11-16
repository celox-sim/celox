//! Generating TypeScript module definitions.

use super::*;

// ---------------------------------------------------------------------------
//  TypeScript type generation
// ---------------------------------------------------------------------------

#[napi(object)]
pub struct NapiInjectedManifest {
    pub name: String,
    pub manifest: String,
}

pub(super) fn inject_analyzer_components(
    definitions: Option<Vec<NapiInjectedManifest>>,
) -> Result<()> {
    let definitions = definitions.unwrap_or_default();
    let names: Vec<_> = definitions
        .iter()
        .map(|definition| definition.name.as_str())
        .collect();
    veryl_analyzer::tb_component::insert_external_components(&names);
    for definition in definitions {
        let manifest =
            veryl_metadata::ComponentManifest::parse(&definition.manifest).ok_or_else(|| {
                Error::from_reason(format!(
                    "manifest of injected component `{}` cannot be parsed",
                    definition.name
                ))
            })?;
        veryl_analyzer::component_manifest_table::insert(
            veryl_parser::resource_table::insert_str(&definition.name),
            manifest,
        );
    }
    Ok(())
}

/// Generate TypeScript type information as JSON for a Veryl project.
///
/// Equivalent to running `celox-gen-ts --json` from the given project directory.
#[napi]
pub fn gen_ts(
    project_path: String,
    components: Option<Vec<NapiInjectedManifest>>,
) -> Result<String> {
    use celox_ts_gen::{JsonModuleEntry, JsonOutput, generate_all};

    let toml_path = Metadata::search_from(&project_path)
        .map_err(|e| Error::from_reason(format!("Could not find Veryl.toml: {e}")))?;
    let mut metadata = Metadata::load(&toml_path)
        .map_err(|e| Error::from_reason(format!("Failed to load Veryl.toml: {e}")))?;

    let base_path = toml_path
        .parent()
        .unwrap_or(&toml_path)
        .to_string_lossy()
        .to_string();

    let mut paths = metadata
        .paths::<std::path::PathBuf>(&[], true, true)
        .map_err(|e| Error::from_reason(format!("Failed to gather sources: {e}")))?;
    paths.retain(|path| !path.example);

    // Append test-only sources declared in celox.toml
    let project_root = toml_path.parent().unwrap_or(&toml_path).to_path_buf();
    let celox_cfg = load_celox_config(&project_root)?;
    let prj_name = metadata.project.name.clone();
    for dir in &celox_cfg.test.sources {
        let dir_path = project_root.join(dir);
        if !dir_path.exists() {
            continue;
        }
        for src in walkdir(&dir_path)? {
            paths.push(PathSet {
                prj: prj_name.clone(),
                src: src.clone(),
                dst: src.with_extension("sv"),
                map: src.with_extension("map"),
                example: false,
            });
        }
    }

    if let Some(exclude_set) = build_exclude_set(&celox_cfg)? {
        paths.retain(|p| !is_excluded(&p.src, &project_root, &exclude_set));
    }

    if paths.is_empty() {
        return Err(Error::from_reason("No Veryl source files found"));
    }

    // Parse and analyze pass 1
    symbol_table::clear();
    attribute_table::clear();

    let analyzer = Analyzer::new(&metadata);
    inject_analyzer_components(components)?;
    let mut parsers = Vec::new();
    let mut all_warnings = Vec::new();

    for path in &paths {
        let input = std::fs::read_to_string(&path.src)
            .map_err(|e| Error::from_reason(format!("{}: {e}", path.src.display())))?;
        let parser = Parser::parse(&input, &path.src)
            .map_err(|e| Error::from_reason(format!("Parse error: {e}")))?;

        let results = analyzer.analyze_pass1(&path.prj, &parser.veryl);
        let real_errors: Vec<_> = results.iter().filter(|e| e.is_error()).collect();
        if !real_errors.is_empty() {
            return Err(Error::from_reason(format_errors_with_warnings(
                "analysis pass 1",
                &real_errors,
                &all_warnings,
            )));
        }
        all_warnings.extend(results.into_iter().filter(|e| !e.is_error()));

        parsers.push((path.clone(), parser));
    }

    let results = Analyzer::analyze_post_pass1();
    let real_errors: Vec<_> = results.iter().filter(|e| e.is_error()).collect();
    if !real_errors.is_empty() {
        return Err(Error::from_reason(format_errors_with_warnings(
            "post-pass 1 analysis",
            &real_errors,
            &all_warnings,
        )));
    }
    all_warnings.extend(results.into_iter().filter(|e| !e.is_error()));

    // Pass 2: per-file IR → generate

    // Compute all source file relative paths for embedding in generated JS.
    let base_normalized = base_path.replace('\\', "/");
    let all_source_files: Vec<String> = parsers
        .iter()
        .map(|(path, _)| {
            let src_normalized = path
                .src
                .to_string_lossy()
                .replace(r"\\?\", "")
                .replace('\\', "/");
            src_normalized
                .strip_prefix(&base_normalized)
                .unwrap_or(&src_normalized)
                .trim_start_matches('/')
                .to_string()
        })
        .collect();
    let source_file_refs: Vec<&str> = all_source_files.iter().map(|s| s.as_str()).collect();

    let mut all_modules = Vec::new();
    let mut file_modules: HashMap<String, Vec<String>> = HashMap::default();
    let mut post_pass_ir = Ir::default();

    for (i, (_path, parser)) in parsers.iter().enumerate() {
        let mut analyzer_context = Context::default();
        let mut ir = Ir::default();
        let results = analyzer.analyze_pass2(&parser.veryl, &mut analyzer_context, Some(&mut ir));
        let real_errors: Vec<_> = results.iter().filter(|e| e.is_error()).collect();
        if !real_errors.is_empty() {
            return Err(Error::from_reason(format_errors_with_warnings(
                "analysis pass 2",
                &real_errors,
                &all_warnings,
            )));
        }
        all_warnings.extend(results.into_iter().filter(|e| !e.is_error()));

        let modules = generate_all(&ir, &source_file_refs);
        post_pass_ir.append(&mut ir);
        let source_file = all_source_files[i].clone();

        let module_names: Vec<String> = modules.iter().map(|m| m.module_name.clone()).collect();
        if !module_names.is_empty() {
            file_modules.insert(source_file.clone(), module_names);
        }

        for m in modules {
            all_modules.push(JsonModuleEntry {
                module_name: m.module_name,
                source_file: source_file.clone(),
                dts_content: m.dts_content,
                md_content: m.md_content,
                ports: m.ports,
                events: m.events,
                instances: m.instances,
                is_test: m.is_test,
            });
        }
    }

    let results = Analyzer::analyze_post_pass2(&post_pass_ir);
    let real_errors: Vec<_> = results.iter().filter(|e| e.is_error()).collect();
    if !real_errors.is_empty() {
        return Err(Error::from_reason(format_errors_with_warnings(
            "post-pass 2 analysis",
            &real_errors,
            &all_warnings,
        )));
    }
    all_warnings.extend(results.into_iter().filter(|e| !e.is_error()));

    // Sort for deterministic output
    all_modules.sort_by(|a, b| a.module_name.cmp(&b.module_name));

    let warning_msgs: Vec<String> = all_warnings
        .iter()
        .map(|w| celox::render_diagnostic(w))
        .collect();

    let all_sources: Vec<(String, String)> = parsers
        .iter()
        .map(|(p, _)| {
            let path_str = p.src.to_string_lossy().to_string();
            let content = std::fs::read_to_string(&p.src).unwrap_or_default();
            (path_str, content)
        })
        .collect();

    let diagnostics: Vec<celox_ts_gen::JsonDiagnostic> = all_warnings
        .iter()
        .flat_map(|w| analyzer_error_to_diagnostics(w, &all_sources, false))
        .collect();

    let output = JsonOutput {
        project_path: base_path,
        modules: all_modules,
        file_modules,
        warnings: warning_msgs,
        diagnostics,
    };

    serde_json::to_string(&output)
        .map_err(|e| Error::from_reason(format!("Failed to serialize JSON: {e}")))
}

/// Generate TypeScript type information from in-memory HDL sources.
///
/// Like `gen_ts()` but does not require a Veryl.toml or filesystem access.
/// Works on both native and wasm32 targets. Sources are dispatched by
/// extension: `.sv`/`.svh` files go to the SystemVerilog analyzer and every
/// other source to the Veryl analyzer; a mixed set merges both results.
#[napi]
pub fn gen_ts_from_source(
    sources: Vec<NapiSourceFile>,
    components: Option<Vec<NapiInjectedManifest>>,
) -> Result<String> {
    let (veryl_sources, sv_sources): (Vec<NapiSourceFile>, Vec<NapiSourceFile>) = sources
        .into_iter()
        .partition(|source| !is_sv_path(std::path::Path::new(&source.path)));
    match (veryl_sources.is_empty(), sv_sources.is_empty()) {
        (false, true) => gen_ts_from_veryl_sources(veryl_sources, components),
        (true, false) => gen_ts_from_sv_sources(&sv_sources),
        (true, true) => Err(Error::from_reason("No source files provided")),
        (false, false) => {
            let veryl_json = gen_ts_from_veryl_sources(veryl_sources, components)?;
            let sv_json = gen_ts_from_sv_sources(&sv_sources)?;
            merge_gen_ts_json(&veryl_json, &sv_json)
        }
    }
}

/// Merge two `gen_ts` JSON outputs, concatenating their modules.
fn merge_gen_ts_json(veryl_json: &str, sv_json: &str) -> Result<String> {
    let mut merged: serde_json::Value = serde_json::from_str(veryl_json)
        .map_err(|e| Error::from_reason(format!("Failed to merge genTs output: {e}")))?;
    let sv: serde_json::Value = serde_json::from_str(sv_json)
        .map_err(|e| Error::from_reason(format!("Failed to merge genTs output: {e}")))?;
    if let (Some(modules), Some(sv_modules)) = (
        merged.get_mut("modules").and_then(|m| m.as_array_mut()),
        sv.get("modules").and_then(|m| m.as_array()),
    ) {
        modules.extend(sv_modules.iter().cloned());
    }
    if let (Some(file_modules), Some(sv_file_modules)) = (
        merged
            .get_mut("fileModules")
            .and_then(|m| m.as_object_mut()),
        sv.get("fileModules").and_then(|m| m.as_object()),
    ) {
        for (file, modules) in sv_file_modules {
            file_modules.insert(file.clone(), modules.clone());
        }
    }
    serde_json::to_string(&merged)
        .map_err(|e| Error::from_reason(format!("Failed to serialize JSON: {e}")))
}

/// Generate TypeScript type information from in-memory SystemVerilog sources.
///
/// All sources are parsed together and their packages are analyzed once, so
/// a module may use typedefs and constants from a package declared in
/// another file. Each declared module is then analyzed with that shared
/// package context and the module interfaces of the whole source set.
fn gen_ts_from_sv_sources(sources: &[NapiSourceFile]) -> Result<String> {
    use celox_sv_analyzer::ir::{PortDirection, TypeKind};
    use celox_ts_gen::{JsonModuleEntry, JsonOutput, JsonPortInfo};

    let mut parsed = Vec::new();
    for src in sources {
        let path = std::path::PathBuf::from(&src.path);
        parsed.push(
            celox_sv_analyzer::ParsedSource::parse(&src.content, &path)
                .map_err(|e| Error::from_reason(format!("{e}")))?,
        );
    }
    let packages =
        celox_sv_analyzer::ParsedSource::analyze_packages(&parsed.iter().collect::<Vec<_>>())
            .map_err(|e| Error::from_reason(format!("{e}")))?;
    let mut interfaces = celox_sv_analyzer::ModuleInterfaces::default();
    for source in &parsed {
        interfaces.extend(source.module_interfaces().clone());
    }

    let mut all_modules = Vec::new();
    let mut file_modules: HashMap<String, Vec<String>> = HashMap::default();

    for (src, source) in sources.iter().zip(&parsed) {
        let source_file = src.path.replace('\\', "/");
        let mut module_names = Vec::new();
        for name in source.module_names() {
            let mut design = celox_sv_analyzer::Design::default();
            let ir = source
                .analyze_module(
                    name,
                    &HashMap::default(),
                    &[],
                    &[],
                    &interfaces,
                    &packages,
                    &mut design,
                )
                .map_err(|e| Error::from_reason(format!("{e}")))?;
            let Some(module) = ir.modules().iter().find(|m| m.name() == name) else {
                continue;
            };
            let mut ports: HashMap<String, JsonPortInfo> = HashMap::default();
            for port in module.ports() {
                let (r#type, is4state) = match port.r#type().kind() {
                    TypeKind::Bit => ("bit", false),
                    _ => ("logic", true),
                };
                ports.insert(
                    port.name().to_string(),
                    JsonPortInfo {
                        direction: match port.direction() {
                            PortDirection::Input => "input",
                            PortDirection::Output => "output",
                            _ => "inout",
                        },
                        r#type,
                        width: port.r#type().resolved_width().unwrap_or(1),
                        is4state,
                        array_dims: None,
                        interface: None,
                    },
                );
            }
            module_names.push(module.name().to_string());
            all_modules.push(JsonModuleEntry {
                module_name: module.name().to_string(),
                source_file: source_file.clone(),
                dts_content: String::new(),
                md_content: String::new(),
                ports,
                events: Vec::new(),
                instances: Vec::new(),
                is_test: false,
            });
        }
        if !module_names.is_empty() {
            file_modules.insert(source_file, module_names);
        }
    }

    all_modules.sort_by(|a, b| a.module_name.cmp(&b.module_name));
    let output = JsonOutput {
        project_path: String::new(),
        modules: all_modules,
        file_modules,
        warnings: Vec::new(),
        diagnostics: Vec::new(),
    };
    serde_json::to_string(&output)
        .map_err(|e| Error::from_reason(format!("Failed to serialize JSON: {e}")))
}

fn gen_ts_from_veryl_sources(
    sources: Vec<NapiSourceFile>,
    components: Option<Vec<NapiInjectedManifest>>,
) -> Result<String> {
    use celox_ts_gen::{JsonModuleEntry, JsonOutput, generate_all};

    if sources.is_empty() {
        return Err(Error::from_reason("No source files provided"));
    }

    let metadata = Metadata::create_default("playground")
        .map_err(|e| Error::from_reason(format!("Failed to create default metadata: {e}")))?;

    // Parse and analyze pass 1
    symbol_table::clear();
    attribute_table::clear();

    let analyzer = Analyzer::new(&metadata);
    inject_analyzer_components(components)?;
    let mut parsers = Vec::new();
    let mut all_warnings = Vec::new();

    for src in &sources {
        let path = std::path::PathBuf::from(&src.path);
        let parser = Parser::parse(&src.content, &path)
            .map_err(|e| Error::from_reason(format!("Parse error: {e}")))?;

        let prj = "playground".to_string();
        let results = analyzer.analyze_pass1(&prj, &parser.veryl);
        let real_errors: Vec<_> = results.iter().filter(|e| e.is_error()).collect();
        if !real_errors.is_empty() {
            return Err(Error::from_reason(format_errors_with_warnings(
                "analysis pass 1",
                &real_errors,
                &all_warnings,
            )));
        }
        all_warnings.extend(results.into_iter().filter(|e| !e.is_error()));

        parsers.push((prj, path, parser));
    }

    let results = Analyzer::analyze_post_pass1();
    let real_errors: Vec<_> = results.iter().filter(|e| e.is_error()).collect();
    if !real_errors.is_empty() {
        return Err(Error::from_reason(format_errors_with_warnings(
            "post-pass 1 analysis",
            &real_errors,
            &all_warnings,
        )));
    }
    all_warnings.extend(results.into_iter().filter(|e| !e.is_error()));

    // Pass 2: per-file IR → generate
    let all_source_files: Vec<String> = parsers
        .iter()
        .map(|(_, p, _)| p.to_string_lossy().replace('\\', "/"))
        .collect();
    let source_file_refs: Vec<&str> = all_source_files.iter().map(|s| s.as_str()).collect();

    let mut all_modules = Vec::new();
    let mut file_modules: HashMap<String, Vec<String>> = HashMap::default();
    let mut post_pass_ir = Ir::default();

    for (i, (_prj, _path, parser)) in parsers.iter().enumerate() {
        let mut analyzer_context = Context::default();
        let mut ir = Ir::default();
        let results = analyzer.analyze_pass2(&parser.veryl, &mut analyzer_context, Some(&mut ir));
        let real_errors: Vec<_> = results.iter().filter(|e| e.is_error()).collect();
        if !real_errors.is_empty() {
            return Err(Error::from_reason(format_errors_with_warnings(
                "analysis pass 2",
                &real_errors,
                &all_warnings,
            )));
        }
        all_warnings.extend(results.into_iter().filter(|e| !e.is_error()));

        let modules = generate_all(&ir, &source_file_refs);
        post_pass_ir.append(&mut ir);
        let source_file = all_source_files[i].clone();

        let module_names: Vec<String> = modules.iter().map(|m| m.module_name.clone()).collect();
        if !module_names.is_empty() {
            file_modules.insert(source_file.clone(), module_names);
        }

        for m in modules {
            all_modules.push(JsonModuleEntry {
                module_name: m.module_name,
                source_file: source_file.clone(),
                dts_content: m.dts_content,
                md_content: m.md_content,
                ports: m.ports,
                events: m.events,
                instances: m.instances,
                is_test: m.is_test,
            });
        }
    }

    let results = Analyzer::analyze_post_pass2(&post_pass_ir);
    let real_errors: Vec<_> = results.iter().filter(|e| e.is_error()).collect();
    if !real_errors.is_empty() {
        return Err(Error::from_reason(format_errors_with_warnings(
            "post-pass 2 analysis",
            &real_errors,
            &all_warnings,
        )));
    }
    all_warnings.extend(results.into_iter().filter(|e| !e.is_error()));

    // Sort for deterministic output
    all_modules.sort_by(|a, b| a.module_name.cmp(&b.module_name));

    let warning_msgs: Vec<String> = all_warnings
        .iter()
        .map(|w| celox::render_diagnostic(w))
        .collect();

    let all_sources: Vec<(String, String)> = sources
        .iter()
        .map(|s| (s.path.clone(), s.content.clone()))
        .collect();

    let diagnostics: Vec<celox_ts_gen::JsonDiagnostic> = all_warnings
        .iter()
        .flat_map(|w| analyzer_error_to_diagnostics(w, &all_sources, false))
        .collect();

    let output = JsonOutput {
        project_path: String::new(),
        modules: all_modules,
        file_modules,
        warnings: warning_msgs,
        diagnostics,
    };

    serde_json::to_string(&output)
        .map_err(|e| Error::from_reason(format!("Failed to serialize JSON: {e}")))
}
