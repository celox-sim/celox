//! Run with --profile heliodor-dev; arguments: elements, time steps, samples.
//! No optimizer defaults or persistent state are changed by this experiment.

use std::io::Write;

#[cfg(all(feature = "host-runtime", target_arch = "x86_64"))]
mod affine_support;

#[cfg(all(feature = "host-runtime", target_arch = "x86_64"))]
fn main() {
    use affine_support::{Fixture, Kind, WORK, native::Executable};
    use celox_analysis::polyhedral::{ScheduleOptions, Tile, schedule};
    use std::time::Instant;

    let args = std::env::args()
        .skip(1)
        .map(|s| s.parse::<usize>().expect("positive integer argument"))
        .collect::<Vec<_>>();
    let n = args.first().copied().unwrap_or(262_144);
    let steps = args.get(1).copied().unwrap_or(16);
    let samples = args.get(2).copied().unwrap_or(9);
    assert!(n >= 3 && steps > 0 && samples >= 3);
    println!(
        "kernel,n,steps,variant,extract_ms,schedule_ms,lower_ms,backend_ms,code_bytes,baseline_us,variant_us,speedup,baseline_p25_us,baseline_p75_us,variant_p25_us,variant_p75_us"
    );
    let mut diagnostics = std::io::stderr().lock();
    for kind in [Kind::Shift, Kind::Jacobi] {
        // Exercise the SLT boundary independently before using CFG extraction
        // for timing. The integration test also checks 4-state semantics.
        let small = Fixture::new(kind, 19, 3);
        let layout = small.layout(false);
        let initial = small.initial(false, 42);
        let kernel = small.slt_kernel(false);
        let slt_schedule = schedule(kernel.region(), &ScheduleOptions::default()).unwrap();
        let mut baseline = Executable::new(&small.original(false), &layout);
        let mut transformed =
            Executable::new(&kernel.lower(&slt_schedule, None, WORK).unwrap(), &layout);
        baseline.initialize(&layout, &initial);
        transformed.initialize(&layout, &initial);
        baseline.run();
        transformed.run();
        assert_eq!(
            baseline.cells(&layout, small.objects.len(), small.n),
            transformed.cells(&layout, small.objects.len(), small.n)
        );

        let fixture = Fixture::new(kind, n, steps);
        let original = fixture.original(false);
        let start = Instant::now();
        let kernel = celox_sir::affine::extract(&original, &fixture.objects).unwrap();
        let extract_ms = start.elapsed().as_secs_f64() * 1e3;
        let start = Instant::now();
        let chosen = schedule(kernel.region(), &ScheduleOptions::default()).unwrap();
        let schedule_ms = start.elapsed().as_secs_f64() * 1e3;
        writeln!(
            diagnostics,
            "{kind:?}: rows={:?}, bands={:?}, stats={:?}",
            chosen.rows, chosen.bands, chosen.stats
        )
        .unwrap();
        let layout = fixture.layout(false);
        let start = Instant::now();
        let mut baseline = Executable::new(&original, &layout);
        let base_backend_ms = start.elapsed().as_secs_f64() * 1e3;
        writeln!(
            diagnostics,
            "{kind:?}: original backend_ms={base_backend_ms:.3}, code_bytes={}",
            baseline.code_bytes
        )
        .unwrap();
        let tiles = match kind {
            Kind::Shift => vec![
                ("scheduled", None),
                ("tile_256", Some(vec![256])),
                ("tile_4096", Some(vec![4096])),
            ],
            Kind::Jacobi => vec![
                ("scheduled", None),
                ("tile_4x256", Some(vec![4, 256])),
                ("tile_16x4096", Some(vec![16, 4096])),
                ("tile_32x8192", Some(vec![32, 8192])),
            ],
        };
        for (name, sizes) in tiles {
            let tile = sizes.map(|sizes| Tile { band: 0, sizes });
            let start = Instant::now();
            let unit = kernel.lower(&chosen, tile.as_ref(), WORK).unwrap();
            let lower_ms = start.elapsed().as_secs_f64() * 1e3;
            let start = Instant::now();
            let mut candidate = Executable::new(&unit, &layout);
            let backend_ms = start.elapsed().as_secs_f64() * 1e3;
            let initialize = |executable: &mut Executable| {
                let mut random = 42u64;
                for slot in &mut executable.state[..layout.total_size / 8] {
                    random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
                    *slot = random;
                }
            };
            initialize(&mut baseline);
            initialize(&mut candidate);
            baseline.run();
            candidate.run();
            assert_eq!(
                &baseline.state[..layout.total_size / 8],
                &candidate.state[..layout.total_size / 8],
                "{kind:?} {name}"
            );
            let mut base_times = Vec::new();
            let mut candidate_times = Vec::new();
            let measure = |executable: &mut Executable| {
                let start = Instant::now();
                executable.run();
                std::hint::black_box(&executable.state);
                start.elapsed().as_secs_f64() * 1e6
            };
            // Alternate ordering to reduce drift; initialization and full
            // state comparison are outside both execution timers.
            for sample in 0..samples {
                initialize(&mut baseline);
                initialize(&mut candidate);
                if sample % 2 == 0 {
                    base_times.push(measure(&mut baseline));
                    candidate_times.push(measure(&mut candidate));
                } else {
                    candidate_times.push(measure(&mut candidate));
                    base_times.push(measure(&mut baseline));
                }
                assert_eq!(
                    &baseline.state[..layout.total_size / 8],
                    &candidate.state[..layout.total_size / 8]
                );
            }
            writeln!(
                diagnostics,
                "{kind:?} {name}: samples_us baseline={base_times:?}, variant={candidate_times:?}"
            )
            .unwrap();
            base_times.sort_by(f64::total_cmp);
            candidate_times.sort_by(f64::total_cmp);
            let base_us = base_times[samples / 2];
            let candidate_us = candidate_times[samples / 2];
            println!(
                "{kind:?},{n},{steps},{name},{extract_ms:.3},{schedule_ms:.3},{lower_ms:.3},{backend_ms:.3},{},{base_us:.3},{candidate_us:.3},{:.3},{:.3},{:.3},{:.3},{:.3}",
                candidate.code_bytes,
                base_us / candidate_us,
                base_times[samples / 4],
                base_times[3 * samples / 4],
                candidate_times[samples / 4],
                candidate_times[3 * samples / 4]
            );
        }
    }
}

#[cfg(not(all(feature = "host-runtime", target_arch = "x86_64")))]
fn main() {
    writeln!(
        std::io::stderr().lock(),
        "This experiment requires host-runtime on x86_64."
    )
    .unwrap();
}
