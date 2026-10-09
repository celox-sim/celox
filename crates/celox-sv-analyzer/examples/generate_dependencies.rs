//! Generate-scope signal scheduling, excluding backend compilation/simulation.
//! `cargo run --locked -p celox-sv-analyzer --profile heliodor-dev --example generate_dependencies`

use std::{fmt::Write, path::Path, time::Instant};

use celox_sv_analyzer::{analyze, ast, syntax};
use fxhash::FxHashSet as HashSet;

// Compile the production scheduler without adding a public analyzer API.
#[path = "../src/ast/generate/dependency_order.rs"]
mod dependency_order;
use dependency_order::DependencyOrder;

fn main() {
    let mut args: Vec<_> = std::env::args().skip(1).collect();
    let scheduler = args.first().is_some_and(|arg| arg == "--scheduler");
    if scheduler {
        args.remove(0);
    }
    let assignments = args.first().is_some_and(|arg| arg == "--assignments");
    if assignments {
        args.remove(0);
    }
    let counts: Vec<usize> = args
        .into_iter()
        .map(|arg| {
            let count = arg.parse().expect("counts must be positive integers");
            assert!(count > 0, "counts must be positive integers");
            count
        })
        .collect();
    let counts = if counts.is_empty() {
        vec![64, 256, 1024]
    } else {
        counts
    };
    if scheduler {
        scheduler_scaling(&counts);
        return;
    }
    println!("signals,bytes,parse_ms,ast_ms,ir_ms");
    for count in counts {
        let mut code = String::from("module Top(); if (1) begin : g\n");
        for i in 0..count {
            writeln!(code, "logic [7:0] s{i};").unwrap();
            if assignments {
                writeln!(code, "assign s{i} = 8'h5a;").unwrap();
            }
        }
        code.push_str("end endmodule\n");
        let mut samples = Vec::new();
        for _ in 0..3 {
            let start = Instant::now();
            let tree = syntax::parse_source(&code, Path::new("generate_dependencies.sv")).unwrap();
            let parsed = Instant::now();
            let source = ast::Source::from_syntax(&tree).unwrap();
            let lowered = Instant::now();
            let ir = analyze::analyze_source(source).unwrap();
            let finished = Instant::now();
            assert_eq!(ir.modules().len(), 1);
            assert_eq!(ir.modules()[0].signals().len(), count);
            assert_eq!(
                ir.modules()[0].comb_processes().len(),
                if assignments { count } else { 0 }
            );
            let names: HashSet<_> = ir.modules()[0]
                .signals()
                .iter()
                .map(|signal| signal.name())
                .collect();
            assert_eq!(names.len(), count);
            for i in 0..count {
                assert!(names.contains(format!("g.s{i}").as_str()));
            }
            for signal in ir.modules()[0].signals() {
                assert_eq!(signal.r#type().resolved_width(), Some(8));
            }
            samples.push([
                (parsed - start).as_secs_f64() * 1000.0,
                (lowered - parsed).as_secs_f64() * 1000.0,
                (finished - lowered).as_secs_f64() * 1000.0,
            ]);
        }
        let medians: Vec<_> = (0..3)
            .map(|phase| {
                let mut values: Vec<_> = samples.iter().map(|sample| sample[phase]).collect();
                values.sort_by(f64::total_cmp);
                values[1]
            })
            .collect();
        println!(
            "{count},{},{:.3},{:.3},{:.3}",
            code.len(),
            medians[0],
            medians[1],
            medians[2]
        );
    }
}

// The former readiness loop, isolated from AST parsing/binding. Signal-first
// and declaration-order priority is a total order on the original indices.
fn legacy_order(names: &[String], dependencies: &[HashSet<String>]) -> Vec<usize> {
    let mut remaining: Vec<_> = (0..names.len()).collect();
    let mut result = Vec::new();
    while !remaining.is_empty() {
        let unresolved: HashSet<_> = remaining
            .iter()
            .map(|&index| names[index].clone())
            .collect();
        let Some(position) = remaining
            .iter()
            .position(|&index| dependencies[index].is_disjoint(&unresolved))
        else {
            break;
        };
        result.push(remaining.remove(position));
    }
    result
}

fn scheduler_scaling(counts: &[usize]) {
    println!("graph,vertices,edges,legacy_ms,graph_ms");
    for kind in ["independent", "reverse_chain", "fanout"] {
        for &count in counts {
            let names: Vec<_> = (0..count).map(|index| format!("n{index}")).collect();
            let dependencies: Vec<HashSet<String>> = (0..count)
                .map(|index| {
                    let dependency = match kind {
                        "reverse_chain" => names.get(index + 1),
                        "fanout" if index + 1 < count => names.last(),
                        _ => None,
                    };
                    dependency
                        .map(|name| HashSet::from_iter([name.clone()]))
                        .unwrap_or_default()
                })
                .collect();
            let expected: Vec<_> = match kind {
                "reverse_chain" => (0..count).rev().collect(),
                "fanout" => std::iter::once(count - 1).chain(0..count - 1).collect(),
                _ => (0..count).collect(),
            };
            let mut legacy_samples = Vec::new();
            let mut graph_samples = Vec::new();
            for _ in 0..3 {
                let start = Instant::now();
                let legacy = legacy_order(&names, &dependencies);
                legacy_samples.push(start.elapsed().as_secs_f64() * 1000.0);
                assert_eq!(legacy, expected);
                let start = Instant::now();
                let mut order = DependencyOrder::new(
                    names
                        .iter()
                        .zip(&dependencies)
                        .map(|(name, deps)| (name.as_str(), deps)),
                );
                let mut actual = Vec::with_capacity(count);
                while let Some(index) = order.pop_ready() {
                    actual.push(index);
                    order.complete(index);
                }
                graph_samples.push(start.elapsed().as_secs_f64() * 1000.0);
                assert!(order.is_complete());
                assert_eq!(actual, expected);
            }
            legacy_samples.sort_by(f64::total_cmp);
            graph_samples.sort_by(f64::total_cmp);
            println!(
                "{kind},{count},{},{:.3},{:.3}",
                dependencies.iter().map(HashSet::len).sum::<usize>(),
                legacy_samples[1],
                graph_samples[1]
            );
        }
    }
}
