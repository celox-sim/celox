use hwverify_ir as ir;
#[cfg(test)]
use hwverify_ir::lower as frontend;
#[cfg(test)]
use hwverify_solver as solver;
#[cfg(test)]
use hwverify_solver::kernel;
#[cfg(test)]
use hwverify_solver::partition as program;
use hwverify_verify as checker;
#[cfg(test)]
mod json_input {
    pub use hwverify_syntax::parse_json as parse;
}
#[cfg(test)]
use crate::checker::check;
use crate::ir::Res;
use serde_json::{json, Value};
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
        return Err("usage: hwverify-rs DESIGN.{json,hwv} [--out DIR] [--z3 PATH] [--format json|hwv] [--check] [--emit-json FILE] [--finite-search-hint query|sat|unsat]".into());
    }
    if args[0] == "--help" || args[0] == "-h" {
        println!("hwverify-rs DESIGN.{{json,hwv}} [--out DIR] [--z3 PATH] [--format json|hwv] [--check] [--emit-json FILE] [--finite-search-hint query|sat|unsat]\n--check and --emit-json validate all fields without running a solver.\nZ3_BIN sets the default solver executable.\nHWVERIFY_SOLVER=finite selects the bounded scalar Bool/BV backend without Z3 fallback.\n--finite-search-hint query (default) follows each query expectation; sat/unsat override finite search order only.\nHWVERIFY_FINITE_SEARCH_HINT sets the same default; the CLI option takes precedence.");
        return Ok(0);
    }
    let mut out = PathBuf::from("results");
    let mut z3 = env::var("Z3_BIN").unwrap_or("z3".into());
    let mut format = if std::path::Path::new(&args[0])
        .extension()
        .is_some_and(|x| x == "hwv")
    {
        "hwv".to_string()
    } else {
        "json".to_string()
    };
    let mut check_only = false;
    let mut emit_json = None;
    let mut finite_search_hint = None;
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
            "--out" => out = PathBuf::from(&args[n + 1]),
            "--z3" => z3 = args[n + 1].clone(),
            "--finite-search-hint" => {
                hwverify_solver::parse_finite_search_hint(&args[n + 1])?;
                if finite_search_hint.is_some() {
                    return Err("--finite-search-hint may only be specified once".into());
                }
                finite_search_hint = Some(args[n + 1].clone());
            }
            "--format" => {
                format = args[n + 1].clone();
                if format != "json" && format != "hwv" {
                    return Err("--format must be json or hwv".into());
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
        env::set_var("HWVERIFY_FINITE_SEARCH_HINT", hint);
    } else {
        match env::var("HWVERIFY_FINITE_SEARCH_HINT") {
            Ok(hint) => {
                hwverify_solver::parse_finite_search_hint(&hint)?;
            }
            Err(env::VarError::NotPresent) => {}
            Err(_) => return Err("HWVERIFY_FINITE_SEARCH_HINT is not valid UTF-8".into()),
        }
    }
    fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let outcome = (|| -> Res<Value> {
        let bytes = fs::read(&args[0]).map_err(|e| e.to_string())?;
        let design = if format == "hwv" {
            let source = std::str::from_utf8(&bytes)
                .map_err(|e| format!("{}: invalid UTF-8 source: {e}", args[0]))?;
            let parsed =
                hwverify_syntax::parse_document(source, &args[0]).map_err(|e| e.to_string())?;
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
            let doc = hwverify_syntax::parse_json(&bytes)?;
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
        if check_only {
            Ok(
                json!({"status":"validated", "name":design.document().get("name"), "input_format":format, "claim":"Syntax, names and types validated; no proof obligations executed"}),
            )
        } else {
            match &design {
                Input::Design(d) => checker::check_design(d, z3, out.clone()),
                Input::Specification(s) => checker::check_specification(s, z3, out.clone()),
                Input::ScopedSpecification(s) => {
                    checker::check_scoped_specification(s, z3, out.clone())
                }
            }
        }
    })();
    let result = match outcome {
        Ok(value) => value,
        Err(error) => json!({"status":"invalid_or_tool_error","error":error}),
    };
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
