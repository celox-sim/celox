mod generator;

use proc_macro::TokenStream;
use std::env;
use std::fs;
use std::path::PathBuf;
use syn::LitStr;
use veryl_analyzer::ir::Ir;
use veryl_analyzer::{Analyzer, Context};
use veryl_metadata::Metadata;
use veryl_parser::Parser;

#[proc_macro]
pub fn veryl_test(input: TokenStream) -> TokenStream {
    let project_path_lit = match syn::parse::<LitStr>(input.clone()) {
        Ok(lit) => lit,
        Err(err) => return err.to_compile_error().into(),
    };
    let project_path_str = project_path_lit.value();
    let span = project_path_lit.span();

    macro_rules! exit_with_error {
        ($msg:expr) => {{
            let err_msg = $msg;
            return quote::quote_spanned! { span => compile_error!(#err_msg); }.into();
        }};
    }

    // 1. Find Veryl.toml
    let manifest_dir = match env::var("CARGO_MANIFEST_DIR") {
        Ok(dir) => dir,
        Err(_) => exit_with_error!("CARGO_MANIFEST_DIR is not set"),
    };
    let base_dir = PathBuf::from(manifest_dir);
    let target_dir = base_dir.join(project_path_str);

    let metadata_path = match Metadata::search_from(&target_dir) {
        Ok(path) => path,
        Err(_) => exit_with_error!(&format!("Failed to find Veryl.toml from {:?}", target_dir)),
    };

    let mut metadata = match Metadata::load(&metadata_path) {
        Ok(meta) => meta,
        Err(e) => exit_with_error!(&format!("Failed to load Veryl.toml: {}", e)),
    };

    // 2. Run Analysis
    veryl_analyzer::symbol_table::clear();
    veryl_analyzer::attribute_table::clear();
    let mut ir = Ir::default();
    let mut context = Context::default();
    let analyzer = Analyzer::new(&metadata);
    analyzer.clear();

    let paths = match metadata.paths::<PathBuf>(&[], true, true) {
        Ok(p) => p,
        Err(e) => exit_with_error!(&format!("Failed to gather paths: {}", e)),
    };

    // Veryl CLI dependency logic
    let mut table = std::collections::HashMap::new();
    for path in &paths {
        table.insert(path.src.clone(), path);
    }

    let mut prj_namespace = veryl_analyzer::namespace::Namespace::new();
    prj_namespace.push(veryl_parser::resource_table::insert_str(
        &metadata.project.name,
    ));

    let candidate_symbols: Vec<_> = veryl_analyzer::type_dag::connected_components()
        .into_iter()
        .filter(|symbols| symbols[0].namespace.included(&prj_namespace))
        .flatten()
        .collect();

    let mut used_paths = std::collections::HashMap::new();
    for symbol in &candidate_symbols {
        if let veryl_parser::veryl_token::TokenSource::File { path, .. } = symbol.token.source {
            let path = PathBuf::from(format!("{path}"));
            if let Some(x) = table.remove(&path) {
                used_paths.insert(path, x);
            }
        }
    }

    let mut sorted_paths = vec![];
    let sorted_symbols = veryl_analyzer::type_dag::toposort();
    for symbol in sorted_symbols {
        if matches!(
            symbol.kind,
            veryl_analyzer::symbol::SymbolKind::Module(_)
                | veryl_analyzer::symbol::SymbolKind::Interface(_)
                | veryl_analyzer::symbol::SymbolKind::Package(_)
        ) {
            if let veryl_parser::veryl_token::TokenSource::File { path, .. } = symbol.token.source {
                let path = PathBuf::from(format!("{path}"));
                if let Some(x) = used_paths.remove(&path) {
                    sorted_paths.push(x.clone());
                }
            }
        }
    }

    for path in used_paths.into_values() {
        sorted_paths.push(path.clone());
    }

    // Now sorted_paths still might not include $std if we only sorted candidate_symbols from AST DAG.
    // Actually, `metadata.paths(..., true, true)` includes stdlib.
    // The problem in the macro was passing `false` for load_std. Let's just use `paths` if we don't care about the rigorous emit order. Wait, the first pass must populate the symbol table before sort.

    let mut parsed_files = Vec::new();
    for path_set in paths {
        let code = match fs::read_to_string(&path_set.src) {
            Ok(c) => c,
            Err(e) => {
                exit_with_error!(&format!("Failed to read {}: {}", path_set.src.display(), e))
            }
        };
        parsed_files.push((path_set, code));
    }

    let mut parsers = Vec::new();
    for (path_set, code) in &parsed_files {
        let parser = match Parser::parse(code, &path_set.src) {
            Ok(p) => p,
            Err(_) => exit_with_error!(&format!("Failed to parse {}", path_set.src.display())),
        };
        analyzer.analyze_pass1(&path_set.prj, &parser.veryl);
        parsers.push(parser);
    }
    Analyzer::analyze_post_pass1();

    for (i, (path_set, _code)) in parsed_files.iter().enumerate() {
        let parser = &parsers[i];
        analyzer.analyze_pass2(&path_set.prj, &parser.veryl, &mut context, Some(&mut ir));
    }
    Analyzer::analyze_post_pass2();

    // 3. Generate TokenStream
    let expanded = generator::generate_project(&ir);

    TokenStream::from(expanded)
}
