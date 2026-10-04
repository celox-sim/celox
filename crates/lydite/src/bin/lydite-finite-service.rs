use lydite_ir::{Env, Lower, ty, var};
use lydite_solver::finite::{Limits, SearchHint, solve_with_hint};
use serde_json::{Value, json};
use std::io::{BufRead, Write};
fn query(v: Value) -> Result<Value, String> {
    let mut env = Env::new();
    for (name, sort) in v["declarations"].as_object().ok_or("declarations")? {
        env.insert(name.clone(), var(name.clone(), ty(sort)?));
    }
    let mut lower = Lower::default();
    let formula = lower.expr(&v["formula"], &env)?;
    let mut context = Env::new();
    for (name, expr) in v["context"].as_object().ok_or("context")? {
        context.insert(name.clone(), lower.expr(expr, &env)?);
    }
    let hint = if v["hint"] == "sat" {
        SearchHint::Sat
    } else {
        SearchHint::Unsat
    };
    let result = solve_with_hint(&formula, &context, Limits::default(), hint);
    let mut diagnostic = result.diagnostics();
    diagnostic.as_object_mut().unwrap().remove("assignments");
    Ok(diagnostic)
}
fn main() {
    for line in std::io::stdin().lock().lines() {
        let result = line
            .map_err(|e| e.to_string())
            .and_then(|line| serde_json::from_str(&line).map_err(|e| e.to_string()))
            .and_then(query);
        let out = match result {
            Ok(v) => v,
            Err(e) => json!({"error":e}),
        };
        println!("{}", out);
        std::io::stdout().flush().unwrap();
    }
}
