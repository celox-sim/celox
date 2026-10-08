//! Reproducible analyzer scaling probe. Run with an optimized profile:
//! `cargo run --locked -p celox-sv-analyzer --profile heliodor-dev --example scaling`

use std::{fmt::Write, path::Path, time::Instant};

use celox_sv_analyzer::{ModuleInterfaces, ParsedSource, analyze, ast, syntax};
use fxhash::FxHashMap as HashMap;

fn source(kind: &str, count: usize) -> String {
    if kind == "generate" {
        return format!(
            "module Top(input logic [7:0] a);\n\
            for (genvar i = 0; i < {count}; i++) begin : g\n\
            logic [7:0] s; assign s = a + i; end\nendmodule\n"
        );
    }
    let mut code = String::from("module Top(input logic clk, input logic [7:0] a);\n");
    for i in 0..count {
        writeln!(code, "logic [7:0] s{i};").unwrap();
    }
    for i in 0..count {
        match kind {
            "assign" => writeln!(code, "assign s{i} = a + 8'd1;"),
            "comb" => writeln!(code, "always_comb s{i} = a + 8'd1;"),
            "locals" => writeln!(
                code,
                "always_comb begin logic [7:0] t{i}; t{i} = a + 8'd1; s{i} = t{i}; end"
            ),
            "ff" => writeln!(code, "always_ff @(posedge clk) s{i} <= a + 8'd1;"),
            _ => unreachable!(),
        }
        .unwrap();
    }
    code.push_str("endmodule\n");
    code
}

fn main() {
    let mut args: Vec<_> = std::env::args().skip(1).collect();
    let hierarchy = args.first().is_some_and(|arg| arg == "--hierarchy");
    if hierarchy {
        args.remove(0);
    }
    let counts: Vec<usize> = args
        .into_iter()
        .map(|arg| arg.parse().expect("counts must be positive integers"))
        .collect();
    let counts = if counts.is_empty() {
        if hierarchy {
            vec![8, 32, 128]
        } else {
            vec![128, 512, 2048]
        }
    } else {
        counts
    };
    if hierarchy {
        hierarchy_scaling(&counts);
        return;
    }
    println!("kind,count,bytes,parse_ms,ast_ms,ir_ms");
    for kind in ["assign", "comb", "locals", "ff", "generate"] {
        for &count in &counts {
            let code = source(kind, count);
            // Use the median of three runs to limit scheduler noise. Each run
            // parses and lowers fresh input; there is no global warm cache.
            let mut samples = Vec::new();
            for _ in 0..3 {
                let start = Instant::now();
                let tree = syntax::parse_source(&code, Path::new("scaling.sv")).unwrap();
                let parsed = Instant::now();
                let source = ast::Source::from_syntax(&tree).unwrap();
                let lowered = Instant::now();
                let ir = analyze::analyze_source(source).unwrap();
                let finished = Instant::now();
                assert_eq!(
                    ir.modules()[0].signals().len(),
                    if kind == "locals" { 2 * count } else { count }
                );
                samples.push([
                    (parsed - start).as_secs_f64() * 1000.0,
                    (lowered - parsed).as_secs_f64() * 1000.0,
                    (finished - lowered).as_secs_f64() * 1000.0,
                ]);
            }
            let median: Vec<_> = (0..3)
                .map(|phase| {
                    let mut times: Vec<_> = samples.iter().map(|sample| sample[phase]).collect();
                    times.sort_by(f64::total_cmp);
                    times[1]
                })
                .collect();
            println!(
                "{kind},{count},{},{:.3},{:.3},{:.3}",
                code.len(),
                median[0],
                median[1],
                median[2]
            );
        }
    }
}

// Compare reparsing a multi-module source per module with reusing it. Include
// the reusable source's initial parse, and lower every module in both modes.
fn hierarchy_scaling(counts: &[usize]) {
    println!("mode,modules,bytes,total_ms");
    for &count in counts {
        let mut code = String::new();
        for i in 0..count {
            writeln!(code, "module M{i} #(parameter N = 8) (input logic [N-1:0] a, output logic [N-1:0] y); assign y = a + 1'b1; endmodule").unwrap();
        }
        for reuse in [false, true] {
            let mut samples = Vec::new();
            for _ in 0..3 {
                let start = Instant::now();
                let path = Path::new("hierarchy.sv");
                let parsed = reuse.then(|| ParsedSource::parse(&code, path).unwrap());
                for i in 0..count {
                    let name = format!("M{i}");
                    let overrides = HashMap::default();
                    let interfaces = ModuleInterfaces::default();
                    let ir = if let Some(parsed) = &parsed {
                        parsed.analyze_module_with_parameter_expr_overrides(
                            &name,
                            &overrides,
                            &interfaces,
                        )
                    } else {
                        celox_sv_analyzer::analyze_source_module_with_parameter_expr_overrides(
                            &code,
                            path,
                            &name,
                            &overrides,
                            &interfaces,
                        )
                    }
                    .unwrap();
                    assert_eq!(ir.modules().len(), 1);
                    assert_eq!(ir.modules()[0].name(), name);
                }
                samples.push(start.elapsed().as_secs_f64() * 1000.0);
            }
            samples.sort_by(f64::total_cmp);
            println!(
                "{},{count},{},{:.3}",
                if reuse { "reused" } else { "fresh" },
                code.len(),
                samples[1]
            );
        }
    }
}
