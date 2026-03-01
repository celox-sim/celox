window.BENCHMARK_DATA = {
  "lastUpdate": 1772363809704,
  "repoUrl": "https://github.com/celox-sim/celox",
  "entries": {
    "Rust Benchmarks": [
      {
        "commit": {
          "author": {
            "email": "tignear+m@gmail.com",
            "name": "tignear",
            "username": "tignear"
          },
          "committer": {
            "email": "tignear+m@gmail.com",
            "name": "tignear",
            "username": "tignear"
          },
          "distinct": true,
          "id": "fecaa2a5d89d8050fc445c07bada037b3d2d7c27",
          "message": "Introduce BuildConfig to resolve generic Reset/Clock types from veryl.toml\n\nGeneric TypeKind::Reset and TypeKind::Clock were hardcoded in the parser\ninstead of respecting veryl.toml settings. This adds a BuildConfig struct\nthat extracts clock_type and reset_type from Metadata and threads it\nthrough the parser pipeline so generic types resolve correctly.\n\nCo-Authored-By: Claude Opus 4.6 <noreply@anthropic.com>",
          "timestamp": "2026-03-01T08:42:09Z",
          "tree_id": "331b6c74c691aa1a9d40c9260401ba6d03631eb2",
          "url": "https://github.com/celox-sim/celox/commit/fecaa2a5d89d8050fc445c07bada037b3d2d7c27"
        },
        "date": 1772355882039,
        "tool": "cargo",
        "benches": [
          {
            "name": "simulator_tick_x10000",
            "value": 1592260,
            "range": "± 44808",
            "unit": "ns/iter"
          },
          {
            "name": "simulation_step_x20000",
            "value": 10111123,
            "range": "± 412582",
            "unit": "ns/iter"
          },
          {
            "name": "simulation_build_top_n1000",
            "value": 761915390,
            "range": "± 6861285",
            "unit": "ns/iter"
          },
          {
            "name": "simulation_tick_top_n1000_x1",
            "value": 158,
            "range": "± 0",
            "unit": "ns/iter"
          },
          {
            "name": "simulation_tick_top_n1000_x1000000",
            "value": 159109150,
            "range": "± 573740",
            "unit": "ns/iter"
          },
          {
            "name": "testbench_tick_top_n1000_x1",
            "value": 303,
            "range": "± 4",
            "unit": "ns/iter"
          },
          {
            "name": "testbench_tick_top_n1000_x1000000",
            "value": 324513887,
            "range": "± 565658",
            "unit": "ns/iter"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "tignear+m@gmail.com",
            "name": "tignear",
            "username": "tignear"
          },
          "committer": {
            "email": "tignear+m@gmail.com",
            "name": "tignear",
            "username": "tignear"
          },
          "distinct": true,
          "id": "2f714fc8331e5faa848a2137aa4b9430a1d5a808",
          "message": "Unify bench units to µs and add interactive VitePress dashboard\n\n- Add scripts/convert-rust-bench.mjs to convert Criterion ns/iter → µs\n- Update scripts/convert-bench.mjs to output µs instead of ms\n- Update CI workflow to use customSmallerIsBetter for both Rust and TS\n- Add Chart.js-based BenchmarkDashboard Vue component with category tabs,\n  Rust vs TS overlay charts, and adaptive unit formatting\n- Embed dashboard in EN/JA benchmark pages via ClientOnly\n- Fix sidebar link /guide/benchmarks → /benchmarks/\n- Add chart.js and vue-chartjs devDependencies\n\nCo-Authored-By: Claude Opus 4.6 <noreply@anthropic.com>",
          "timestamp": "2026-03-01T11:02:48Z",
          "tree_id": "acb337afe8aade2d12c7b5612e41ece2e4e21ac9",
          "url": "https://github.com/celox-sim/celox/commit/2f714fc8331e5faa848a2137aa4b9430a1d5a808"
        },
        "date": 1772363808724,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "rust/simulator_tick_x10000",
            "value": 1594.939,
            "range": "± 28.356 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_step_x20000",
            "value": 9693.058,
            "range": "± 77.971 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_top_n1000",
            "value": 738343.083,
            "range": "± 3978.965 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1",
            "value": 0.158,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1000000",
            "value": 159092.331,
            "range": "± 192.138 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1",
            "value": 0.314,
            "range": "± 0.010 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1000000",
            "value": 322416.066,
            "range": "± 411.641 us",
            "unit": "us"
          }
        ]
      }
    ],
    "TypeScript Benchmarks": [
      {
        "commit": {
          "author": {
            "email": "tignear+m@gmail.com",
            "name": "tignear",
            "username": "tignear"
          },
          "committer": {
            "email": "tignear+m@gmail.com",
            "name": "tignear",
            "username": "tignear"
          },
          "distinct": true,
          "id": "fecaa2a5d89d8050fc445c07bada037b3d2d7c27",
          "message": "Introduce BuildConfig to resolve generic Reset/Clock types from veryl.toml\n\nGeneric TypeKind::Reset and TypeKind::Clock were hardcoded in the parser\ninstead of respecting veryl.toml settings. This adds a BuildConfig struct\nthat extracts clock_type and reset_type from Metadata and threads it\nthrough the parser pipeline so generic types resolve correctly.\n\nCo-Authored-By: Claude Opus 4.6 <noreply@anthropic.com>",
          "timestamp": "2026-03-01T08:42:09Z",
          "tree_id": "331b6c74c691aa1a9d40c9260401ba6d03631eb2",
          "url": "https://github.com/celox-sim/celox/commit/fecaa2a5d89d8050fc445c07bada037b3d2d7c27"
        },
        "date": 1772355883604,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ts/simulation_build_top_n1000",
            "value": 790.075193,
            "range": "± 1.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1",
            "value": 0.0005170599313748532,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "967006 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1000000",
            "value": 373.1711613333343,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1",
            "value": 0.0007207447281128125,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "693727 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1000000",
            "value": 604.748067666665,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1",
            "value": 0.0007010101295878304,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "713257 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1000000",
            "value": 592.9274106666671,
            "range": "± 2.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulator_tick_x10000",
            "value": 3.7948846666671066,
            "range": "± 1.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_step_x20000",
            "value": 13.531399000001935,
            "range": "± 1.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_build_top_n1000",
            "value": 793.1613996666662,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_step_x1",
            "value": 0.0008318319849343213,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "601083 samples"
          },
          {
            "name": "ts/simulation_time_step_x1000000",
            "value": 676.8198526666674,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_runUntil_1000000",
            "value": 102.29673366666248,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/waitForCycles_x1000",
            "value": 0.7403969999965435,
            "range": "± 8.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/manual_step_loop_x2000",
            "value": 0.8023923333360775,
            "range": "± 3.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_fast_path_100000",
            "value": 4.238777666665555,
            "range": "± 2.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_guarded_100000",
            "value": 9.089089666665435,
            "range": "± 3.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_without_optimize",
            "value": 805.3649146666672,
            "range": "± 1.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_with_optimize",
            "value": 807.4631313333302,
            "range": "± 1.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_without_optimize",
            "value": 3.728383333334932,
            "range": "± 1.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_with_optimize",
            "value": 3.6633920000022044,
            "range": "± 0.4%",
            "unit": "ms",
            "extra": "3 samples"
          }
        ]
      }
    ]
  }
}