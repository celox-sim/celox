use std::path::{Path, PathBuf};

use veryl_analyzer::conv::utils::{TypePosition, eval_const_assign, eval_expr, get_component};
use veryl_analyzer::ir::{
    AssignDestination, Comptime, Expression, Signature, VarId, VarIndex, VarPath, VarSelect,
};
use veryl_analyzer::symbol::{Affiliation, ClockDomain, SymbolKind};
use veryl_analyzer::value::Value;
use veryl_analyzer::{Analyzer, AnalyzerError, Context, attribute_table, ir::Ir, symbol_table};
use veryl_metadata::{ClockType, Component, ComponentBackendKind, Metadata, ResetType};
use veryl_parser::Parser;
use veryl_parser::resource_table;

use crate::parser::BuildConfig;

mod analyze;
mod compile;
pub(in crate::simulator) use analyze::*;
pub use compile::*;

#[cfg(feature = "systemverilog")]
fn sv_compilation_warnings(
    warnings: Vec<celox_frontend_sv::FrontendWarning>,
) -> Vec<CompilationWarning> {
    warnings
        .into_iter()
        .map(CompilationWarning::SystemVerilog)
        .collect()
}
use crate::{
    CompilationWarning, FrontendDiagnostic, HashMap, ParserError, SimulatorError,
    SimulatorErrorKind, ir::OptimizedSir, parser,
};

fn component_library_path(
    component: &Component,
    root: &Path,
    target_dir: &Path,
    backend: Option<ComponentBackendKind>,
) -> Option<std::path::PathBuf> {
    let wasm = component
        .wasm
        .as_ref()
        .map(|path| root.join(path))
        .filter(|path| path.is_file());
    let crate_dir = root.join(&component.path);
    let native = component_library_target_name(&crate_dir).and_then(|name| {
        let name = name.replace('-', "_");
        let path = target_dir.join("release").join(format!(
            "{}{}{}",
            std::env::consts::DLL_PREFIX,
            name,
            std::env::consts::DLL_SUFFIX
        ));
        path.is_file().then_some(path)
    });
    match backend {
        Some(ComponentBackendKind::Native) => native,
        Some(ComponentBackendKind::Wasm) => wasm,
        None => native.or(wasm),
    }
}

fn component_library_target_name(crate_dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(crate_dir.join("Cargo.toml")).ok()?;
    let value: toml::Value = toml::from_str(&text).ok()?;
    value
        .get("lib")
        .and_then(|lib| lib.get("name"))
        .and_then(toml::Value::as_str)
        .or_else(|| {
            value
                .get("package")
                .and_then(|package| package.get("name"))
                .and_then(toml::Value::as_str)
        })
        .map(str::to_owned)
}

fn component_runtime_config(
    metadata: &Metadata,
) -> (
    Vec<celox_testbench::ComponentLibrary>,
    Option<std::path::PathBuf>,
) {
    if metadata.metadata_path.as_os_str().is_empty() {
        return (Vec::new(), None);
    }
    let mut libraries = Vec::new();
    let mut collect =
        |components: &[Component], root: &Path, target_dir: &Path, project: Option<&str>| {
            for component in components {
                let Some(path) = component_library_path(
                    component,
                    root,
                    target_dir,
                    metadata.test.component_backend,
                ) else {
                    continue;
                };
                for (type_name, _) in component.collect_manifests(root, target_dir) {
                    let export = project
                        .map(|project| format!("{project}::{type_name}"))
                        .unwrap_or_else(|| type_name.clone());
                    if libraries
                        .iter()
                        .any(|library: &celox_testbench::ComponentLibrary| library.export == export)
                    {
                        continue;
                    }
                    libraries.push(celox_testbench::ComponentLibrary {
                        export,
                        type_name,
                        path: path.clone(),
                    });
                }
            }
        };
    let root = metadata.project_path();
    let target_dir = root.join("target/veryl-components");
    collect(&metadata.components, &root, &target_dir, None);
    if let Ok(dependencies) = metadata.collect_dependency_components() {
        for dependency in dependencies {
            collect(
                &dependency.components,
                &dependency.root,
                &dependency.target_dir,
                Some(&dependency.project),
            );
        }
    }
    (libraries, Some(root))
}

