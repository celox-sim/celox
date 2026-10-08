//! Compile-only typed frontend exporter. Never executes a circuit or reads
//! assertion operands. Uses the same explicit semantic FF extension as Celox's
//! reusable suite adapter, recorded in every output.
use celox_frontend_veryl::{BuildConfig, loop_provenance::LoopSourceTable};
use lydite_celox::compiled::{Compiled, Signal};
use serde_json::{Value, json};
use std::path::Path;
use veryl_analyzer::{Analyzer, AnalyzerError, Context, attribute_table, ir::Ir, symbol_table};
use veryl_metadata::Metadata;
use veryl_parser::{Parser, resource_table};

fn compile(design: &Value) -> Result<Compiled, String> {
    symbol_table::clear();
    attribute_table::clear();
    let metadata = Metadata::create_default("prj").map_err(|e| e.to_string())?;
    let analyzer = Analyzer::new(&metadata);
    let mut parsed = vec![];
    let mut errors = vec![];
    for (index, source) in design["sources"]
        .as_array()
        .ok_or("no sources")?
        .iter()
        .enumerate()
    {
        let name = format!("source-{index}.veryl");
        let path = source["path"].as_str().unwrap_or(&name);
        let ast = Parser::parse(source["text"].as_str().ok_or("no text")?, &Path::new(path))
            .map_err(|e| format!("parse: {e}"))?;
        errors.extend(analyzer.analyze_pass1("prj", &ast.veryl));
        parsed.push(ast);
    }
    let loop_sources = LoopSourceTable::collect(parsed.iter().map(|p| &p.veryl));
    errors.extend(Analyzer::analyze_post_pass1());
    let mut context = Context::default();
    let mut ir = Ir::default();
    for ast in &parsed {
        errors.extend(analyzer.analyze_pass2(&ast.veryl, &mut context, Some(&mut ir)));
    }
    errors.extend(context.drain_errors());
    errors.extend(Analyzer::analyze_post_pass2(&ir));
    let mut allowed = vec![];
    let errors = errors
        .into_iter()
        .filter(|error| {
            if matches!(
                error,
                AnalyzerError::SideEffectFunctionCallInAlwaysFf { .. }
                    | AnalyzerError::FunctionOutputInAlwaysFf { .. }
            ) {
                allowed.push(format!("{error:?}"));
                false
            } else {
                error.is_error()
            }
        })
        .collect::<Vec<_>>();
    if !errors.is_empty() {
        let diagnostics = errors
            .iter()
            .map(|error| match error {
                AnalyzerError::InvalidForRange { kind, .. } => {
                    json!({"code":"InvalidForRange","kind":format!("{kind:?}")})
                }
                _ => json!({"code":format!("{error:?}").split(" {").next().unwrap_or("Unknown")}),
            })
            .collect::<Vec<_>>();
        return Err(format!(
            "analyzer-diagnostics:{}",
            json!({"stage":"analyzer","diagnostics":diagnostics,"detail":format!("{errors:?}")})
        ));
    }
    let mut diagnostics = celox_frontend_veryl::check_dynamic_for_bounds(&ir);
    diagnostics.extend(celox_frontend_veryl::check_function_output_aliases(&ir));
    if diagnostics.iter().any(|e| e.is_error()) {
        return Err(format!("frontend diagnostics: {diagnostics:?}"));
    }
    celox_frontend_veryl::lower_interface_captures(&mut ir);
    let provenance = loop_sources.match_unrolled(&ir);
    let config = BuildConfig::from(&metadata.build);
    let top = resource_table::insert_str(design["top"].as_str().ok_or("no top")?);
    let symbolic =
        celox_frontend_veryl::parse_ir_with_loop_provenance(&ir, &provenance, &config, &top)
            .map_err(|e| format!("lower: {e:?}"))?;
    let output = celox_frontend_veryl::schedule_symbolic_rtl(
        symbolic,
        &config,
        &[],
        &[],
        design["four_state"].as_bool().unwrap_or(false),
        &celox_frontend_core::ParallelScheduleOptions::SEQUENTIAL,
        &Default::default(),
        None,
    )
    .map_err(|e| format!("schedule: {e:?}"))?;
    let lookup = &output.scheduled.frontend_lookup;
    let mut signals = Vec::new();
    for (instance_path, instance_id) in &lookup.instance_ids {
        let module_id = &lookup.instance_module[instance_id];
        for (id, variable) in &lookup.module_variables[module_id] {
            let address = celox_frontend_core::SourceAddr {
                instance_id: *instance_id,
                var_id: *id,
            };
            if let Some(state) = lookup.source_to_state.get(&address) {
                signals.push(Signal {
                    instances: instance_path.0.clone(),
                    path: variable.path.clone(),
                    kind: variable.var_kind,
                    signed: variable.signed,
                    metadata: variable.metadata.clone(),
                    address: *state,
                });
            }
        }
    }
    let scheduled = output.scheduled;
    Ok(Compiled {
        status: "compiled_only_not_verified".into(),
        four_state: design["four_state"].as_bool().unwrap_or(false),
        allow_always_ff_function_effects: true,
        allowed_diagnostics: allowed,
        signals,
        runtime_event_sites: scheduled.runtime_schema.runtime_event_sites,
        design: scheduled.design.into(),
        sir: scheduled.sir.into(),
        frontend_lookup: format!("{:?}", scheduled.frontend_lookup),
    })
}
fn main() {
    let path = std::env::args().nth(1).expect("DESIGN.json");
    let design: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    match compile(&design) {
        Ok(compiled) => println!("{}", compiled.to_json().unwrap()),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1)
        }
    }
}
