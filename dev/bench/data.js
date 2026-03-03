window.BENCHMARK_DATA = {
  "lastUpdate": 1772557784025,
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
          "id": "d4c979988f96b48d6ea1891f5ec9a168c6ce7be5",
          "message": "Update veryl submodule to celox/proto-support branch\n\nCo-Authored-By: Claude Opus 4.6 <noreply@anthropic.com>",
          "timestamp": "2026-03-01T23:17:45Z",
          "tree_id": "94b8e5e9bc1bc8443314dafd8be4cd0ab874c407",
          "url": "https://github.com/celox-sim/celox/commit/d4c979988f96b48d6ea1891f5ec9a168c6ce7be5"
        },
        "date": 1772407608862,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "rust/simulator_tick_x10000",
            "value": 1593.051,
            "range": "± 23.809 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_step_x20000",
            "value": 10005.302,
            "range": "± 59.618 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_top_n1000",
            "value": 754976.929,
            "range": "± 5086.331 us",
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
            "value": 159286.207,
            "range": "± 604.664 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1",
            "value": 0.276,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1000000",
            "value": 336055.926,
            "range": "± 671.586 us",
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
          "id": "07b7299e4b58006ef0ff8e0d215c616612f8fc2a",
          "message": "Remove resolve_module_name helper and inline its logic at call sites",
          "timestamp": "2026-03-02T12:05:14Z",
          "tree_id": "11da32332e9dcc49c93b67588c682dc937419c4a",
          "url": "https://github.com/celox-sim/celox/commit/07b7299e4b58006ef0ff8e0d215c616612f8fc2a"
        },
        "date": 1772454446928,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "rust/simulator_tick_x10000",
            "value": 1591.397,
            "range": "± 24.811 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_step_x20000",
            "value": 9878.82,
            "range": "± 18.982 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_top_n1000",
            "value": 748637.214,
            "range": "± 4777.445 us",
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
            "value": 159687.976,
            "range": "± 772.069 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1",
            "value": 0.19,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1000000",
            "value": 341094.948,
            "range": "± 597.921 us",
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
          "id": "91db8c8d004befc3193a74df8fb769c726fba464",
          "message": "Support numeric ValueVariant in Op::As width cast\n\nThe upstream veryl analyzer now represents numeric width casts\n(e.g. `x as 128`) as ValueVariant::Numeric instead of ValueVariant::Type.\nHandle both variants in context_width, ff parser, and comb evaluator.\n\nAlso return errors instead of silently ignoring unrecognized cast targets.",
          "timestamp": "2026-03-02T13:19:08Z",
          "tree_id": "f3576d1cccc7d4ce1b5d213fba1d015d580e9e49",
          "url": "https://github.com/celox-sim/celox/commit/91db8c8d004befc3193a74df8fb769c726fba464"
        },
        "date": 1772458175684,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "rust/simulator_tick_x10000",
            "value": 1450.409,
            "range": "± 12.247 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_step_x20000",
            "value": 8979.806,
            "range": "± 16.513 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_top_n1000",
            "value": 719129.532,
            "range": "± 6103.637 us",
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
            "value": 145047.47,
            "range": "± 484.528 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1",
            "value": 0.194,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1000000",
            "value": 198972.935,
            "range": "± 1250.656 us",
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
          "id": "5cf87d6c995c9925c0897c39a5b5be78514e352f",
          "message": "Fix benchmark asymmetries: full-cycle tick and consistent testbench workload\n\n- Verilator tick() was a half-cycle toggle; now does a full cycle\n  (low→eval→high→eval) so 1M ticks = 1M posedges on both sides\n- testbench benchmarks now do tick + read cnt0 (output) on both sides,\n  dropping the spurious rst=0 write (rst is already 0 after reset)\n- Add cnt0 output port (assign cnt0 = cnt[0]) to both Top.sv and the\n  Veryl CODE string so both sides read the same 32-bit output signal",
          "timestamp": "2026-03-02T13:40:05Z",
          "tree_id": "9816a41cb5161d798455fe909d61392ac9e72755",
          "url": "https://github.com/celox-sim/celox/commit/5cf87d6c995c9925c0897c39a5b5be78514e352f"
        },
        "date": 1772459352367,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "rust/simulator_tick_x10000",
            "value": 1614.842,
            "range": "± 19.333 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_step_x20000",
            "value": 9722.463,
            "range": "± 91.884 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_top_n1000",
            "value": 769186.642,
            "range": "± 9100.968 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1",
            "value": 0.158,
            "range": "± 0.003 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1000000",
            "value": 159478.924,
            "range": "± 1133.021 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1",
            "value": 0.257,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1000000",
            "value": 282348.397,
            "range": "± 3073.242 us",
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
          "id": "72bb0b5ec3580ffe82f31885e027622bdc414ee1",
          "message": "Add guide pages for parameter overrides, hierarchy, combinational loops, and VCD\n\n- Add parameter-overrides, hierarchy, combinational-loops, vcd guides (EN + JA)\n- Update sidebar: add new pages, remove benchmarks from guide sidebar\n- Fix bigint literals (100 -> 100n) in getting-started examples\n- Replace tick()-on-combinational Adder with Reg example in writing-tests\n- Add factory method comparison (create / fromProject / fromSource)\n- Fix incorrect claims: false loop \"2-pass\", bit-level path narrowing",
          "timestamp": "2026-03-02T14:33:29Z",
          "tree_id": "79d68beb2ab22e161734abc7dc5fe80811ce1f62",
          "url": "https://github.com/celox-sim/celox/commit/72bb0b5ec3580ffe82f31885e027622bdc414ee1"
        },
        "date": 1772462569276,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "rust/simulator_tick_x10000",
            "value": 1590.508,
            "range": "± 38.680 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_step_x20000",
            "value": 9661.136,
            "range": "± 23.746 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_top_n1000",
            "value": 771251.151,
            "range": "± 9556.475 us",
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
            "value": 159406.292,
            "range": "± 2692.314 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1",
            "value": 0.256,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1000000",
            "value": 282590.176,
            "range": "± 598.351 us",
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
          "id": "da3ea46e7c586cb30a9038e06e2a7f135d26cbeb",
          "message": "Add celox.toml support for test-only Veryl source directories\n\nIntroduces an optional celox.toml alongside Veryl.toml:\n\n  [test]\n  sources = [\"test_veryl\"]\n\nDirectories listed under [test].sources are included when running\nsimulations via fromProject and when generating TypeScript type stubs\nvia genTs. Missing directories are silently skipped.",
          "timestamp": "2026-03-02T15:16:04Z",
          "tree_id": "bcb88e005893d2d76f425c17cf26761d0bb00369",
          "url": "https://github.com/celox-sim/celox/commit/da3ea46e7c586cb30a9038e06e2a7f135d26cbeb"
        },
        "date": 1772465139644,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "rust/simulator_tick_x10000",
            "value": 1450.684,
            "range": "± 20.945 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_step_x20000",
            "value": 8809.335,
            "range": "± 18.943 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_top_n1000",
            "value": 701583.93,
            "range": "± 3595.494 us",
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
            "value": 145699.088,
            "range": "± 1031.821 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1",
            "value": 0.185,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1000000",
            "value": 183861.703,
            "range": "± 147.137 us",
            "unit": "us"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "tignear+s@gmail.com",
            "name": "tig",
            "username": "tignear"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "6e9c7785592164679176dbd284d3eba787b72074",
          "message": "Merge pull request #2 from celox-sim/claude/param-override\n\nSupport static partial assignments in function body lowering",
          "timestamp": "2026-03-03T02:16:28+09:00",
          "tree_id": "91cfd658fa24ffecfd621c3fdc2d9f9ec8a89557",
          "url": "https://github.com/celox-sim/celox/commit/6e9c7785592164679176dbd284d3eba787b72074"
        },
        "date": 1772472321183,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "rust/simulator_tick_x10000",
            "value": 1592.148,
            "range": "± 34.769 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_step_x20000",
            "value": 9877.877,
            "range": "± 35.336 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_top_n1000",
            "value": 763972.259,
            "range": "± 9761.406 us",
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
            "value": 159468.323,
            "range": "± 2834.050 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1",
            "value": 0.319,
            "range": "± 0.008 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1000000",
            "value": 323365.096,
            "range": "± 1508.555 us",
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
          "id": "a359d77652f39e1e2e47dfcf357839eb9e196e6b",
          "message": "Fix bench CI: install libbenchmark-dev and propagate script failures\n\nThe bench-verilator job was missing the Google Benchmark library\n(libbenchmark-dev), causing the make step to fail at link time.\nThe failure was silently swallowed because the outer shell only\nchecked tee's exit code. Add the missing apt package and set\npipefail so script errors are properly surfaced.",
          "timestamp": "2026-03-03T16:53:40Z",
          "tree_id": "7557fa99486060c5f48906812f9b1fc126389d21",
          "url": "https://github.com/celox-sim/celox/commit/a359d77652f39e1e2e47dfcf357839eb9e196e6b"
        },
        "date": 1772557777552,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "rust/simulator_tick_x10000",
            "value": 1593.691,
            "range": "± 32.432 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_step_x20000",
            "value": 9876.919,
            "range": "± 28.509 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_top_n1000",
            "value": 768112.799,
            "range": "± 6679.023 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1",
            "value": 0.309,
            "range": "± 0.002 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1000000",
            "value": 310038.683,
            "range": "± 872.774 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1",
            "value": 0.329,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1000000",
            "value": 329789.204,
            "range": "± 513.004 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_linear_sec_p6",
            "value": 162836.692,
            "range": "± 1377.757 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1",
            "value": 0.791,
            "range": "± 0.002 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1000000",
            "value": 798824.469,
            "range": "± 1373.340 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_eval_linear_sec_p6_x1000000",
            "value": 795531.551,
            "range": "± 1200.030 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_countones_w64",
            "value": 104881.856,
            "range": "± 1167.643 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1",
            "value": 0.427,
            "range": "± 0.002 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1000000",
            "value": 429419.248,
            "range": "± 562.213 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_std_counter_w32",
            "value": 7995.745,
            "range": "± 103.442 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1",
            "value": 0.009,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1000000",
            "value": 9881.656,
            "range": "± 30.713 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_std_counter_w32_x1000000",
            "value": 32775.837,
            "range": "± 69.764 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_gray_counter_w32",
            "value": 11496,
            "range": "± 74.027 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_gray_counter_w32_x1",
            "value": 0.01,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_gray_counter_w32_x1000000",
            "value": 10748.567,
            "range": "± 129.171 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_gray_counter_w32_x1000000",
            "value": 37098.288,
            "range": "± 62.714 us",
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
        "date": 1772398867092,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ts/simulation_build_top_n1000",
            "value": 776.081557333333,
            "range": "± 1.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1",
            "value": 0.0005113928427503525,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "977722 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1000000",
            "value": 403.74815300000046,
            "range": "± 4.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1",
            "value": 0.0008532212008263303,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "586015 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1000000",
            "value": 698.1213086666661,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1",
            "value": 0.0008399901183687316,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "595246 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1000000",
            "value": 692.7459506666662,
            "range": "± 1.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulator_tick_x10000",
            "value": 3.9014420000021346,
            "range": "± 0.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_step_x20000",
            "value": 13.26227366666717,
            "range": "± 0.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_build_top_n1000",
            "value": 783.035064000001,
            "range": "± 3.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_step_x1",
            "value": 0.0008279753031674371,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "603883 samples"
          },
          {
            "name": "ts/simulation_time_step_x1000000",
            "value": 667.7923143333319,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_runUntil_1000000",
            "value": 102.63120133333238,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/waitForCycles_x1000",
            "value": 0.7199873333326346,
            "range": "± 4.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/manual_step_loop_x2000",
            "value": 0.7194663333333059,
            "range": "± 6.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_fast_path_100000",
            "value": 4.207174666664893,
            "range": "± 1.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_guarded_100000",
            "value": 8.928092333332946,
            "range": "± 0.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_without_optimize",
            "value": 773.6080963333346,
            "range": "± 0.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_with_optimize",
            "value": 783.4944990000004,
            "range": "± 1.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_without_optimize",
            "value": 3.9890883333331053,
            "range": "± 1.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_with_optimize",
            "value": 3.759610999991613,
            "range": "± 0.7%",
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
          "id": "d4c979988f96b48d6ea1891f5ec9a168c6ce7be5",
          "message": "Update veryl submodule to celox/proto-support branch\n\nCo-Authored-By: Claude Opus 4.6 <noreply@anthropic.com>",
          "timestamp": "2026-03-01T23:17:45Z",
          "tree_id": "94b8e5e9bc1bc8443314dafd8be4cd0ab874c407",
          "url": "https://github.com/celox-sim/celox/commit/d4c979988f96b48d6ea1891f5ec9a168c6ce7be5"
        },
        "date": 1772407610298,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ts/simulation_build_top_n1000",
            "value": 796.2869030000007,
            "range": "± 1.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1",
            "value": 0.0005207249552168484,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "960200 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1000000",
            "value": 379.1208110000007,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1",
            "value": 0.0008550824726932656,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "584739 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1000000",
            "value": 726.0639573333344,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1",
            "value": 0.0008319768843861424,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "600979 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1000000",
            "value": 706.5446996666675,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulator_tick_x10000",
            "value": 3.8476299999989956,
            "range": "± 1.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_step_x20000",
            "value": 13.422460666663634,
            "range": "± 1.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_build_top_n1000",
            "value": 773.7881549999996,
            "range": "± 0.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_step_x1",
            "value": 0.0008055550265728069,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "620691 samples"
          },
          {
            "name": "ts/simulation_time_step_x1000000",
            "value": 673.6819676666662,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_runUntil_1000000",
            "value": 100.79563233333708,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/waitForCycles_x1000",
            "value": 0.6697719999962525,
            "range": "± 3.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/manual_step_loop_x2000",
            "value": 0.6868806666655777,
            "range": "± 4.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_fast_path_100000",
            "value": 4.153519666666398,
            "range": "± 0.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_guarded_100000",
            "value": 8.94576366666782,
            "range": "± 0.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_without_optimize",
            "value": 782.3554996666669,
            "range": "± 2.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_with_optimize",
            "value": 781.4693436666663,
            "range": "± 3.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_without_optimize",
            "value": 3.862753666660865,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_with_optimize",
            "value": 3.8621623333359216,
            "range": "± 1.2%",
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
          "id": "07b7299e4b58006ef0ff8e0d215c616612f8fc2a",
          "message": "Remove resolve_module_name helper and inline its logic at call sites",
          "timestamp": "2026-03-02T12:05:14Z",
          "tree_id": "11da32332e9dcc49c93b67588c682dc937419c4a",
          "url": "https://github.com/celox-sim/celox/commit/07b7299e4b58006ef0ff8e0d215c616612f8fc2a"
        },
        "date": 1772454449759,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ts/simulation_build_top_n1000",
            "value": 766.6364100000004,
            "range": "± 2.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1",
            "value": 0.0005176974123430861,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "965816 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1000000",
            "value": 389.63816733333323,
            "range": "± 1.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1",
            "value": 0.0008694914363805566,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "575049 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1000000",
            "value": 710.738115666667,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1",
            "value": 0.0008542376708040708,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "585318 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1000000",
            "value": 708.7675573333339,
            "range": "± 1.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulator_tick_x10000",
            "value": 3.7564360000008796,
            "range": "± 0.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_step_x20000",
            "value": 13.344358000002103,
            "range": "± 0.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_build_top_n1000",
            "value": 763.4361860000014,
            "range": "± 2.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_step_x1",
            "value": 0.0008088171054974275,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "618187 samples"
          },
          {
            "name": "ts/simulation_time_step_x1000000",
            "value": 660.6608256666659,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_runUntil_1000000",
            "value": 102.3248250000009,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/waitForCycles_x1000",
            "value": 0.93460266666807,
            "range": "± 95.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/manual_step_loop_x2000",
            "value": 0.7162473333325275,
            "range": "± 7.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_fast_path_100000",
            "value": 4.245675666667618,
            "range": "± 1.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_guarded_100000",
            "value": 8.650679333334361,
            "range": "± 0.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_without_optimize",
            "value": 760.1492806666672,
            "range": "± 1.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_with_optimize",
            "value": 765.5609810000024,
            "range": "± 1.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_without_optimize",
            "value": 3.8947639999969397,
            "range": "± 1.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_with_optimize",
            "value": 3.7417073333393396,
            "range": "± 0.2%",
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
          "id": "91db8c8d004befc3193a74df8fb769c726fba464",
          "message": "Support numeric ValueVariant in Op::As width cast\n\nThe upstream veryl analyzer now represents numeric width casts\n(e.g. `x as 128`) as ValueVariant::Numeric instead of ValueVariant::Type.\nHandle both variants in context_width, ff parser, and comb evaluator.\n\nAlso return errors instead of silently ignoring unrecognized cast targets.",
          "timestamp": "2026-03-02T13:19:08Z",
          "tree_id": "f3576d1cccc7d4ce1b5d213fba1d015d580e9e49",
          "url": "https://github.com/celox-sim/celox/commit/91db8c8d004befc3193a74df8fb769c726fba464"
        },
        "date": 1772458177669,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ts/simulation_build_top_n1000",
            "value": 773.4375850000002,
            "range": "± 0.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1",
            "value": 0.000514809252971224,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "971234 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1000000",
            "value": 382.85775233333214,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1",
            "value": 0.0008420526854678003,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "593788 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1000000",
            "value": 691.6470520000003,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1",
            "value": 0.0008227551500313218,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "607715 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1000000",
            "value": 671.789883,
            "range": "± 1.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulator_tick_x10000",
            "value": 3.8210966666641375,
            "range": "± 1.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_step_x20000",
            "value": 13.282994999998968,
            "range": "± 1.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_build_top_n1000",
            "value": 780.263433666667,
            "range": "± 0.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_step_x1",
            "value": 0.0008513082158023696,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "587332 samples"
          },
          {
            "name": "ts/simulation_time_step_x1000000",
            "value": 678.4830926666691,
            "range": "± 0.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_runUntil_1000000",
            "value": 101.86539833333275,
            "range": "± 0.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/waitForCycles_x1000",
            "value": 0.7448986666665102,
            "range": "± 13.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/manual_step_loop_x2000",
            "value": 0.7337976666676695,
            "range": "± 3.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_fast_path_100000",
            "value": 4.259559333334134,
            "range": "± 2.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_guarded_100000",
            "value": 8.959506999999576,
            "range": "± 1.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_without_optimize",
            "value": 771.3399890000001,
            "range": "± 2.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_with_optimize",
            "value": 786.153366999999,
            "range": "± 2.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_without_optimize",
            "value": 3.920811333329766,
            "range": "± 3.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_with_optimize",
            "value": 3.8234036666641864,
            "range": "± 0.3%",
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
          "id": "5cf87d6c995c9925c0897c39a5b5be78514e352f",
          "message": "Fix benchmark asymmetries: full-cycle tick and consistent testbench workload\n\n- Verilator tick() was a half-cycle toggle; now does a full cycle\n  (low→eval→high→eval) so 1M ticks = 1M posedges on both sides\n- testbench benchmarks now do tick + read cnt0 (output) on both sides,\n  dropping the spurious rst=0 write (rst is already 0 after reset)\n- Add cnt0 output port (assign cnt0 = cnt[0]) to both Top.sv and the\n  Veryl CODE string so both sides read the same 32-bit output signal",
          "timestamp": "2026-03-02T13:40:05Z",
          "tree_id": "9816a41cb5161d798455fe909d61392ac9e72755",
          "url": "https://github.com/celox-sim/celox/commit/5cf87d6c995c9925c0897c39a5b5be78514e352f"
        },
        "date": 1772459354500,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ts/simulation_build_top_n1000",
            "value": 716.3155220000002,
            "range": "± 2.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1",
            "value": 0.0005019367349220248,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "996142 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1000000",
            "value": 347.13445200000024,
            "range": "± 1.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1",
            "value": 0.0008372377018174139,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "597202 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1000000",
            "value": 684.9594486666671,
            "range": "± 1.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1",
            "value": 0.0008119791841266486,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "615780 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1000000",
            "value": 672.4965196666659,
            "range": "± 1.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulator_tick_x10000",
            "value": 3.6908913333330324,
            "range": "± 1.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_step_x20000",
            "value": 13.866088333333513,
            "range": "± 45.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_build_top_n1000",
            "value": 701.7306066666667,
            "range": "± 1.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_step_x1",
            "value": 0.0007838539917666643,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "637875 samples"
          },
          {
            "name": "ts/simulation_time_step_x1000000",
            "value": 654.4297966666661,
            "range": "± 3.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_runUntil_1000000",
            "value": 98.5263343333354,
            "range": "± 1.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/waitForCycles_x1000",
            "value": 0.75704199999988,
            "range": "± 1.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/manual_step_loop_x2000",
            "value": 1.260857000001124,
            "range": "± 83.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_fast_path_100000",
            "value": 4.361482666669569,
            "range": "± 1.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_guarded_100000",
            "value": 9.438729999999245,
            "range": "± 1.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_without_optimize",
            "value": 733.6463196666688,
            "range": "± 3.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_with_optimize",
            "value": 748.5959866666672,
            "range": "± 2.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_without_optimize",
            "value": 3.7521989999998673,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_with_optimize",
            "value": 3.7606643333322913,
            "range": "± 3.4%",
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
          "id": "72bb0b5ec3580ffe82f31885e027622bdc414ee1",
          "message": "Add guide pages for parameter overrides, hierarchy, combinational loops, and VCD\n\n- Add parameter-overrides, hierarchy, combinational-loops, vcd guides (EN + JA)\n- Update sidebar: add new pages, remove benchmarks from guide sidebar\n- Fix bigint literals (100 -> 100n) in getting-started examples\n- Replace tick()-on-combinational Adder with Reg example in writing-tests\n- Add factory method comparison (create / fromProject / fromSource)\n- Fix incorrect claims: false loop \"2-pass\", bit-level path narrowing",
          "timestamp": "2026-03-02T14:33:29Z",
          "tree_id": "79d68beb2ab22e161734abc7dc5fe80811ce1f62",
          "url": "https://github.com/celox-sim/celox/commit/72bb0b5ec3580ffe82f31885e027622bdc414ee1"
        },
        "date": 1772462571585,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ts/simulation_build_top_n1000",
            "value": 763.9008590000007,
            "range": "± 1.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1",
            "value": 0.0005156656497754116,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "969621 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1000000",
            "value": 365.01842633333223,
            "range": "± 0.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1",
            "value": 0.0008393067245229774,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "595730 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1000000",
            "value": 708.3144749999992,
            "range": "± 0.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1",
            "value": 0.0008459157468791304,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "591076 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1000000",
            "value": 697.8380230000002,
            "range": "± 1.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulator_tick_x10000",
            "value": 3.622174999999212,
            "range": "± 1.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_step_x20000",
            "value": 13.466898666665656,
            "range": "± 0.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_build_top_n1000",
            "value": 766.1017996666682,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_step_x1",
            "value": 0.000812631682881752,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "615285 samples"
          },
          {
            "name": "ts/simulation_time_step_x1000000",
            "value": 671.1789856666679,
            "range": "± 0.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_runUntil_1000000",
            "value": 102.22617599999892,
            "range": "± 0.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/waitForCycles_x1000",
            "value": 0.7206059999977393,
            "range": "± 2.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/manual_step_loop_x2000",
            "value": 0.7299940000011702,
            "range": "± 3.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_fast_path_100000",
            "value": 4.348848000001453,
            "range": "± 0.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_guarded_100000",
            "value": 8.781489666667767,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_without_optimize",
            "value": 760.2371150000012,
            "range": "± 1.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_with_optimize",
            "value": 764.9229896666682,
            "range": "± 2.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_without_optimize",
            "value": 4.039082999996026,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_with_optimize",
            "value": 3.6749219999959073,
            "range": "± 2.3%",
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
          "id": "da3ea46e7c586cb30a9038e06e2a7f135d26cbeb",
          "message": "Add celox.toml support for test-only Veryl source directories\n\nIntroduces an optional celox.toml alongside Veryl.toml:\n\n  [test]\n  sources = [\"test_veryl\"]\n\nDirectories listed under [test].sources are included when running\nsimulations via fromProject and when generating TypeScript type stubs\nvia genTs. Missing directories are silently skipped.",
          "timestamp": "2026-03-02T15:16:04Z",
          "tree_id": "bcb88e005893d2d76f425c17cf26761d0bb00369",
          "url": "https://github.com/celox-sim/celox/commit/da3ea46e7c586cb30a9038e06e2a7f135d26cbeb"
        },
        "date": 1772465141263,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ts/simulation_build_top_n1000",
            "value": 789.0714709999993,
            "range": "± 0.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1",
            "value": 0.0005299106210961013,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "943556 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1000000",
            "value": 382.4764779999993,
            "range": "± 1.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1",
            "value": 0.0008657470443411445,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "577536 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1000000",
            "value": 705.9546433333333,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1",
            "value": 0.0008421514675350397,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "593718 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1000000",
            "value": 686.5537223333353,
            "range": "± 1.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulator_tick_x10000",
            "value": 3.8426036666666428,
            "range": "± 0.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_step_x20000",
            "value": 13.619855000002039,
            "range": "± 2.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_build_top_n1000",
            "value": 788.206558333334,
            "range": "± 3.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_step_x1",
            "value": 0.000817635673393868,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "611520 samples"
          },
          {
            "name": "ts/simulation_time_step_x1000000",
            "value": 677.666756999999,
            "range": "± 0.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_runUntil_1000000",
            "value": 102.48236700000173,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/waitForCycles_x1000",
            "value": 0.795899999999771,
            "range": "± 25.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/manual_step_loop_x2000",
            "value": 0.7734806666630902,
            "range": "± 5.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_fast_path_100000",
            "value": 4.317906666665901,
            "range": "± 1.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_guarded_100000",
            "value": 9.126412666667116,
            "range": "± 1.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_without_optimize",
            "value": 794.0255516666657,
            "range": "± 4.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_with_optimize",
            "value": 793.2997076666726,
            "range": "± 1.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_without_optimize",
            "value": 3.7114239999985634,
            "range": "± 0.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_with_optimize",
            "value": 3.8056836666704235,
            "range": "± 0.6%",
            "unit": "ms",
            "extra": "3 samples"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "tignear+s@gmail.com",
            "name": "tig",
            "username": "tignear"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "6e9c7785592164679176dbd284d3eba787b72074",
          "message": "Merge pull request #2 from celox-sim/claude/param-override\n\nSupport static partial assignments in function body lowering",
          "timestamp": "2026-03-03T02:16:28+09:00",
          "tree_id": "91cfd658fa24ffecfd621c3fdc2d9f9ec8a89557",
          "url": "https://github.com/celox-sim/celox/commit/6e9c7785592164679176dbd284d3eba787b72074"
        },
        "date": 1772472323060,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ts/simulation_build_top_n1000",
            "value": 783.3862026666666,
            "range": "± 6.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1",
            "value": 0.0005230497639997677,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "955932 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1000000",
            "value": 370.1234886666668,
            "range": "± 0.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1",
            "value": 0.000862233413228122,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "579890 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1000000",
            "value": 725.8037696666653,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1",
            "value": 0.000850773677433658,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "587701 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1000000",
            "value": 713.120426666667,
            "range": "± 1.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulator_tick_x10000",
            "value": 3.7068846666661557,
            "range": "± 0.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_step_x20000",
            "value": 13.115767666665002,
            "range": "± 1.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_build_top_n1000",
            "value": 787.1679533333323,
            "range": "± 6.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_step_x1",
            "value": 0.0008043004734073309,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "621659 samples"
          },
          {
            "name": "ts/simulation_time_step_x1000000",
            "value": 665.6017503333327,
            "range": "± 0.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_runUntil_1000000",
            "value": 99.39008533333497,
            "range": "± 0.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/waitForCycles_x1000",
            "value": 0.7140259999990425,
            "range": "± 3.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/manual_step_loop_x2000",
            "value": 0.7040470000016891,
            "range": "± 3.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_fast_path_100000",
            "value": 4.349452666666669,
            "range": "± 4.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_guarded_100000",
            "value": 9.002790000001065,
            "range": "± 0.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_without_optimize",
            "value": 774.1513349999999,
            "range": "± 1.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_with_optimize",
            "value": 782.3106440000048,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_without_optimize",
            "value": 3.7196246666717343,
            "range": "± 0.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_with_optimize",
            "value": 3.7529999999921224,
            "range": "± 1.8%",
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
        "date": 1772398866635,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "verilator/simulation_build_top_n1000",
            "value": 12288242.12,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1",
            "value": 0.15458000000000002,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1000000",
            "value": 165396.379,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1",
            "value": 0.15485,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1000000",
            "value": 154174.732,
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
          "id": "d4c979988f96b48d6ea1891f5ec9a168c6ce7be5",
          "message": "Update veryl submodule to celox/proto-support branch\n\nCo-Authored-By: Claude Opus 4.6 <noreply@anthropic.com>",
          "timestamp": "2026-03-01T23:17:45Z",
          "tree_id": "94b8e5e9bc1bc8443314dafd8be4cd0ab874c407",
          "url": "https://github.com/celox-sim/celox/commit/d4c979988f96b48d6ea1891f5ec9a168c6ce7be5"
        },
        "date": 1772407609967,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "verilator/simulation_build_top_n1000",
            "value": 5909576.226,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1",
            "value": 0.15364,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1000000",
            "value": 154525.967,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1",
            "value": 0.15269,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1000000",
            "value": 155592.723,
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
          "id": "07b7299e4b58006ef0ff8e0d215c616612f8fc2a",
          "message": "Remove resolve_module_name helper and inline its logic at call sites",
          "timestamp": "2026-03-02T12:05:14Z",
          "tree_id": "11da32332e9dcc49c93b67588c682dc937419c4a",
          "url": "https://github.com/celox-sim/celox/commit/07b7299e4b58006ef0ff8e0d215c616612f8fc2a"
        },
        "date": 1772454448617,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "verilator/simulation_build_top_n1000",
            "value": 13471905.35,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1",
            "value": 0.22122999999999998,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1000000",
            "value": 182295.816,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1",
            "value": 0.17084,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1000000",
            "value": 170317.685,
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
          "id": "91db8c8d004befc3193a74df8fb769c726fba464",
          "message": "Support numeric ValueVariant in Op::As width cast\n\nThe upstream veryl analyzer now represents numeric width casts\n(e.g. `x as 128`) as ValueVariant::Numeric instead of ValueVariant::Type.\nHandle both variants in context_width, ff parser, and comb evaluator.\n\nAlso return errors instead of silently ignoring unrecognized cast targets.",
          "timestamp": "2026-03-02T13:19:08Z",
          "tree_id": "f3576d1cccc7d4ce1b5d213fba1d015d580e9e49",
          "url": "https://github.com/celox-sim/celox/commit/91db8c8d004befc3193a74df8fb769c726fba464"
        },
        "date": 1772458177222,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "verilator/simulation_build_top_n1000",
            "value": 13079021.099,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1",
            "value": 0.17642,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1000000",
            "value": 189983.978,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1",
            "value": 0.19853,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1000000",
            "value": 175597.441,
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
          "id": "5cf87d6c995c9925c0897c39a5b5be78514e352f",
          "message": "Fix benchmark asymmetries: full-cycle tick and consistent testbench workload\n\n- Verilator tick() was a half-cycle toggle; now does a full cycle\n  (low→eval→high→eval) so 1M ticks = 1M posedges on both sides\n- testbench benchmarks now do tick + read cnt0 (output) on both sides,\n  dropping the spurious rst=0 write (rst is already 0 after reset)\n- Add cnt0 output port (assign cnt0 = cnt[0]) to both Top.sv and the\n  Veryl CODE string so both sides read the same 32-bit output signal",
          "timestamp": "2026-03-02T13:40:05Z",
          "tree_id": "9816a41cb5161d798455fe909d61392ac9e72755",
          "url": "https://github.com/celox-sim/celox/commit/5cf87d6c995c9925c0897c39a5b5be78514e352f"
        },
        "date": 1772459354044,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "verilator/simulation_build_top_n1000",
            "value": 11629992.296,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1",
            "value": 0.34019,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1000000",
            "value": 359378.815,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1",
            "value": 0.36236,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1000000",
            "value": 340288.346,
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
          "id": "72bb0b5ec3580ffe82f31885e027622bdc414ee1",
          "message": "Add guide pages for parameter overrides, hierarchy, combinational loops, and VCD\n\n- Add parameter-overrides, hierarchy, combinational-loops, vcd guides (EN + JA)\n- Update sidebar: add new pages, remove benchmarks from guide sidebar\n- Fix bigint literals (100 -> 100n) in getting-started examples\n- Replace tick()-on-combinational Adder with Reg example in writing-tests\n- Add factory method comparison (create / fromProject / fromSource)\n- Fix incorrect claims: false loop \"2-pass\", bit-level path narrowing",
          "timestamp": "2026-03-02T14:33:29Z",
          "tree_id": "79d68beb2ab22e161734abc7dc5fe80811ce1f62",
          "url": "https://github.com/celox-sim/celox/commit/72bb0b5ec3580ffe82f31885e027622bdc414ee1"
        },
        "date": 1772462571097,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "verilator/simulation_build_top_n1000",
            "value": 15723393.223,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1",
            "value": 0.34014,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1000000",
            "value": 355408.978,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1",
            "value": 0.36174,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1000000",
            "value": 342056.13,
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
          "id": "da3ea46e7c586cb30a9038e06e2a7f135d26cbeb",
          "message": "Add celox.toml support for test-only Veryl source directories\n\nIntroduces an optional celox.toml alongside Veryl.toml:\n\n  [test]\n  sources = [\"test_veryl\"]\n\nDirectories listed under [test].sources are included when running\nsimulations via fromProject and when generating TypeScript type stubs\nvia genTs. Missing directories are silently skipped.",
          "timestamp": "2026-03-02T15:16:04Z",
          "tree_id": "bcb88e005893d2d76f425c17cf26761d0bb00369",
          "url": "https://github.com/celox-sim/celox/commit/da3ea46e7c586cb30a9038e06e2a7f135d26cbeb"
        },
        "date": 1772465140868,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "verilator/simulation_build_top_n1000",
            "value": 8131256.662,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1",
            "value": 0.3137,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1000000",
            "value": 313376.206,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1",
            "value": 0.31618,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1000000",
            "value": 314360.469,
            "unit": "us"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "tignear+s@gmail.com",
            "name": "tig",
            "username": "tignear"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "6e9c7785592164679176dbd284d3eba787b72074",
          "message": "Merge pull request #2 from celox-sim/claude/param-override\n\nSupport static partial assignments in function body lowering",
          "timestamp": "2026-03-03T02:16:28+09:00",
          "tree_id": "91cfd658fa24ffecfd621c3fdc2d9f9ec8a89557",
          "url": "https://github.com/celox-sim/celox/commit/6e9c7785592164679176dbd284d3eba787b72074"
        },
        "date": 1772472322546,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "verilator/simulation_build_top_n1000",
            "value": 6390258.269,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1",
            "value": 0.3405,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1000000",
            "value": 354101.647,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1",
            "value": 0.36219,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1000000",
            "value": 340405.84,
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
          "id": "a359d77652f39e1e2e47dfcf357839eb9e196e6b",
          "message": "Fix bench CI: install libbenchmark-dev and propagate script failures\n\nThe bench-verilator job was missing the Google Benchmark library\n(libbenchmark-dev), causing the make step to fail at link time.\nThe failure was silently swallowed because the outer shell only\nchecked tee's exit code. Add the missing apt package and set\npipefail so script errors are properly surfaced.",
          "timestamp": "2026-03-03T16:53:40Z",
          "tree_id": "7557fa99486060c5f48906812f9b1fc126389d21",
          "url": "https://github.com/celox-sim/celox/commit/a359d77652f39e1e2e47dfcf357839eb9e196e6b"
        },
        "date": 1772557783867,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "verilator/simulation_build_top_n1000",
            "value": 12587714.173,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_linear_sec_p6",
            "value": 1138063.197,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1",
            "value": 0.342283925638039,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1000000",
            "value": 354320.49566666665,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1",
            "value": 0.35898324946259197,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1000000",
            "value": 352636.4096666667,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1",
            "value": 0.035733313783215606,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1000000",
            "value": 35220.62066666666,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_eval_linear_sec_p6_x1000000",
            "value": 36221.237,
            "unit": "us"
          }
        ]
      }
    ]
  }
}