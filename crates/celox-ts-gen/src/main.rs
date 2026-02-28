use clap::Parser as ClapParser;
use miette::{IntoDiagnostic, Result, bail};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use veryl_analyzer::{Analyzer, Context, attribute_table, ir::Ir, symbol_table};
use veryl_metadata::Metadata;
use veryl_parser::Parser;
use celox_ts_gen::{JsonModuleEntry, JsonOutput, generate_all};

#[derive(ClapParser)]
#[command(name = "celox-gen-ts", about = "Generate TypeScript bindings from Veryl sources")]
struct Cli {
    /// Output directory for generated .d.ts and .js files
    #[arg(long, default_value = "generated")]
    out_dir: PathBuf,

    /// Output structured JSON to stdout instead of writing files
    #[arg(long)]
    json: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Find and load Veryl.toml
    let metadata_path =
        Metadata::search_from_current().into_diagnostic()?;
    let mut metadata = Metadata::load(&metadata_path).into_diagnostic()?;

    let project_path = metadata_path
        .parent()
        .unwrap_or(&metadata_path)
        .to_string_lossy()
        .to_string();

    // Gather source files
    let paths = metadata.paths::<PathBuf>(&[], true, true).into_diagnostic()?;
    if paths.is_empty() {
        bail!("No Veryl source files found");
    }

    // Parse and analyze pass 1
    symbol_table::clear();
    attribute_table::clear();

    let analyzer = Analyzer::new(&metadata);
    let mut parsers = Vec::new();

    for path in &paths {
        let input = fs::read_to_string(&path.src).into_diagnostic()?;
        let parser = Parser::parse(&input, &path.src)?;

        let mut errors = analyzer.analyze_pass1(&path.prj, &parser.veryl);
        if !errors.is_empty() {
            for e in errors.drain(..) {
                eprintln!("{e}");
            }
            bail!("Errors in analysis pass 1");
        }

        parsers.push((path.clone(), parser));
    }

    let mut errors = Analyzer::analyze_post_pass1();
    if !errors.is_empty() {
        for e in errors.drain(..) {
            eprintln!("{e}");
        }
        bail!("Errors in post-pass 1 analysis");
    }

    if cli.json {
        // JSON mode: create a separate IR per file to track which file produces which modules
        let mut all_modules = Vec::new();
        let mut file_modules: HashMap<String, Vec<String>> = HashMap::new();

        let mut has_errors = false;
        for (path, parser) in &parsers {
            let mut analyzer_context = Context::default();
            let mut ir = Ir::default();
            let errors = analyzer.analyze_pass2(
                &path.prj,
                &parser.veryl,
                &mut analyzer_context,
                Some(&mut ir),
            );
            for e in &errors {
                eprintln!("Warning: {e}");
            }
            if !errors.is_empty() {
                has_errors = true;
            }

            let modules = generate_all(&ir);
            let source_file = path
                .src
                .strip_prefix(&project_path)
                .unwrap_or(&path.src)
                .to_string_lossy()
                .trim_start_matches('/')
                .to_string();

            let module_names: Vec<String> =
                modules.iter().map(|m| m.module_name.clone()).collect();
            if !module_names.is_empty() {
                file_modules.insert(source_file.clone(), module_names);
            }

            for m in modules {
                all_modules.push(JsonModuleEntry {
                    module_name: m.module_name,
                    source_file: source_file.clone(),
                    dts_content: m.dts_content,
                    ports: m.ports,
                    events: m.events,
                });
            }
        }

        let errors = Analyzer::analyze_post_pass2();
        for e in &errors {
            eprintln!("Warning: {e}");
        }
        if !errors.is_empty() {
            has_errors = true;
        }

        if has_errors {
            eprintln!("Note: some analysis warnings occurred; generating bindings for supported modules");
        }

        // Sort for deterministic output
        all_modules.sort_by(|a, b| a.module_name.cmp(&b.module_name));

        let output = JsonOutput {
            project_path,
            modules: all_modules,
            file_modules,
        };

        let json = serde_json::to_string_pretty(&output).into_diagnostic()?;
        println!("{json}");
    } else {
        // Original file-writing mode
        let mut analyzer_context = Context::default();
        let mut ir = Ir::default();

        let mut has_errors = false;
        for (path, parser) in &parsers {
            let errors =
                analyzer.analyze_pass2(&path.prj, &parser.veryl, &mut analyzer_context, Some(&mut ir));
            for e in &errors {
                eprintln!("Warning: {e}");
            }
            if !errors.is_empty() {
                has_errors = true;
            }
        }

        let errors = Analyzer::analyze_post_pass2();
        for e in &errors {
            eprintln!("Warning: {e}");
        }
        if !errors.is_empty() {
            has_errors = true;
        }

        if has_errors {
            eprintln!("Note: some analysis warnings occurred; generating bindings for supported modules");
        }

        let modules = generate_all(&ir);
        if modules.is_empty() {
            eprintln!("Warning: no modules found in IR");
            return Ok(());
        }

        fs::create_dir_all(&cli.out_dir).into_diagnostic()?;

        for module in &modules {
            let dts_path = cli.out_dir.join(format!("{}.d.ts", module.module_name));
            let js_path = cli.out_dir.join(format!("{}.js", module.module_name));

            fs::write(&dts_path, &module.dts_content).into_diagnostic()?;
            fs::write(&js_path, &module.js_content).into_diagnostic()?;

            eprintln!(
                "Generated {}.d.ts and {}.js",
                module.module_name, module.module_name
            );
        }

        eprintln!(
            "Done: {} module(s) written to {}",
            modules.len(),
            cli.out_dir.display()
        );
    }

    Ok(())
}
