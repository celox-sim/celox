//! Real Veryl eligibility and execution experiments (optimizer defaults unchanged).
//! `affine_veryl audit [elements=32] [dump-directory]`
//! `affine_veryl scope [elements=32] [dump-directory]`
//! `affine_veryl repo` audits four existing repository designs at their defaults.
//! `affine_veryl bench [elements=32] [samples=15] [dump-directory]`

use celox_design::{RegionedStateAddr, StateAddr};
use celox_sir::{ExecutionUnit, SIRInstruction, cfg::SirCfg};
use celox_slt::SLTNode;
use std::{io::Write, path::Path};

#[cfg(all(feature = "host-runtime", target_arch = "x86_64"))]
#[allow(dead_code)]
mod affine_support;
mod affine_veryl_support;

fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let mode = args.first().map(String::as_str).unwrap_or("audit");
    let n = args
        .get(1)
        .map(|s| s.parse::<usize>().expect("element count"))
        .unwrap_or(32);
    assert!(n >= 3);
    match mode {
        "audit" | "scope" | "repo" => audit(n, args.get(2).map(String::as_str), mode),
        "bench" => bench(
            n,
            args.get(2)
                .map(|s| s.parse::<usize>().expect("sample count"))
                .unwrap_or(15),
            args.get(3).map(String::as_str),
        ),
        _ => panic!(
            "usage: affine_veryl audit|scope [elements] [dump-directory] | repo | bench [elements] [samples]"
        ),
    }
}

fn audit(n: usize, dump: Option<&str>, mode: &str) {
    let cases = if mode == "repo" {
        vec![
            (
                "counters_1000",
                include_str!("../testdata/veryl/top_n1000.veryl").to_string(),
                "Top",
            ),
            (
                "linear_sorter_8",
                include_str!("../tests/macro_project/src/linear_sorter.veryl").to_string(),
                "LinearSorter",
            ),
            (
                "sorter_pull_100",
                include_str!("../tests/fixtures/linear_sorter_pull_mre.veryl").to_string(),
                "LinearSorterPullMreU16",
            ),
            (
                "axi_lite_regs",
                include_str!("../tests/fixtures/bitslice/axi_lite_reg_file.veryl").to_string(),
                "AxiLiteRegFile",
            ),
        ]
    } else {
        let cases = if mode == "scope" {
            affine_veryl_support::scope_cases(n)
        } else {
            affine_veryl_support::cases(n)
        };
        cases
            .into_iter()
            .map(|(name, code)| (name, code, "Top"))
            .collect()
    };
    let elements = if mode == "repo" {
        String::new()
    } else {
        n.to_string()
    };
    let mut diagnostics = std::io::stderr().lock();
    println!(
        "case,n,four_state,phase,units,loops,loop_header_params,slt_arena_nodes,slt_folds,slt_groups,stores,canonical_eligible,recovered_eligible,compile_ms,region_eligible_units,regions,region_stores,region_ms"
    );
    for (name, code, top) in cases {
        for four_state in [false, true] {
            let (program, trace, compile_ms) =
                affine_veryl_support::compile_top(&code, top, four_state);
            let objects = affine_veryl_support::objects(&program);
            let (nodes, folds, groups) = trace
                .flattened_comb_blocks
                .as_ref()
                .map(|(_, arena)| {
                    (
                        arena.len(),
                        arena
                            .iter()
                            .filter(|n| matches!(n, SLTNode::ForFold { .. }))
                            .count(),
                        arena
                            .iter()
                            .filter(|n| matches!(n, SLTNode::ForFoldGroup { .. }))
                            .count(),
                    )
                })
                .unwrap_or_default();
            if let Some(directory) = dump {
                let path = Path::new(directory);
                std::fs::create_dir_all(path).unwrap();
                std::fs::write(path.join(format!("{name}_{four_state}.veryl")), &code).unwrap();
                std::fs::write(
                    path.join(format!("{name}_{four_state}_slt.txt")),
                    format!("{:#?}", trace.flattened_comb_blocks),
                )
                .unwrap();
                std::fs::write(
                    path.join(format!("{name}_{four_state}_pre.txt")),
                    trace.format_pre_optimized_sir().unwrap(),
                )
                .unwrap();
                std::fs::write(
                    path.join(format!("{name}_{four_state}_post.txt")),
                    trace.format_post_optimized_sir().unwrap(),
                )
                .unwrap();
            }
            let pre = trace.pre_optimized_sir.as_ref().unwrap();
            for (phase, sir) in [("pre", &pre.sir), ("post", &program.sir)] {
                let units = units(sir);
                let mut loops = 0;
                let mut parameters = 0;
                let mut eligible = 0;
                let mut recovered = 0;
                let mut region_units = 0;
                let mut regions = 0;
                let mut region_stores = 0;
                let mut region_ms = 0.0;
                let mut rejected = std::collections::BTreeMap::<String, usize>::new();
                for unit in &units {
                    let start = std::time::Instant::now();
                    match celox_sir::affine::recover_independent_regions(
                        unit,
                        &objects,
                        &Default::default(),
                    ) {
                        Ok(found) => {
                            region_units += usize::from(!found.is_empty());
                            regions += found.len();
                            region_stores += found.store_count();
                            for rejection in &found.rejections {
                                *rejected
                                    .entry(format!("region: {}", rejection.reason))
                                    .or_default() += 1;
                            }
                        }
                        Err(error) => {
                            *rejected.entry(format!("regions: {error}")).or_default() += 1
                        }
                    }
                    region_ms += start.elapsed().as_secs_f64() * 1e3;
                    let cfg = SirCfg::analyze_forward_structure(unit).unwrap();
                    loops += cfg.loops.len();
                    // Includes induction and countdown parameters, not just
                    // values carried between source iterations.
                    parameters += cfg
                        .loops
                        .iter()
                        .map(|l| unit.blocks[&cfg.block_ids[l.header]].params.len())
                        .sum::<usize>();
                    for (method, result) in [
                        ("canonical", celox_sir::affine::extract(unit, &objects)),
                        (
                            "recovered",
                            celox_sir::affine::recover_independent_stores(
                                unit,
                                &objects,
                                &Default::default(),
                            ),
                        ),
                    ] {
                        match result {
                            Ok(kernel) => {
                                if method == "canonical" {
                                    eligible += 1;
                                } else {
                                    recovered += 1;
                                }
                                writeln!(diagnostics, "{name} four_state={four_state} {phase} {method}: statements={}", kernel.region().statements.len()).unwrap();
                            }
                            Err(error) => {
                                *rejected.entry(format!("{method}: {error}")).or_default() += 1;
                            }
                        }
                    }
                }
                writeln!(
                    diagnostics,
                    "{name} four_state={four_state} {phase}: rejected={rejected:?}"
                )
                .unwrap();
                let stores = units
                    .iter()
                    .flat_map(|u| u.blocks.values())
                    .flat_map(|b| &b.instructions)
                    .filter(|i| matches!(i, SIRInstruction::Store(..)))
                    .count();
                println!(
                    "{name},{elements},{four_state},{phase},{},{loops},{parameters},{nodes},{folds},{groups},{stores},{eligible},{recovered},{compile_ms:.3},{region_units},{regions},{region_stores},{region_ms:.3}",
                    units.len()
                );
            }
        }
    }
}

