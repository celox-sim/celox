//! Helpers for compiling Veryl test sources through the emitted SystemVerilog
//! frontend.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use veryl_analyzer::{Analyzer, Context, attribute_table, ir::Ir, symbol_table};
use veryl_emitter::Emitter;
use veryl_metadata::Metadata;
use veryl_parser::Parser;

/// Owned SystemVerilog sources produced from a set of Veryl sources.
pub struct EmittedSources {
    sources: Vec<(String, PathBuf)>,
    events: BTreeMap<String, BTreeMap<String, bool>>,
}

impl EmittedSources {
    /// Event polarities from declared port types, including ports forwarded to
    /// child modules. `true` means a rising edge, `false` a falling edge.
    pub fn event_edges(&self, top: &str) -> Option<&BTreeMap<String, bool>> {
        self.events.get(top)
    }

    /// Borrow the emitted sources in the form accepted by SV compiler adapters.
    pub fn as_sv_sources(&self) -> Vec<(&str, &Path)> {
        self.sources
            .iter()
            .map(|(source, path)| (source.as_str(), path.as_path()))
            .collect()
    }
}

/// Analyze and emit every Veryl source in `sources` as SystemVerilog.
pub fn emit_veryl_sources(sources: &[(&str, &Path)]) -> EmittedSources {
    symbol_table::clear();
    attribute_table::clear();

    let mut metadata = Metadata::create_default("prj").unwrap();
    // In-memory test designs name their top module without a project prefix.
    metadata.build.omit_project_prefix = true;
    metadata.build.strip_comments = true;

    let analyzer = Analyzer::new(&metadata);
    let mut parsed_sources = Vec::with_capacity(sources.len());

    for (code, path) in sources {
        let parsed = Parser::parse(code, path).unwrap_or_else(|error| {
            panic!("failed to parse Veryl source {}: {error}", path.display())
        });
        let errors = analyzer.analyze_pass1("prj", &parsed.veryl);
        assert!(
            !errors.iter().any(|error| error.is_error()),
            "Veryl analyze_pass1 errors in {}: {errors:?}",
            path.display()
        );
        parsed_sources.push((*code, *path, parsed));
    }

    let errors = Analyzer::analyze_post_pass1();
    assert!(
        !errors.iter().any(|error| error.is_error()),
        "Veryl analyze_post_pass1 errors: {errors:?}"
    );

    let mut context = Context::default();
    let mut ir = Ir::default();
    for (_, _, parsed) in &parsed_sources {
        let errors = analyzer.analyze_pass2(&parsed.veryl, &mut context, Some(&mut ir));
        assert!(
            !errors.iter().any(|error| error.is_error()),
            "Veryl analyze_pass2 errors: {errors:?}"
        );
    }

    let errors = Analyzer::analyze_post_pass2(&ir);
    assert!(
        !errors.iter().any(|error| error.is_error()),
        "Veryl analyze_post_pass2 errors: {errors:?}"
    );

    let emitted = parsed_sources
        .into_iter()
        .enumerate()
        .map(|(index, (code, source_path, parsed))| {
            let output_path = emitted_path(index, source_path);
            let map_path = output_path.with_extension("sv.map");
            let mut emitter = Emitter::new(&metadata, "prj", source_path, &output_path, &map_path);
            emitter.emit(&parsed.veryl, code);
            (emitter.as_str().to_string(), output_path)
        })
        .collect();

    let mut events = BTreeMap::new();
    for component in &ir.components {
        if let veryl_analyzer::ir::Component::Module(module) = component {
            let mut ports = BTreeMap::new();
            for (path, (ty, _)) in &module.port_types {
                use veryl_analyzer::ir::TypeKind;
                let rising = match ty.kind {
                    TypeKind::Clock | TypeKind::ClockPosedge | TypeKind::ResetAsyncHigh => true,
                    TypeKind::ClockNegedge | TypeKind::Reset | TypeKind::ResetAsyncLow => false,
                    _ => continue,
                };
                ports.insert(path.to_string(), rising);
            }
            events.insert(module.name.to_string(), ports);
        }
    }
    EmittedSources {
        sources: emitted,
        events,
    }
}

fn emitted_path(index: usize, source_path: &Path) -> PathBuf {
    if source_path.as_os_str().is_empty() {
        PathBuf::from(format!("source_{index}.sv"))
    } else {
        let mut path = source_path.to_path_buf();
        path.set_extension("sv");
        path
    }
}
