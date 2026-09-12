//! Real frontend inputs shared by the audit, timing driver and differential tests.

use celox::{CompilationTrace, OptimizeOptions, OptimizedSir, TraceOptions};
use celox_design::RegionedStateAddr;
use celox_sir::affine::MemoryObject;
use fxhash::FxHashMap as HashMap;
use std::{path::Path, time::Instant};

pub fn cases(n: usize) -> Vec<(&'static str, String)> {
    assert!(n >= 3);
    vec![
        (
            "map",
            format!(
                r#"module Top (a: input logic<32>[{n}], y: output logic<32>[{n}]) {{
            always_comb {{ for i in 0..{n} {{ y[i] = a[i] * 32'd3; }} }}
        }}"#
            ),
        ),
        (
            "producer_consumer",
            format!(
                r#"module Top (a: input logic<32>[{n}], y: output logic<32>[{n}]) {{
            var b: logic<32>[{n}];
            always_comb {{
                for i in 0..{n} {{ b[i] = a[i] * 32'd3; }}
                y[0] = 0; y[{last}] = 0;
                for i in 1..{last} {{ y[i] = b[i-1] + b[i] + b[i+1]; }}
            }}
        }}"#,
                last = n - 1
            ),
        ),
        (
            "reduction",
            format!(
                r#"module Top (a: input logic<32>[{n}], y: output logic<32>) {{
            always_comb {{ y = 0; for i in 0..{n} {{ y += a[i]; }} }}
        }}"#
            ),
        ),
        (
            "ff_shift",
            format!(
                r#"module Top (clk: input clock, a: input logic<32>, y: output logic<32>[{n}]) {{
            always_ff (clk) {{ y[0] = a; for i in 1..{n} {{ y[i] = y[i-1]; }} }}
        }}"#
            ),
        ),
    ]
}

/// Broader expression and access families, plus deliberately unsupported cases.
#[allow(dead_code)] // Also included by the earlier, narrower tuning driver.
pub fn scope_cases(n: usize) -> Vec<(&'static str, String)> {
    let mut cases = cases(n);
    for (name, inputs, body) in [
        (
            "fir5",
            format!("a: input logic<32>[{}]", n + 4),
            "y[i] = a[i] * 32'd3 + a[i+1] * 32'd5 + a[i+2] * 32'd7 + a[i+3] * 32'd5 + a[i+4] * 32'd3;".to_string(),
        ),
        (
            "strided_pair",
            format!("a: input logic<32>[{}]", 2 * n),
            "y[i] = a[2*i] * 32'd3 + a[2*i+1] * 32'd5;".to_string(),
        ),
        (
            "reverse",
            format!("a: input logic<32>[{n}]"),
            format!("y[i] = a[{}-i] * 32'd3;", n - 1),
        ),
        (
            "broadcast",
            format!("a: input logic<32>[{n}], coeff: input logic<32>[4]"),
            "y[i] = a[i] * coeff[0] + coeff[3];".to_string(),
        ),
        (
            "lane_mux",
            format!("a: input logic<32>[{n}], b: input logic<32>[{n}]"),
            "y[i] = if a[i][0] ? a[i] + b[i] : a[i] ^ b[i];".to_string(),
        ),
        (
            "rotate",
            format!("a: input logic<32>[{n}]"),
            format!("y[i] = a[(i+1)%{n}];"),
        ),
        (
            "indexed_gather",
            format!("a: input logic<32>[{n}], idx: input logic<32>[{n}]"),
            format!("y[i] = a[idx[i] % 32'd{n}];"),
        ),
        (
            "interleave",
            format!("a: input logic<32>[{n}]"),
            "y[i] = if i % 2 == 0 ? a[i] * 32'd3 : a[i] * 32'd5;".to_string(),
        ),
    ] {
        cases.push((name, format!("module Top ({inputs}, y: output logic<32>[{n}]) {{ always_comb {{ for i in 0..{n} {{ {body} }} }} }}")));
    }
    cases
}

#[allow(dead_code)] // Audit-only builds call compile_top directly.
pub fn compile_mode(code: &str, four_state: bool) -> (OptimizedSir, CompilationTrace, f64) {
    compile_top(code, "Top", four_state)
}

pub fn compile_top(
    code: &str,
    top: &str,
    four_state: bool,
) -> (OptimizedSir, CompilationTrace, f64) {
    let mut trace = CompilationTrace::default();
    let options = TraceOptions {
        flattened_comb_blocks: true,
        pre_optimized_sir: true,
        post_optimized_sir: true,
        ..Default::default()
    };
    let start = Instant::now();
    let (program, _) = celox::compile_to_sir(
        &[(code, Path::new("affine_veryl.veryl"))],
        top,
        &[],
        &[],
        four_state,
        &options,
        Some(&mut trace),
        None,
        None,
        None,
        &[],
        &OptimizeOptions::all(),
    )
    .unwrap();
    (program, trace, start.elapsed().as_secs_f64() * 1e3)
}

pub fn objects(program: &OptimizedSir) -> HashMap<RegionedStateAddr, MemoryObject> {
    let mut objects = HashMap::default();
    for (&address, metadata) in &program.design.state_objects {
        let elements = metadata.array_dims.iter().product::<usize>();
        assert!(elements > 0 && metadata.width.is_multiple_of(elements));
        let object = MemoryObject {
            element_width: metadata.width / elements,
            elements,
        };
        for region in [0, 1, 2] {
            objects.insert(
                RegionedStateAddr::from_absolute_addr(region, address),
                object.clone(),
            );
        }
    }
    objects
}