// ── JIT-specific types and builders (native only) ────────────────────

#[cfg(feature = "host-runtime")]
mod host;

#[cfg(feature = "host-runtime")]
pub use host::*;

#[cfg(test)]
mod component_library_tests {
    use super::component_library_target_name;

    #[test]
    fn cargo_lib_target_name_overrides_package_name() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            r#"
                [package]
                name = "package-name"
                version = "0.1.0"

                [lib]
                name = "actual_component"
            "#,
        )
        .unwrap();

        assert_eq!(
            component_library_target_name(dir.path()).as_deref(),
            Some("actual_component")
        );
    }
}

#[cfg(test)]
mod parallel_option_tests;

/// The report `build_plan` returns for `program`.
#[cfg(any(
    all(target_arch = "x86_64", not(feature = "arm64-codegen")),
    all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
))]
fn plan_report(program: &crate::ir::LaidOutProgram, warnings: &[CompilationWarning]) -> String {
    use std::fmt::Write;
    let schema = &program.runtime.runtime_schema;
    let sir = &program.sir;
    let instructions = |units: &[celox_sir::ExecutionUnit<_>]| -> usize {
        units
            .iter()
            .flat_map(|unit| unit.blocks.values())
            .map(|block| block.instructions.len())
            .sum()
    };
    let mut report = String::new();
    let _ = writeln!(report, "processes: {}", schema.processes.len());
    // Without a compiled plan, as with the reference scheduler mode, the
    // reference loop runs every kernel.
    let scheduler = schema
        .scheduler
        .as_ref()
        .filter(|scheduler| scheduler.fallback.len() == schema.processes.len());
    for index in 0..schema.processes.len() {
        let origin = schema
            .process_origins
            .get(index)
            .map_or_else(|| "?".to_string(), ToString::to_string);
        let fallback =
            scheduler.and_then(|scheduler| scheduler.fallback.get(index).cloned().flatten());
        let writes = scheduler
            .and_then(|scheduler| scheduler.kernel_events.get(index))
            .map(|events| {
                events
                    .iter()
                    .map(|&event| program.get_path(&scheduler.unwrap().events[event]))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let status = match (scheduler, fallback) {
            (None, _) => "reference loop".to_string(),
            (Some(_), None) => "scheduled".to_string(),
            (Some(_), Some(reason)) => format!("fallback ({reason})"),
        };
        let _ = writeln!(
            report,
            "kernel {index}: {origin}: {status} writes {writes:?} instructions {}",
            sir.processes
                .get(index)
                .map_or(0, |unit| instructions(std::slice::from_ref(unit)))
        );
    }
    match scheduler {
        Some(scheduler) => {
            let _ = writeln!(
                report,
                "scheduler unit: {:?}\nevents: {:?}\nclocks: {:?}\ndomain waits: {}\nupdate points: {}",
                scheduler.unit,
                scheduler
                    .events
                    .iter()
                    .map(|event| program.get_path(event))
                    .collect::<Vec<_>>(),
                scheduler
                    .clocks
                    .iter()
                    .map(|event| program.get_path(event))
                    .collect::<Vec<_>>(),
                scheduler
                    .domain_waits
                    .iter()
                    .filter(|&&waits| waits)
                    .count(),
                scheduler.update_points,
            );
        }
        None => report.push_str("scheduler: none (reference loop)\n"),
    }
    for warning in warnings {
        let _ = writeln!(report, "warning: {warning}");
    }
    let _ = writeln!(
        report,
        "sir: eval_comb {} units {} instructions; processes {} units {} instructions",
        sir.eval_comb.len(),
        instructions(&sir.eval_comb),
        sir.processes.len(),
        instructions(&sir.processes),
    );
    report
}
