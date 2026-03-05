window.BENCHMARK_DATA = {
  "lastUpdate": 1772714141523,
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
          "id": "0f0c6ec79d1978cac82e3e3d187a1e3a2160ea0f",
          "message": "Rebase veryl submodule onto upstream master and adapt to IR API changes\n\n- Update deps/veryl to upstream/master (10b3d0d0)\n- Add Box<Comptime> field to Expression variant patterns and constructors\n- Remove TokenRange from Factor::Variable/Value/FunctionCall patterns\n- Migrate FunctionCall.ret to FunctionCall.comptime\n- Guard eval_constexpr with is_const/evaluated checks to prevent\n  non-constant variables (now carrying Numeric Comptime) from being\n  treated as compile-time constants",
          "timestamp": "2026-03-03T18:04:03Z",
          "tree_id": "2f9bec6e79b3772a6749a12e3b33bcc493186ca6",
          "url": "https://github.com/celox-sim/celox/commit/0f0c6ec79d1978cac82e3e3d187a1e3a2160ea0f"
        },
        "date": 1772562000755,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "rust/simulator_tick_x10000",
            "value": 1592.938,
            "range": "± 47.647 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_step_x20000",
            "value": 9877.437,
            "range": "± 28.260 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_top_n1000",
            "value": 744016.865,
            "range": "± 6442.454 us",
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
            "value": 159426.451,
            "range": "± 385.455 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1",
            "value": 0.178,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1000000",
            "value": 322499.931,
            "range": "± 423.122 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_linear_sec_p6",
            "value": 219828.441,
            "range": "± 1641.302 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1",
            "value": 1.063,
            "range": "± 0.005 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1000000",
            "value": 1062084.653,
            "range": "± 1404.273 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_eval_linear_sec_p6_x1000000",
            "value": 1050918.689,
            "range": "± 1451.251 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_countones_w64",
            "value": 101967.437,
            "range": "± 1912.320 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1",
            "value": 0.417,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1000000",
            "value": 422005.058,
            "range": "± 708.314 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_std_counter_w32",
            "value": 7967.393,
            "range": "± 106.925 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1",
            "value": 0.008,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1000000",
            "value": 8767.083,
            "range": "± 156.149 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_std_counter_w32_x1000000",
            "value": 31608.056,
            "range": "± 131.845 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_gray_counter_w32",
            "value": 11388.873,
            "range": "± 59.360 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_gray_counter_w32_x1",
            "value": 0.011,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_gray_counter_w32_x1000000",
            "value": 12006.838,
            "range": "± 29.054 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_gray_counter_w32_x1000000",
            "value": 36727.561,
            "range": "± 60.536 us",
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
          "id": "e743cbc7e8b0ffc86ba7eda6fa854313fe5ff267",
          "message": "Rewrite benchmark dashboard with tab-based layout and mini-chart grid\n\nReplace the flat checkbox list + single shared chart with a tab-based UI\n(Counter, Std Library, API, Optimize) and a responsive 2-column grid of\nmini-charts. Each card groups the same operation across runtimes (max 3\nlines: Rust/TS/Verilator) with clickable Chart.js legends. Tabs use\npriority-based matching (API > Optimize > Stdlib > Counter) and sub-group\nsections for Std Library and API tabs.",
          "timestamp": "2026-03-03T20:42:12Z",
          "tree_id": "8a36ba14fc102b953283d48e8eca7c0650b2b4a2",
          "url": "https://github.com/celox-sim/celox/commit/e743cbc7e8b0ffc86ba7eda6fa854313fe5ff267"
        },
        "date": 1772571514608,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "rust/simulator_tick_x10000",
            "value": 1620.56,
            "range": "± 60.860 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_step_x20000",
            "value": 9963.853,
            "range": "± 66.928 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_top_n1000",
            "value": 762492.558,
            "range": "± 7474.094 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1",
            "value": 0.158,
            "range": "± 0.015 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1000000",
            "value": 159571.169,
            "range": "± 842.607 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1",
            "value": 0.179,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1000000",
            "value": 322548.363,
            "range": "± 540.942 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_linear_sec_p6",
            "value": 232291.137,
            "range": "± 2475.419 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1",
            "value": 1.063,
            "range": "± 0.003 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1000000",
            "value": 1066208.944,
            "range": "± 1381.995 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_eval_linear_sec_p6_x1000000",
            "value": 1052223.8,
            "range": "± 1044.944 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_countones_w64",
            "value": 107276.674,
            "range": "± 2159.013 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1",
            "value": 0.417,
            "range": "± 0.008 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1000000",
            "value": 420856.145,
            "range": "± 607.304 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_std_counter_w32",
            "value": 8657.28,
            "range": "± 306.874 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1",
            "value": 0.008,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1000000",
            "value": 8855.373,
            "range": "± 38.422 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_std_counter_w32_x1000000",
            "value": 31379.571,
            "range": "± 60.812 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_gray_counter_w32",
            "value": 12349.022,
            "range": "± 534.183 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_gray_counter_w32_x1",
            "value": 0.011,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_gray_counter_w32_x1000000",
            "value": 12025.094,
            "range": "± 25.624 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_gray_counter_w32_x1000000",
            "value": 34630.692,
            "range": "± 130.615 us",
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
          "id": "80644b5dd01a28cae84104a0978bb48136313d38",
          "message": "Ignore tests blocked by upstream Veryl IR bugs and update SIR snapshot\n\n- array_literal: 2 tests using `'{0}` (UnsupportedByIr at conv/utils.rs:231)\n- for_loop_unroll: 1 test using `'{0}` (same IR issue)\n- compare_matrix: 3 tests producing incorrect simulation results\n- param_override: 2 tests producing incorrect simulation results\n- false_loop: accept SIR snapshot diff from upstream register renumbering",
          "timestamp": "2026-03-03T22:42:09Z",
          "tree_id": "3938e4585335dc56d0411c597a60ebe8ff6c7665",
          "url": "https://github.com/celox-sim/celox/commit/80644b5dd01a28cae84104a0978bb48136313d38"
        },
        "date": 1772578694635,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "rust/simulator_tick_x10000",
            "value": 1451.116,
            "range": "± 19.508 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_step_x20000",
            "value": 8726.668,
            "range": "± 30.901 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_top_n1000",
            "value": 722356.256,
            "range": "± 5352.251 us",
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
            "value": 145531.317,
            "range": "± 2310.868 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1",
            "value": 0.18,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1000000",
            "value": 185153.438,
            "range": "± 169.115 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_linear_sec_p6",
            "value": 221608.322,
            "range": "± 2772.184 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1",
            "value": 0.839,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1000000",
            "value": 840906.793,
            "range": "± 3320.196 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_eval_linear_sec_p6_x1000000",
            "value": 838877.506,
            "range": "± 2858.278 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_countones_w64",
            "value": 100461.088,
            "range": "± 1324.641 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1",
            "value": 0.605,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1000000",
            "value": 617165.097,
            "range": "± 2579.060 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_std_counter_w32",
            "value": 8138.751,
            "range": "± 126.809 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1",
            "value": 0.006,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1000000",
            "value": 6487.294,
            "range": "± 175.264 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_std_counter_w32_x1000000",
            "value": 26456.646,
            "range": "± 131.726 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_gray_counter_w32",
            "value": 11607.968,
            "range": "± 103.860 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_gray_counter_w32_x1",
            "value": 0.008,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_gray_counter_w32_x1000000",
            "value": 8872.743,
            "range": "± 16.726 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_gray_counter_w32_x1000000",
            "value": 29464.933,
            "range": "± 229.645 us",
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
          "id": "2e10bb451c3b3260b7ef3217d8e8d21ce7f813b4",
          "message": "Skip param override E2E test blocked by upstream Veryl IR bug",
          "timestamp": "2026-03-03T23:14:57Z",
          "tree_id": "6ac83fa262c7d08f2d2bf174c267e87c74ec8302",
          "url": "https://github.com/celox-sim/celox/commit/2e10bb451c3b3260b7ef3217d8e8d21ce7f813b4"
        },
        "date": 1772580731030,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "rust/simulator_tick_x10000",
            "value": 1593.976,
            "range": "± 30.102 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_step_x20000",
            "value": 9849.804,
            "range": "± 56.700 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_top_n1000",
            "value": 780539.191,
            "range": "± 7434.574 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1",
            "value": 0.16,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1000000",
            "value": 159501.737,
            "range": "± 570.452 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1",
            "value": 0.179,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1000000",
            "value": 322606.612,
            "range": "± 3216.383 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_linear_sec_p6",
            "value": 233197.918,
            "range": "± 3445.846 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1",
            "value": 1.063,
            "range": "± 0.002 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1000000",
            "value": 1066166.882,
            "range": "± 1375.453 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_eval_linear_sec_p6_x1000000",
            "value": 1055507.882,
            "range": "± 1635.753 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_countones_w64",
            "value": 107620.569,
            "range": "± 2445.511 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1",
            "value": 0.417,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1000000",
            "value": 424545.527,
            "range": "± 940.125 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_std_counter_w32",
            "value": 9217.574,
            "range": "± 231.901 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1",
            "value": 0.008,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1000000",
            "value": 9022.411,
            "range": "± 17.577 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_std_counter_w32_x1000000",
            "value": 31495.494,
            "range": "± 166.203 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_gray_counter_w32",
            "value": 11735.566,
            "range": "± 496.157 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_gray_counter_w32_x1",
            "value": 0.012,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_gray_counter_w32_x1000000",
            "value": 12577.465,
            "range": "± 74.949 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_gray_counter_w32_x1000000",
            "value": 35580.018,
            "range": "± 90.915 us",
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
          "id": "db6aada5f195701837eaa84b1f84c6795f42eada",
          "message": "Optimize SIR codegen: bit-extract peephole, shift-by-0 elim, store coalescing fix\n\nThree optimizations targeting eval_comb JIT performance:\n\n1. Shift-by-0 elimination (codegen): Track Imm constants in TranslationState\n   and skip Shr/Shl/Sar when shift amount is 0, emitting identity instead.\n\n2. BitExtractPeepholePass (optimizer): Replace Load(N)+Shr(K)+And((1<<W)-1)\n   chains with a single narrow Load(W, offset=base+K).\n\n3. Store coalescing fix (optimizer): Run store coalescing twice in\n   optimize_block — before and after eliminate_redundant_loads — so that\n   stores unblocked by load elimination can still be coalesced.\n\nLinear SEC P=6 results: SIR instructions -70%, stores -88%, ~7.6% faster.",
          "timestamp": "2026-03-04T06:01:52Z",
          "tree_id": "c43771cad3a187cc15c4450c8b1ba0f9e142819a",
          "url": "https://github.com/celox-sim/celox/commit/db6aada5f195701837eaa84b1f84c6795f42eada"
        },
        "date": 1772605527937,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "rust/simulator_tick_x10000",
            "value": 1591.386,
            "range": "± 37.437 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_step_x20000",
            "value": 9761.87,
            "range": "± 30.296 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_top_n1000",
            "value": 763857.753,
            "range": "± 10306.427 us",
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
            "value": 159435.276,
            "range": "± 1326.292 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1",
            "value": 0.257,
            "range": "± 0.003 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1000000",
            "value": 263216.708,
            "range": "± 1983.718 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_linear_sec_p6",
            "value": 164065.763,
            "range": "± 1331.197 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1",
            "value": 0.949,
            "range": "± 0.016 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1000000",
            "value": 953493.153,
            "range": "± 3141.785 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_eval_linear_sec_p6_x1000000",
            "value": 952645.548,
            "range": "± 3624.259 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6",
            "value": 0.889,
            "range": "± 0.042 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6_x1000000",
            "value": 890302.787,
            "range": "± 2270.707 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6",
            "value": 0.882,
            "range": "± 0.015 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6_x1000000",
            "value": 883081.311,
            "range": "± 5697.135 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6",
            "value": 0.912,
            "range": "± 0.003 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6_x1000000",
            "value": 899001.343,
            "range": "± 4940.817 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6",
            "value": 0.896,
            "range": "± 0.025 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6_x1000000",
            "value": 896696.529,
            "range": "± 5021.227 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_countones_w64",
            "value": 62567.032,
            "range": "± 1435.994 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1",
            "value": 0.737,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1000000",
            "value": 741766.676,
            "range": "± 2923.172 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_std_counter_w32",
            "value": 7836.404,
            "range": "± 224.251 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1",
            "value": 0.008,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1000000",
            "value": 8720.332,
            "range": "± 254.950 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_std_counter_w32_x1000000",
            "value": 31117.469,
            "range": "± 1159.967 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_gray_counter_w32",
            "value": 11083.591,
            "range": "± 80.874 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_gray_counter_w32_x1",
            "value": 0.012,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_gray_counter_w32_x1000000",
            "value": 12248.208,
            "range": "± 226.175 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_gray_counter_w32_x1000000",
            "value": 34604.18,
            "range": "± 655.624 us",
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
          "id": "56e76f9dcf138d2ea4f734b2bc5a4c6b04005dfd",
          "message": "Remove trueLoops section from combinational-loops guide\n\ntrueLoops cannot be used with Veryl sources due to the analyzer's\nUnassignVariable check on self-referential assigns. Remove the\nsection entirely rather than documenting an unusable API.",
          "timestamp": "2026-03-04T16:01:38Z",
          "tree_id": "e830352a6560d005e57d0a820d545557fedc86a5",
          "url": "https://github.com/celox-sim/celox/commit/56e76f9dcf138d2ea4f734b2bc5a4c6b04005dfd"
        },
        "date": 1772641536277,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "rust/simulator_tick_x10000",
            "value": 1595.195,
            "range": "± 71.619 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_step_x20000",
            "value": 9891.773,
            "range": "± 215.493 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_top_n1000",
            "value": 728733.749,
            "range": "± 10343.991 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1",
            "value": 0.158,
            "range": "± 0.004 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1000000",
            "value": 159074.039,
            "range": "± 1420.226 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1",
            "value": 0.31,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1000000",
            "value": 262270.41,
            "range": "± 3005.109 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_linear_sec_p6",
            "value": 169704.531,
            "range": "± 1925.314 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1",
            "value": 0.956,
            "range": "± 0.006 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1000000",
            "value": 947295.068,
            "range": "± 2638.172 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_eval_linear_sec_p6_x1000000",
            "value": 952294.029,
            "range": "± 2808.635 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6",
            "value": 0.887,
            "range": "± 0.009 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6_x1000000",
            "value": 890819.138,
            "range": "± 3372.881 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6",
            "value": 0.891,
            "range": "± 0.007 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6_x1000000",
            "value": 887918.041,
            "range": "± 2405.213 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6",
            "value": 0.885,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6_x1000000",
            "value": 887768.521,
            "range": "± 3566.102 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6",
            "value": 0.911,
            "range": "± 0.002 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6_x1000000",
            "value": 892825.42,
            "range": "± 4396.427 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_countones_w64",
            "value": 64864.504,
            "range": "± 1017.193 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1",
            "value": 0.738,
            "range": "± 0.030 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1000000",
            "value": 739643.115,
            "range": "± 2964.924 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_std_counter_w32",
            "value": 7989.539,
            "range": "± 160.277 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1",
            "value": 0.008,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1000000",
            "value": 8667.447,
            "range": "± 48.907 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_std_counter_w32_x1000000",
            "value": 31128.74,
            "range": "± 173.043 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_gray_counter_w32",
            "value": 11375.895,
            "range": "± 262.515 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_gray_counter_w32_x1",
            "value": 0.012,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_gray_counter_w32_x1000000",
            "value": 12163.23,
            "range": "± 74.430 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_gray_counter_w32_x1000000",
            "value": 34921.572,
            "range": "± 168.283 us",
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
          "id": "9b05b617839ec740f344230e5c4c6df121f6fb3e",
          "message": "Add Dead Store Elimination guide pages and update related docs\n\n- New guide: docs/guide/dead-store-elimination.md (EN)\n- New guide: docs/ja/guide/dead-store-elimination.md (JA)\n- Add sidebar entries in config.mts\n- Update hierarchy guides with DSE interaction notes\n- Update vite-plugin guides with ?dse= query parameter docs\n- Update writing-tests guides with deadStorePolicy option",
          "timestamp": "2026-03-04T19:03:47Z",
          "tree_id": "fa05eea29a5e4ec9777dfca78dba22409bbbe927",
          "url": "https://github.com/celox-sim/celox/commit/9b05b617839ec740f344230e5c4c6df121f6fb3e"
        },
        "date": 1772652621634,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "rust/simulator_tick_x10000",
            "value": 1594.426,
            "range": "± 16.800 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_step_x20000",
            "value": 9786.337,
            "range": "± 373.612 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_top_n1000",
            "value": 775789.578,
            "range": "± 9531.588 us",
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
            "value": 159541.133,
            "range": "± 784.264 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1",
            "value": 0.286,
            "range": "± 0.004 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1000000",
            "value": 272906.449,
            "range": "± 2237.992 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_linear_sec_p6",
            "value": 185910.972,
            "range": "± 2062.859 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1",
            "value": 0.948,
            "range": "± 0.002 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1000000",
            "value": 953745.206,
            "range": "± 1445.793 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_eval_linear_sec_p6_x1000000",
            "value": 957136.014,
            "range": "± 873.179 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6",
            "value": 0.887,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6_x1000000",
            "value": 891120.009,
            "range": "± 1499.846 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6",
            "value": 0.891,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6_x1000000",
            "value": 900774.008,
            "range": "± 633.196 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6",
            "value": 0.91,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6_x1000000",
            "value": 899642.056,
            "range": "± 1083.917 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6",
            "value": 0.905,
            "range": "± 0.002 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6_x1000000",
            "value": 902393.594,
            "range": "± 1236.497 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_countones_w64",
            "value": 72179.852,
            "range": "± 1019.229 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1",
            "value": 0.47,
            "range": "± 0.003 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1000000",
            "value": 476450.862,
            "range": "± 824.996 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_std_counter_w32",
            "value": 7933.471,
            "range": "± 272.947 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1",
            "value": 0.008,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1000000",
            "value": 9016.07,
            "range": "± 17.709 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_std_counter_w32_x1000000",
            "value": 31453.186,
            "range": "± 73.292 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_gray_counter_w32",
            "value": 11294.484,
            "range": "± 93.538 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_gray_counter_w32_x1",
            "value": 0.012,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_gray_counter_w32_x1000000",
            "value": 12468.874,
            "range": "± 39.027 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_gray_counter_w32_x1000000",
            "value": 36997.259,
            "range": "± 147.758 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_countones_w64",
            "value": 58538.495,
            "range": "± 502.439 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_countones_w64_x1",
            "value": 0.077,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_countones_w64_x1000000",
            "value": 77357.617,
            "range": "± 168.911 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_linear_sec_p6",
            "value": 181775.962,
            "range": "± 2383.191 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1",
            "value": 0.91,
            "range": "± 0.002 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1000000",
            "value": 917442.093,
            "range": "± 511.413 us",
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
          "id": "18488642321e897a9719ef2403a55c2cdbe6f577",
          "message": "Merge branch 'claude/ts-tb-proto-fn'",
          "timestamp": "2026-03-04T20:35:42Z",
          "tree_id": "8ba9150ab61b8fdbd67d85e07bda97f595cb6a0c",
          "url": "https://github.com/celox-sim/celox/commit/18488642321e897a9719ef2403a55c2cdbe6f577"
        },
        "date": 1772658117804,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "rust/simulator_tick_x10000",
            "value": 2127.492,
            "range": "± 3.556 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_step_x20000",
            "value": 9718.349,
            "range": "± 28.542 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_top_n1000",
            "value": 776469.329,
            "range": "± 11092.528 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1",
            "value": 0.16,
            "range": "± 0.004 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1000000",
            "value": 159475.792,
            "range": "± 263.414 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1",
            "value": 0.268,
            "range": "± 0.002 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1000000",
            "value": 264208.712,
            "range": "± 578.288 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_linear_sec_p6",
            "value": 183497.546,
            "range": "± 2180.164 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1",
            "value": 0.945,
            "range": "± 0.003 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1000000",
            "value": 944370.307,
            "range": "± 2020.550 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_eval_linear_sec_p6_x1000000",
            "value": 943185.964,
            "range": "± 1985.757 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6",
            "value": 0.891,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6_x1000000",
            "value": 891484.511,
            "range": "± 380.974 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6",
            "value": 0.894,
            "range": "± 0.016 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6_x1000000",
            "value": 894102.392,
            "range": "± 586.300 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6",
            "value": 0.907,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6_x1000000",
            "value": 896423.826,
            "range": "± 1833.310 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6",
            "value": 0.911,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6_x1000000",
            "value": 899886.339,
            "range": "± 372.435 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_countones_w64",
            "value": 72995.509,
            "range": "± 776.943 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1",
            "value": 0.466,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1000000",
            "value": 473851.013,
            "range": "± 907.959 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_std_counter_w32",
            "value": 7949.434,
            "range": "± 105.433 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1",
            "value": 0.008,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1000000",
            "value": 8747.722,
            "range": "± 27.780 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_std_counter_w32_x1000000",
            "value": 30653.542,
            "range": "± 112.418 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_gray_counter_w32",
            "value": 11239.443,
            "range": "± 83.851 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_gray_counter_w32_x1",
            "value": 0.012,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_gray_counter_w32_x1000000",
            "value": 12555.813,
            "range": "± 52.571 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_gray_counter_w32_x1000000",
            "value": 35863.132,
            "range": "± 133.660 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_countones_w64",
            "value": 56043.833,
            "range": "± 878.734 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_countones_w64_x1",
            "value": 0.076,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_countones_w64_x1000000",
            "value": 77313.872,
            "range": "± 318.418 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_linear_sec_p6",
            "value": 178104.558,
            "range": "± 1494.008 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1",
            "value": 0.907,
            "range": "± 0.002 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1000000",
            "value": 907107.11,
            "range": "± 623.725 us",
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
          "id": "4c12e240363179cc33ab7db1a8f9a51e2c728dfd",
          "message": "Fix biome formatting in packages",
          "timestamp": "2026-03-04T22:24:33Z",
          "tree_id": "8a081bcc1f996822bebea056ec77017cafcb0119",
          "url": "https://github.com/celox-sim/celox/commit/4c12e240363179cc33ab7db1a8f9a51e2c728dfd"
        },
        "date": 1772664694480,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "rust/simulator_tick_x10000",
            "value": 1590.737,
            "range": "± 20.617 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_step_x20000",
            "value": 9729.697,
            "range": "± 16.610 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_top_n1000",
            "value": 743242.41,
            "range": "± 8638.961 us",
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
            "value": 159108.655,
            "range": "± 316.551 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1",
            "value": 0.264,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1000000",
            "value": 294064.944,
            "range": "± 229.754 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_linear_sec_p6",
            "value": 188737.733,
            "range": "± 2252.740 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1",
            "value": 0.952,
            "range": "± 0.004 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1000000",
            "value": 950360.26,
            "range": "± 2576.412 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_eval_linear_sec_p6_x1000000",
            "value": 950611.854,
            "range": "± 1746.539 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6",
            "value": 0.888,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6_x1000000",
            "value": 889065.327,
            "range": "± 2958.746 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6",
            "value": 0.902,
            "range": "± 0.004 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6_x1000000",
            "value": 890296.828,
            "range": "± 804.158 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6",
            "value": 0.902,
            "range": "± 0.002 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6_x1000000",
            "value": 894692.779,
            "range": "± 736.086 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6",
            "value": 0.905,
            "range": "± 0.020 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6_x1000000",
            "value": 899055.532,
            "range": "± 2048.129 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_countones_w64",
            "value": 70716.432,
            "range": "± 1318.765 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1",
            "value": 0.467,
            "range": "± 0.003 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1000000",
            "value": 473551.983,
            "range": "± 544.883 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_std_counter_w32",
            "value": 8861.084,
            "range": "± 349.252 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1",
            "value": 0.008,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1000000",
            "value": 8795.148,
            "range": "± 22.908 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_std_counter_w32_x1000000",
            "value": 31345.83,
            "range": "± 65.606 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_gray_counter_w32",
            "value": 12620.475,
            "range": "± 215.470 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_gray_counter_w32_x1",
            "value": 0.012,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_gray_counter_w32_x1000000",
            "value": 12720.297,
            "range": "± 60.790 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_gray_counter_w32_x1000000",
            "value": 36096.902,
            "range": "± 106.504 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_countones_w64",
            "value": 55122.7,
            "range": "± 874.446 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_countones_w64_x1",
            "value": 0.075,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_countones_w64_x1000000",
            "value": 78870.395,
            "range": "± 514.448 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_linear_sec_p6",
            "value": 181145.721,
            "range": "± 3312.956 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1",
            "value": 0.901,
            "range": "± 0.004 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1000000",
            "value": 913568.103,
            "range": "± 3403.775 us",
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
          "id": "744f8bd0d66ee1b5dd2ca605da2873439a55dde5",
          "message": "Fix biome formatting in simulation.ts",
          "timestamp": "2026-03-05T00:04:46Z",
          "tree_id": "b92fab63708fc94462c42e04fa6720f665fdabe9",
          "url": "https://github.com/celox-sim/celox/commit/744f8bd0d66ee1b5dd2ca605da2873439a55dde5"
        },
        "date": 1772670662327,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "rust/simulator_tick_x10000",
            "value": 1624.188,
            "range": "± 9.243 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_step_x20000",
            "value": 9746.904,
            "range": "± 43.923 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_top_n1000",
            "value": 772939.873,
            "range": "± 10582.510 us",
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
            "value": 159370.595,
            "range": "± 234.939 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1",
            "value": 0.325,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1000000",
            "value": 326652.465,
            "range": "± 2197.550 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_linear_sec_p6",
            "value": 184151.837,
            "range": "± 1418.694 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1",
            "value": 0.951,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1000000",
            "value": 957514.035,
            "range": "± 1646.298 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_eval_linear_sec_p6_x1000000",
            "value": 955160.856,
            "range": "± 1475.854 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6",
            "value": 0.886,
            "range": "± 0.002 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6_x1000000",
            "value": 886126.599,
            "range": "± 947.972 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6",
            "value": 0.887,
            "range": "± 0.003 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6_x1000000",
            "value": 890944.631,
            "range": "± 703.290 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6",
            "value": 0.894,
            "range": "± 0.004 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6_x1000000",
            "value": 903582.64,
            "range": "± 2288.125 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6",
            "value": 0.91,
            "range": "± 0.002 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6_x1000000",
            "value": 907280.446,
            "range": "± 1881.546 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_countones_w64",
            "value": 73215.68,
            "range": "± 850.060 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1",
            "value": 0.47,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1000000",
            "value": 477168.474,
            "range": "± 201.981 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_std_counter_w32",
            "value": 7955.792,
            "range": "± 119.949 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1",
            "value": 0.008,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1000000",
            "value": 8826.558,
            "range": "± 48.285 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_std_counter_w32_x1000000",
            "value": 31927.423,
            "range": "± 101.197 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_gray_counter_w32",
            "value": 11269.601,
            "range": "± 116.806 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_gray_counter_w32_x1",
            "value": 0.012,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_gray_counter_w32_x1000000",
            "value": 12465.351,
            "range": "± 61.712 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_gray_counter_w32_x1000000",
            "value": 36039.073,
            "range": "± 62.257 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_countones_w64",
            "value": 57741.949,
            "range": "± 1206.844 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_countones_w64_x1",
            "value": 0.077,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_countones_w64_x1000000",
            "value": 77169.361,
            "range": "± 210.984 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_linear_sec_p6",
            "value": 180736.157,
            "range": "± 2185.265 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1",
            "value": 0.909,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1000000",
            "value": 916001.934,
            "range": "± 698.490 us",
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
          "id": "8e74e819769db8f1ae2925fef0076b3d00cf66bc",
          "message": "Fix cargo fmt formatting in celox-ts-gen test",
          "timestamp": "2026-03-05T03:15:52Z",
          "tree_id": "5d4b3d26af990f8d49733cc64bdca2e2b0c349d3",
          "url": "https://github.com/celox-sim/celox/commit/8e74e819769db8f1ae2925fef0076b3d00cf66bc"
        },
        "date": 1772682103441,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "rust/simulator_tick_x10000",
            "value": 1965.933,
            "range": "± 32.996 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_step_x20000",
            "value": 8978.322,
            "range": "± 25.566 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_top_n1000",
            "value": 824842.364,
            "range": "± 4625.202 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1",
            "value": 0.196,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1000000",
            "value": 196766.221,
            "range": "± 15964.748 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1",
            "value": 0.216,
            "range": "± 0.005 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1000000",
            "value": 208832.181,
            "range": "± 2073.929 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_linear_sec_p6",
            "value": 188999.429,
            "range": "± 977.139 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1",
            "value": 0.922,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1000000",
            "value": 926493.7,
            "range": "± 1405.049 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_eval_linear_sec_p6_x1000000",
            "value": 923425.97,
            "range": "± 989.088 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6",
            "value": 0.844,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6_x1000000",
            "value": 845511.462,
            "range": "± 6608.566 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6",
            "value": 0.846,
            "range": "± 0.003 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6_x1000000",
            "value": 846826.888,
            "range": "± 724.488 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6",
            "value": 0.848,
            "range": "± 0.003 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6_x1000000",
            "value": 848654.522,
            "range": "± 2460.797 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6",
            "value": 0.876,
            "range": "± 0.010 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6_x1000000",
            "value": 851767.616,
            "range": "± 2092.461 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_countones_w64",
            "value": 75189.937,
            "range": "± 724.645 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1",
            "value": 0.407,
            "range": "± 0.002 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1000000",
            "value": 411132.069,
            "range": "± 214.852 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_std_counter_w32",
            "value": 8319.382,
            "range": "± 117.890 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1",
            "value": 0.008,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1000000",
            "value": 8377.057,
            "range": "± 236.319 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_std_counter_w32_x1000000",
            "value": 31337.578,
            "range": "± 209.175 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_gray_counter_w32",
            "value": 11832.696,
            "range": "± 205.571 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_gray_counter_w32_x1",
            "value": 0.012,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_gray_counter_w32_x1000000",
            "value": 12159.564,
            "range": "± 18.237 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_gray_counter_w32_x1000000",
            "value": 34425.033,
            "range": "± 93.684 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_countones_w64",
            "value": 58373.734,
            "range": "± 694.185 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_countones_w64_x1",
            "value": 0.084,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_countones_w64_x1000000",
            "value": 84854.981,
            "range": "± 1077.644 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_linear_sec_p6",
            "value": 185462.871,
            "range": "± 792.806 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1",
            "value": 0.881,
            "range": "± 0.005 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1000000",
            "value": 890877.905,
            "range": "± 2541.303 us",
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
          "id": "e01768994102e89ea3943b884006429d5a0ceb28",
          "message": "Fix cargo fmt formatting in eval_factor",
          "timestamp": "2026-03-05T12:12:58Z",
          "tree_id": "e1b80a2afff3b8612ea03e3a0f296c2a8e15641f",
          "url": "https://github.com/celox-sim/celox/commit/e01768994102e89ea3943b884006429d5a0ceb28"
        },
        "date": 1772714138946,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "rust/simulator_tick_x10000",
            "value": 1593.701,
            "range": "± 28.747 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_step_x20000",
            "value": 9899.791,
            "range": "± 269.255 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_top_n1000",
            "value": 774823.839,
            "range": "± 9457.412 us",
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
            "value": 159203.38,
            "range": "± 508.618 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1",
            "value": 0.324,
            "range": "± 0.007 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1000000",
            "value": 280542.077,
            "range": "± 4003.788 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_linear_sec_p6",
            "value": 149151.618,
            "range": "± 1883.162 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1",
            "value": 0.643,
            "range": "± 0.002 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1000000",
            "value": 649089.414,
            "range": "± 2873.931 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_eval_linear_sec_p6_x1000000",
            "value": 652615.734,
            "range": "± 732.510 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6",
            "value": 0.597,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6_x1000000",
            "value": 597354.249,
            "range": "± 182.262 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6",
            "value": 0.598,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6_x1000000",
            "value": 597945.888,
            "range": "± 1266.928 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6",
            "value": 0.6,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6_x1000000",
            "value": 602005.785,
            "range": "± 698.029 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6",
            "value": 0.605,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6_x1000000",
            "value": 601815.715,
            "range": "± 546.627 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_countones_w64",
            "value": 73307.49,
            "range": "± 1267.183 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1",
            "value": 0.479,
            "range": "± 0.004 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1000000",
            "value": 484038.127,
            "range": "± 1944.566 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_std_counter_w32",
            "value": 7931.8,
            "range": "± 137.498 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1",
            "value": 0.008,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1000000",
            "value": 8686.901,
            "range": "± 18.542 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_std_counter_w32_x1000000",
            "value": 32082.347,
            "range": "± 56.561 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_gray_counter_w32",
            "value": 11414.977,
            "range": "± 240.432 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_gray_counter_w32_x1",
            "value": 0.011,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_gray_counter_w32_x1000000",
            "value": 11601.57,
            "range": "± 42.717 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_gray_counter_w32_x1000000",
            "value": 34602.233,
            "range": "± 77.383 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_countones_w64",
            "value": 57243.113,
            "range": "± 1178.767 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_countones_w64_x1",
            "value": 0.08,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_countones_w64_x1000000",
            "value": 85351.068,
            "range": "± 905.662 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_linear_sec_p6",
            "value": 145786.746,
            "range": "± 2753.614 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1",
            "value": 0.608,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1000000",
            "value": 620938.701,
            "range": "± 275.045 us",
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
        "date": 1772557784280,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ts/simulation_build_top_n1000",
            "value": 798.4436923333329,
            "range": "± 1.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1",
            "value": 0.0005169113890033119,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "967284 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1000000",
            "value": 409.480703999999,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1",
            "value": 0.0008699074384028798,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "574774 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1000000",
            "value": 726.8488260000013,
            "range": "± 1.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1",
            "value": 0.0008635135433378447,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "579030 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1000000",
            "value": 715.0113236666657,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulator_tick_x10000",
            "value": 4.125522000002093,
            "range": "± 1.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_step_x20000",
            "value": 13.718732333334628,
            "range": "± 0.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_build_top_n1000",
            "value": 793.8406743333351,
            "range": "± 2.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_step_x1",
            "value": 0.0008328251452842325,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "600367 samples"
          },
          {
            "name": "ts/simulation_time_step_x1000000",
            "value": 676.6442203333306,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_runUntil_1000000",
            "value": 102.62149366666806,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/waitForCycles_x1000",
            "value": 0.7936616666653814,
            "range": "± 3.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/manual_step_loop_x2000",
            "value": 0.78808733332941,
            "range": "± 4.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_fast_path_100000",
            "value": 4.379875333334591,
            "range": "± 3.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_guarded_100000",
            "value": 9.441419666666965,
            "range": "± 0.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_without_optimize",
            "value": 806.8881546666671,
            "range": "± 2.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_with_optimize",
            "value": 814.2777486666697,
            "range": "± 1.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_without_optimize",
            "value": 4.126074666671532,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_with_optimize",
            "value": 4.0916469999986775,
            "range": "± 0.0%",
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
          "id": "0f0c6ec79d1978cac82e3e3d187a1e3a2160ea0f",
          "message": "Rebase veryl submodule onto upstream master and adapt to IR API changes\n\n- Update deps/veryl to upstream/master (10b3d0d0)\n- Add Box<Comptime> field to Expression variant patterns and constructors\n- Remove TokenRange from Factor::Variable/Value/FunctionCall patterns\n- Migrate FunctionCall.ret to FunctionCall.comptime\n- Guard eval_constexpr with is_const/evaluated checks to prevent\n  non-constant variables (now carrying Numeric Comptime) from being\n  treated as compile-time constants",
          "timestamp": "2026-03-03T18:04:03Z",
          "tree_id": "2f9bec6e79b3772a6749a12e3b33bcc493186ca6",
          "url": "https://github.com/celox-sim/celox/commit/0f0c6ec79d1978cac82e3e3d187a1e3a2160ea0f"
        },
        "date": 1772562003196,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ts/simulation_build_top_n1000",
            "value": 768.9239150000006,
            "range": "± 0.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1",
            "value": 0.0005438934110600172,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "919298 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1000000",
            "value": 404.20215266666736,
            "range": "± 0.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1",
            "value": 0.0008862932847405247,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "564148 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1000000",
            "value": 723.5537263333341,
            "range": "± 0.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1",
            "value": 0.0008744305728755969,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "571801 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1000000",
            "value": 712.2683509999964,
            "range": "± 3.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulator_tick_x10000",
            "value": 4.349402333335699,
            "range": "± 2.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_step_x20000",
            "value": 13.626845666665758,
            "range": "± 1.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_build_top_n1000",
            "value": 780.1909470000004,
            "range": "± 4.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_step_x1",
            "value": 0.0008333641516859779,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "599978 samples"
          },
          {
            "name": "ts/simulation_time_step_x1000000",
            "value": 673.5640836666668,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_runUntil_1000000",
            "value": 102.39227400000043,
            "range": "± 0.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/waitForCycles_x1000",
            "value": 0.8651603333322176,
            "range": "± 73.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/manual_step_loop_x2000",
            "value": 0.7208396666659004,
            "range": "± 3.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_fast_path_100000",
            "value": 4.271553333329696,
            "range": "± 1.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_guarded_100000",
            "value": 8.97778533333379,
            "range": "± 1.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_without_optimize",
            "value": 775.2809549999996,
            "range": "± 4.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_with_optimize",
            "value": 776.7564433333367,
            "range": "± 0.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_without_optimize",
            "value": 4.348608000005091,
            "range": "± 0.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_with_optimize",
            "value": 4.029598999996476,
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
          "id": "e743cbc7e8b0ffc86ba7eda6fa854313fe5ff267",
          "message": "Rewrite benchmark dashboard with tab-based layout and mini-chart grid\n\nReplace the flat checkbox list + single shared chart with a tab-based UI\n(Counter, Std Library, API, Optimize) and a responsive 2-column grid of\nmini-charts. Each card groups the same operation across runtimes (max 3\nlines: Rust/TS/Verilator) with clickable Chart.js legends. Tabs use\npriority-based matching (API > Optimize > Stdlib > Counter) and sub-group\nsections for Std Library and API tabs.",
          "timestamp": "2026-03-03T20:42:12Z",
          "tree_id": "8a36ba14fc102b953283d48e8eca7c0650b2b4a2",
          "url": "https://github.com/celox-sim/celox/commit/e743cbc7e8b0ffc86ba7eda6fa854313fe5ff267"
        },
        "date": 1772571516685,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ts/simulation_build_top_n1000",
            "value": 775.1585160000001,
            "range": "± 0.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1",
            "value": 0.000511182873542048,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "978124 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1000000",
            "value": 368.67410400000034,
            "range": "± 0.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1",
            "value": 0.0008404393648290873,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "594927 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1000000",
            "value": 715.2190183333345,
            "range": "± 0.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1",
            "value": 0.0008403117468694031,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "595018 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1000000",
            "value": 712.5215870000005,
            "range": "± 1.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulator_tick_x10000",
            "value": 3.6799640000002305,
            "range": "± 2.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_step_x20000",
            "value": 13.458135333334212,
            "range": "± 0.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_build_top_n1000",
            "value": 776.9493563333357,
            "range": "± 1.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_step_x1",
            "value": 0.0008145842238081306,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "613811 samples"
          },
          {
            "name": "ts/simulation_time_step_x1000000",
            "value": 675.5520553333336,
            "range": "± 0.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_runUntil_1000000",
            "value": 101.99991766666547,
            "range": "± 0.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/waitForCycles_x1000",
            "value": 0.7218276666681049,
            "range": "± 3.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/manual_step_loop_x2000",
            "value": 0.7106399999950858,
            "range": "± 6.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_fast_path_100000",
            "value": 4.3426536666665925,
            "range": "± 2.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_guarded_100000",
            "value": 8.932810999998765,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_without_optimize",
            "value": 790.4810376666695,
            "range": "± 6.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_with_optimize",
            "value": 778.3699546666637,
            "range": "± 3.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_without_optimize",
            "value": 3.744684333326101,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_with_optimize",
            "value": 3.8767506666627014,
            "range": "± 1.6%",
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
          "id": "80644b5dd01a28cae84104a0978bb48136313d38",
          "message": "Ignore tests blocked by upstream Veryl IR bugs and update SIR snapshot\n\n- array_literal: 2 tests using `'{0}` (UnsupportedByIr at conv/utils.rs:231)\n- for_loop_unroll: 1 test using `'{0}` (same IR issue)\n- compare_matrix: 3 tests producing incorrect simulation results\n- param_override: 2 tests producing incorrect simulation results\n- false_loop: accept SIR snapshot diff from upstream register renumbering",
          "timestamp": "2026-03-03T22:42:09Z",
          "tree_id": "3938e4585335dc56d0411c597a60ebe8ff6c7665",
          "url": "https://github.com/celox-sim/celox/commit/80644b5dd01a28cae84104a0978bb48136313d38"
        },
        "date": 1772578697831,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ts/simulation_build_top_n1000",
            "value": 786.3906186666669,
            "range": "± 3.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1",
            "value": 0.000526711442376843,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "949287 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1000000",
            "value": 394.60933800000083,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1",
            "value": 0.0008692084970316473,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "575236 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1000000",
            "value": 715.6959756666669,
            "range": "± 0.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1",
            "value": 0.0008542988128705604,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "585276 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1000000",
            "value": 715.3360590000011,
            "range": "± 0.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulator_tick_x10000",
            "value": 3.91626233333227,
            "range": "± 2.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_step_x20000",
            "value": 13.322239999998905,
            "range": "± 0.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_build_top_n1000",
            "value": 784.6249286666667,
            "range": "± 2.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_step_x1",
            "value": 0.0008206574955564979,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "609268 samples"
          },
          {
            "name": "ts/simulation_time_step_x1000000",
            "value": 667.3625546666711,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_runUntil_1000000",
            "value": 101.52232033333469,
            "range": "± 1.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/waitForCycles_x1000",
            "value": 0.7024246666672601,
            "range": "± 5.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/manual_step_loop_x2000",
            "value": 0.7312953333336433,
            "range": "± 13.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_fast_path_100000",
            "value": 4.313041666670567,
            "range": "± 0.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_guarded_100000",
            "value": 9.120949000001323,
            "range": "± 8.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_without_optimize",
            "value": 797.3246603333342,
            "range": "± 6.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_with_optimize",
            "value": 774.4107346666666,
            "range": "± 2.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_without_optimize",
            "value": 4.2497933333312785,
            "range": "± 3.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_with_optimize",
            "value": 4.0030266666726675,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_linear_sec_p6",
            "value": 241698.08266666465,
            "range": "± 0.5%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1",
            "value": 1.5544502763711072,
            "range": "± 0.3%",
            "unit": "us",
            "extra": "321658 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1000000",
            "value": 1418056.9009999987,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_eval_linear_sec_p6_x1000000",
            "value": 1357382.3256666656,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_countones_w64",
            "value": 108459.00633333561,
            "range": "± 0.4%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1",
            "value": 1.33574697787304,
            "range": "± 0.4%",
            "unit": "us",
            "extra": "374323 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1000000",
            "value": 1196268.3389999971,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_std_counter_w32",
            "value": 8712.108666664184,
            "range": "± 5.7%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1",
            "value": 0.22297408194825208,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "2242414 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1000000",
            "value": 116446.19466666579,
            "range": "± 0.6%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_std_counter_w32_x1000000",
            "value": 137698.99533332986,
            "range": "± 0.7%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_gray_counter_w32",
            "value": 12224.719333336301,
            "range": "± 3.6%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1",
            "value": 0.22771068587013785,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "2195769 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1000000",
            "value": 117689.76033333456,
            "range": "± 1.2%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_gray_counter_w32_x1000000",
            "value": 135254.2346666693,
            "range": "± 0.2%",
            "unit": "us",
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
          "id": "2e10bb451c3b3260b7ef3217d8e8d21ce7f813b4",
          "message": "Skip param override E2E test blocked by upstream Veryl IR bug",
          "timestamp": "2026-03-03T23:14:57Z",
          "tree_id": "6ac83fa262c7d08f2d2bf174c267e87c74ec8302",
          "url": "https://github.com/celox-sim/celox/commit/2e10bb451c3b3260b7ef3217d8e8d21ce7f813b4"
        },
        "date": 1772580734543,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ts/simulation_build_top_n1000",
            "value": 781.630925333333,
            "range": "± 1.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1",
            "value": 0.0005187717632538259,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "963815 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1000000",
            "value": 389.80716099999944,
            "range": "± 1.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1",
            "value": 0.0008716727829045593,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "573610 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1000000",
            "value": 743.3138976666669,
            "range": "± 0.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1",
            "value": 0.0008529552830271582,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "586198 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1000000",
            "value": 722.4867593333378,
            "range": "± 0.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulator_tick_x10000",
            "value": 4.000867000000047,
            "range": "± 1.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_step_x20000",
            "value": 13.307069666666697,
            "range": "± 0.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_build_top_n1000",
            "value": 764.1351183333343,
            "range": "± 3.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_step_x1",
            "value": 0.0008163131143741609,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "612511 samples"
          },
          {
            "name": "ts/simulation_time_step_x1000000",
            "value": 670.2913213333328,
            "range": "± 0.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_runUntil_1000000",
            "value": 101.06347666666989,
            "range": "± 0.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/waitForCycles_x1000",
            "value": 0.7116553333350263,
            "range": "± 3.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/manual_step_loop_x2000",
            "value": 0.7013763333355504,
            "range": "± 2.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_fast_path_100000",
            "value": 4.283893333333253,
            "range": "± 3.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_guarded_100000",
            "value": 9.556999333331381,
            "range": "± 0.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_without_optimize",
            "value": 772.0192203333306,
            "range": "± 4.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_with_optimize",
            "value": 769.9143493333249,
            "range": "± 1.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_without_optimize",
            "value": 3.84227199999926,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_with_optimize",
            "value": 3.9006476666691015,
            "range": "± 0.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_linear_sec_p6",
            "value": 234566.61200000477,
            "range": "± 0.4%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1",
            "value": 1.548811832275631,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "322829 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1000000",
            "value": 1402751.0230000091,
            "range": "± 0.0%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_eval_linear_sec_p6_x1000000",
            "value": 1352890.5920000058,
            "range": "± 0.8%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_countones_w64",
            "value": 107339.96333333683,
            "range": "± 2.3%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1",
            "value": 1.3315963247993396,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "375490 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1000000",
            "value": 1201196.2379999992,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_std_counter_w32",
            "value": 8354.050666666202,
            "range": "± 2.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1",
            "value": 0.2230395391080736,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "2241755 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1000000",
            "value": 110212.94566666377,
            "range": "± 0.0%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_std_counter_w32_x1000000",
            "value": 131001.6459999994,
            "range": "± 0.6%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_gray_counter_w32",
            "value": 11724.65333332851,
            "range": "± 0.6%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1",
            "value": 0.23019792554859037,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "2172044 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1000000",
            "value": 121234.8639999982,
            "range": "± 2.4%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_gray_counter_w32_x1000000",
            "value": 132809.12999999904,
            "range": "± 0.5%",
            "unit": "us",
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
          "id": "db6aada5f195701837eaa84b1f84c6795f42eada",
          "message": "Optimize SIR codegen: bit-extract peephole, shift-by-0 elim, store coalescing fix\n\nThree optimizations targeting eval_comb JIT performance:\n\n1. Shift-by-0 elimination (codegen): Track Imm constants in TranslationState\n   and skip Shr/Shl/Sar when shift amount is 0, emitting identity instead.\n\n2. BitExtractPeepholePass (optimizer): Replace Load(N)+Shr(K)+And((1<<W)-1)\n   chains with a single narrow Load(W, offset=base+K).\n\n3. Store coalescing fix (optimizer): Run store coalescing twice in\n   optimize_block — before and after eliminate_redundant_loads — so that\n   stores unblocked by load elimination can still be coalesced.\n\nLinear SEC P=6 results: SIR instructions -70%, stores -88%, ~7.6% faster.",
          "timestamp": "2026-03-04T06:01:52Z",
          "tree_id": "c43771cad3a187cc15c4450c8b1ba0f9e142819a",
          "url": "https://github.com/celox-sim/celox/commit/db6aada5f195701837eaa84b1f84c6795f42eada"
        },
        "date": 1772605530791,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ts/simulation_build_top_n1000",
            "value": 778.1908636666667,
            "range": "± 0.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1",
            "value": 0.0005140120237682732,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "972740 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1000000",
            "value": 397.0995613333337,
            "range": "± 0.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1",
            "value": 0.0008553373020926547,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "584565 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1000000",
            "value": 707.5914699999994,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1",
            "value": 0.0008396852611911961,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "595462 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1000000",
            "value": 706.2348816666685,
            "range": "± 1.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulator_tick_x10000",
            "value": 3.8934546666641836,
            "range": "± 0.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_step_x20000",
            "value": 13.412514000003284,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_build_top_n1000",
            "value": 797.45794866667,
            "range": "± 0.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_step_x1",
            "value": 0.0007999103333900035,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "625071 samples"
          },
          {
            "name": "ts/simulation_time_step_x1000000",
            "value": 672.2299526666685,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_runUntil_1000000",
            "value": 101.62960033333366,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/waitForCycles_x1000",
            "value": 0.7331586666647733,
            "range": "± 4.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/manual_step_loop_x2000",
            "value": 0.730176333335597,
            "range": "± 2.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_fast_path_100000",
            "value": 4.311508000001292,
            "range": "± 1.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_guarded_100000",
            "value": 8.952187999995658,
            "range": "± 1.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_without_optimize",
            "value": 785.7553710000017,
            "range": "± 1.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_with_optimize",
            "value": 786.7298623333336,
            "range": "± 1.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_without_optimize",
            "value": 3.9110376666649245,
            "range": "± 0.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_with_optimize",
            "value": 3.899004999999306,
            "range": "± 0.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_linear_sec_p6",
            "value": 181231.22599999866,
            "range": "± 2.5%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1",
            "value": 1.5565487603740522,
            "range": "± 0.4%",
            "unit": "us",
            "extra": "321224 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1000000",
            "value": 1407957.3996666684,
            "range": "± 0.4%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_eval_linear_sec_p6_x1000000",
            "value": 1343823.8933333294,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_countones_w64",
            "value": 69564.37300000107,
            "range": "± 0.9%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1",
            "value": 1.013472313999026,
            "range": "± 0.4%",
            "unit": "us",
            "extra": "493354 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1000000",
            "value": 874819.9766666658,
            "range": "± 7.6%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_std_counter_w32",
            "value": 8782.290666664872,
            "range": "± 6.3%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1",
            "value": 0.19773644428028833,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "2528619 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1000000",
            "value": 112383.62866666769,
            "range": "± 0.6%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_std_counter_w32_x1000000",
            "value": 128600.11566666557,
            "range": "± 0.7%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_gray_counter_w32",
            "value": 12528.928666661765,
            "range": "± 6.4%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1",
            "value": 0.20234404030054995,
            "range": "± 0.4%",
            "unit": "us",
            "extra": "2471039 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1000000",
            "value": 111908.16133333526,
            "range": "± 0.6%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_gray_counter_w32_x1000000",
            "value": 131821.17600000734,
            "range": "± 0.6%",
            "unit": "us",
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
          "id": "56e76f9dcf138d2ea4f734b2bc5a4c6b04005dfd",
          "message": "Remove trueLoops section from combinational-loops guide\n\ntrueLoops cannot be used with Veryl sources due to the analyzer's\nUnassignVariable check on self-referential assigns. Remove the\nsection entirely rather than documenting an unusable API.",
          "timestamp": "2026-03-04T16:01:38Z",
          "tree_id": "e830352a6560d005e57d0a820d545557fedc86a5",
          "url": "https://github.com/celox-sim/celox/commit/56e76f9dcf138d2ea4f734b2bc5a4c6b04005dfd"
        },
        "date": 1772641539518,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ts/simulation_build_top_n1000",
            "value": 797.9556023333334,
            "range": "± 1.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1",
            "value": 0.0005835547409131552,
            "range": "± 0.4%",
            "unit": "ms",
            "extra": "856818 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1000000",
            "value": 420.8375973333338,
            "range": "± 1.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1",
            "value": 0.0009488070374840308,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "526978 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1000000",
            "value": 801.4985173333334,
            "range": "± 0.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1",
            "value": 0.0009781509050827853,
            "range": "± 0.6%",
            "unit": "ms",
            "extra": "511169 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1000000",
            "value": 801.4784373333315,
            "range": "± 4.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulator_tick_x10000",
            "value": 4.165462999997544,
            "range": "± 0.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_step_x20000",
            "value": 13.243631333330995,
            "range": "± 0.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_build_top_n1000",
            "value": 780.6303903333368,
            "range": "± 1.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_step_x1",
            "value": 0.0008000947774764537,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "624927 samples"
          },
          {
            "name": "ts/simulation_time_step_x1000000",
            "value": 667.6259136666631,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_runUntil_1000000",
            "value": 104.29070499999943,
            "range": "± 0.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/waitForCycles_x1000",
            "value": 0.7216003333354214,
            "range": "± 2.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/manual_step_loop_x2000",
            "value": 0.7241850000015498,
            "range": "± 3.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_fast_path_100000",
            "value": 4.583686999998463,
            "range": "± 2.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_guarded_100000",
            "value": 9.058697666667285,
            "range": "± 2.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_without_optimize",
            "value": 775.3528926666647,
            "range": "± 2.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_with_optimize",
            "value": 782.411061666673,
            "range": "± 3.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_without_optimize",
            "value": 4.3264986666617915,
            "range": "± 12.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_with_optimize",
            "value": 4.250612333334478,
            "range": "± 14.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_linear_sec_p6",
            "value": 192831.78699999794,
            "range": "± 11.2%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1",
            "value": 1.5747588887235797,
            "range": "± 0.6%",
            "unit": "us",
            "extra": "317509 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1000000",
            "value": 1455831.1946666702,
            "range": "± 1.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_eval_linear_sec_p6_x1000000",
            "value": 1395359.3923333294,
            "range": "± 1.4%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_countones_w64",
            "value": 66394.34499999818,
            "range": "± 4.5%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1",
            "value": 1.0495668500537576,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "476387 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1000000",
            "value": 932528.3383333299,
            "range": "± 2.7%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_std_counter_w32",
            "value": 8459.479000002224,
            "range": "± 7.5%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1",
            "value": 0.24090587762156115,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "2075500 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1000000",
            "value": 151540.3896666685,
            "range": "± 1.5%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_std_counter_w32_x1000000",
            "value": 170829.49366666435,
            "range": "± 1.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_gray_counter_w32",
            "value": 11674.591000002692,
            "range": "± 6.2%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1",
            "value": 0.251870125570665,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "1985210 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1000000",
            "value": 148836.13966666357,
            "range": "± 1.5%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_gray_counter_w32_x1000000",
            "value": 169574.01233333317,
            "range": "± 1.3%",
            "unit": "us",
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
          "id": "9b05b617839ec740f344230e5c4c6df121f6fb3e",
          "message": "Add Dead Store Elimination guide pages and update related docs\n\n- New guide: docs/guide/dead-store-elimination.md (EN)\n- New guide: docs/ja/guide/dead-store-elimination.md (JA)\n- Add sidebar entries in config.mts\n- Update hierarchy guides with DSE interaction notes\n- Update vite-plugin guides with ?dse= query parameter docs\n- Update writing-tests guides with deadStorePolicy option",
          "timestamp": "2026-03-04T19:03:47Z",
          "tree_id": "fa05eea29a5e4ec9777dfca78dba22409bbbe927",
          "url": "https://github.com/celox-sim/celox/commit/9b05b617839ec740f344230e5c4c6df121f6fb3e"
        },
        "date": 1772652623912,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ts/simulation_build_top_n1000",
            "value": 763.2863913333337,
            "range": "± 2.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1",
            "value": 0.0005150839761570422,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "970716 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1000000",
            "value": 411.4641040000012,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1",
            "value": 0.000848706564910337,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "589132 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1000000",
            "value": 732.5447426666676,
            "range": "± 0.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1",
            "value": 0.0008392111336395608,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "595798 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1000000",
            "value": 697.8595463333307,
            "range": "± 0.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulator_tick_x10000",
            "value": 4.279993000003742,
            "range": "± 1.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_step_x20000",
            "value": 13.127506999999847,
            "range": "± 1.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_build_top_n1000",
            "value": 755.9490716666672,
            "range": "± 1.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_step_x1",
            "value": 0.0008135347533257525,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "614603 samples"
          },
          {
            "name": "ts/simulation_time_step_x1000000",
            "value": 654.4862073333328,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_runUntil_1000000",
            "value": 101.2778529999996,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/waitForCycles_x1000",
            "value": 0.7072703333348423,
            "range": "± 4.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/manual_step_loop_x2000",
            "value": 0.703395999999581,
            "range": "± 2.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_fast_path_100000",
            "value": 4.240427000001849,
            "range": "± 2.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_guarded_100000",
            "value": 8.595146666666551,
            "range": "± 0.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_without_optimize",
            "value": 750.6925010000026,
            "range": "± 0.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_with_optimize",
            "value": 762.2853736666706,
            "range": "± 2.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_without_optimize",
            "value": 4.143253000002005,
            "range": "± 4.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_with_optimize",
            "value": 4.11119699999593,
            "range": "± 0.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_linear_sec_p6",
            "value": 198665.31933333803,
            "range": "± 2.2%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1",
            "value": 1.5352191429163382,
            "range": "± 0.6%",
            "unit": "us",
            "extra": "325687 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1000000",
            "value": 1401607.1543333335,
            "range": "± 0.5%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_eval_linear_sec_p6_x1000000",
            "value": 1333299.6373333298,
            "range": "± 0.9%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_countones_w64",
            "value": 76797.36300000029,
            "range": "± 1.7%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1",
            "value": 1.2811862482750938,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "390264 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1000000",
            "value": 1156087.3153333378,
            "range": "± 0.3%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_std_counter_w32",
            "value": 9071.82366666287,
            "range": "± 2.4%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1",
            "value": 0.20518296257277865,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "2436870 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1000000",
            "value": 105881.45700000071,
            "range": "± 1.3%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_std_counter_w32_x1000000",
            "value": 138822.4470000035,
            "range": "± 0.4%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_gray_counter_w32",
            "value": 12999.558000000738,
            "range": "± 15.4%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1",
            "value": 0.21148446012407648,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "2364240 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1000000",
            "value": 111611.94966666517,
            "range": "± 10.3%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_gray_counter_w32_x1000000",
            "value": 144529.038000003,
            "range": "± 0.3%",
            "unit": "us",
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
          "id": "18488642321e897a9719ef2403a55c2cdbe6f577",
          "message": "Merge branch 'claude/ts-tb-proto-fn'",
          "timestamp": "2026-03-04T20:35:42Z",
          "tree_id": "8ba9150ab61b8fdbd67d85e07bda97f595cb6a0c",
          "url": "https://github.com/celox-sim/celox/commit/18488642321e897a9719ef2403a55c2cdbe6f577"
        },
        "date": 1772658120146,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ts/simulation_build_top_n1000",
            "value": 808.6237579999997,
            "range": "± 2.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1",
            "value": 0.0005422742616123736,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "922043 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1000000",
            "value": 382.2506619999998,
            "range": "± 2.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1",
            "value": 0.0008839968264522983,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "565613 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1000000",
            "value": 715.0579613333344,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1",
            "value": 0.000861402714103899,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "580449 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1000000",
            "value": 717.0101946666642,
            "range": "± 0.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulator_tick_x10000",
            "value": 3.7694493333353116,
            "range": "± 1.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_step_x20000",
            "value": 13.15723966666701,
            "range": "± 2.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_build_top_n1000",
            "value": 789.6135513333332,
            "range": "± 0.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_step_x1",
            "value": 0.0008125196734995566,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "615371 samples"
          },
          {
            "name": "ts/simulation_time_step_x1000000",
            "value": 656.1447960000006,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_runUntil_1000000",
            "value": 105.37827100000383,
            "range": "± 1.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/waitForCycles_x1000",
            "value": 0.6993900000040109,
            "range": "± 4.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/manual_step_loop_x2000",
            "value": 0.7367756666668962,
            "range": "± 18.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_fast_path_100000",
            "value": 4.240703999998611,
            "range": "± 0.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_guarded_100000",
            "value": 8.713970999999825,
            "range": "± 1.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_without_optimize",
            "value": 785.5541066666677,
            "range": "± 0.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_with_optimize",
            "value": 790.7832639999979,
            "range": "± 2.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_without_optimize",
            "value": 3.797311666664124,
            "range": "± 1.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_with_optimize",
            "value": 3.911049999995157,
            "range": "± 15.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_linear_sec_p6",
            "value": 194964.14733333222,
            "range": "± 2.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1",
            "value": 1.5420192721061388,
            "range": "± 0.5%",
            "unit": "us",
            "extra": "324251 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1000000",
            "value": 1402919.305333334,
            "range": "± 0.0%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_eval_linear_sec_p6_x1000000",
            "value": 1354314.8429999952,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_countones_w64",
            "value": 76314.83133333192,
            "range": "± 0.4%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1",
            "value": 1.2774666325913364,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "391400 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1000000",
            "value": 1165542.7476666663,
            "range": "± 1.6%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_std_counter_w32",
            "value": 8403.39433332944,
            "range": "± 1.4%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1",
            "value": 0.23206821362751776,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "2154540 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1000000",
            "value": 101816.92499999674,
            "range": "± 0.6%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_std_counter_w32_x1000000",
            "value": 133287.36866667168,
            "range": "± 0.4%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_gray_counter_w32",
            "value": 11805.738333336194,
            "range": "± 1.5%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1",
            "value": 0.23778291044407895,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "2102759 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1000000",
            "value": 111452.39166666094,
            "range": "± 0.4%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_gray_counter_w32_x1000000",
            "value": 140076.9823333297,
            "range": "± 1.2%",
            "unit": "us",
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
          "id": "4c12e240363179cc33ab7db1a8f9a51e2c728dfd",
          "message": "Fix biome formatting in packages",
          "timestamp": "2026-03-04T22:24:33Z",
          "tree_id": "8a081bcc1f996822bebea056ec77017cafcb0119",
          "url": "https://github.com/celox-sim/celox/commit/4c12e240363179cc33ab7db1a8f9a51e2c728dfd"
        },
        "date": 1772664697687,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ts/simulation_build_top_n1000",
            "value": 738.3307233333331,
            "range": "± 2.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1",
            "value": 0.0004050283932863542,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "1234482 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1000000",
            "value": 256.38813433333416,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1",
            "value": 0.0007134330450575032,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "700837 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1000000",
            "value": 646.2576916666658,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1",
            "value": 0.0007193952554510685,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "695029 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1000000",
            "value": 635.4577183333349,
            "range": "± 1.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulator_tick_x10000",
            "value": 2.643849666666938,
            "range": "± 4.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_step_x20000",
            "value": 12.0653206666672,
            "range": "± 4.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_build_top_n1000",
            "value": 733.333243333332,
            "range": "± 1.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_step_x1",
            "value": 0.000782702156970536,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "638813 samples"
          },
          {
            "name": "ts/simulation_time_step_x1000000",
            "value": 612.4086156666696,
            "range": "± 0.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_runUntil_1000000",
            "value": 90.4613856666668,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/waitForCycles_x1000",
            "value": 1.1024510000012622,
            "range": "± 68.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/manual_step_loop_x2000",
            "value": 0.9090130000016264,
            "range": "± 4.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_fast_path_100000",
            "value": 4.665980333333816,
            "range": "± 0.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_guarded_100000",
            "value": 12.323474333332948,
            "range": "± 0.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_without_optimize",
            "value": 734.6447579999998,
            "range": "± 2.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_with_optimize",
            "value": 721.2520213333313,
            "range": "± 1.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_without_optimize",
            "value": 3.0827496666631,
            "range": "± 62.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_with_optimize",
            "value": 2.5920613333340348,
            "range": "± 1.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_linear_sec_p6",
            "value": 191067.4119999991,
            "range": "± 1.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1",
            "value": 1.3239455171287833,
            "range": "± 0.3%",
            "unit": "us",
            "extra": "377660 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1000000",
            "value": 1200083.323666661,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_eval_linear_sec_p6_x1000000",
            "value": 1154506.4030000067,
            "range": "± 0.5%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_countones_w64",
            "value": 74726.6896666697,
            "range": "± 6.3%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1",
            "value": 1.1568893922704004,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "432194 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1000000",
            "value": 1049313.1223333282,
            "range": "± 0.5%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_std_counter_w32",
            "value": 8973.111666671079,
            "range": "± 3.4%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1",
            "value": 0.23378760495652215,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "2138694 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1000000",
            "value": 102895.83300000231,
            "range": "± 4.0%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_std_counter_w32_x1000000",
            "value": 136263.7963333303,
            "range": "± 3.6%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_gray_counter_w32",
            "value": 12293.014666667053,
            "range": "± 0.4%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1",
            "value": 0.23523535578602012,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "2125531 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1000000",
            "value": 98018.16599999923,
            "range": "± 0.4%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_gray_counter_w32_x1000000",
            "value": 138135.19366666637,
            "range": "± 4.0%",
            "unit": "us",
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
          "id": "744f8bd0d66ee1b5dd2ca605da2873439a55dde5",
          "message": "Fix biome formatting in simulation.ts",
          "timestamp": "2026-03-05T00:04:46Z",
          "tree_id": "b92fab63708fc94462c42e04fa6720f665fdabe9",
          "url": "https://github.com/celox-sim/celox/commit/744f8bd0d66ee1b5dd2ca605da2873439a55dde5"
        },
        "date": 1772670665742,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ts/simulation_build_top_n1000",
            "value": 783.107435,
            "range": "± 2.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1",
            "value": 0.0005326351481634602,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "938729 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1000000",
            "value": 423.56193633333294,
            "range": "± 1.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1",
            "value": 0.0008487113026167967,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "589129 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1000000",
            "value": 710.5810319999995,
            "range": "± 1.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1",
            "value": 0.0008221120061165755,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "608190 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1000000",
            "value": 690.8484763333328,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulator_tick_x10000",
            "value": 4.195811000000201,
            "range": "± 1.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_step_x20000",
            "value": 13.244822333334014,
            "range": "± 1.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_build_top_n1000",
            "value": 768.8736910000007,
            "range": "± 0.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_step_x1",
            "value": 0.0008013019479673922,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "623985 samples"
          },
          {
            "name": "ts/simulation_time_step_x1000000",
            "value": 668.2347000000009,
            "range": "± 1.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_runUntil_1000000",
            "value": 102.60306966666879,
            "range": "± 1.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/waitForCycles_x1000",
            "value": 0.7168366666640699,
            "range": "± 3.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/manual_step_loop_x2000",
            "value": 0.721859333338216,
            "range": "± 2.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_fast_path_100000",
            "value": 4.556043666663754,
            "range": "± 6.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_guarded_100000",
            "value": 9.110349333333337,
            "range": "± 1.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_without_optimize",
            "value": 779.1894573333338,
            "range": "± 2.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_with_optimize",
            "value": 777.5560989999989,
            "range": "± 3.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_without_optimize",
            "value": 4.134859666669702,
            "range": "± 0.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_with_optimize",
            "value": 4.098287666667602,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_linear_sec_p6",
            "value": 194184.73966666593,
            "range": "± 3.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1",
            "value": 1.526494681699084,
            "range": "± 0.3%",
            "unit": "us",
            "extra": "327548 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1000000",
            "value": 1392525.2276666677,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_eval_linear_sec_p6_x1000000",
            "value": 1331850.020999996,
            "range": "± 0.3%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_countones_w64",
            "value": 77765.33133333335,
            "range": "± 1.6%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1",
            "value": 1.2853906042986263,
            "range": "± 0.4%",
            "unit": "us",
            "extra": "388987 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1000000",
            "value": 1143434.156333334,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_std_counter_w32",
            "value": 9087.690333331315,
            "range": "± 2.7%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1",
            "value": 0.20518114287550315,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "2436872 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1000000",
            "value": 108226.72133333496,
            "range": "± 0.3%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_std_counter_w32_x1000000",
            "value": 127304.28466666974,
            "range": "± 0.4%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_gray_counter_w32",
            "value": 12167.574000001574,
            "range": "± 1.7%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1",
            "value": 0.21194811680095316,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "2359068 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1000000",
            "value": 110496.13900000502,
            "range": "± 1.0%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_gray_counter_w32_x1000000",
            "value": 132345.81266666646,
            "range": "± 0.5%",
            "unit": "us",
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
          "id": "8e74e819769db8f1ae2925fef0076b3d00cf66bc",
          "message": "Fix cargo fmt formatting in celox-ts-gen test",
          "timestamp": "2026-03-05T03:15:52Z",
          "tree_id": "5d4b3d26af990f8d49733cc64bdca2e2b0c349d3",
          "url": "https://github.com/celox-sim/celox/commit/8e74e819769db8f1ae2925fef0076b3d00cf66bc"
        },
        "date": 1772682107390,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ts/simulation_build_top_n1000",
            "value": 740.5777713333331,
            "range": "± 1.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1",
            "value": 0.0004077600162122207,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "1226212 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1000000",
            "value": 288.827573999999,
            "range": "± 11.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1",
            "value": 0.000724355362911545,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "690269 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1000000",
            "value": 652.7033260000002,
            "range": "± 0.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1",
            "value": 0.000737055398479719,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "678376 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1000000",
            "value": 647.4012210000013,
            "range": "± 1.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulator_tick_x10000",
            "value": 2.706659999999829,
            "range": "± 10.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_step_x20000",
            "value": 13.73464133333376,
            "range": "± 0.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_build_top_n1000",
            "value": 743.1482540000022,
            "range": "± 0.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_step_x1",
            "value": 0.0007835587958262731,
            "range": "± 0.7%",
            "unit": "ms",
            "extra": "638115 samples"
          },
          {
            "name": "ts/simulation_time_step_x1000000",
            "value": 684.8355626666672,
            "range": "± 0.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_runUntil_1000000",
            "value": 90.8626230000009,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/waitForCycles_x1000",
            "value": 0.9272546666688868,
            "range": "± 1.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/manual_step_loop_x2000",
            "value": 0.9751459999994646,
            "range": "± 21.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_fast_path_100000",
            "value": 4.752712333332359,
            "range": "± 0.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_guarded_100000",
            "value": 11.250743666663766,
            "range": "± 1.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_without_optimize",
            "value": 745.4353849999994,
            "range": "± 1.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_with_optimize",
            "value": 744.9937399999982,
            "range": "± 0.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_without_optimize",
            "value": 2.758020333336996,
            "range": "± 23.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_with_optimize",
            "value": 2.592596999997719,
            "range": "± 0.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_linear_sec_p6",
            "value": 193789.58500000104,
            "range": "± 1.9%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1",
            "value": 1.3460805359528563,
            "range": "± 0.7%",
            "unit": "us",
            "extra": "371449 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1000000",
            "value": 1191994.400333332,
            "range": "± 0.0%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_eval_linear_sec_p6_x1000000",
            "value": 1156430.6176666676,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_countones_w64",
            "value": 77473.19966666691,
            "range": "± 2.9%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1",
            "value": 1.1582005003442144,
            "range": "± 0.4%",
            "unit": "us",
            "extra": "431705 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1000000",
            "value": 1050345.6120000046,
            "range": "± 0.6%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_std_counter_w32",
            "value": 9658.679666671864,
            "range": "± 3.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1",
            "value": 0.23521747778652657,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "2125693 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1000000",
            "value": 100045.47933333379,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_std_counter_w32_x1000000",
            "value": 135693.90233333493,
            "range": "± 1.0%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_gray_counter_w32",
            "value": 13335.291333331648,
            "range": "± 2.0%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1",
            "value": 0.23929221400510137,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "2089496 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1000000",
            "value": 115096.99233334202,
            "range": "± 2.8%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_gray_counter_w32_x1000000",
            "value": 142164.87699999803,
            "range": "± 8.0%",
            "unit": "us",
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
          "id": "0f0c6ec79d1978cac82e3e3d187a1e3a2160ea0f",
          "message": "Rebase veryl submodule onto upstream master and adapt to IR API changes\n\n- Update deps/veryl to upstream/master (10b3d0d0)\n- Add Box<Comptime> field to Expression variant patterns and constructors\n- Remove TokenRange from Factor::Variable/Value/FunctionCall patterns\n- Migrate FunctionCall.ret to FunctionCall.comptime\n- Guard eval_constexpr with is_const/evaluated checks to prevent\n  non-constant variables (now carrying Numeric Comptime) from being\n  treated as compile-time constants",
          "timestamp": "2026-03-03T18:04:03Z",
          "tree_id": "2f9bec6e79b3772a6749a12e3b33bcc493186ca6",
          "url": "https://github.com/celox-sim/celox/commit/0f0c6ec79d1978cac82e3e3d187a1e3a2160ea0f"
        },
        "date": 1772562002655,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "verilator/simulation_build_top_n1000",
            "value": 14460311.853,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_linear_sec_p6",
            "value": 1073910.624,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1",
            "value": 0.3221742432676916,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1000000",
            "value": 319221.5533333333,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1",
            "value": 0.32020686484181327,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1000000",
            "value": 318922.467,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1",
            "value": 0.03739479566235872,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1000000",
            "value": 37318.960666666666,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_eval_linear_sec_p6_x1000000",
            "value": 37201.595,
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
          "id": "e743cbc7e8b0ffc86ba7eda6fa854313fe5ff267",
          "message": "Rewrite benchmark dashboard with tab-based layout and mini-chart grid\n\nReplace the flat checkbox list + single shared chart with a tab-based UI\n(Counter, Std Library, API, Optimize) and a responsive 2-column grid of\nmini-charts. Each card groups the same operation across runtimes (max 3\nlines: Rust/TS/Verilator) with clickable Chart.js legends. Tabs use\npriority-based matching (API > Optimize > Stdlib > Counter) and sub-group\nsections for Std Library and API tabs.",
          "timestamp": "2026-03-03T20:42:12Z",
          "tree_id": "8a36ba14fc102b953283d48e8eca7c0650b2b4a2",
          "url": "https://github.com/celox-sim/celox/commit/e743cbc7e8b0ffc86ba7eda6fa854313fe5ff267"
        },
        "date": 1772571516091,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "verilator/simulation_build_top_n1000",
            "value": 10625440.652,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_linear_sec_p6",
            "value": 1240968.339,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1",
            "value": 0.3405603645611292,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1000000",
            "value": 340911.50033333333,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1",
            "value": 0.34197907614334394,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1000000",
            "value": 352917.54699999996,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1",
            "value": 0.03569561272669057,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1000000",
            "value": 35393.20133333334,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_eval_linear_sec_p6_x1000000",
            "value": 36257.43233333334,
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
          "id": "80644b5dd01a28cae84104a0978bb48136313d38",
          "message": "Ignore tests blocked by upstream Veryl IR bugs and update SIR snapshot\n\n- array_literal: 2 tests using `'{0}` (UnsupportedByIr at conv/utils.rs:231)\n- for_loop_unroll: 1 test using `'{0}` (same IR issue)\n- compare_matrix: 3 tests producing incorrect simulation results\n- param_override: 2 tests producing incorrect simulation results\n- false_loop: accept SIR snapshot diff from upstream register renumbering",
          "timestamp": "2026-03-03T22:42:09Z",
          "tree_id": "3938e4585335dc56d0411c597a60ebe8ff6c7665",
          "url": "https://github.com/celox-sim/celox/commit/80644b5dd01a28cae84104a0978bb48136313d38"
        },
        "date": 1772578697238,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "verilator/simulation_build_top_n1000",
            "value": 5595562.343,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_linear_sec_p6",
            "value": 1065474.601,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_countones_w64",
            "value": 993041.216,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_std_counter_w32",
            "value": 994783.821,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_gray_counter_w32",
            "value": 989688.992,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1",
            "value": 0.3111607616081037,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1000000",
            "value": 353428.535,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1",
            "value": 0.3303462868959258,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1000000",
            "value": 323116.9743333334,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1",
            "value": 0.03759168146359234,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1000000",
            "value": 37682.486,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_eval_linear_sec_p6_x1000000",
            "value": 37553.90699999999,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1",
            "value": 0.03388855995151067,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1000000",
            "value": 34152.32066666667,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1",
            "value": 0.04607136716557898,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1000000",
            "value": 45906.227999999996,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_std_counter_w32_x1000000",
            "value": 46084.149333333335,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1",
            "value": 0.04641959076374218,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1000000",
            "value": 48558.617000000006,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_gray_counter_w32_x1000000",
            "value": 46152.096333333335,
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
          "id": "2e10bb451c3b3260b7ef3217d8e8d21ce7f813b4",
          "message": "Skip param override E2E test blocked by upstream Veryl IR bug",
          "timestamp": "2026-03-03T23:14:57Z",
          "tree_id": "6ac83fa262c7d08f2d2bf174c267e87c74ec8302",
          "url": "https://github.com/celox-sim/celox/commit/2e10bb451c3b3260b7ef3217d8e8d21ce7f813b4"
        },
        "date": 1772580733385,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "verilator/simulation_build_top_n1000",
            "value": 13262969.09,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_linear_sec_p6",
            "value": 1118678.674,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_countones_w64",
            "value": 1059546.531,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_std_counter_w32",
            "value": 1056990.782,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_gray_counter_w32",
            "value": 1070230.084,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1",
            "value": 0.34015039728271756,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1000000",
            "value": 341460.9003333333,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1",
            "value": 0.3416446230425996,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1000000",
            "value": 352335.14300000004,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1",
            "value": 0.03513122634105813,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1000000",
            "value": 35416.73900000001,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_eval_linear_sec_p6_x1000000",
            "value": 35529.093,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1",
            "value": 0.03457836449120653,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1000000",
            "value": 35072.61033333334,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1",
            "value": 0.03456144484227055,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1000000",
            "value": 34569.566666666666,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_std_counter_w32_x1000000",
            "value": 34704.83733333333,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1",
            "value": 0.03499351364361886,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1000000",
            "value": 34995.54433333334,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_gray_counter_w32_x1000000",
            "value": 34995.030333333336,
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
          "id": "db6aada5f195701837eaa84b1f84c6795f42eada",
          "message": "Optimize SIR codegen: bit-extract peephole, shift-by-0 elim, store coalescing fix\n\nThree optimizations targeting eval_comb JIT performance:\n\n1. Shift-by-0 elimination (codegen): Track Imm constants in TranslationState\n   and skip Shr/Shl/Sar when shift amount is 0, emitting identity instead.\n\n2. BitExtractPeepholePass (optimizer): Replace Load(N)+Shr(K)+And((1<<W)-1)\n   chains with a single narrow Load(W, offset=base+K).\n\n3. Store coalescing fix (optimizer): Run store coalescing twice in\n   optimize_block — before and after eliminate_redundant_loads — so that\n   stores unblocked by load elimination can still be coalesced.\n\nLinear SEC P=6 results: SIR instructions -70%, stores -88%, ~7.6% faster.",
          "timestamp": "2026-03-04T06:01:52Z",
          "tree_id": "c43771cad3a187cc15c4450c8b1ba0f9e142819a",
          "url": "https://github.com/celox-sim/celox/commit/db6aada5f195701837eaa84b1f84c6795f42eada"
        },
        "date": 1772605530183,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "verilator/simulation_build_top_n1000",
            "value": 6593834.571,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_linear_sec_p6",
            "value": 1112339.63,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_countones_w64",
            "value": 1055536.696,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_std_counter_w32",
            "value": 1076934.161,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_gray_counter_w32",
            "value": 1065798.219,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1",
            "value": 0.3407047829327791,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1000000",
            "value": 338712.79400000005,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1",
            "value": 0.33970411778975157,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1000000",
            "value": 357532.72200000007,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1",
            "value": 0.03509361919065639,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1000000",
            "value": 35440.442,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_eval_linear_sec_p6_x1000000",
            "value": 35621.12266666666,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1",
            "value": 0.034567523221121235,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1000000",
            "value": 35040.147333333334,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1",
            "value": 0.036117472937201946,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1000000",
            "value": 36146.32066666667,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_std_counter_w32_x1000000",
            "value": 34998.80366666667,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1",
            "value": 0.035002826354568334,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1000000",
            "value": 34967.35433333334,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_gray_counter_w32_x1000000",
            "value": 35100.026666666665,
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
          "id": "56e76f9dcf138d2ea4f734b2bc5a4c6b04005dfd",
          "message": "Remove trueLoops section from combinational-loops guide\n\ntrueLoops cannot be used with Veryl sources due to the analyzer's\nUnassignVariable check on self-referential assigns. Remove the\nsection entirely rather than documenting an unusable API.",
          "timestamp": "2026-03-04T16:01:38Z",
          "tree_id": "e830352a6560d005e57d0a820d545557fedc86a5",
          "url": "https://github.com/celox-sim/celox/commit/56e76f9dcf138d2ea4f734b2bc5a4c6b04005dfd"
        },
        "date": 1772641538304,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "verilator/simulation_build_top_n1000",
            "value": 16195410.979,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_linear_sec_p6",
            "value": 1169596.275,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_countones_w64",
            "value": 1094132.747,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_std_counter_w32",
            "value": 1116375.699,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_gray_counter_w32",
            "value": 1111385.807,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1",
            "value": 0.34052295230441737,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1000000",
            "value": 342091.093,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1",
            "value": 0.3427180581439976,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1000000",
            "value": 352847.8146666667,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1",
            "value": 0.03582108923821096,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1000000",
            "value": 35462.88033333333,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_eval_linear_sec_p6_x1000000",
            "value": 38471.805,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1",
            "value": 0.03453903552030246,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1000000",
            "value": 34817.12533333334,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1",
            "value": 0.03466800197053862,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1000000",
            "value": 34557.60166666666,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_std_counter_w32_x1000000",
            "value": 34752.39466666667,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1",
            "value": 0.03490849446358777,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1000000",
            "value": 34998.583,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_gray_counter_w32_x1000000",
            "value": 35016.23966666666,
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
          "id": "9b05b617839ec740f344230e5c4c6df121f6fb3e",
          "message": "Add Dead Store Elimination guide pages and update related docs\n\n- New guide: docs/guide/dead-store-elimination.md (EN)\n- New guide: docs/ja/guide/dead-store-elimination.md (JA)\n- Add sidebar entries in config.mts\n- Update hierarchy guides with DSE interaction notes\n- Update vite-plugin guides with ?dse= query parameter docs\n- Update writing-tests guides with deadStorePolicy option",
          "timestamp": "2026-03-04T19:03:47Z",
          "tree_id": "fa05eea29a5e4ec9777dfca78dba22409bbbe927",
          "url": "https://github.com/celox-sim/celox/commit/9b05b617839ec740f344230e5c4c6df121f6fb3e"
        },
        "date": 1772652623223,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "verilator/simulation_build_top_n1000",
            "value": 14594936.709,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_linear_sec_p6",
            "value": 1171593.325,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_countones_w64",
            "value": 1069206.058,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_std_counter_w32",
            "value": 1088566.274,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_gray_counter_w32",
            "value": 1081924.771,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1",
            "value": 0.34037764410846766,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1000000",
            "value": 340589.3196666667,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1",
            "value": 0.3419245693229655,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1000000",
            "value": 361292.7246666667,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1",
            "value": 0.035092864218253195,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1000000",
            "value": 35527.889333333325,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_eval_linear_sec_p6_x1000000",
            "value": 35553.914,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1",
            "value": 0.034550349489854804,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1000000",
            "value": 35182.96166666667,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1",
            "value": 0.03469277013220409,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1000000",
            "value": 34595.367,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_std_counter_w32_x1000000",
            "value": 34594.219,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1",
            "value": 0.03494837040964625,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1000000",
            "value": 35159.66933333333,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_gray_counter_w32_x1000000",
            "value": 35264.143,
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
          "id": "18488642321e897a9719ef2403a55c2cdbe6f577",
          "message": "Merge branch 'claude/ts-tb-proto-fn'",
          "timestamp": "2026-03-04T20:35:42Z",
          "tree_id": "8ba9150ab61b8fdbd67d85e07bda97f595cb6a0c",
          "url": "https://github.com/celox-sim/celox/commit/18488642321e897a9719ef2403a55c2cdbe6f577"
        },
        "date": 1772658119455,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "verilator/simulation_build_top_n1000",
            "value": 10959869.64,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_linear_sec_p6",
            "value": 1175223.548,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_countones_w64",
            "value": 1177745.232,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_std_counter_w32",
            "value": 1114121.031,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_gray_counter_w32",
            "value": 1125373.889,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1",
            "value": 0.3402284368068044,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1000000",
            "value": 341343.77499999997,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1",
            "value": 0.3410102803214428,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1000000",
            "value": 353432.6596666667,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1",
            "value": 0.03519129911833023,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1000000",
            "value": 35521.134666666665,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_eval_linear_sec_p6_x1000000",
            "value": 35461.967333333334,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1",
            "value": 0.03456978887417202,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1000000",
            "value": 35112.51933333334,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1",
            "value": 0.03463481074551052,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1000000",
            "value": 34499.51466666667,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_std_counter_w32_x1000000",
            "value": 34582.022,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1",
            "value": 0.034978389458784835,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1000000",
            "value": 35145.18833333334,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_gray_counter_w32_x1000000",
            "value": 35336.138,
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
          "id": "4c12e240363179cc33ab7db1a8f9a51e2c728dfd",
          "message": "Fix biome formatting in packages",
          "timestamp": "2026-03-04T22:24:33Z",
          "tree_id": "8a081bcc1f996822bebea056ec77017cafcb0119",
          "url": "https://github.com/celox-sim/celox/commit/4c12e240363179cc33ab7db1a8f9a51e2c728dfd"
        },
        "date": 1772664697008,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "verilator/simulation_build_top_n1000",
            "value": 5694815.691,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_linear_sec_p6",
            "value": 1130000.717,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_countones_w64",
            "value": 1065831.408,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_std_counter_w32",
            "value": 1074904.554,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_gray_counter_w32",
            "value": 1074782.114,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1",
            "value": 0.3406222433084655,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1000000",
            "value": 340490.8836666667,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1",
            "value": 0.3416803854381368,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1000000",
            "value": 351629.4783333334,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1",
            "value": 0.03560308389686228,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1000000",
            "value": 36627.95999999999,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_eval_linear_sec_p6_x1000000",
            "value": 36422.70966666666,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1",
            "value": 0.03455082794548575,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1000000",
            "value": 35050.479666666666,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1",
            "value": 0.034780199450051966,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1000000",
            "value": 34847.78166666666,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_std_counter_w32_x1000000",
            "value": 34535.069,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1",
            "value": 0.03499999436013257,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1000000",
            "value": 34975.24833333334,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_gray_counter_w32_x1000000",
            "value": 35041.51833333333,
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
          "id": "744f8bd0d66ee1b5dd2ca605da2873439a55dde5",
          "message": "Fix biome formatting in simulation.ts",
          "timestamp": "2026-03-05T00:04:46Z",
          "tree_id": "b92fab63708fc94462c42e04fa6720f665fdabe9",
          "url": "https://github.com/celox-sim/celox/commit/744f8bd0d66ee1b5dd2ca605da2873439a55dde5"
        },
        "date": 1772670665051,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "verilator/simulation_build_top_n1000",
            "value": 5966290.354,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_linear_sec_p6",
            "value": 1153528.042,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_countones_w64",
            "value": 1096927.974,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_std_counter_w32",
            "value": 1128376.09,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_gray_counter_w32",
            "value": 1098883.107,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1",
            "value": 0.309799881420674,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1000000",
            "value": 324826.8956666666,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1",
            "value": 0.3284950702844217,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1000000",
            "value": 325219.7353333333,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1",
            "value": 0.03879429318029223,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1000000",
            "value": 38989.02533333333,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_eval_linear_sec_p6_x1000000",
            "value": 38440.77966666666,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1",
            "value": 0.03639540490093195,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1000000",
            "value": 36199.08633333333,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1",
            "value": 0.03585225723365562,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1000000",
            "value": 36015.316333333336,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_std_counter_w32_x1000000",
            "value": 36085.03566666666,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1",
            "value": 0.03595016836608124,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1000000",
            "value": 35780.84366666667,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_gray_counter_w32_x1000000",
            "value": 36084.59999999999,
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
          "id": "8e74e819769db8f1ae2925fef0076b3d00cf66bc",
          "message": "Fix cargo fmt formatting in celox-ts-gen test",
          "timestamp": "2026-03-05T03:15:52Z",
          "tree_id": "5d4b3d26af990f8d49733cc64bdca2e2b0c349d3",
          "url": "https://github.com/celox-sim/celox/commit/8e74e819769db8f1ae2925fef0076b3d00cf66bc"
        },
        "date": 1772682106070,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "verilator/simulation_build_top_n1000",
            "value": 14360109.266,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_linear_sec_p6",
            "value": 1150203.223,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_countones_w64",
            "value": 1092924.11,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_std_counter_w32",
            "value": 1095148.089,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_gray_counter_w32",
            "value": 1079346.04,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1",
            "value": 0.3472280888411326,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1000000",
            "value": 344311.8663333333,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1",
            "value": 0.3418666949023697,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1000000",
            "value": 352529.369,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1",
            "value": 0.03522177101929517,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1000000",
            "value": 35382.22600000001,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_eval_linear_sec_p6_x1000000",
            "value": 35574.318333333336,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1",
            "value": 0.034597923653133016,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1000000",
            "value": 35395.41099999999,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1",
            "value": 0.0346154268669376,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1000000",
            "value": 34638.075,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_std_counter_w32_x1000000",
            "value": 35936.44066666666,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1",
            "value": 0.03494181633805876,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1000000",
            "value": 34966.193,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_gray_counter_w32_x1000000",
            "value": 35067.42500000001,
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
          "id": "e01768994102e89ea3943b884006429d5a0ceb28",
          "message": "Fix cargo fmt formatting in eval_factor",
          "timestamp": "2026-03-05T12:12:58Z",
          "tree_id": "e1b80a2afff3b8612ea03e3a0f296c2a8e15641f",
          "url": "https://github.com/celox-sim/celox/commit/e01768994102e89ea3943b884006429d5a0ceb28"
        },
        "date": 1772714141305,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "verilator/simulation_build_top_n1000",
            "value": 6029116.92,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_linear_sec_p6",
            "value": 1130195.252,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_countones_w64",
            "value": 1083348.635,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_std_counter_w32",
            "value": 1067417.151,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_gray_counter_w32",
            "value": 1084607.184,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1",
            "value": 0.34015367789440043,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1000000",
            "value": 340934.636,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1",
            "value": 0.3392091700849476,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1000000",
            "value": 350131.497,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1",
            "value": 0.03515719894909411,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1000000",
            "value": 35235.208,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_eval_linear_sec_p6_x1000000",
            "value": 35530.89,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1",
            "value": 0.03454234064115975,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1000000",
            "value": 34995.429333333326,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1",
            "value": 0.03456266511872359,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1000000",
            "value": 34901.674000000006,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_std_counter_w32_x1000000",
            "value": 34582.57099999999,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1",
            "value": 0.03502161479613792,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1000000",
            "value": 35095.89433333334,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_gray_counter_w32_x1000000",
            "value": 35227.740666666665,
            "unit": "us"
          }
        ]
      }
    ]
  }
}