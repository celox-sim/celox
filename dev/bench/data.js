window.BENCHMARK_DATA = {
  "lastUpdate": 1772398866357,
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
          "id": "36ba941c49341c77c2c4c029bb12fdf7054d3e0b",
          "message": "Parallelize benchmark jobs for faster CI\n\nSplit the single sequential bench job into three parallel jobs\n(bench-rust, bench-verilator, bench-ts) that upload artifacts,\nfollowed by a lightweight publish job that converts and pushes results.\n\nWall-clock time: Rust + Verilator + TS → max(Rust, Verilator, TS)\n\nCo-Authored-By: Claude Opus 4.6 <noreply@anthropic.com>",
          "timestamp": "2026-03-01T11:29:28Z",
          "tree_id": "47c2d5f2b1e88e911b4d2db6b3acb7179b02d1d7",
          "url": "https://github.com/celox-sim/celox/commit/36ba941c49341c77c2c4c029bb12fdf7054d3e0b"
        },
        "date": 1772365354115,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "rust/simulator_tick_x10000",
            "value": 1449.763,
            "range": "± 11.121 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_step_x20000",
            "value": 8869.602,
            "range": "± 38.359 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_top_n1000",
            "value": 703154.454,
            "range": "± 3995.526 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1",
            "value": 0.145,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1000000",
            "value": 144992.69,
            "range": "± 1371.726 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1",
            "value": 0.183,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1000000",
            "value": 199716.341,
            "range": "± 289.372 us",
            "unit": "us"
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
          "id": "8d149808a083ce434b92b708a2182431edebe778",
          "message": "Fix counter test reset polarity for async_low reset type\n\nThe project uses reset_type = \"async_low\", so reset is active-low.\nThe test was asserting rst=1 (inactive) and releasing with rst=0\n(active), which is backwards.\n\nCo-Authored-By: Claude Opus 4.6 <noreply@anthropic.com>",
          "timestamp": "2026-03-01T12:07:43Z",
          "tree_id": "3f83d5cd703b2e01f9fb60bab87492f1c372c323",
          "url": "https://github.com/celox-sim/celox/commit/8d149808a083ce434b92b708a2182431edebe778"
        },
        "date": 1772367418254,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "rust/simulator_tick_x10000",
            "value": 1592.917,
            "range": "± 32.833 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_step_x20000",
            "value": 9851.003,
            "range": "± 58.575 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_top_n1000",
            "value": 764589.111,
            "range": "± 7885.955 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1",
            "value": 0.158,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1000000",
            "value": 159334.907,
            "range": "± 632.294 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1",
            "value": 0.326,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1000000",
            "value": 333774.663,
            "range": "± 2692.970 us",
            "unit": "us"
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
          "id": "2649e277ae41c1812317db51d96e784fcca83282",
          "message": "Merge branch 'claude/fix-always-comb-eval'",
          "timestamp": "2026-03-01T20:19:29Z",
          "tree_id": "526a9fd19d0f1cbe99197326f8cc6e209404dc1b",
          "url": "https://github.com/celox-sim/celox/commit/2649e277ae41c1812317db51d96e784fcca83282"
        },
        "date": 1772396976218,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "rust/simulator_tick_x10000",
            "value": 1592.872,
            "range": "± 26.031 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_step_x20000",
            "value": 9859.733,
            "range": "± 18.568 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_top_n1000",
            "value": 765546.74,
            "range": "± 9324.028 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1",
            "value": 0.158,
            "range": "± 0.002 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1000000",
            "value": 159263.75,
            "range": "± 1088.116 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1",
            "value": 0.327,
            "range": "± 0.009 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1000000",
            "value": 333784.181,
            "range": "± 2835.626 us",
            "unit": "us"
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
          "id": "62b8ddffda41525315fd755b173b6ff20e478a48",
          "message": "Introduce ModuleId for generic module instantiation support\n\nReplace StrId-keyed module maps with ModuleId(usize) to uniquely\nidentify each concrete instantiation of generic modules. Previously,\nGenericPass::<Byte> and GenericPass::<Word> shared a single SimModule\nwith unresolved 1-bit types, producing wrong simulation values.\n\nKey changes:\n- Add ModuleId newtype; GlueBlock stores module_id instead of module_name\n- parse_ir uses worklist to discover modules, assigning unique ModuleIds\n  per generic instantiation while deduplicating non-generic modules\n- ModuleParser receives pre-assigned inst_ids slice instead of registry\n- Remove ModuleRegistry (port types resolved directly from InstDeclaration\n  component modules)\n- Update flatten, expand, relocate_units, module_variables and all\n  downstream consumers to use ModuleId\n- Replace compare_matrix test with test_generic_module_instantiation\n  that verifies 8-bit and 16-bit passthrough via proto package generics\n\nCo-Authored-By: Claude Opus 4.6 <noreply@anthropic.com>",
          "timestamp": "2026-03-01T20:49:56Z",
          "tree_id": "1e8fde7dd37913ee2d9157204fe64961100394fd",
          "url": "https://github.com/celox-sim/celox/commit/62b8ddffda41525315fd755b173b6ff20e478a48"
        },
        "date": 1772398864693,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "rust/simulator_tick_x10000",
            "value": 1590.705,
            "range": "± 25.697 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_step_x20000",
            "value": 9934.545,
            "range": "± 121.252 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_top_n1000",
            "value": 762200.035,
            "range": "± 5323.158 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1",
            "value": 0.158,
            "range": "± 0.002 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1000000",
            "value": 159068.876,
            "range": "± 626.087 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1",
            "value": 0.27,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1000000",
            "value": 338815.135,
            "range": "± 1086.359 us",
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
        "date": 1772363810373,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ts/simulation_build_top_n1000",
            "value": 775.4899123333338,
            "range": "± 3.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1",
            "value": 0.0005100142181747574,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "980365 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1000000",
            "value": 383.2439733333352,
            "range": "± 0.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1",
            "value": 0.0007130768454053993,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "701187 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1000000",
            "value": 587.7964900000007,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1",
            "value": 0.0006975421712774165,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "716803 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1000000",
            "value": 587.3184699999996,
            "range": "± 1.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulator_tick_x10000",
            "value": 3.97900933333464,
            "range": "± 1.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_step_x20000",
            "value": 13.281697666666636,
            "range": "± 0.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_build_top_n1000",
            "value": 768.4984699999986,
            "range": "± 1.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_step_x1",
            "value": 0.0008101158108653652,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "617196 samples"
          },
          {
            "name": "ts/simulation_time_step_x1000000",
            "value": 658.9075673333331,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_runUntil_1000000",
            "value": 101.86749033333035,
            "range": "± 0.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/waitForCycles_x1000",
            "value": 0.7037689999997383,
            "range": "± 3.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/manual_step_loop_x2000",
            "value": 0.7110496666646213,
            "range": "± 4.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_fast_path_100000",
            "value": 4.326324333332498,
            "range": "± 3.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_guarded_100000",
            "value": 9.126416999999492,
            "range": "± 0.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_without_optimize",
            "value": 767.2688273333333,
            "range": "± 1.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_with_optimize",
            "value": 771.7857606666634,
            "range": "± 0.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_without_optimize",
            "value": 3.8383496666671513,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_with_optimize",
            "value": 3.6189679999976456,
            "range": "± 0.4%",
            "unit": "ms",
            "extra": "3 samples"
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
          "id": "36ba941c49341c77c2c4c029bb12fdf7054d3e0b",
          "message": "Parallelize benchmark jobs for faster CI\n\nSplit the single sequential bench job into three parallel jobs\n(bench-rust, bench-verilator, bench-ts) that upload artifacts,\nfollowed by a lightweight publish job that converts and pushes results.\n\nWall-clock time: Rust + Verilator + TS → max(Rust, Verilator, TS)\n\nCo-Authored-By: Claude Opus 4.6 <noreply@anthropic.com>",
          "timestamp": "2026-03-01T11:29:28Z",
          "tree_id": "47c2d5f2b1e88e911b4d2db6b3acb7179b02d1d7",
          "url": "https://github.com/celox-sim/celox/commit/36ba941c49341c77c2c4c029bb12fdf7054d3e0b"
        },
        "date": 1772365355593,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ts/simulation_build_top_n1000",
            "value": 765.3750389999992,
            "range": "± 1.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1",
            "value": 0.0005147326120909156,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "971379 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1000000",
            "value": 372.736204333333,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1",
            "value": 0.000736746316267428,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "678660 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1000000",
            "value": 587.3985866666662,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1",
            "value": 0.0007211818198856339,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "693307 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1000000",
            "value": 563.4805556666664,
            "range": "± 0.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulator_tick_x10000",
            "value": 3.696258666665623,
            "range": "± 0.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_step_x20000",
            "value": 13.104672000001301,
            "range": "± 1.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_build_top_n1000",
            "value": 771.104158333333,
            "range": "± 0.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_step_x1",
            "value": 0.0007986800200637268,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "626033 samples"
          },
          {
            "name": "ts/simulation_time_step_x1000000",
            "value": 666.5207506666678,
            "range": "± 0.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_runUntil_1000000",
            "value": 102.55897766666506,
            "range": "± 2.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/waitForCycles_x1000",
            "value": 0.7188580000001821,
            "range": "± 10.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/manual_step_loop_x2000",
            "value": 0.6891753333329689,
            "range": "± 4.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_fast_path_100000",
            "value": 4.295509000000796,
            "range": "± 0.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_guarded_100000",
            "value": 9.3724933333336,
            "range": "± 4.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_without_optimize",
            "value": 774.1526086666685,
            "range": "± 1.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_with_optimize",
            "value": 777.9020600000027,
            "range": "± 2.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_without_optimize",
            "value": 3.6719859999987725,
            "range": "± 1.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_with_optimize",
            "value": 4.090403333335416,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "3 samples"
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
          "id": "8d149808a083ce434b92b708a2182431edebe778",
          "message": "Fix counter test reset polarity for async_low reset type\n\nThe project uses reset_type = \"async_low\", so reset is active-low.\nThe test was asserting rst=1 (inactive) and releasing with rst=0\n(active), which is backwards.\n\nCo-Authored-By: Claude Opus 4.6 <noreply@anthropic.com>",
          "timestamp": "2026-03-01T12:07:43Z",
          "tree_id": "3f83d5cd703b2e01f9fb60bab87492f1c372c323",
          "url": "https://github.com/celox-sim/celox/commit/8d149808a083ce434b92b708a2182431edebe778"
        },
        "date": 1772367420528,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ts/simulation_build_top_n1000",
            "value": 771.531186666667,
            "range": "± 1.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1",
            "value": 0.0005263267381152484,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "949981 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1000000",
            "value": 368.8633539999998,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1",
            "value": 0.0007154987121023021,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "698814 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1000000",
            "value": 580.0590493333342,
            "range": "± 0.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1",
            "value": 0.0007033929106526215,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "710841 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1000000",
            "value": 575.0878026666663,
            "range": "± 0.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulator_tick_x10000",
            "value": 3.6903070000007574,
            "range": "± 1.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_step_x20000",
            "value": 13.227404333333348,
            "range": "± 1.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_build_top_n1000",
            "value": 770.7729263333338,
            "range": "± 2.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_step_x1",
            "value": 0.0008068503487157293,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "619694 samples"
          },
          {
            "name": "ts/simulation_time_step_x1000000",
            "value": 668.1270403333353,
            "range": "± 0.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_runUntil_1000000",
            "value": 100.59745799999898,
            "range": "± 0.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/waitForCycles_x1000",
            "value": 0.7263976666678597,
            "range": "± 3.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/manual_step_loop_x2000",
            "value": 0.7087479999997109,
            "range": "± 6.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_fast_path_100000",
            "value": 4.233932666664866,
            "range": "± 2.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_guarded_100000",
            "value": 8.689901333331363,
            "range": "± 2.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_without_optimize",
            "value": 771.7574326666679,
            "range": "± 4.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_with_optimize",
            "value": 776.375821666666,
            "range": "± 1.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_without_optimize",
            "value": 3.882839666667375,
            "range": "± 2.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_with_optimize",
            "value": 3.88700733333341,
            "range": "± 0.8%",
            "unit": "ms",
            "extra": "3 samples"
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
          "id": "2649e277ae41c1812317db51d96e784fcca83282",
          "message": "Merge branch 'claude/fix-always-comb-eval'",
          "timestamp": "2026-03-01T20:19:29Z",
          "tree_id": "526a9fd19d0f1cbe99197326f8cc6e209404dc1b",
          "url": "https://github.com/celox-sim/celox/commit/2649e277ae41c1812317db51d96e784fcca83282"
        },
        "date": 1772396977760,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ts/simulation_build_top_n1000",
            "value": 801.9612196666664,
            "range": "± 8.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1",
            "value": 0.0005382478715033172,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "928941 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1000000",
            "value": 397.67830933333363,
            "range": "± 1.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1",
            "value": 0.0008503566342228747,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "587989 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1000000",
            "value": 700.2426496666668,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1",
            "value": 0.0008412829635361984,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "594331 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1000000",
            "value": 671.158729666667,
            "range": "± 4.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulator_tick_x10000",
            "value": 3.9460776666674064,
            "range": "± 1.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_step_x20000",
            "value": 13.15004166666525,
            "range": "± 1.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_build_top_n1000",
            "value": 767.7008929999987,
            "range": "± 0.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_step_x1",
            "value": 0.0008102735108779834,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "617076 samples"
          },
          {
            "name": "ts/simulation_time_step_x1000000",
            "value": 652.3299183333318,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_runUntil_1000000",
            "value": 100.5210706666694,
            "range": "± 0.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/waitForCycles_x1000",
            "value": 0.7060836666666243,
            "range": "± 4.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/manual_step_loop_x2000",
            "value": 0.7017790000002909,
            "range": "± 4.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_fast_path_100000",
            "value": 4.223918000000897,
            "range": "± 0.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_guarded_100000",
            "value": 8.799047333333874,
            "range": "± 3.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_without_optimize",
            "value": 766.2895193333317,
            "range": "± 3.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_with_optimize",
            "value": 771.138999666667,
            "range": "± 1.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_without_optimize",
            "value": 3.936978666659949,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_with_optimize",
            "value": 3.9268926666651773,
            "range": "± 0.5%",
            "unit": "ms",
            "extra": "3 samples"
          }
        ]
      }
    ],
    "Verilator Benchmarks": [
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
          "id": "36ba941c49341c77c2c4c029bb12fdf7054d3e0b",
          "message": "Parallelize benchmark jobs for faster CI\n\nSplit the single sequential bench job into three parallel jobs\n(bench-rust, bench-verilator, bench-ts) that upload artifacts,\nfollowed by a lightweight publish job that converts and pushes results.\n\nWall-clock time: Rust + Verilator + TS → max(Rust, Verilator, TS)\n\nCo-Authored-By: Claude Opus 4.6 <noreply@anthropic.com>",
          "timestamp": "2026-03-01T11:29:28Z",
          "tree_id": "47c2d5f2b1e88e911b4d2db6b3acb7179b02d1d7",
          "url": "https://github.com/celox-sim/celox/commit/36ba941c49341c77c2c4c029bb12fdf7054d3e0b"
        },
        "date": 1772365355187,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "verilator/simulation_build_top_n1000",
            "value": 9824974.659,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1",
            "value": 0.15469999999999998,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1000000",
            "value": 165841.637,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1",
            "value": 0.15406999999999998,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1000000",
            "value": 153975.336,
            "unit": "us"
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
          "id": "8d149808a083ce434b92b708a2182431edebe778",
          "message": "Fix counter test reset polarity for async_low reset type\n\nThe project uses reset_type = \"async_low\", so reset is active-low.\nThe test was asserting rst=1 (inactive) and releasing with rst=0\n(active), which is backwards.\n\nCo-Authored-By: Claude Opus 4.6 <noreply@anthropic.com>",
          "timestamp": "2026-03-01T12:07:43Z",
          "tree_id": "3f83d5cd703b2e01f9fb60bab87492f1c372c323",
          "url": "https://github.com/celox-sim/celox/commit/8d149808a083ce434b92b708a2182431edebe778"
        },
        "date": 1772367420100,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "verilator/simulation_build_top_n1000",
            "value": 13319417.058,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1",
            "value": 0.1579,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1000000",
            "value": 164785.722,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1",
            "value": 0.15391,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1000000",
            "value": 153741.941,
            "unit": "us"
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
          "id": "2649e277ae41c1812317db51d96e784fcca83282",
          "message": "Merge branch 'claude/fix-always-comb-eval'",
          "timestamp": "2026-03-01T20:19:29Z",
          "tree_id": "526a9fd19d0f1cbe99197326f8cc6e209404dc1b",
          "url": "https://github.com/celox-sim/celox/commit/2649e277ae41c1812317db51d96e784fcca83282"
        },
        "date": 1772396977312,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "verilator/simulation_build_top_n1000",
            "value": 13735209.267,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1",
            "value": 0.17485,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1000000",
            "value": 152128.027,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1",
            "value": 0.15009,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1000000",
            "value": 151177.716,
            "unit": "us"
          }
        ]
      }
    ]
  }
}