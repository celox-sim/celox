//! Two-candidate experiment on the existing 1000-counter clock execution unit.
//! `affine_regions [readers|all]` prints six decisions (two paths, three state modes).
//! Each decision is conditional on its stated path; it does not install a policy.

#[cfg(all(feature = "host-runtime", target_arch = "x86_64"))]
#[allow(dead_code)]
mod affine_region_support;
#[cfg(all(feature = "host-runtime", target_arch = "x86_64"))]
#[allow(dead_code)]
mod affine_support;
#[cfg(all(feature = "host-runtime", target_arch = "x86_64"))]
#[allow(dead_code)]
mod affine_veryl_support;

#[cfg(all(feature = "host-runtime", target_arch = "x86_64"))]
fn main() {
    use affine_region_support::{Machine, native};
    use affine_support::native::Executable;
    use celox_analysis::polyhedral::{AccessKind, tuning::faster_probability};
    use celox_design::RegionedStateAddr;
    use celox_sir::{
        SIRValue,
        affine::{CodegenOptions, recover_independent_regions},
    };
    use celox_state_layout::{MemoryLayout, MemoryLayoutMode};
    use std::{io::Write, time::Instant};

    fn measure(unit: &mut Executable, repeats: usize) -> u64 {
        let start = Instant::now();
        for _ in 0..repeats {
            unit.run();
        }
        std::hint::black_box(&unit.state);
        start.elapsed().as_nanos().try_into().unwrap()
    }
    fn median(samples: &[u64]) -> u64 {
        let mut samples = samples.to_vec();
        samples.sort_unstable();
        samples[samples.len() / 2]
    }

    let policy = std::env::args().nth(1).unwrap_or_else(|| "readers".into());
    assert!(["all", "readers"].contains(&policy.as_str()));
    println!(
        "case,n,four_state,masks,path,policy,regions,lowered_regions,unroll,frontend_ms,recover_ms,baseline_backend_ms,candidate_compile_ms,decision_ms,baseline_bytes,candidate_bytes,repeats,baseline_us,candidate_us,speedup,p_numerator,p_denominator,selected"
    );
    let mut log = std::io::stderr().lock();
    for (four_state, unknown) in [(false, false), (true, false), (true, true)] {
        let masks = if unknown { "mixed" } else { "known" };
        let (program, trace, frontend_ms) = affine_veryl_support::compile_top(
            include_str!("../testdata/veryl/top_n1000.veryl"),
            "Top",
            four_state,
        );
        let clk = program.get_addr(&[], &["clk"]).unwrap();
        let reset =
            RegionedStateAddr::from_absolute_addr(0, program.get_addr(&[], &["rst"]).unwrap());
        let pre = &trace.pre_optimized_sir.as_ref().unwrap().sir.eval_apply_ffs[&clk];
        assert_eq!(pre.len(), 1);
        assert_eq!(program.sir.eval_apply_ffs[&clk].len(), 1);
        let pre = &pre[0];
        let layout = MemoryLayout::build(&program, four_state, MemoryLayoutMode::Packed);
        let objects = native::unit_objects(pre, &affine_veryl_support::objects(&program), &layout);
        let words = layout.total_size.div_ceil(8);
        for (path, rst) in [("reset", 0u32), ("increment", 1u32)] {
            let initial = |seed| {
                let mut state = Machine::new(&objects, seed, unknown);
                state.values.insert(reset, SIRValue::new(rst));
                state
            };
            let start = Instant::now();
            let mut baseline = Executable::new(&program.sir.eval_apply_ffs[&clk][0], &layout);
            let baseline_backend_ms = start.elapsed().as_secs_f64() * 1e3;
            let decision = Instant::now();
            let start = Instant::now();
            let recovered =
                recover_independent_regions(pre, &objects, &Default::default()).unwrap();
            assert_eq!(recovered.len(), 2);
            // Leave constant fills to the ordinary optimizer in the readers
            // experiment. This is a policy, not a general profitability model.
            let selected_regions = recovered
                .kernels()
                .enumerate()
                .filter_map(|(i, kernel)| {
                    (policy == "all"
                        || kernel
                            .region()
                            .statements
                            .iter()
                            .flat_map(|s| &s.accesses)
                            .any(|a| a.kind == AccessKind::Read))
                    .then_some(i)
                })
                .collect::<Vec<_>>();
            let recover_ms = start.elapsed().as_secs_f64() * 1e3;
            let start = Instant::now();
            let mut candidates = Vec::new();
            for unroll in [16, 32] {
                let unit = recovered
                    .lower_selected_with_options(
                        &selected_regions,
                        &Default::default(),
                        &CodegenOptions {
                            unroll,
                            max_instructions: 16_384,
                        },
                    )
                    .unwrap();
                let unit = affine_region_support::optimize_clock_candidate(
                    trace.pre_optimized_sir.as_ref().unwrap(),
                    &program,
                    clk,
                    unit,
                    four_state,
                );
                native::unit_objects(&unit, &objects, &layout);
                let native = Executable::new(&unit, &layout);
                assert!(native.code_bytes <= 65_536);
                candidates.push((unroll, native));
            }
            let candidate_compile_ms = start.elapsed().as_secs_f64() * 1e3;
            // One complete clock unit, including both Commit operations, is
            // checked against the interpreter before any native timings.
            let input = initial(41);
            let mut expected = input.clone();
            celox::execute_unit(pre, &mut expected, &[], four_state).unwrap();
            native::initialize(&mut baseline, &layout, &input);
            baseline.run();
            native::assert_stable(&baseline, &layout, &expected);
            for (_, candidate) in &mut candidates {
                native::initialize(candidate, &layout, &input);
                candidate.run();
                native::assert_stable(candidate, &layout, &expected);
                assert_eq!(&candidate.state[..words], &baseline.state[..words]);
            }

            let mut repeats = 16;
            loop {
                let elapsed = measure(&mut baseline, repeats);
                if elapsed >= 250_000 || repeats == 65_536 {
                    repeats = ((250_000u128 * repeats as u128).div_ceil(u128::from(elapsed.max(1)))
                        as usize)
                        .clamp(8, 65_536);
                    break;
                }
                repeats = (repeats * 4).min(65_536);
            }
            for (_, candidate) in &mut candidates {
                measure(candidate, repeats);
            }
            let mut training = vec![Vec::new(); candidates.len()];
            for sample in 0..3 {
                let input = initial(42 + sample as u64);
                native::initialize(&mut baseline, &layout, &input);
                measure(&mut baseline, repeats);
                for visit in 0..candidates.len() {
                    let index = (sample + visit) % candidates.len();
                    let candidate = &mut candidates[index].1;
                    native::initialize(candidate, &layout, &input);
                    training[index].push(measure(candidate, repeats));
                    assert_eq!(&candidate.state[..words], &baseline.state[..words]);
                }
            }
            writeln!(log, "four_state={four_state} masks={masks} path={path}: repeats={repeats}, candidates={:?}, training_batch_ns={training:?}", candidates.iter().map(|(u, c)| (*u, c.code_bytes)).collect::<Vec<_>>()).unwrap();
            let best = (0..candidates.len())
                .min_by_key(|&i| (median(&training[i]), candidates[i].0))
                .unwrap();
            let (unroll, mut candidate) = candidates.swap_remove(best);
            let mut base_samples = Vec::new();
            let mut candidate_samples = Vec::new();
            for sample in 0..15 {
                let input = initial(1000 + sample);
                native::initialize(&mut baseline, &layout, &input);
                native::initialize(&mut candidate, &layout, &input);
                if sample % 2 == 0 {
                    base_samples.push(measure(&mut baseline, repeats));
                    candidate_samples.push(measure(&mut candidate, repeats));
                } else {
                    candidate_samples.push(measure(&mut candidate, repeats));
                    base_samples.push(measure(&mut baseline, repeats));
                }
                assert_eq!(&candidate.state[..words], &baseline.state[..words]);
            }
            let probability = faster_probability(&candidate_samples, &base_samples).unwrap();
            let base_ns = median(&base_samples);
            let candidate_ns = median(&candidate_samples);
            let selected = if candidate_ns < base_ns
                && probability.numerator * 20 <= probability.denominator
            {
                "candidate"
            } else {
                "baseline"
            };
            let decision_ms = decision.elapsed().as_secs_f64() * 1e3;
            writeln!(log, "four_state={four_state} masks={masks} path={path}: validation_batch_ns baseline={base_samples:?}, candidate={candidate_samples:?}").unwrap();
            println!(
                "counters_1000,1000,{four_state},{masks},{path},{policy},{},{},{unroll},{frontend_ms:.3},{recover_ms:.3},{baseline_backend_ms:.3},{candidate_compile_ms:.3},{decision_ms:.3},{},{},{repeats},{:.4},{:.4},{:.3},{},{},{selected}",
                recovered.len(),
                selected_regions.len(),
                baseline.code_bytes,
                candidate.code_bytes,
                base_ns as f64 / repeats as f64 / 1000.0,
                candidate_ns as f64 / repeats as f64 / 1000.0,
                base_ns as f64 / candidate_ns as f64,
                probability.numerator,
                probability.denominator
            );
        }
    }
}

#[cfg(not(all(feature = "host-runtime", target_arch = "x86_64")))]
fn main() {
    eprintln!("This experiment requires host-runtime on x86_64.");
}
