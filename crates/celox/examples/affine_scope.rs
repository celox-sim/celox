//! Broader Veryl coverage with two fixed candidates, independent of hill climbing.
//! `affine_scope [elements=255] [case=all]`

#[cfg(all(feature = "host-runtime", target_arch = "x86_64"))]
#[allow(dead_code)]
mod affine_support;
#[cfg(all(feature = "host-runtime", target_arch = "x86_64"))]
#[allow(dead_code)]
mod affine_veryl_support;

#[cfg(all(feature = "host-runtime", target_arch = "x86_64"))]
fn main() {
    use affine_support::{WORK, native::Executable};
    use celox_analysis::polyhedral::{schedule, tuning::faster_probability};
    use celox_sir::affine::{CodegenOptions, recover_independent_stores};
    use celox_state_layout::{MemoryLayout, MemoryLayoutMode};
    use std::{io::Write, time::Instant};

    fn measure(unit: &mut Executable, repeats: usize) -> u64 {
        let start = Instant::now();
        for _ in 0..repeats {
            unit.run();
        }
        std::hint::black_box(&unit.state);
        u64::try_from(start.elapsed().as_nanos()).unwrap()
    }
    fn median(samples: &[u64]) -> u64 {
        let mut values = samples.to_vec();
        values.sort_unstable();
        values[values.len() / 2]
    }

    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let n = args
        .first()
        .map(|s| s.parse::<usize>().unwrap())
        .unwrap_or(255);
    assert!(n >= 3);
    let filter = args.get(1).map(String::as_str).unwrap_or("all");
    let mut log = std::io::stderr().lock();
    println!(
        "case,n,four_state,masks,statements,unroll,compile_ms,recover_schedule_ms,baseline_backend_ms,candidate_compile_ms,decision_ms,baseline_bytes,candidate_bytes,repeats,baseline_us,candidate_us,speedup,p_numerator,p_denominator,selected"
    );
    for (name, code) in affine_veryl_support::scope_cases(n) {
        if filter != "all" && name != filter {
            continue;
        }
        for (four_state, unknown) in [(false, false), (true, false), (true, true)] {
            let masks = if unknown { "mixed" } else { "known" };
            let (program, trace, compile_ms) =
                affine_veryl_support::compile_mode(&code, four_state);
            if !program.layout_requirements.is_empty() || program.sir.eval_comb.len() != 1 {
                writeln!(
                    log,
                    "{name} four_state={four_state} masks={masks}: skipped layout/units"
                )
                .unwrap();
                continue;
            }
            let objects = affine_veryl_support::objects(&program);
            let start = Instant::now();
            let kernel = match recover_independent_stores(
                &trace.pre_optimized_sir.as_ref().unwrap().sir.eval_comb[0],
                &objects,
                &Default::default(),
            ) {
                Ok(kernel) => kernel,
                Err(error) => {
                    writeln!(
                        log,
                        "{name} four_state={four_state} masks={masks}: skipped {error}"
                    )
                    .unwrap();
                    continue;
                }
            };
            let chosen = match schedule(kernel.region(), &Default::default()) {
                Ok(chosen) => chosen,
                Err(error) => {
                    writeln!(
                        log,
                        "{name} four_state={four_state} masks={masks}: skipped schedule {error}"
                    )
                    .unwrap();
                    continue;
                }
            };
            let recover_schedule_ms = start.elapsed().as_secs_f64() * 1e3;
            let layout = MemoryLayout::build(&program, four_state, MemoryLayoutMode::Packed);
            let start = Instant::now();
            let mut baseline = Executable::new(&program.sir.eval_comb[0], &layout);
            let baseline_backend_ms = start.elapsed().as_secs_f64() * 1e3;
            let words = layout.total_size.div_ceil(8);
            let initialize = |unit: &mut Executable, seed: u64| {
                let mut random = seed;
                for word in &mut unit.state[4..words] {
                    random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
                    *word = random;
                }
                if four_state && !unknown {
                    for (address, &base) in &layout.offsets {
                        let bytes = layout.widths[address].div_ceil(8);
                        for byte in base + bytes..base + 2 * bytes {
                            unit.state[byte / 8] &= !(0xffu64 << (8 * (byte % 8)));
                        }
                    }
                }
            };
            // The decision includes compilation, calibration, correctness
            // comparisons, training and fresh validation, plus analysis above.
            let decision = Instant::now();
            let start = Instant::now();
            let mut candidates = Vec::new();
            for unroll in [16, 32] {
                let options = CodegenOptions {
                    unroll,
                    max_instructions: 16_384,
                };
                match kernel.lower_with_options(&chosen, None, &options, WORK) {
                    Ok(unit) => {
                        let executable = Executable::new(&unit, &layout);
                        if executable.code_bytes <= 65_536 {
                            candidates.push((unroll, executable));
                        } else {
                            writeln!(log, "{name} unroll={unroll}: rejected code size").unwrap();
                        }
                    }
                    Err(error) => {
                        writeln!(log, "{name} unroll={unroll}: rejected {error}").unwrap()
                    }
                }
            }
            let candidate_compile_ms = start.elapsed().as_secs_f64() * 1e3;
            if candidates.is_empty() {
                continue;
            }

            initialize(&mut baseline, 41);
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
                initialize(candidate, 41);
                candidate.run();
                assert_eq!(&candidate.state[..words], &baseline.state[..words]);
                measure(candidate, repeats);
            }
            let mut train = vec![Vec::new(); candidates.len()];
            for sample in 0..3 {
                initialize(&mut baseline, 42 + sample as u64);
                baseline.run();
                for visit in 0..candidates.len() {
                    let index = (sample + visit) % candidates.len();
                    let candidate = &mut candidates[index].1;
                    initialize(candidate, 42 + sample as u64);
                    candidate.run();
                    assert_eq!(&candidate.state[..words], &baseline.state[..words]);
                    train[index].push(measure(candidate, repeats));
                    assert_eq!(&candidate.state[..words], &baseline.state[..words]);
                }
            }
            writeln!(log, "{name} four_state={four_state} masks={masks}: repeats={repeats}, candidates={:?}, training_batch_ns={train:?}", candidates.iter().map(|(u, e)| (*u, e.code_bytes)).collect::<Vec<_>>()).unwrap();
            let best = (0..candidates.len())
                .min_by_key(|&i| (median(&train[i]), candidates[i].0))
                .unwrap();
            let (unroll, mut candidate) = candidates.swap_remove(best);
            measure(&mut baseline, repeats);
            measure(&mut candidate, repeats);
            let mut base_samples = Vec::new();
            let mut candidate_samples = Vec::new();
            for sample in 0..15 {
                initialize(&mut baseline, 1000 + sample);
                initialize(&mut candidate, 1000 + sample);
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
            let decision_ms = recover_schedule_ms + decision.elapsed().as_secs_f64() * 1e3;
            writeln!(log, "{name} four_state={four_state} masks={masks}: rows={:?}, validation_batch_ns baseline={base_samples:?}, candidate={candidate_samples:?}", chosen.rows).unwrap();
            println!(
                "{name},{n},{four_state},{masks},{},{unroll},{compile_ms:.3},{recover_schedule_ms:.3},{baseline_backend_ms:.3},{candidate_compile_ms:.3},{decision_ms:.3},{},{},{repeats},{:.4},{:.4},{:.3},{},{},{selected}",
                kernel.region().statements.len(),
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