fn units(
    program: &celox_sir::SirProgram<StateAddr, RegionedStateAddr>,
) -> Vec<&ExecutionUnit<RegionedStateAddr>> {
    program
        .eval_comb
        .iter()
        .chain(program.eval_apply_ffs.values().flatten())
        .chain(program.eval_comb_apply_ffs.values().flatten())
        .chain(program.eval_only_ffs.values().flatten())
        .chain(program.apply_ffs.values().flatten())
        .collect()
}

#[cfg(all(feature = "host-runtime", target_arch = "x86_64"))]
fn bench(n: usize, samples: usize, dump: Option<&str>) {
    use affine_support::{WORK, native::Executable};
    use celox_analysis::polyhedral::{Tile, schedule};
    use celox_sir::affine::recover_independent_stores;
    use celox_state_layout::{MemoryLayout, MemoryLayoutMode};
    use std::time::Instant;

    assert!(samples >= 3);
    let mut diagnostics = std::io::stderr().lock();
    println!(
        "case,n,four_state,variant,compile_ms,recover_ms,schedule_ms,lower_ms,baseline_backend_ms,candidate_backend_ms,baseline_bytes,candidate_bytes,repeats,baseline_us,candidate_us,speedup,baseline_p25_us,baseline_p75_us,candidate_p25_us,candidate_p75_us"
    );
    for (name, code) in affine_veryl_support::cases(n).into_iter().take(2) {
        for four_state in [false, true] {
            let (program, trace, compile_ms) =
                affine_veryl_support::compile_mode(&code, four_state);
            assert!(
                program.layout_requirements.is_empty(),
                "rescheduling requires independent storage"
            );
            let objects = affine_veryl_support::objects(&program);
            let pre = &trace.pre_optimized_sir.as_ref().unwrap().sir;
            assert_eq!(pre.eval_comb.len(), 1);
            assert_eq!(program.sir.eval_comb.len(), 1);
            let start = Instant::now();
            let kernel =
                recover_independent_stores(&pre.eval_comb[0], &objects, &Default::default())
                    .unwrap();
            let recover_ms = start.elapsed().as_secs_f64() * 1e3;
            let start = Instant::now();
            let chosen = schedule(kernel.region(), &Default::default()).unwrap();
            let schedule_ms = start.elapsed().as_secs_f64() * 1e3;
            writeln!(
                diagnostics,
                "{name} four_state={four_state}: region={:?}, rows={:?}, bands={:?}",
                kernel.region(),
                chosen.rows,
                chosen.bands
            )
            .unwrap();
            let layout = MemoryLayout::build(&program, four_state, MemoryLayoutMode::Packed);
            let start = Instant::now();
            let mut baseline = Executable::new(&program.sir.eval_comb[0], &layout);
            let baseline_backend_ms = start.elapsed().as_secs_f64() * 1e3;
            if let Some(directory) = dump {
                std::fs::create_dir_all(directory).unwrap();
                std::fs::write(
                    Path::new(directory).join(format!("{name}_{four_state}_baseline.bin")),
                    baseline.code.image(),
                )
                .unwrap();
            }
            for (variant, tile) in [
                ("scheduled", None),
                (
                    "tile_64",
                    Some(Tile {
                        band: 0,
                        sizes: vec![64],
                    }),
                ),
            ] {
                let start = Instant::now();
                let candidate_unit = kernel.lower(&chosen, tile.as_ref(), WORK).unwrap();
                let lower_ms = start.elapsed().as_secs_f64() * 1e3;
                let start = Instant::now();
                let mut candidate = Executable::new(&candidate_unit, &layout);
                let candidate_backend_ms = start.elapsed().as_secs_f64() * 1e3;
                if let Some(directory) = dump {
                    let path = Path::new(directory);
                    std::fs::write(
                        path.join(format!("{name}_{four_state}_{variant}.bin")),
                        candidate.code.image(),
                    )
                    .unwrap();
                    std::fs::write(
                        path.join(format!("{name}_{four_state}_{variant}.sir.txt")),
                        candidate_unit.to_string(),
                    )
                    .unwrap();
                }
                let words = layout.total_size.div_ceil(8);
                let initialize = |executable: &mut Executable, seed: u64| {
                    let mut random = seed;
                    // Leave the runtime header zero. Compare all stable-state
                    // bytes, including internal arrays, after each batch.
                    for word in &mut executable.state[4..words] {
                        random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
                        *word = random;
                    }
                };
                let repeats = (2_000_000 / n).clamp(32, 100_000);
                let measure = |executable: &mut Executable| {
                    let start = Instant::now();
                    for _ in 0..repeats {
                        executable.run();
                    }
                    std::hint::black_box(&executable.state);
                    start.elapsed().as_secs_f64() * 1e6 / repeats as f64
                };
                initialize(&mut baseline, 42);
                initialize(&mut candidate, 42);
                baseline.run();
                candidate.run();
                assert_eq!(&baseline.state[..words], &candidate.state[..words]);
                // Warm both instruction streams before collecting samples.
                measure(&mut baseline);
                measure(&mut candidate);
                let mut base_times = Vec::new();
                let mut candidate_times = Vec::new();
                for sample in 0..samples {
                    initialize(&mut baseline, 42 + sample as u64);
                    initialize(&mut candidate, 42 + sample as u64);
                    if sample % 2 == 0 {
                        base_times.push(measure(&mut baseline));
                        candidate_times.push(measure(&mut candidate));
                    } else {
                        candidate_times.push(measure(&mut candidate));
                        base_times.push(measure(&mut baseline));
                    }
                    assert_eq!(&baseline.state[..words], &candidate.state[..words]);
                }
                writeln!(diagnostics, "{name} {variant} four_state={four_state}: baseline_us={base_times:?}, candidate_us={candidate_times:?}").unwrap();
                base_times.sort_by(f64::total_cmp);
                candidate_times.sort_by(f64::total_cmp);
                let base = base_times[samples / 2];
                let candidate_time = candidate_times[samples / 2];
                println!(
                    "{name},{n},{four_state},{variant},{compile_ms:.3},{recover_ms:.3},{schedule_ms:.3},{lower_ms:.3},{baseline_backend_ms:.3},{candidate_backend_ms:.3},{},{},{repeats},{base:.4},{candidate_time:.4},{:.3},{:.4},{:.4},{:.4},{:.4}",
                    baseline.code_bytes,
                    candidate.code_bytes,
                    base / candidate_time,
                    base_times[samples / 4],
                    base_times[3 * samples / 4],
                    candidate_times[samples / 4],
                    candidate_times[3 * samples / 4]
                );
            }
        }
    }
}

#[cfg(not(all(feature = "host-runtime", target_arch = "x86_64")))]
fn bench(_: usize, _: usize, _: Option<&str>) {
    panic!("Native timings require host-runtime on x86_64; audit is portable.");
}
