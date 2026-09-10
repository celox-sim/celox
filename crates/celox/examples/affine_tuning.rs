//! Tune a fixed affine schedule on frontend-compiled Veryl microkernels.
//! Arguments: elements (512), validation samples (15), evaluation budget (48).

#[cfg(all(feature = "host-runtime", target_arch = "x86_64"))]
#[allow(dead_code)]
mod affine_support;
#[cfg(all(feature = "host-runtime", target_arch = "x86_64"))]
mod affine_veryl_support;

#[cfg(all(feature = "host-runtime", target_arch = "x86_64"))]
fn main() {
    use affine_support::{WORK, native::Executable};
    use celox_analysis::polyhedral::{
        Tile, schedule,
        tuning::{Parameter, TuningOptions, faster_probability, hill_climb},
    };
    use celox_sir::affine::{CodegenOptions, recover_independent_stores};
    use celox_state_layout::{MemoryLayout, MemoryLayoutMode};
    use std::{collections::BTreeMap, io::Write, time::Instant};

    let args = std::env::args()
        .skip(1)
        .map(|s| s.parse::<usize>().expect("integer argument"))
        .collect::<Vec<_>>();
    let n = args.first().copied().unwrap_or(512);
    let validation_samples = args.get(1).copied().unwrap_or(15);
    let budget = args.get(2).copied().unwrap_or(48);
    assert!(
        n >= 3
            && (3..=15).contains(&validation_samples)
            && !validation_samples.is_multiple_of(2)
            && budget > 0
    );
    let mut log = std::io::stderr().lock();
    println!(
        "case,n,four_state,masks,tile,unroll,compile_ms,recover_schedule_ms,baseline_backend_ms,search_ms,evaluations,rejected,steps,stop,baseline_bytes,tuned_bytes,repeats,baseline_us,tuned_us,speedup,baseline_p25_us,baseline_p75_us,tuned_p25_us,tuned_p75_us,p_numerator,p_denominator,selected,search_seed_us,search_best_us"
    );
    for (name, code) in affine_veryl_support::cases(n).into_iter().take(2) {
        for (four_state, unknown) in [(false, false), (true, false), (true, true)] {
            let masks = if unknown { "mixed" } else { "known" };
            let (program, trace, compile_ms) =
                affine_veryl_support::compile_mode(&code, four_state);
            assert!(program.layout_requirements.is_empty());
            assert_eq!(program.sir.eval_comb.len(), 1);
            let objects = affine_veryl_support::objects(&program);
            let start = Instant::now();
            let kernel = recover_independent_stores(
                &trace.pre_optimized_sir.as_ref().unwrap().sir.eval_comb[0],
                &objects,
                &Default::default(),
            )
            .unwrap();
            let chosen = schedule(kernel.region(), &Default::default()).unwrap();
            let recover_schedule_ms = start.elapsed().as_secs_f64() * 1e3;
            assert_eq!(chosen.bands[0].len(), 1);
            writeln!(
                log,
                "{name} four_state={four_state} masks={masks}: rows={:?}, bands={:?}",
                chosen.rows, chosen.bands
            )
            .unwrap();
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
            let repeats = (2_000_000 / n).clamp(32, 100_000);
            let measure = |unit: &mut Executable| {
                let start = Instant::now();
                for _ in 0..repeats {
                    unit.run();
                }
                std::hint::black_box(&unit.state);
                u64::try_from(start.elapsed().as_nanos()).unwrap()
            };
            let mut tiles = [16, 32, 64, 128, 256, 512, n as u64]
                .into_iter()
                .filter(|&v| v <= n as u64)
                .collect::<Vec<_>>();
            tiles.sort_unstable();
            tiles.dedup();
            let space = [
                Parameter { values: tiles },
                Parameter {
                    values: vec![1, 2, 4, 8, 16, 32],
                },
            ];
            // A full-extent tile preserves the previously measured untiled
            // starting shape. All search points use this same tiling structure.
            let seed = [n as u64, 1];
            let options = TuningOptions {
                max_evaluations: budget,
                ..Default::default()
            };
            let mut executables = BTreeMap::new();
            let start = Instant::now();
            let result = hill_climb(&space, &seed, &options, |point| {
                let start = Instant::now();
                let tile = Tile {
                    band: 0,
                    sizes: vec![point[0] as i64],
                };
                let codegen = CodegenOptions {
                    unroll: point[1] as usize,
                    max_instructions: 16_384,
                };
                let unit = match kernel.lower_with_options(&chosen, Some(&tile), &codegen, WORK) {
                    Ok(unit) => unit,
                    Err(error) => {
                        writeln!(log, "{name} four_state={four_state} masks={masks} point={point:?}: rejected={error}").unwrap();
                        return None;
                    }
                };
                let lower_ms = start.elapsed().as_secs_f64() * 1e3;
                let start = Instant::now();
                let mut executable = Executable::new(&unit, &layout);
                let backend_ms = start.elapsed().as_secs_f64() * 1e3;
                if executable.code_bytes > 65_536 {
                    writeln!(log, "{name} four_state={four_state} masks={masks} point={point:?}: code budget exceeded, bytes={}", executable.code_bytes).unwrap();
                    return None;
                }
                let mut samples = Vec::new();
                for sample in 0..options.samples {
                    initialize(&mut baseline, 42 + sample as u64);
                    initialize(&mut executable, 42 + sample as u64);
                    baseline.run();
                    executable.run();
                    assert_eq!(
                        &baseline.state[..words],
                        &executable.state[..words],
                        "{name} {point:?}"
                    );
                    // Warm each generated instruction stream before timing.
                    measure(&mut executable);
                    samples.push(measure(&mut executable));
                    assert_eq!(&baseline.state[..words], &executable.state[..words]);
                }
                writeln!(log, "{name} four_state={four_state} masks={masks} point={point:?}: lower_ms={lower_ms:.3}, backend_ms={backend_ms:.3}, bytes={}, samples_batch_ns={samples:?}", executable.code_bytes).unwrap();
                executables.insert(point.to_vec(), executable);
                Some(samples)
            })
            .unwrap();
            let search_ms = start.elapsed().as_secs_f64() * 1e3;
            writeln!(
                log,
                "{name} four_state={four_state} masks={masks}: search={result:?}"
            )
            .unwrap();
            let mut tuned = executables.remove(&result.best().point).unwrap();
            let mut base_samples = Vec::new();
            let mut tuned_samples = Vec::new();
            initialize(&mut baseline, 999);
            initialize(&mut tuned, 999);
            measure(&mut baseline);
            measure(&mut tuned);
            // Fresh seeds and alternating order; search samples do not enter
            // this acceptance comparison against the original optimized unit.
            for sample in 0..validation_samples {
                initialize(&mut baseline, 1000 + sample as u64);
                initialize(&mut tuned, 1000 + sample as u64);
                if sample % 2 == 0 {
                    base_samples.push(measure(&mut baseline));
                    tuned_samples.push(measure(&mut tuned));
                } else {
                    tuned_samples.push(measure(&mut tuned));
                    base_samples.push(measure(&mut baseline));
                }
                assert_eq!(&baseline.state[..words], &tuned.state[..words]);
            }
            writeln!(log, "{name} four_state={four_state} masks={masks}: validation_batch_ns baseline={base_samples:?}, tuned={tuned_samples:?}").unwrap();
            let probability = faster_probability(&tuned_samples, &base_samples).unwrap();
            base_samples.sort_unstable();
            tuned_samples.sort_unstable();
            let mid = validation_samples / 2;
            let selected = if tuned_samples[mid] < base_samples[mid]
                && u128::from(probability.numerator) * u128::from(options.alpha.denominator)
                    <= u128::from(options.alpha.numerator) * u128::from(probability.denominator)
            {
                "tuned"
            } else {
                "baseline"
            };
            let us = |ns: u64| ns as f64 / repeats as f64 / 1000.0;
            let search_median = |samples: &[u64]| {
                let mut s = samples.to_vec();
                s.sort_unstable();
                us(s[s.len() / 2])
            };
            println!(
                "{name},{n},{four_state},{masks},{},{},{compile_ms:.3},{recover_schedule_ms:.3},{baseline_backend_ms:.3},{search_ms:.3},{},{},{},{:?},{},{},{repeats},{:.4},{:.4},{:.3},{:.4},{:.4},{:.4},{:.4},{},{},{selected},{:.4},{:.4}",
                result.best().point[0],
                result.best().point[1],
                result.evaluations,
                result.rejected,
                result.trajectory.len() - 1,
                result.stop,
                baseline.code_bytes,
                tuned.code_bytes,
                us(base_samples[mid]),
                us(tuned_samples[mid]),
                base_samples[mid] as f64 / tuned_samples[mid] as f64,
                us(base_samples[validation_samples / 4]),
                us(base_samples[3 * validation_samples / 4]),
                us(tuned_samples[validation_samples / 4]),
                us(tuned_samples[3 * validation_samples / 4]),
                probability.numerator,
                probability.denominator,
                search_median(&result.trajectory[0].samples),
                search_median(&result.best().samples)
            );
        }
    }
}

#[cfg(not(all(feature = "host-runtime", target_arch = "x86_64")))]
fn main() {
    eprintln!("This experiment requires host-runtime on x86_64.");
}
