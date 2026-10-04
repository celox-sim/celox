use lydite_ir as ir;
#[cfg(test)]
use lydite_ir::lower as frontend;
#[cfg(test)]
use lydite_solver as solver;
#[cfg(test)]
use lydite_solver::kernel;
#[cfg(test)]
use lydite_solver::partition as program;
use lydite_verify as checker;
#[cfg(test)]
mod json_input {
    pub use lydite_syntax::parse_json as parse;
}
#[cfg(test)]
use crate::checker::check;
use crate::ir::Res;
use serde_json::{Value, json};
use std::{env, fs, path::PathBuf};
enum Input {
    Design(ir::Design),
    Specification(ir::Specification),
    ScopedSpecification(ir::ScopedSpecification),
}
impl Input {
    fn document(&self) -> &Value {
        match self {
            Self::Design(d) => d.document(),
            Self::Specification(s) => s.document(),
            Self::ScopedSpecification(s) => s.document(),
        }
    }
}
fn run() -> Res<i32> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.is_empty() {
        return Err("usage: lydite DESIGN.{json,lyd} [--out DIR] [--z3 PATH] [--format json|lyd] [--check] [--emit-json FILE] [--lemmas FILE.lyd] [--structural-artifact FILE.json] [--finite-search-hint query|sat|unsat]".into());
    }
    if args[0] == "--help" || args[0] == "-h" {
        println!(
            "lydite DESIGN.{{json,lyd}} [--out DIR] [--z3 PATH] [--format json|lyd] [--check] [--emit-json FILE] [--lemmas FILE.lyd] [--structural-artifact FILE.json] [--finite-search-hint query|sat|unsat]\n--check and --emit-json validate all fields without running a solver.\nZ3_BIN sets the default solver executable.\nLYDITE_SOLVER=finite selects the bounded scalar Bool/BV backend without Z3 fallback.\n--finite-search-hint query (default) follows each query expectation; sat/unsat override finite search order only.\nLYDITE_FINITE_SEARCH_HINT sets the same default; the CLI option takes precedence."
        );
        return Ok(0);
    }
    let mut out = PathBuf::from("results");
    let mut z3 = env::var("Z3_BIN").unwrap_or("z3".into());
    let mut format = if std::path::Path::new(&args[0])
        .extension()
        .is_some_and(|x| x == "lyd")
    {
        "lyd".to_string()
    } else {
        "json".to_string()
    };
    let mut check_only = false;
    let mut emit_json = None;
    let mut lemma_source: Option<PathBuf> = None;
    let mut finite_search_hint = None;
    let mut structural_artifact_path: Option<PathBuf> = None;
    let mut n = 1;
    while n < args.len() {
        if args[n] == "--check" {
            check_only = true;
            n += 1;
            continue;
        }
        if n + 1 >= args.len() {
            return Err("missing option value".into());
        }
        match args[n].as_str() {
            "--lemmas" => {
                if lemma_source.is_some() {
                    return Err("--lemmas may only be specified once".into());
                }
                lemma_source = Some(PathBuf::from(&args[n + 1]));
            }
            "--structural-artifact" => {
                if structural_artifact_path.is_some() {
                    return Err("--structural-artifact may only be specified once".into());
                }
                structural_artifact_path = Some(PathBuf::from(&args[n + 1]));
            }
            "--out" => out = PathBuf::from(&args[n + 1]),
            "--z3" => z3 = args[n + 1].clone(),
            "--finite-search-hint" => {
                lydite_solver::parse_finite_search_hint(&args[n + 1])?;
                if finite_search_hint.is_some() {
                    return Err("--finite-search-hint may only be specified once".into());
                }
                finite_search_hint = Some(args[n + 1].clone());
            }
            "--format" => {
                format = args[n + 1].clone();
                if format != "json" && format != "lyd" {
                    return Err("--format must be json or lyd".into());
                }
            }
            "--emit-json" => {
                emit_json = Some(PathBuf::from(&args[n + 1]));
                check_only = true;
            }
            _ => return Err("unsupported option".into()),
        }
        n += 2;
    }
    // This single-threaded CLI selects the process-wide default. Library users
    // can instead pass QueryOptions without changing process environment.
    if let Some(hint) = finite_search_hint {
        // FIXME: Audit that the environment access only happens in single-threaded code.
        unsafe { env::set_var("LYDITE_FINITE_SEARCH_HINT", hint) };
    } else {
        match env::var("LYDITE_FINITE_SEARCH_HINT") {
            Ok(hint) => {
                lydite_solver::parse_finite_search_hint(&hint)?;
            }
            Err(env::VarError::NotPresent) => {}
            Err(_) => return Err("LYDITE_FINITE_SEARCH_HINT is not valid UTF-8".into()),
        }
    }
    fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let mut source_spans = std::collections::BTreeMap::new();
    let outcome = (|| -> Res<Value> {
        let bytes = fs::read(&args[0]).map_err(|e| e.to_string())?;
        let mut design = if format == "lyd" {
            let source = std::str::from_utf8(&bytes)
                .map_err(|e| format!("{}: invalid UTF-8 source: {e}", args[0]))?;
            let parsed =
                lydite_syntax::parse_document(source, &args[0]).map_err(|e| e.to_string())?;
            source_spans = parsed.spans.clone();
            if parsed.canonical["version"] == 4 {
                Input::ScopedSpecification(
                    parsed
                        .validate_scoped_specification()
                        .map_err(|e| e.to_string())?,
                )
            } else if parsed.canonical["kind"] == "specification" {
                Input::Specification(parsed.validate_specification().map_err(|e| e.to_string())?)
            } else {
                Input::Design(parsed.validate().map_err(|e| e.to_string())?)
            }
        } else {
            let doc = lydite_syntax::parse_json(&bytes)?;
            if doc["version"] == 4 {
                Input::ScopedSpecification(
                    ir::ScopedSpecification::from_json(&doc)
                        .map_err(|e| format!("{}: {e}", args[0]))?,
                )
            } else if doc["kind"] == "specification" {
                Input::Specification(
                    ir::Specification::from_json(&doc).map_err(|e| format!("{}: {e}", args[0]))?,
                )
            } else {
                Input::Design(ir::Design::from_json(&doc).map_err(|e| format!("{}: {e}", args[0]))?)
            }
        };
        if let Some(path) = lemma_source {
            let Input::Design(original) = &design else {
                return Err(
                    "native lemma modules currently require a design, not a scoped specification"
                        .into(),
                );
            };
            lydite_verify::validate_proof_metadata(original)?;
            let source =
                fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            let mut doc = original.document().clone();
            doc["proof_programs"] = lydite_syntax::merge_lemma_source(
                &source,
                &path.to_string_lossy(),
                doc.get("proof_programs"),
            )
            .map_err(|e| e.to_string())?;
            design = Input::Design(ir::Design::from_json(&doc).map_err(|e| e.to_string())?);
        }
        if let Input::Design(d) = &design {
            lydite_verify::validate_proof_metadata(d).map_err(|e| format!("{}: {e}", args[0]))?;
        }
        if let Some(destination) = emit_json {
            fs::write(
                destination,
                format!(
                    "{}\n",
                    serde_json::to_string_pretty(design.document()).unwrap()
                ),
            )
            .map_err(|e| e.to_string())?;
        }
        let structural_artifact = if let Some(path) = &structural_artifact_path {
            if !matches!(&design, Input::Specification(_)) {
                return Err("structural artifacts require a v3 specification".into());
            }
            let bytes = fs::read(path).map_err(|e| e.to_string())?;
            Some(lydite_syntax::parse_json(&bytes).map_err(|e| e.to_string())?)
        } else {
            None
        };
        if check_only {
            Ok(
                json!({"status":"validated", "name":design.document().get("name"), "input_format":format, "claim":"Syntax, names and types validated; no proof obligations executed"}),
            )
        } else {
            match &design {
                Input::Design(d) => checker::check_design(d, z3, out.clone()),
                Input::Specification(s) => checker::check_specification_structural(
                    s,
                    z3,
                    out.clone(),
                    structural_artifact.as_ref(),
                ),
                Input::ScopedSpecification(s) => {
                    checker::check_scoped_specification(s, z3, out.clone())
                }
            }
        }
    })();
    let mut result = match outcome {
        Ok(value) => value,
        Err(error) => json!({"status":"invalid_or_tool_error","error":error}),
    };
    lydite_syntax::locate_report(&mut result, &args[0], &source_spans);
    fs::write(
        out.join("report.json"),
        serde_json::to_string_pretty(&result).unwrap(),
    )
    .map_err(|e| e.to_string())?;
    println!("{}", serde_json::to_string_pretty(&result).unwrap());
    Ok(
        if result["status"] == "stuttering_refinement_verified"
            || result["status"] == "program_and_refinement_verified"
            || result["status"] == "validated"
            || result["status"] == "spec_examples_passed"
            || result["status"] == "spec_examples_and_binding_verified"
            || result["status"] == "binding_verified_no_examples"
        {
            0
        } else if result["status"] == "counterexample"
            || result["status"] == "spec_examples_failed"
            || result["status"] == "implementation_binding_failed"
            || result["status"] == "structural_contract_failed"
        {
            1
        } else if result["status"] == "invalid_or_tool_error" {
            2
        } else {
            3
        },
    )
}
fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            println!("{}", json!({"status":"invalid_or_tool_error","error":e}));
            std::process::exit(2)
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod kernel_tests;
