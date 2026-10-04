use hwverify_ir::*;
use hwverify_solver::{
    finite::{self, Limits, SearchHint, Verdict},
    kernel,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, time::Instant};
fn run() -> Res<()> {
    let path = std::env::args().nth(1).ok_or("DESIGN.json")?;
    let doc: Value = serde_json::from_slice(&fs::read(path).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let d = Design::from_json(&doc).map_err(|e| e.to_string())?;
    let s = d.spec();
    let m = d.implementation();
    let i = d.inputs();
    let rst = i[text(&doc["reset_input"])?].clone();
    let commit = m.outputs[text(&doc["commit"])?].clone();
    let mut lower = Lower::default();
    let relation = lower.expr(
        &doc["binding"],
        &relation_env(&s.state, &m.state, &Env::new()),
    )?;
    let ns = s
        .next
        .iter()
        .map(|(name, value)| {
            (
                name.clone(),
                ite(
                    rst.clone(),
                    s.reset[name].clone(),
                    ite(commit.clone(), value.clone(), s.state[name].clone()),
                ),
            )
        })
        .collect::<Env>();
    let nm = m
        .next
        .iter()
        .map(|(name, value)| {
            (
                name.clone(),
                ite(rst.clone(), m.reset[name].clone(), value.clone()),
            )
        })
        .collect::<Env>();
    let after = lower.expr(&doc["binding"], &relation_env(&ns, &nm, &Env::new()))?;
    let original = and(relation, not(after));
    let mut ctx = relation_env(&s.state, &m.state, i);
    ctx.extend(
        ns.iter()
            .map(|(n, v)| (format!("spec_next.{n}"), v.clone())),
    );
    ctx.extend(
        nm.iter()
            .map(|(n, v)| (format!("impl_next.{n}"), v.clone())),
    );
    let variables = i
        .values()
        .chain(m.state.values())
        .chain(s.state.values())
        .filter(|v| v.0.sort == Sort::Bool)
        .cloned()
        .collect::<Vec<_>>();
    if variables.len() > 8 {
        return Err("probe limited to8 Boolean control symbols".into());
    }
    println!(
        "{}",
        json!({"controls":variables.iter().map(|t|&t.0.op).collect::<Vec<_>>(),"complete_cases":1usize<<variables.len()})
    );
    let start = Instant::now();
    let mut work = 0;
    let mut clauses = 0;
    let mut closed = 0;
    let mut overall = "unsat";
    for case in 0..(1usize << variables.len()) {
        if work >= 100_000_000 || clauses >= 1_000_000 || start.elapsed().as_millis() >= 10_000 {
            overall = "unknown";
            break;
        }
        let replacements = variables
            .iter()
            .enumerate()
            .map(|(index, t)| (t.clone(), boolv(case & (1 << index) != 0)))
            .collect::<BTreeMap<_, _>>();
        let raw = substitute(&original, &replacements);
        let formula = kernel::simplify(&raw, &[]);
        let ks = Instant::now();
        let attempt = kernel::refute(&formula);
        if attempt.closed {
            closed += 1;
            println!(
                "{}",
                json!({"case":case,"status":"kernel_unsat","kernel_seconds":ks.elapsed().as_secs_f64()})
            );
            continue;
        }
        let context = ctx
            .iter()
            .map(|(name, value)| {
                (
                    name.clone(),
                    kernel::simplify(&substitute(value, &replacements), &[]),
                )
            })
            .collect::<Env>();
        let remaining_ms = 10_000u64.saturating_sub(start.elapsed().as_millis() as u64);
        let result = finite::solve_with_hint(
            &formula,
            &context,
            Limits {
                max_work: 100_000_000 - work,
                max_clauses: 1_000_000 - clauses,
                timeout_ms: remaining_ms,
                ..Limits::default()
            },
            SearchHint::Unsat,
        );
        work += result.stats.work;
        clauses += result.stats.clauses;
        println!(
            "{}",
            json!({"case":case,"kernel_seconds":ks.elapsed().as_secs_f64(),"finite":result.diagnostics(),"cumulative_work":work,"cumulative_clauses":clauses})
        );
        match result.verdict {
            Verdict::Unsat => closed += 1,
            Verdict::Sat => {
                overall = "sat_case";
                break;
            }
            Verdict::Unknown => {
                overall = "unknown";
                break;
            }
        }
    }
    println!(
        "{}",
        json!({"overall":overall,"closed_cases":closed,"total_cases":1usize<<variables.len(),"seconds":start.elapsed().as_secs_f64(),"work":work,"clauses":clauses,"scope":"experimental complete Boolean cofactor cover; SAT case model not yet reconstructed for original query"})
    );
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(2)
    }
}
