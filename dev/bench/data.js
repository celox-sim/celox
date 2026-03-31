window.BENCHMARK_DATA = {
  "lastUpdate": 1774994943452,
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
          "id": "fe52243e73c8a04f0bae1025b5603d5593e28424",
          "message": "Merge branch 'claude/streamed-strolling-whistle': Add Z signal support",
          "timestamp": "2026-03-05T22:37:13Z",
          "tree_id": "397c2ddc41628122d6e57f6d7bc642175969e0db",
          "url": "https://github.com/celox-sim/celox/commit/fe52243e73c8a04f0bae1025b5603d5593e28424"
        },
        "date": 1772753402697,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "rust/simulator_tick_x10000",
            "value": 1590.252,
            "range": "± 23.404 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_step_x20000",
            "value": 9706.816,
            "range": "± 35.478 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_top_n1000",
            "value": 795242.837,
            "range": "± 12286.501 us",
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
            "value": 159409.989,
            "range": "± 451.608 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1",
            "value": 0.179,
            "range": "± 0.007 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1000000",
            "value": 187584.219,
            "range": "± 1135.881 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_linear_sec_p6",
            "value": 147685.89,
            "range": "± 2138.645 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1",
            "value": 0.639,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1000000",
            "value": 643545.856,
            "range": "± 947.006 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_eval_linear_sec_p6_x1000000",
            "value": 642136.677,
            "range": "± 1839.955 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6",
            "value": 0.6,
            "range": "± 0.021 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6_x1000000",
            "value": 600755.772,
            "range": "± 1839.899 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6",
            "value": 0.597,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6_x1000000",
            "value": 598036.073,
            "range": "± 322.420 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6",
            "value": 0.6,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6_x1000000",
            "value": 599633.915,
            "range": "± 1316.539 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6",
            "value": 0.602,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6_x1000000",
            "value": 606415.587,
            "range": "± 2024.182 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_countones_w64",
            "value": 73575.32,
            "range": "± 1506.661 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1",
            "value": 0.469,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1000000",
            "value": 475307.829,
            "range": "± 1388.066 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_std_counter_w32",
            "value": 7964.352,
            "range": "± 236.237 us",
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
            "value": 8799.91,
            "range": "± 85.273 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_std_counter_w32_x1000000",
            "value": 31312.765,
            "range": "± 62.040 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_gray_counter_w32",
            "value": 11217.396,
            "range": "± 182.497 us",
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
            "value": 12744.624,
            "range": "± 53.230 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_gray_counter_w32_x1000000",
            "value": 36295.061,
            "range": "± 219.989 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_countones_w64",
            "value": 57885.07,
            "range": "± 946.756 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_countones_w64_x1",
            "value": 0.077,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_countones_w64_x1000000",
            "value": 77156.125,
            "range": "± 253.343 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_linear_sec_p6",
            "value": 143753.123,
            "range": "± 2022.529 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1",
            "value": 0.602,
            "range": "± 0.011 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1000000",
            "value": 607056.998,
            "range": "± 434.386 us",
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
          "id": "b3a4fb2e5f26957bcc2c872e94bcbb903e7ad710",
          "message": "Fix dut tests for new X encoding (v=1, m=1)\n\nZ support changed 4-state encoding from X=(v=0,m=1) to X=(v=1,m=1).\nwriteAllX was updated but three tests still expected the old encoding.\n\nAlso fix biome config to only lint source files (not dist/ build\nartifacts), and suppress clippy::needless_borrow in celox-bench-sv.",
          "timestamp": "2026-03-06T01:15:07Z",
          "tree_id": "83196d261356b3ddcbc709f4f7a807a98e3f186a",
          "url": "https://github.com/celox-sim/celox/commit/b3a4fb2e5f26957bcc2c872e94bcbb903e7ad710"
        },
        "date": 1772761058493,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "rust/simulator_tick_x10000",
            "value": 1592.262,
            "range": "± 35.412 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_step_x20000",
            "value": 9840.988,
            "range": "± 136.939 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_top_n1000",
            "value": 794827.671,
            "range": "± 12968.072 us",
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
            "value": 159377.068,
            "range": "± 250.397 us",
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
            "value": 188081.565,
            "range": "± 1784.648 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_linear_sec_p6",
            "value": 151668.08,
            "range": "± 1791.862 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1",
            "value": 0.64,
            "range": "± 0.002 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1000000",
            "value": 643515.497,
            "range": "± 3482.364 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_eval_linear_sec_p6_x1000000",
            "value": 641599.515,
            "range": "± 2734.673 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6",
            "value": 0.601,
            "range": "± 0.005 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6_x1000000",
            "value": 599495.83,
            "range": "± 2000.657 us",
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
            "value": 598489.322,
            "range": "± 1930.018 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6",
            "value": 0.601,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6_x1000000",
            "value": 599704.941,
            "range": "± 309.333 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6",
            "value": 0.601,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6_x1000000",
            "value": 604802.573,
            "range": "± 2999.131 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_countones_w64",
            "value": 75486.354,
            "range": "± 1040.869 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1",
            "value": 0.471,
            "range": "± 0.006 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1000000",
            "value": 476135.915,
            "range": "± 1892.399 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_std_counter_w32",
            "value": 7960.293,
            "range": "± 139.678 us",
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
            "value": 8868.273,
            "range": "± 327.001 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_std_counter_w32_x1000000",
            "value": 32028.209,
            "range": "± 187.039 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_gray_counter_w32",
            "value": 11358.432,
            "range": "± 196.324 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_gray_counter_w32_x1",
            "value": 0.009,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_gray_counter_w32_x1000000",
            "value": 10178.53,
            "range": "± 290.533 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_gray_counter_w32_x1000000",
            "value": 36507.091,
            "range": "± 66.836 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_countones_w64",
            "value": 58719.837,
            "range": "± 865.880 us",
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
            "value": 77213.146,
            "range": "± 191.130 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_linear_sec_p6",
            "value": 146318.724,
            "range": "± 3641.392 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1",
            "value": 0.602,
            "range": "± 0.002 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1000000",
            "value": 607142.162,
            "range": "± 2754.469 us",
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
          "id": "1844e23e7b5e07da0d8ee1635e413d4792e88ea3",
          "message": "Fix dead link in gap analysis doc\n\nThe relative path from docs/ts_testbench_gap_analysis.md to\ndocs/guide/parameter-overrides.md should be ./guide/ not ../guide/.",
          "timestamp": "2026-03-06T02:40:56Z",
          "tree_id": "3b1d485b5bf9f4589ff49cd19587ec1963392dea",
          "url": "https://github.com/celox-sim/celox/commit/1844e23e7b5e07da0d8ee1635e413d4792e88ea3"
        },
        "date": 1772766186872,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "rust/simulator_tick_x10000",
            "value": 1592.317,
            "range": "± 191.873 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_step_x20000",
            "value": 9738.735,
            "range": "± 46.158 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_top_n1000",
            "value": 790910.836,
            "range": "± 10387.431 us",
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
            "value": 159492.241,
            "range": "± 1064.483 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1",
            "value": 0.178,
            "range": "± 0.002 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1000000",
            "value": 186922.665,
            "range": "± 1116.702 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_linear_sec_p6",
            "value": 148026.015,
            "range": "± 1453.335 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1",
            "value": 0.639,
            "range": "± 0.006 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1000000",
            "value": 642534.862,
            "range": "± 2473.558 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_eval_linear_sec_p6_x1000000",
            "value": 641266.496,
            "range": "± 1418.576 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6",
            "value": 0.6,
            "range": "± 0.006 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6_x1000000",
            "value": 600708.32,
            "range": "± 1545.907 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6",
            "value": 0.597,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6_x1000000",
            "value": 597847.955,
            "range": "± 11865.271 us",
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
            "value": 599663.752,
            "range": "± 2717.213 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6",
            "value": 0.603,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6_x1000000",
            "value": 608573.637,
            "range": "± 1789.271 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_countones_w64",
            "value": 76159.213,
            "range": "± 1028.570 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1",
            "value": 0.47,
            "range": "± 0.010 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1000000",
            "value": 476596.202,
            "range": "± 565.820 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_std_counter_w32",
            "value": 8056.474,
            "range": "± 146.479 us",
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
            "value": 9033.445,
            "range": "± 49.418 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_std_counter_w32_x1000000",
            "value": 31959.003,
            "range": "± 182.216 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_gray_counter_w32",
            "value": 11484.271,
            "range": "± 174.132 us",
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
            "value": 11989.325,
            "range": "± 18.737 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_gray_counter_w32_x1000000",
            "value": 35307.064,
            "range": "± 202.615 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_countones_w64",
            "value": 59908.728,
            "range": "± 882.585 us",
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
            "value": 78360.791,
            "range": "± 255.224 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_linear_sec_p6",
            "value": 147585.57,
            "range": "± 2611.274 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1",
            "value": 0.602,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1000000",
            "value": 607716.579,
            "range": "± 336.232 us",
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
          "id": "dd3d6e778f1e278917cfa7ace10b9e4143d2b02a",
          "message": "Add pre-push submodule validation and test gate via lefthook\n\nEnsure submodules are initialized, in sync, and their commits exist on\nthe remote before allowing push. Also run the full test suite (pnpm test)\nas a final gate. Use piped execution so failures block subsequent steps.",
          "timestamp": "2026-03-06T10:44:19Z",
          "tree_id": "4cb69d58108de90e3275cc11403552d653222edc",
          "url": "https://github.com/celox-sim/celox/commit/dd3d6e778f1e278917cfa7ace10b9e4143d2b02a"
        },
        "date": 1772795240708,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "rust/simulator_tick_x10000",
            "value": 1592.871,
            "range": "± 61.231 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_step_x20000",
            "value": 9907.053,
            "range": "± 40.954 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_top_n1000",
            "value": 336763.165,
            "range": "± 4293.929 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1",
            "value": 0.158,
            "range": "± 0.006 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1000000",
            "value": 159478.845,
            "range": "± 525.347 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1",
            "value": 0.249,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1000000",
            "value": 256683.037,
            "range": "± 1669.928 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_linear_sec_p6",
            "value": 146193.626,
            "range": "± 1875.528 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1",
            "value": 0.638,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1000000",
            "value": 639673.723,
            "range": "± 1173.478 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_eval_linear_sec_p6_x1000000",
            "value": 638259.317,
            "range": "± 989.070 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6",
            "value": 0.599,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6_x1000000",
            "value": 598980.384,
            "range": "± 1405.736 us",
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
            "value": 598244.926,
            "range": "± 908.344 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6",
            "value": 0.605,
            "range": "± 0.003 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6_x1000000",
            "value": 599674.289,
            "range": "± 552.541 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6",
            "value": 0.603,
            "range": "± 0.007 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6_x1000000",
            "value": 604504.985,
            "range": "± 1361.295 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_countones_w64",
            "value": 46025.708,
            "range": "± 856.084 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1",
            "value": 0.469,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1000000",
            "value": 475487.765,
            "range": "± 1055.899 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_std_counter_w32",
            "value": 7722.081,
            "range": "± 180.846 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1",
            "value": 0.007,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1000000",
            "value": 7863.888,
            "range": "± 96.685 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_std_counter_w32_x1000000",
            "value": 27149.617,
            "range": "± 52.150 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_gray_counter_w32",
            "value": 11749.593,
            "range": "± 347.702 us",
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
            "value": 11030.68,
            "range": "± 19.305 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_gray_counter_w32_x1000000",
            "value": 32624.515,
            "range": "± 392.313 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_countones_w64",
            "value": 30119.834,
            "range": "± 1164.256 us",
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
            "value": 77194.621,
            "range": "± 218.465 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_linear_sec_p6",
            "value": 145727.223,
            "range": "± 2360.730 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1",
            "value": 0.6,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1000000",
            "value": 607151.241,
            "range": "± 1068.860 us",
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
          "id": "cde884593abab6375c496a99b2f80a8d231fadd7",
          "message": "Fix fill-literal ('0/'1/'x/'z) width=0 causing bit loss in always_comb/always_ff\n\nFill-literals have width 0 from the Veryl analyzer (context-dependent).\nThe Factor::Value handler in both comb and FF paths emitted 0-width\nSLTNode::Constant / SIR registers, which caused Mux lowering to produce\n0-bit masks that zeroed out else-arm values in multi-branch if/else if\nalways_comb blocks.\n\nPer IEEE 1800-2023 §5.7.1, fill-literals replicate their single bit\nacross the full context width (1 bit in self-determined contexts).\nExpand both value and mask_xz by replicating bit 0 to the target width.",
          "timestamp": "2026-03-06T14:42:08Z",
          "tree_id": "f5c0dbc897cdabc00cea7cb9a39c94adfdc8af86",
          "url": "https://github.com/celox-sim/celox/commit/cde884593abab6375c496a99b2f80a8d231fadd7"
        },
        "date": 1772809420457,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "rust/simulator_tick_x10000",
            "value": 1450.56,
            "range": "± 11.602 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_step_x20000",
            "value": 8921.584,
            "range": "± 292.568 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_top_n1000",
            "value": 322417.537,
            "range": "± 2807.984 us",
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
            "value": 145858.795,
            "range": "± 2791.912 us",
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
            "value": 183227.42,
            "range": "± 376.588 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_linear_sec_p6",
            "value": 142693.526,
            "range": "± 1012.602 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1",
            "value": 0.456,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1000000",
            "value": 458842.449,
            "range": "± 480.241 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_eval_linear_sec_p6_x1000000",
            "value": 457096.162,
            "range": "± 1003.868 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6",
            "value": 0.421,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6_x1000000",
            "value": 421654.328,
            "range": "± 241.586 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6",
            "value": 0.422,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6_x1000000",
            "value": 421798.795,
            "range": "± 338.186 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6",
            "value": 0.424,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6_x1000000",
            "value": 425362.516,
            "range": "± 875.190 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6",
            "value": 0.427,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6_x1000000",
            "value": 428150.056,
            "range": "± 234.991 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_countones_w64",
            "value": 46520.751,
            "range": "± 592.719 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1",
            "value": 0.6,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1000000",
            "value": 624289.788,
            "range": "± 480.475 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_std_counter_w32",
            "value": 8044.324,
            "range": "± 192.619 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1",
            "value": 0.005,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1000000",
            "value": 5479.102,
            "range": "± 76.832 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_std_counter_w32_x1000000",
            "value": 28032.317,
            "range": "± 57.946 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_gray_counter_w32",
            "value": 11543.438,
            "range": "± 204.086 us",
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
            "value": 7932.126,
            "range": "± 11.463 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_gray_counter_w32_x1000000",
            "value": 30227.82,
            "range": "± 133.781 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_countones_w64",
            "value": 30464.356,
            "range": "± 1239.037 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_countones_w64_x1",
            "value": 0.068,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_countones_w64_x1000000",
            "value": 69272.891,
            "range": "± 163.563 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_linear_sec_p6",
            "value": 140655.15,
            "range": "± 2320.383 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1",
            "value": 0.42,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1000000",
            "value": 423050.198,
            "range": "± 248.771 us",
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
          "id": "fa0347fccb316ac5c78006c28a32cd1965a95b8f",
          "message": "Add untracked file detection to pre-push clean-tree check\n\nPrevents pushing when untracked files exist, catching cases where\ntests reference local files (e.g. via include_str!) that aren't\ncommitted. Previously only tracked file changes were detected.",
          "timestamp": "2026-03-06T15:14:47Z",
          "tree_id": "82e54feef12a5e1c65eba78a3e33bd9bf9d22e1e",
          "url": "https://github.com/celox-sim/celox/commit/fa0347fccb316ac5c78006c28a32cd1965a95b8f"
        },
        "date": 1772811501584,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "rust/simulator_tick_x10000",
            "value": 1595.896,
            "range": "± 12.326 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_step_x20000",
            "value": 9706.688,
            "range": "± 71.481 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_top_n1000",
            "value": 332564.693,
            "range": "± 5589.945 us",
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
            "value": 159180.876,
            "range": "± 2980.347 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1",
            "value": 0.179,
            "range": "± 0.003 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1000000",
            "value": 186456.323,
            "range": "± 779.909 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_linear_sec_p6",
            "value": 146052.233,
            "range": "± 2781.813 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1",
            "value": 0.645,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1000000",
            "value": 650246.983,
            "range": "± 3024.018 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_eval_linear_sec_p6_x1000000",
            "value": 647124.87,
            "range": "± 2868.145 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6",
            "value": 0.598,
            "range": "± 0.002 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6_x1000000",
            "value": 597895.086,
            "range": "± 2684.294 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6",
            "value": 0.597,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6_x1000000",
            "value": 597958.329,
            "range": "± 1424.008 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6",
            "value": 0.601,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6_x1000000",
            "value": 601831.746,
            "range": "± 939.401 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6",
            "value": 0.606,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6_x1000000",
            "value": 604640.821,
            "range": "± 1150.150 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_countones_w64",
            "value": 47755.629,
            "range": "± 1463.761 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1",
            "value": 0.469,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1000000",
            "value": 475413.013,
            "range": "± 708.432 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_std_counter_w32",
            "value": 7990.833,
            "range": "± 87.659 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1",
            "value": 0.007,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1000000",
            "value": 7263.67,
            "range": "± 48.534 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_std_counter_w32_x1000000",
            "value": 28873.343,
            "range": "± 130.884 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_gray_counter_w32",
            "value": 11558.97,
            "range": "± 266.542 us",
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
            "value": 11816.699,
            "range": "± 28.519 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_gray_counter_w32_x1000000",
            "value": 33264.945,
            "range": "± 230.577 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_countones_w64",
            "value": 31329.687,
            "range": "± 1008.582 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_countones_w64_x1",
            "value": 0.078,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_countones_w64_x1000000",
            "value": 76541.673,
            "range": "± 1330.077 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_linear_sec_p6",
            "value": 143735.325,
            "range": "± 1603.940 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1",
            "value": 0.61,
            "range": "± 0.002 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1000000",
            "value": 611273.753,
            "range": "± 1814.042 us",
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
          "id": "9a6ef5d0cc81ed4bf680c5b6fad3d2a21554bb41",
          "message": "Box SimBackend::Full to fix clippy large_enum_variant warning",
          "timestamp": "2026-03-07T10:39:04Z",
          "tree_id": "324129867e926b82150e673d111e12a1fe763c36",
          "url": "https://github.com/celox-sim/celox/commit/9a6ef5d0cc81ed4bf680c5b6fad3d2a21554bb41"
        },
        "date": 1772881474975,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "rust/simulator_tick_x10000",
            "value": 3275.501,
            "range": "± 4.167 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_step_x20000",
            "value": 9747.022,
            "range": "± 244.665 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_top_n1000",
            "value": 332664.88,
            "range": "± 5869.585 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1",
            "value": 0.327,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1000000",
            "value": 342808.935,
            "range": "± 1906.708 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1",
            "value": 0.374,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1000000",
            "value": 376301.857,
            "range": "± 2016.077 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_linear_sec_p6",
            "value": 139353.391,
            "range": "± 3021.274 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1",
            "value": 0.639,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1000000",
            "value": 643001.404,
            "range": "± 2476.820 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_eval_linear_sec_p6_x1000000",
            "value": 641286.172,
            "range": "± 2739.378 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6",
            "value": 0.597,
            "range": "± 0.006 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6_x1000000",
            "value": 597114.155,
            "range": "± 2674.799 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6",
            "value": 0.603,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6_x1000000",
            "value": 604618.665,
            "range": "± 3098.201 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6",
            "value": 0.603,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6_x1000000",
            "value": 606361.942,
            "range": "± 2239.924 us",
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
            "value": 606581.383,
            "range": "± 1892.896 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_countones_w64",
            "value": 44027.367,
            "range": "± 1016.945 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1",
            "value": 0.471,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1000000",
            "value": 476180.794,
            "range": "± 1691.957 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_std_counter_w32",
            "value": 7629.156,
            "range": "± 54.258 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1",
            "value": 0.007,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1000000",
            "value": 7215.71,
            "range": "± 27.991 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_std_counter_w32_x1000000",
            "value": 29672.608,
            "range": "± 107.224 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_gray_counter_w32",
            "value": 10847.964,
            "range": "± 190.759 us",
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
            "value": 11494.878,
            "range": "± 60.256 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_gray_counter_w32_x1000000",
            "value": 33576.055,
            "range": "± 129.258 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_countones_w64",
            "value": 28554.27,
            "range": "± 1173.625 us",
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
            "value": 77281.797,
            "range": "± 1621.833 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_linear_sec_p6",
            "value": 136744.758,
            "range": "± 2781.734 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1",
            "value": 0.601,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1000000",
            "value": 607792.652,
            "range": "± 3502.419 us",
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
          "id": "308dd9af7e68375c3a1406b1a0f074926a7ce657",
          "message": "Revert Box<Simulator> — use allow(clippy::large_enum_variant) instead\n\nBoxing adds indirection on every tick call in the hot path.",
          "timestamp": "2026-03-07T11:28:14Z",
          "tree_id": "9d27eac47590a27fba99f8bf9d04e19473b9ed19",
          "url": "https://github.com/celox-sim/celox/commit/308dd9af7e68375c3a1406b1a0f074926a7ce657"
        },
        "date": 1772884295062,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "rust/simulator_tick_x10000",
            "value": 3280.956,
            "range": "± 48.198 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_step_x20000",
            "value": 9848.719,
            "range": "± 76.973 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_top_n1000",
            "value": 329501.243,
            "range": "± 4744.020 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1",
            "value": 0.327,
            "range": "± 0.003 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1000000",
            "value": 342725.818,
            "range": "± 1977.487 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1",
            "value": 0.374,
            "range": "± 0.002 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1000000",
            "value": 375815.335,
            "range": "± 2602.989 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_linear_sec_p6",
            "value": 136985.474,
            "range": "± 1881.259 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1",
            "value": 0.638,
            "range": "± 0.003 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1000000",
            "value": 641179.868,
            "range": "± 3688.676 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_eval_linear_sec_p6_x1000000",
            "value": 640574.034,
            "range": "± 3362.549 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6",
            "value": 0.597,
            "range": "± 0.006 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6_x1000000",
            "value": 596034.31,
            "range": "± 3972.070 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6",
            "value": 0.602,
            "range": "± 0.003 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6_x1000000",
            "value": 600593.143,
            "range": "± 3772.018 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6",
            "value": 0.599,
            "range": "± 0.008 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6_x1000000",
            "value": 602227.152,
            "range": "± 3518.206 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6",
            "value": 0.602,
            "range": "± 0.006 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6_x1000000",
            "value": 602051.34,
            "range": "± 3093.696 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_countones_w64",
            "value": 43061.051,
            "range": "± 622.933 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1",
            "value": 0.466,
            "range": "± 0.005 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1000000",
            "value": 472600.8,
            "range": "± 3265.262 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_std_counter_w32",
            "value": 7616.79,
            "range": "± 67.013 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1",
            "value": 0.007,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1000000",
            "value": 7836.426,
            "range": "± 99.446 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_std_counter_w32_x1000000",
            "value": 29705.832,
            "range": "± 284.588 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_gray_counter_w32",
            "value": 10837.783,
            "range": "± 107.186 us",
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
            "value": 11154.748,
            "range": "± 83.801 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_gray_counter_w32_x1000000",
            "value": 32822.555,
            "range": "± 505.457 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_countones_w64",
            "value": 27088.085,
            "range": "± 799.030 us",
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
            "value": 76513.238,
            "range": "± 1205.124 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_linear_sec_p6",
            "value": 134917.364,
            "range": "± 2810.663 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1",
            "value": 0.601,
            "range": "± 0.003 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1000000",
            "value": 607244.124,
            "range": "± 5441.855 us",
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
          "id": "1c00e87c78fbe6c21c4d495439d93a582bd9c581",
          "message": "Remove unnecessary cast flagged by clippy",
          "timestamp": "2026-03-07T11:54:17Z",
          "tree_id": "8f4484551df424cac5506ba3d1ba0094442d2a50",
          "url": "https://github.com/celox-sim/celox/commit/1c00e87c78fbe6c21c4d495439d93a582bd9c581"
        },
        "date": 1772885996453,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "rust/simulator_tick_x10000",
            "value": 3430.887,
            "range": "± 8.595 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_step_x20000",
            "value": 9980.458,
            "range": "± 31.626 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_top_n1000",
            "value": 336256.174,
            "range": "± 4079.605 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1",
            "value": 0.341,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1000000",
            "value": 343236.212,
            "range": "± 373.519 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1",
            "value": 0.407,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1000000",
            "value": 408040.632,
            "range": "± 439.449 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_linear_sec_p6",
            "value": 141820.049,
            "range": "± 1901.054 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1",
            "value": 0.642,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1000000",
            "value": 644536.342,
            "range": "± 934.163 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_eval_linear_sec_p6_x1000000",
            "value": 641618.781,
            "range": "± 651.471 us",
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
            "value": 597811.029,
            "range": "± 452.770 us",
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
            "value": 598530.434,
            "range": "± 300.221 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6",
            "value": 0.602,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6_x1000000",
            "value": 600399.003,
            "range": "± 779.261 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6",
            "value": 0.602,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6_x1000000",
            "value": 602849.996,
            "range": "± 886.682 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_countones_w64",
            "value": 44479.067,
            "range": "± 742.123 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1",
            "value": 0.467,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1000000",
            "value": 473927.557,
            "range": "± 769.488 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_std_counter_w32",
            "value": 7655.462,
            "range": "± 47.909 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1",
            "value": 0.007,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1000000",
            "value": 7406.091,
            "range": "± 64.917 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_std_counter_w32_x1000000",
            "value": 35037.017,
            "range": "± 45.756 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_gray_counter_w32",
            "value": 10920.769,
            "range": "± 90.533 us",
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
            "value": 11490.865,
            "range": "± 72.665 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_gray_counter_w32_x1000000",
            "value": 33058.502,
            "range": "± 80.911 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_countones_w64",
            "value": 28105.191,
            "range": "± 857.848 us",
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
            "value": 76127.033,
            "range": "± 393.773 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_linear_sec_p6",
            "value": 140061.442,
            "range": "± 2568.406 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1",
            "value": 0.603,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1000000",
            "value": 607801.494,
            "range": "± 978.803 us",
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
          "id": "1c00e87c78fbe6c21c4d495439d93a582bd9c581",
          "message": "Remove unnecessary cast flagged by clippy",
          "timestamp": "2026-03-07T11:54:17Z",
          "tree_id": "8f4484551df424cac5506ba3d1ba0094442d2a50",
          "url": "https://github.com/celox-sim/celox/commit/1c00e87c78fbe6c21c4d495439d93a582bd9c581"
        },
        "date": 1772894742021,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "rust/simulator_tick_x10000",
            "value": 3428.873,
            "range": "± 7.459 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_step_x20000",
            "value": 10012.956,
            "range": "± 426.866 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_top_n1000",
            "value": 340573.818,
            "range": "± 4572.210 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1",
            "value": 0.341,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1000000",
            "value": 343164.449,
            "range": "± 2457.967 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1",
            "value": 0.407,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1000000",
            "value": 407880.344,
            "range": "± 1865.972 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_linear_sec_p6",
            "value": 142680.958,
            "range": "± 1858.216 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1",
            "value": 0.642,
            "range": "± 0.002 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1000000",
            "value": 644414.673,
            "range": "± 346.196 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_eval_linear_sec_p6_x1000000",
            "value": 641240.512,
            "range": "± 1698.370 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6",
            "value": 0.597,
            "range": "± 0.002 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6_x1000000",
            "value": 596869.624,
            "range": "± 1615.023 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6",
            "value": 0.599,
            "range": "± 0.005 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6_x1000000",
            "value": 598014.188,
            "range": "± 1929.135 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6",
            "value": 0.602,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6_x1000000",
            "value": 601212.656,
            "range": "± 1685.624 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6",
            "value": 0.602,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6_x1000000",
            "value": 603674.406,
            "range": "± 421.205 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_countones_w64",
            "value": 45177.113,
            "range": "± 1499.910 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1",
            "value": 0.469,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1000000",
            "value": 475418.378,
            "range": "± 2116.069 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_std_counter_w32",
            "value": 7656.985,
            "range": "± 209.048 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1",
            "value": 0.007,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1000000",
            "value": 7212.089,
            "range": "± 50.468 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_std_counter_w32_x1000000",
            "value": 35043.003,
            "range": "± 1038.414 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_gray_counter_w32",
            "value": 10922.289,
            "range": "± 122.363 us",
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
            "value": 11492.419,
            "range": "± 72.138 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_gray_counter_w32_x1000000",
            "value": 33141.215,
            "range": "± 101.754 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_countones_w64",
            "value": 28027.548,
            "range": "± 924.345 us",
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
            "value": 76548.873,
            "range": "± 640.947 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_linear_sec_p6",
            "value": 140532.909,
            "range": "± 2482.604 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1",
            "value": 0.602,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1000000",
            "value": 607133.908,
            "range": "± 1740.946 us",
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
          "id": "b71d5fa481a9704008d413cdb84169eefd93b188",
          "message": "Fix .at() returning wrong values for non-byte-aligned array elements\n\nThe JIT stores all array elements bit-packed (element i starts at bit\ni*W), but createArrayDut assumed byte-aligned stride for elements >= 8\nbits wide. For widths not divisible by 8 (e.g. logic<34>[4]), this\ncaused a cumulative bit shift on every element after index 0.\n\nAdd readBitPackedWide/writeBitPackedWide for BigInt-based bit-level\narray access, and split createArrayDut into three paths: sub-byte,\nnon-byte-aligned (>= 8 bits), and byte-aligned.",
          "timestamp": "2026-03-07T14:55:05Z",
          "tree_id": "6a0ddef2b7369e4a710afc1b0bb9f7c1206d3b12",
          "url": "https://github.com/celox-sim/celox/commit/b71d5fa481a9704008d413cdb84169eefd93b188"
        },
        "date": 1772896714309,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "rust/simulator_tick_x10000",
            "value": 3428.529,
            "range": "± 12.825 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_step_x20000",
            "value": 10027.742,
            "range": "± 18.864 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_top_n1000",
            "value": 331414.853,
            "range": "± 4139.977 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1",
            "value": 0.341,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1000000",
            "value": 343169.054,
            "range": "± 756.041 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1",
            "value": 0.407,
            "range": "± 0.003 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1000000",
            "value": 407877.784,
            "range": "± 1589.791 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_linear_sec_p6",
            "value": 141056.023,
            "range": "± 2157.370 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1",
            "value": 0.642,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1000000",
            "value": 644273.774,
            "range": "± 665.268 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_eval_linear_sec_p6_x1000000",
            "value": 641353.104,
            "range": "± 1679.533 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6",
            "value": 0.597,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6_x1000000",
            "value": 597747.321,
            "range": "± 1647.501 us",
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
            "value": 598587.515,
            "range": "± 1631.575 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6",
            "value": 0.602,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6_x1000000",
            "value": 600230.783,
            "range": "± 5647.048 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6",
            "value": 0.602,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6_x1000000",
            "value": 602740.324,
            "range": "± 5420.028 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_countones_w64",
            "value": 43683.321,
            "range": "± 1085.098 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1",
            "value": 0.47,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1000000",
            "value": 476282.009,
            "range": "± 503.133 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_std_counter_w32",
            "value": 7603.169,
            "range": "± 42.850 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1",
            "value": 0.007,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1000000",
            "value": 7427.395,
            "range": "± 30.560 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_std_counter_w32_x1000000",
            "value": 35032.163,
            "range": "± 492.721 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_gray_counter_w32",
            "value": 10856.827,
            "range": "± 319.734 us",
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
            "value": 11769.111,
            "range": "± 36.734 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_gray_counter_w32_x1000000",
            "value": 33827.57,
            "range": "± 106.871 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_countones_w64",
            "value": 29351.228,
            "range": "± 1112.465 us",
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
            "value": 75762.95,
            "range": "± 205.568 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_linear_sec_p6",
            "value": 140800.227,
            "range": "± 2106.314 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1",
            "value": 0.604,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1000000",
            "value": 607247.841,
            "range": "± 1563.981 us",
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
          "id": "3acefb671902ffb2d55a66293508db14c5264ec7",
          "message": "Cache comb_func on JitBackend to avoid Arc dereference on hot path\n\neval_comb() is called on every tick. Accessing the function pointer\nthrough self.shared.comb_func requires an Arc pointer chase that\npenalizes performance (~8% locally, potentially worse under cache\npressure in CI). Copy the SimFunc pointer directly onto JitBackend\nat construction time.",
          "timestamp": "2026-03-07T15:48:17Z",
          "tree_id": "d1d1c439a60294d15f20663a261f3674424505b3",
          "url": "https://github.com/celox-sim/celox/commit/3acefb671902ffb2d55a66293508db14c5264ec7"
        },
        "date": 1772899921721,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "rust/simulator_tick_x10000",
            "value": 3418.207,
            "range": "± 9.129 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_step_x20000",
            "value": 9968.462,
            "range": "± 110.849 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_top_n1000",
            "value": 335031.044,
            "range": "± 4804.445 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1",
            "value": 0.327,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1000000",
            "value": 342356.959,
            "range": "± 874.649 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1",
            "value": 0.401,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1000000",
            "value": 402940.383,
            "range": "± 718.309 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_linear_sec_p6",
            "value": 144871.324,
            "range": "± 1949.594 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1",
            "value": 0.635,
            "range": "± 0.002 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1000000",
            "value": 641252.045,
            "range": "± 456.835 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_eval_linear_sec_p6_x1000000",
            "value": 639263.043,
            "range": "± 617.795 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6",
            "value": 0.598,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6_x1000000",
            "value": 597982.13,
            "range": "± 842.139 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6",
            "value": 0.598,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6_x1000000",
            "value": 602527.358,
            "range": "± 226.667 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6",
            "value": 0.604,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6_x1000000",
            "value": 601453.366,
            "range": "± 718.353 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6",
            "value": 0.603,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6_x1000000",
            "value": 603354.891,
            "range": "± 292.167 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_countones_w64",
            "value": 44293.116,
            "range": "± 600.878 us",
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
            "value": 477156.586,
            "range": "± 1478.458 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_std_counter_w32",
            "value": 7736.151,
            "range": "± 363.661 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1",
            "value": 0.007,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1000000",
            "value": 7421.593,
            "range": "± 28.258 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_std_counter_w32_x1000000",
            "value": 28235.54,
            "range": "± 621.932 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_gray_counter_w32",
            "value": 10908.483,
            "range": "± 119.533 us",
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
            "value": 11327.472,
            "range": "± 47.387 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_gray_counter_w32_x1000000",
            "value": 33168.933,
            "range": "± 143.296 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_countones_w64",
            "value": 28127.087,
            "range": "± 904.366 us",
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
            "value": 76299.766,
            "range": "± 246.771 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_linear_sec_p6",
            "value": 139542.478,
            "range": "± 2683.070 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1",
            "value": 0.6,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1000000",
            "value": 608762.448,
            "range": "± 366.371 us",
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
          "id": "c03af0f45dbf2eab7f759d48d1500d6023bce397",
          "message": "Fix AsyncLow reset polarity in Verilator top_n1000 benchmark\n\nThe reset sequence had rst=1 (release) before rst=0 (assert), leaving\nthe counters in reset during the entire benchmark. This matches the\nsame bug that was fixed for the Celox Rust benchmarks in 20cb7a2.",
          "timestamp": "2026-03-07T16:55:34Z",
          "tree_id": "965a33644531d48055c3acce1c6a9e10127a5ab2",
          "url": "https://github.com/celox-sim/celox/commit/c03af0f45dbf2eab7f759d48d1500d6023bce397"
        },
        "date": 1772903989887,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "rust/simulator_tick_x10000",
            "value": 3418.516,
            "range": "± 72.701 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_step_x20000",
            "value": 9994.176,
            "range": "± 49.438 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_top_n1000",
            "value": 331535.83,
            "range": "± 3902.809 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1",
            "value": 0.327,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1000000",
            "value": 342398.893,
            "range": "± 1222.800 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1",
            "value": 0.401,
            "range": "± 0.005 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1000000",
            "value": 402891.297,
            "range": "± 390.235 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_linear_sec_p6",
            "value": 141142.496,
            "range": "± 1755.726 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1",
            "value": 0.635,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1000000",
            "value": 640502.999,
            "range": "± 1810.800 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_eval_linear_sec_p6_x1000000",
            "value": 639725.617,
            "range": "± 766.566 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6",
            "value": 0.597,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6_x1000000",
            "value": 596817.991,
            "range": "± 2021.636 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6",
            "value": 0.598,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6_x1000000",
            "value": 602520.152,
            "range": "± 4052.088 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6",
            "value": 0.603,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6_x1000000",
            "value": 601425.882,
            "range": "± 1329.018 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6",
            "value": 0.602,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6_x1000000",
            "value": 603404.351,
            "range": "± 1806.684 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_countones_w64",
            "value": 43340.401,
            "range": "± 668.292 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1",
            "value": 0.469,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1000000",
            "value": 476437.859,
            "range": "± 2027.820 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_std_counter_w32",
            "value": 7620.934,
            "range": "± 56.454 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1",
            "value": 0.007,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1000000",
            "value": 7310.687,
            "range": "± 37.890 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_std_counter_w32_x1000000",
            "value": 28344.221,
            "range": "± 69.746 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_gray_counter_w32",
            "value": 10829.439,
            "range": "± 67.527 us",
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
            "value": 11284.502,
            "range": "± 54.579 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_gray_counter_w32_x1000000",
            "value": 32617.403,
            "range": "± 112.863 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_countones_w64",
            "value": 27957.63,
            "range": "± 920.408 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_countones_w64_x1",
            "value": 0.075,
            "range": "± 0.002 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_countones_w64_x1000000",
            "value": 77355.105,
            "range": "± 719.802 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_linear_sec_p6",
            "value": 137905.855,
            "range": "± 3175.661 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1",
            "value": 0.6,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1000000",
            "value": 608228.769,
            "range": "± 353.378 us",
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
          "id": "d7529d58a27f5681d9ef8751d5a49c32b27d8e91",
          "message": "Share Veryl benchmark sources between Celox and Verilator benchmarks\n\nExtract wrapper module sources into benches/veryl/*.veryl and reference\nthem via include_str! from both simulation.rs and celox-bench-sv.\nThis guarantees the Celox JIT and Verilator benchmarks run identical\ncircuits compiled from the same Veryl source.\n\nAlso unify the LinearSec top module name to \"Top\" (was \"LinearSecTop\"\nin the Verilator bench only), matching the Celox Rust benchmarks.",
          "timestamp": "2026-03-07T18:11:59Z",
          "tree_id": "ecdb903854ea4d42d06881cf4994785e9f7f96aa",
          "url": "https://github.com/celox-sim/celox/commit/d7529d58a27f5681d9ef8751d5a49c32b27d8e91"
        },
        "date": 1772908575857,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "rust/simulator_tick_x10000",
            "value": 3418.055,
            "range": "± 13.222 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_step_x20000",
            "value": 10089.895,
            "range": "± 52.625 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_top_n1000",
            "value": 339586.833,
            "range": "± 3410.781 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1",
            "value": 0.341,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1000000",
            "value": 341725.3,
            "range": "± 593.692 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1",
            "value": 0.402,
            "range": "± 0.004 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1000000",
            "value": 404014.288,
            "range": "± 2368.441 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_linear_sec_p6",
            "value": 145692.87,
            "range": "± 1565.307 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1",
            "value": 0.643,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1000000",
            "value": 645812.321,
            "range": "± 434.643 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_eval_linear_sec_p6_x1000000",
            "value": 645549.084,
            "range": "± 1933.435 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6",
            "value": 0.596,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6_x1000000",
            "value": 597390.541,
            "range": "± 264.567 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6",
            "value": 0.598,
            "range": "± 0.004 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6_x1000000",
            "value": 599171.546,
            "range": "± 511.518 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6",
            "value": 0.604,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6_x1000000",
            "value": 600468.308,
            "range": "± 1922.815 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6",
            "value": 0.603,
            "range": "± 0.002 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6_x1000000",
            "value": 602674.699,
            "range": "± 423.422 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_countones_w64",
            "value": 45816.178,
            "range": "± 624.057 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1",
            "value": 0.47,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1000000",
            "value": 476759.293,
            "range": "± 435.103 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_std_counter_w32",
            "value": 7767.428,
            "range": "± 58.780 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1",
            "value": 0.007,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1000000",
            "value": 7699.625,
            "range": "± 24.442 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_std_counter_w32_x1000000",
            "value": 28962.852,
            "range": "± 74.958 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_gray_counter_w32",
            "value": 11201.76,
            "range": "± 202.589 us",
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
            "value": 11969.872,
            "range": "± 60.480 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_gray_counter_w32_x1000000",
            "value": 33418.401,
            "range": "± 169.602 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_countones_w64",
            "value": 29856.396,
            "range": "± 733.566 us",
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
            "value": 76412.219,
            "range": "± 241.705 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_linear_sec_p6",
            "value": 143940.381,
            "range": "± 2144.437 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1",
            "value": 0.601,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1000000",
            "value": 607364.386,
            "range": "± 380.940 us",
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
          "id": "0bd3099f2bfb1978b820362773a3ae7d94e154f4",
          "message": "Fix eval_constexpr to recursively evaluate param expressions\n\nWhen the Veryl analyzer marks compound expressions like `N - 1` as\nis_const=true but leaves the top-level value as Unknown, eval_constexpr\nreturned None. This broke constant folding in parse_if_statement (param-\nbased conditions not folded), parse_binary for Pow (param exponents\nrejected), and concatenation replication counts.\n\nRecursively evaluate Binary/Unary sub-expressions when the top-level\ncomptime value is missing, supporting arithmetic, bitwise, shift, and\ncomparison operators.",
          "timestamp": "2026-03-07T22:59:04Z",
          "tree_id": "56f11d3c48848207e5260ac40b901a45e89f63a5",
          "url": "https://github.com/celox-sim/celox/commit/0bd3099f2bfb1978b820362773a3ae7d94e154f4"
        },
        "date": 1772925811698,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "rust/simulator_tick_x10000",
            "value": 3404.942,
            "range": "± 43.498 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_step_x20000",
            "value": 9766.405,
            "range": "± 21.322 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_top_n1000",
            "value": 338493.346,
            "range": "± 4462.603 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1",
            "value": 0.341,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1000000",
            "value": 341538.88,
            "range": "± 305.626 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1",
            "value": 0.383,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1000000",
            "value": 384620.511,
            "range": "± 468.841 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_linear_sec_p6",
            "value": 147847.281,
            "range": "± 1412.542 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1",
            "value": 0.647,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1000000",
            "value": 648679.612,
            "range": "± 737.202 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_eval_linear_sec_p6_x1000000",
            "value": 643743.871,
            "range": "± 786.119 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6",
            "value": 0.597,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6_x1000000",
            "value": 596598.436,
            "range": "± 314.504 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6",
            "value": 0.599,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6_x1000000",
            "value": 600177.412,
            "range": "± 147.839 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6",
            "value": 0.601,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6_x1000000",
            "value": 602651.23,
            "range": "± 1660.054 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6",
            "value": 0.602,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6_x1000000",
            "value": 603581.982,
            "range": "± 947.126 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_countones_w64",
            "value": 46972.252,
            "range": "± 502.807 us",
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
            "value": 476930.128,
            "range": "± 358.628 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_std_counter_w32",
            "value": 8050.698,
            "range": "± 85.241 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1",
            "value": 0.007,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1000000",
            "value": 7622.159,
            "range": "± 13.982 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_std_counter_w32_x1000000",
            "value": 37348.513,
            "range": "± 133.595 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_gray_counter_w32",
            "value": 11230.78,
            "range": "± 299.611 us",
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
            "value": 11262.008,
            "range": "± 90.551 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_gray_counter_w32_x1000000",
            "value": 33019.832,
            "range": "± 135.140 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_countones_w64",
            "value": 30136.719,
            "range": "± 741.047 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_countones_w64_x1",
            "value": 0.082,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_countones_w64_x1000000",
            "value": 82699.871,
            "range": "± 228.282 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_linear_sec_p6",
            "value": 143293.227,
            "range": "± 2506.111 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1",
            "value": 0.606,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1000000",
            "value": 612415.427,
            "range": "± 423.442 us",
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
          "id": "8f74385654dddb948f9a9cf6e599d76456038be9",
          "message": "Prevent use-after-free on DUT access after dispose\n\nshared_memory() passes a raw pointer to Vec<u64> via\nUint8Array::with_external_data with a no-op finalizer. When dispose()\ndrops the JitBackend, the Vec is freed but the JS ArrayBuffer/DataView\nremain alive, allowing DUT getter/setters to read/write freed memory.\n\nAdd a disposed flag to DirtyState and check it at the top of every DUT\naccessor (defineSignalProperty get/set, createArrayDut at/set for all\nthree paths). Set the flag BEFORE calling _handle.dispose() so the\nguard is active before the Rust memory is freed.",
          "timestamp": "2026-03-08T00:03:06Z",
          "tree_id": "6b6ec3238177dc7f8098542a9f829934d59ddf44",
          "url": "https://github.com/celox-sim/celox/commit/8f74385654dddb948f9a9cf6e599d76456038be9"
        },
        "date": 1772929640661,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "rust/simulator_tick_x10000",
            "value": 3405.401,
            "range": "± 39.842 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_step_x20000",
            "value": 9714.634,
            "range": "± 73.383 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_top_n1000",
            "value": 336007.758,
            "range": "± 3278.172 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1",
            "value": 0.341,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1000000",
            "value": 341540.737,
            "range": "± 2310.689 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1",
            "value": 0.383,
            "range": "± 0.002 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1000000",
            "value": 384233.826,
            "range": "± 632.154 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_linear_sec_p6",
            "value": 147966.638,
            "range": "± 1447.476 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1",
            "value": 0.649,
            "range": "± 0.005 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1000000",
            "value": 650831.062,
            "range": "± 725.619 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_eval_linear_sec_p6_x1000000",
            "value": 645527.508,
            "range": "± 2885.248 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6",
            "value": 0.598,
            "range": "± 0.014 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6_x1000000",
            "value": 597542.187,
            "range": "± 2649.656 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6",
            "value": 0.597,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6_x1000000",
            "value": 600036.796,
            "range": "± 2459.147 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6",
            "value": 0.603,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6_x1000000",
            "value": 602820.675,
            "range": "± 334.416 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6",
            "value": 0.603,
            "range": "± 0.019 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6_x1000000",
            "value": 603368.268,
            "range": "± 2660.807 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_countones_w64",
            "value": 46844.806,
            "range": "± 709.146 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1",
            "value": 0.469,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1000000",
            "value": 476014.88,
            "range": "± 1716.717 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_std_counter_w32",
            "value": 8187.291,
            "range": "± 331.465 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1",
            "value": 0.007,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1000000",
            "value": 7441.638,
            "range": "± 13.326 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_std_counter_w32_x1000000",
            "value": 37371.997,
            "range": "± 1036.513 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_gray_counter_w32",
            "value": 12323.406,
            "range": "± 341.540 us",
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
            "value": 11273.934,
            "range": "± 77.291 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_gray_counter_w32_x1000000",
            "value": 33136.44,
            "range": "± 875.254 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_countones_w64",
            "value": 30717.852,
            "range": "± 538.525 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_countones_w64_x1",
            "value": 0.081,
            "range": "± 0.002 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_countones_w64_x1000000",
            "value": 82629.448,
            "range": "± 815.787 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_linear_sec_p6",
            "value": 144230.549,
            "range": "± 1345.711 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1",
            "value": 0.606,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1000000",
            "value": 612604.279,
            "range": "± 382.394 us",
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
          "id": "5c568535f3416422e789135decda5a56a235b296",
          "message": "test: increase absolute time limits for sorter_tree CI\n\nCI servers are significantly slower than dev machines. Increase limits:\n- N=64: 60s → 120s\n- N=128: 180s → 360s\n\nThe ratio-based scaling tests are the real regression guards;\nabsolute time limits just prevent catastrophic blowup.",
          "timestamp": "2026-03-10T00:08:58Z",
          "tree_id": "4d299b9f61314e7d896f290ddca99160e3447598",
          "url": "https://github.com/celox-sim/celox/commit/5c568535f3416422e789135decda5a56a235b296"
        },
        "date": 1773102834841,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "rust/simulator_tick_x10000",
            "value": 3126.676,
            "range": "± 13.363 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_step_x20000",
            "value": 12815.628,
            "range": "± 117.920 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_top_n1000",
            "value": 375776.71,
            "range": "± 3064.062 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1",
            "value": 0.322,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1000000",
            "value": 312831.128,
            "range": "± 508.434 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1",
            "value": 0.379,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1000000",
            "value": 372623.339,
            "range": "± 2414.012 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_linear_sec_p6",
            "value": 136295.998,
            "range": "± 1340.417 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1",
            "value": 0.637,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1000000",
            "value": 640244.906,
            "range": "± 412.764 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_eval_linear_sec_p6_x1000000",
            "value": 638867.561,
            "range": "± 384.290 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6",
            "value": 0.599,
            "range": "± 0.002 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6_x1000000",
            "value": 598892.989,
            "range": "± 542.829 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6",
            "value": 0.597,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6_x1000000",
            "value": 603012.486,
            "range": "± 719.544 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6",
            "value": 0.602,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6_x1000000",
            "value": 601569.096,
            "range": "± 548.356 us",
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
            "value": 608110.146,
            "range": "± 293.067 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_countones_w64",
            "value": 43641.778,
            "range": "± 793.526 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1",
            "value": 0.476,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1000000",
            "value": 481957.681,
            "range": "± 262.875 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_std_counter_w32",
            "value": 9465.452,
            "range": "± 79.660 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1",
            "value": 0.007,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1000000",
            "value": 5922.234,
            "range": "± 32.566 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_std_counter_w32_x1000000",
            "value": 28006.464,
            "range": "± 221.896 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_gray_counter_w32",
            "value": 13327.989,
            "range": "± 99.850 us",
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
            "value": 8709.692,
            "range": "± 12.079 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_gray_counter_w32_x1000000",
            "value": 31940.229,
            "range": "± 115.099 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_countones_w64",
            "value": 27348.905,
            "range": "± 261.587 us",
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
            "value": 75828.987,
            "range": "± 565.176 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_linear_sec_p6",
            "value": 134945.408,
            "range": "± 1313.198 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1",
            "value": 0.6,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1000000",
            "value": 607140.948,
            "range": "± 387.439 us",
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
          "id": "1cf64ff1f6060c361a0052a8fc396b51cac25b0a",
          "message": "style: cargo fmt",
          "timestamp": "2026-03-10T02:50:52Z",
          "tree_id": "789fadd993f9c630883a5e6b3f4e16125f2db617",
          "url": "https://github.com/celox-sim/celox/commit/1cf64ff1f6060c361a0052a8fc396b51cac25b0a"
        },
        "date": 1773112868247,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "rust/simulator_tick_x10000",
            "value": 3407.958,
            "range": "± 75.330 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_step_x20000",
            "value": 9737.635,
            "range": "± 63.227 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_top_n1000",
            "value": 374980.44,
            "range": "± 4615.662 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1",
            "value": 0.328,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1000000",
            "value": 340826.373,
            "range": "± 1893.020 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1",
            "value": 0.411,
            "range": "± 0.003 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1000000",
            "value": 410388.905,
            "range": "± 1449.505 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_linear_sec_p6",
            "value": 142992.813,
            "range": "± 1931.482 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1",
            "value": 0.641,
            "range": "± 0.002 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1000000",
            "value": 642017.467,
            "range": "± 1897.263 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_eval_linear_sec_p6_x1000000",
            "value": 641154.61,
            "range": "± 2101.019 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6",
            "value": 0.598,
            "range": "± 0.003 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6_x1000000",
            "value": 596798.468,
            "range": "± 1713.570 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6",
            "value": 0.599,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6_x1000000",
            "value": 597887.292,
            "range": "± 2009.083 us",
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
            "value": 604160.481,
            "range": "± 681.388 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6",
            "value": 0.606,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6_x1000000",
            "value": 603223.815,
            "range": "± 353.173 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_countones_w64",
            "value": 44638.882,
            "range": "± 932.381 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1",
            "value": 0.467,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1000000",
            "value": 473878.295,
            "range": "± 539.387 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_std_counter_w32",
            "value": 9473.748,
            "range": "± 99.625 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1",
            "value": 0.007,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1000000",
            "value": 5603.434,
            "range": "± 134.735 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_std_counter_w32_x1000000",
            "value": 27918.19,
            "range": "± 59.664 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_gray_counter_w32",
            "value": 13423.184,
            "range": "± 99.720 us",
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
            "value": 8085.216,
            "range": "± 31.630 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_gray_counter_w32_x1000000",
            "value": 31566.558,
            "range": "± 576.646 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_countones_w64",
            "value": 27927.086,
            "range": "± 423.138 us",
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
            "value": 77324.329,
            "range": "± 1545.366 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_linear_sec_p6",
            "value": 139415.59,
            "range": "± 1240.314 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1",
            "value": 0.6,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1000000",
            "value": 607200.084,
            "range": "± 406.812 us",
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
          "id": "ce4ef3314df71f871eceff94ba337ccd4a840636",
          "message": "Fix clippy manual_unwrap_or lint in ff.rs",
          "timestamp": "2026-03-10T18:27:56Z",
          "tree_id": "0b42ffc5654ef34d1899c4e8294d442269cb4b8f",
          "url": "https://github.com/celox-sim/celox/commit/ce4ef3314df71f871eceff94ba337ccd4a840636"
        },
        "date": 1773168788765,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "rust/simulator_tick_x10000",
            "value": 3406.428,
            "range": "± 9.256 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_step_x20000",
            "value": 9839.84,
            "range": "± 49.064 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_top_n1000",
            "value": 389088.371,
            "range": "± 5233.633 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1",
            "value": 0.329,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1000000",
            "value": 341259.568,
            "range": "± 401.909 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1",
            "value": 0.376,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1000000",
            "value": 390168.537,
            "range": "± 896.178 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_linear_sec_p6",
            "value": 145414.236,
            "range": "± 2558.513 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1",
            "value": 0.639,
            "range": "± 0.002 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1000000",
            "value": 643650.434,
            "range": "± 801.064 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_eval_linear_sec_p6_x1000000",
            "value": 642974.248,
            "range": "± 4329.933 us",
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
            "value": 597424.875,
            "range": "± 683.708 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6",
            "value": 0.601,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6_x1000000",
            "value": 599582.723,
            "range": "± 1555.476 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6",
            "value": 0.604,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6_x1000000",
            "value": 602594.559,
            "range": "± 388.312 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6",
            "value": 0.606,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6_x1000000",
            "value": 602163.856,
            "range": "± 1343.997 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_countones_w64",
            "value": 45320.583,
            "range": "± 1855.430 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1",
            "value": 0.469,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1000000",
            "value": 475796.919,
            "range": "± 1420.726 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_std_counter_w32",
            "value": 9458.159,
            "range": "± 80.426 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1",
            "value": 0.007,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1000000",
            "value": 5749.021,
            "range": "± 21.770 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_std_counter_w32_x1000000",
            "value": 27710.385,
            "range": "± 81.936 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_gray_counter_w32",
            "value": 13629.486,
            "range": "± 343.725 us",
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
            "value": 8087.245,
            "range": "± 23.109 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_gray_counter_w32_x1000000",
            "value": 31567.549,
            "range": "± 185.834 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_countones_w64",
            "value": 29901.859,
            "range": "± 660.586 us",
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
            "value": 76661.141,
            "range": "± 1095.788 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_linear_sec_p6",
            "value": 140204.352,
            "range": "± 2481.999 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1",
            "value": 0.604,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1000000",
            "value": 609624.256,
            "range": "± 4035.893 us",
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
          "id": "d77ad012b5282e8fc5790f128b895d3ac3836f79",
          "message": "Add optimization tuning guide and benchmark tool\n\n- Add docs/guide/optimization-tuning.md (EN) and docs/ja/ (JA) with\n  benchmark-backed guidance on SIRT pass interactions, Cranelift backend\n  options, and design-dependent behavior\n- Add pass_benchmark example for measuring per-pass compile/sim impact\n- Add sidebar entries in VitePress config\n- Link guide from CLAUDE.md optimizer section",
          "timestamp": "2026-03-10T19:23:33Z",
          "tree_id": "fcdafce122d42d04e3114380629ad5c1936d20a4",
          "url": "https://github.com/celox-sim/celox/commit/d77ad012b5282e8fc5790f128b895d3ac3836f79"
        },
        "date": 1773172105732,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "rust/simulator_tick_x10000",
            "value": 3405.189,
            "range": "± 49.616 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_step_x20000",
            "value": 9871.463,
            "range": "± 40.264 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_top_n1000",
            "value": 380460.684,
            "range": "± 2534.618 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1",
            "value": 0.329,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1000000",
            "value": 341236.103,
            "range": "± 320.430 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1",
            "value": 0.376,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1000000",
            "value": 390022.81,
            "range": "± 754.393 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_linear_sec_p6",
            "value": 141140.98,
            "range": "± 1603.127 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1",
            "value": 0.64,
            "range": "± 0.002 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1000000",
            "value": 645129.301,
            "range": "± 4235.755 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_eval_linear_sec_p6_x1000000",
            "value": 644338.596,
            "range": "± 617.573 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6",
            "value": 0.598,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6_x1000000",
            "value": 598871.872,
            "range": "± 596.568 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6",
            "value": 0.601,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6_x1000000",
            "value": 597882.393,
            "range": "± 3157.030 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6",
            "value": 0.604,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6_x1000000",
            "value": 602064.829,
            "range": "± 334.820 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6",
            "value": 0.606,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6_x1000000",
            "value": 603692.272,
            "range": "± 503.417 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_countones_w64",
            "value": 44565.487,
            "range": "± 618.702 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1",
            "value": 0.467,
            "range": "± 0.002 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1000000",
            "value": 473423.597,
            "range": "± 553.656 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_std_counter_w32",
            "value": 9433.374,
            "range": "± 94.114 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1",
            "value": 0.007,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1000000",
            "value": 5650.581,
            "range": "± 19.880 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_std_counter_w32_x1000000",
            "value": 27582.308,
            "range": "± 156.009 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_gray_counter_w32",
            "value": 13391.019,
            "range": "± 140.402 us",
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
            "value": 8700.568,
            "range": "± 14.637 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_gray_counter_w32_x1000000",
            "value": 32153.648,
            "range": "± 76.782 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_countones_w64",
            "value": 28742.382,
            "range": "± 560.977 us",
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
            "value": 76837.338,
            "range": "± 777.799 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_linear_sec_p6",
            "value": 139680.044,
            "range": "± 1375.132 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1",
            "value": 0.602,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1000000",
            "value": 608137.977,
            "range": "± 270.841 us",
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
          "id": "43dde93cf46460d92f9c9bea8fa96f40bd3ffaa4",
          "message": "Add bench example files to .gitignore",
          "timestamp": "2026-03-11T02:13:10Z",
          "tree_id": "146a6f835fb3dd473780e89c89774f67c61af396",
          "url": "https://github.com/celox-sim/celox/commit/43dde93cf46460d92f9c9bea8fa96f40bd3ffaa4"
        },
        "date": 1773196662463,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "rust/simulator_tick_x10000",
            "value": 3395.099,
            "range": "± 10.527 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_step_x20000",
            "value": 9941.861,
            "range": "± 89.789 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_top_n1000",
            "value": 327012.682,
            "range": "± 5548.402 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1",
            "value": 0.342,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1000000",
            "value": 340857.64,
            "range": "± 3023.026 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1",
            "value": 0.372,
            "range": "± 0.002 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1000000",
            "value": 377375.866,
            "range": "± 2911.625 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_linear_sec_p6",
            "value": 136905.866,
            "range": "± 1129.740 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1",
            "value": 0.636,
            "range": "± 0.003 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1000000",
            "value": 639229.931,
            "range": "± 5516.594 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_eval_linear_sec_p6_x1000000",
            "value": 639151.875,
            "range": "± 2501.871 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6",
            "value": 0.597,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6_x1000000",
            "value": 596890.951,
            "range": "± 3823.354 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6",
            "value": 0.597,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6_x1000000",
            "value": 598292.809,
            "range": "± 413.922 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6",
            "value": 0.603,
            "range": "± 0.002 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6_x1000000",
            "value": 602742.272,
            "range": "± 448.483 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6",
            "value": 0.601,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6_x1000000",
            "value": 603024.169,
            "range": "± 7484.910 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_countones_w64",
            "value": 43243.471,
            "range": "± 496.167 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1",
            "value": 0.47,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1000000",
            "value": 476279.357,
            "range": "± 3672.822 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_std_counter_w32",
            "value": 9466.478,
            "range": "± 47.665 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1",
            "value": 0.007,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1000000",
            "value": 5926.67,
            "range": "± 18.371 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_std_counter_w32_x1000000",
            "value": 27709.986,
            "range": "± 57.923 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_gray_counter_w32",
            "value": 13363.598,
            "range": "± 88.165 us",
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
            "value": 8399.122,
            "range": "± 183.967 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_gray_counter_w32_x1000000",
            "value": 31567.266,
            "range": "± 95.278 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_countones_w64",
            "value": 26978.965,
            "range": "± 1888.773 us",
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
            "value": 76431.644,
            "range": "± 211.299 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_linear_sec_p6",
            "value": 134550.735,
            "range": "± 2423.662 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1",
            "value": 0.599,
            "range": "± 0.002 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1000000",
            "value": 604666.603,
            "range": "± 315.916 us",
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
          "id": "c8d3bae50ae1ef6a0e4adbff0c6162aae08a8b93",
          "message": "Fix formatting (cargo fmt)",
          "timestamp": "2026-03-14T22:15:00Z",
          "tree_id": "1664e162817ac93ebcbbb18e6ce72b5db813dfa3",
          "url": "https://github.com/celox-sim/celox/commit/c8d3bae50ae1ef6a0e4adbff0c6162aae08a8b93"
        },
        "date": 1773528249682,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "rust/simulator_tick_x10000",
            "value": 3398.598,
            "range": "± 12.308 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_step_x20000",
            "value": 9951.147,
            "range": "± 61.742 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_top_n1000",
            "value": 319530.353,
            "range": "± 3411.815 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1",
            "value": 0.342,
            "range": "± 0.005 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1000000",
            "value": 341430.96,
            "range": "± 1116.391 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1",
            "value": 0.366,
            "range": "± 0.013 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1000000",
            "value": 376429.159,
            "range": "± 724.412 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_linear_sec_p6",
            "value": 147786.629,
            "range": "± 2195.401 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1",
            "value": 0.635,
            "range": "± 0.003 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1000000",
            "value": 640430.379,
            "range": "± 1656.213 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_eval_linear_sec_p6_x1000000",
            "value": 639360.243,
            "range": "± 1369.525 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6",
            "value": 0.598,
            "range": "± 0.002 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6_x1000000",
            "value": 599456.939,
            "range": "± 2131.619 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6",
            "value": 0.601,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6_x1000000",
            "value": 598439.194,
            "range": "± 353.149 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6",
            "value": 0.599,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6_x1000000",
            "value": 603214.499,
            "range": "± 1635.422 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6",
            "value": 0.603,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6_x1000000",
            "value": 603707.217,
            "range": "± 2042.486 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_countones_w64",
            "value": 49100.205,
            "range": "± 1267.941 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1",
            "value": 0.471,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1000000",
            "value": 477179.38,
            "range": "± 2193.878 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_std_counter_w32",
            "value": 9500.275,
            "range": "± 113.322 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1",
            "value": 0.007,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1000000",
            "value": 5600.583,
            "range": "± 9.716 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_std_counter_w32_x1000000",
            "value": 28089.155,
            "range": "± 54.225 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_gray_counter_w32",
            "value": 13440.109,
            "range": "± 113.373 us",
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
            "value": 8397.911,
            "range": "± 19.382 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_gray_counter_w32_x1000000",
            "value": 31575.273,
            "range": "± 119.328 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_countones_w64",
            "value": 30213.208,
            "range": "± 723.401 us",
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
            "value": 77890.332,
            "range": "± 311.581 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_linear_sec_p6",
            "value": 143649.499,
            "range": "± 1959.267 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1",
            "value": 0.6,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1000000",
            "value": 607463.917,
            "range": "± 312.155 us",
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
          "id": "be71b640110460211405c51067bd7b3c4a40a9ca",
          "message": "Fix FF parser widening shift result by RHS width\n\nThe FF path used max(lhs_width, rhs_width) for shift/pow operations,\ncausing the result to be incorrectly widened when the shift amount\n(e.g. a 32-bit for-loop constant) was wider than the value being\nshifted. This matches the comb path which already uses LHS-only width\nper IEEE 1800-2023 §11.4.10.",
          "timestamp": "2026-03-17T00:40:23Z",
          "tree_id": "99362c5f19276293e0f62fd00af147e277202f7a",
          "url": "https://github.com/celox-sim/celox/commit/be71b640110460211405c51067bd7b3c4a40a9ca"
        },
        "date": 1773710127416,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "rust/simulator_tick_x10000",
            "value": 3383.505,
            "range": "± 75.522 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_step_x20000",
            "value": 9843.12,
            "range": "± 152.671 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_top_n1000",
            "value": 328524.937,
            "range": "± 4583.716 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1",
            "value": 0.336,
            "range": "± 0.002 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1000000",
            "value": 326856.24,
            "range": "± 1708.440 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1",
            "value": 0.383,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1000000",
            "value": 373008.956,
            "range": "± 538.863 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_linear_sec_p6",
            "value": 139668.285,
            "range": "± 1250.088 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1",
            "value": 0.636,
            "range": "± 0.005 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1000000",
            "value": 643081.539,
            "range": "± 2063.235 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_eval_linear_sec_p6_x1000000",
            "value": 641732.39,
            "range": "± 2030.376 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6",
            "value": 0.598,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6_x1000000",
            "value": 597835.72,
            "range": "± 2414.822 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6",
            "value": 0.6,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6_x1000000",
            "value": 599760.332,
            "range": "± 307.062 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6",
            "value": 0.604,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6_x1000000",
            "value": 602970.659,
            "range": "± 2094.666 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6",
            "value": 0.607,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6_x1000000",
            "value": 603798.072,
            "range": "± 2631.242 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_countones_w64",
            "value": 45411.312,
            "range": "± 925.995 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1",
            "value": 0.469,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1000000",
            "value": 476427.204,
            "range": "± 1706.952 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_std_counter_w32",
            "value": 9608.808,
            "range": "± 296.716 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1",
            "value": 0.007,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1000000",
            "value": 5601.415,
            "range": "± 9.612 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_std_counter_w32_x1000000",
            "value": 27836.819,
            "range": "± 61.264 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_gray_counter_w32",
            "value": 13698.819,
            "range": "± 408.037 us",
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
            "value": 8089.475,
            "range": "± 207.163 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_gray_counter_w32_x1000000",
            "value": 31531.453,
            "range": "± 287.748 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_countones_w64",
            "value": 30263.668,
            "range": "± 679.273 us",
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
            "value": 77436.464,
            "range": "± 292.010 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_linear_sec_p6",
            "value": 142838.346,
            "range": "± 1740.415 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1",
            "value": 0.598,
            "range": "± 0.002 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1000000",
            "value": 605210.672,
            "range": "± 1053.754 us",
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
          "id": "d4fafe4a3e0a8a2177145268e98fad53b233108c",
          "message": "Add verilator obj_dir and debug trace files to .gitignore",
          "timestamp": "2026-03-23T12:02:39Z",
          "tree_id": "f0c7fa2e0db8a165397d2366eb7a6ccea2c8bc51",
          "url": "https://github.com/celox-sim/celox/commit/d4fafe4a3e0a8a2177145268e98fad53b233108c"
        },
        "date": 1774268856673,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "rust/simulator_tick_x10000",
            "value": 3267.121,
            "range": "± 47.828 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_step_x20000",
            "value": 9755.503,
            "range": "± 65.109 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_top_n1000",
            "value": 319563.472,
            "range": "± 2348.324 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1",
            "value": 0.337,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1000000",
            "value": 340723.648,
            "range": "± 567.477 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1",
            "value": 0.395,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1000000",
            "value": 396431.64,
            "range": "± 916.827 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_linear_sec_p6",
            "value": 137601.283,
            "range": "± 1095.093 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1",
            "value": 0.64,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1000000",
            "value": 645897.469,
            "range": "± 509.933 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_eval_linear_sec_p6_x1000000",
            "value": 640692.817,
            "range": "± 1190.090 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6",
            "value": 0.593,
            "range": "± 0.002 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6_x1000000",
            "value": 592830.378,
            "range": "± 350.965 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6",
            "value": 0.594,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6_x1000000",
            "value": 595230.254,
            "range": "± 327.858 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6",
            "value": 0.597,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6_x1000000",
            "value": 597492.373,
            "range": "± 387.875 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6",
            "value": 0.599,
            "range": "± 0.004 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6_x1000000",
            "value": 599607.067,
            "range": "± 413.884 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_countones_w64",
            "value": 42846.386,
            "range": "± 491.434 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1",
            "value": 0.636,
            "range": "± 0.002 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1000000",
            "value": 647553.862,
            "range": "± 1012.122 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_std_counter_w32",
            "value": 9401.268,
            "range": "± 62.023 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1",
            "value": 0.007,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1000000",
            "value": 5912.763,
            "range": "± 38.028 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_std_counter_w32_x1000000",
            "value": 28245.925,
            "range": "± 184.607 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_gray_counter_w32",
            "value": 13259.048,
            "range": "± 96.439 us",
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
            "value": 8632.743,
            "range": "± 18.416 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_gray_counter_w32_x1000000",
            "value": 32160.115,
            "range": "± 87.862 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_countones_w64",
            "value": 27042.842,
            "range": "± 90.960 us",
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
            "value": 80224.88,
            "range": "± 386.603 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_linear_sec_p6",
            "value": 134590.986,
            "range": "± 940.473 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1",
            "value": 0.597,
            "range": "± 0.002 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1000000",
            "value": 604886.977,
            "range": "± 287.176 us",
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
          "id": "6b01130bda6cbb89e98aff018061ecbdd262d23a",
          "message": "Add ts-gen tests for duplicate scoped variables and generate-for\n\nVerify that TypeScript type generation handles modules with:\n- Duplicate-named scoped variables in always_comb for-loop scopes\n- Generate-for blocks with scoped variables in always_comb\n- Complex patterns combining scoped vars with for-loop instances",
          "timestamp": "2026-03-23T13:01:54Z",
          "tree_id": "c5a099565af6cf5437a9eacbebe62bd6b7bdecb1",
          "url": "https://github.com/celox-sim/celox/commit/6b01130bda6cbb89e98aff018061ecbdd262d23a"
        },
        "date": 1774272473596,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "rust/simulator_tick_x10000",
            "value": 3287.446,
            "range": "± 28.066 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_step_x20000",
            "value": 9924.654,
            "range": "± 113.135 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_top_n1000",
            "value": 327703.862,
            "range": "± 2454.277 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1",
            "value": 0.332,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1000000",
            "value": 341376.792,
            "range": "± 330.430 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1",
            "value": 0.384,
            "range": "± 0.002 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1000000",
            "value": 381453.514,
            "range": "± 1237.799 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_linear_sec_p6",
            "value": 147924.548,
            "range": "± 2927.364 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1",
            "value": 0.636,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1000000",
            "value": 642216.773,
            "range": "± 1262.819 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_eval_linear_sec_p6_x1000000",
            "value": 639287.426,
            "range": "± 310.702 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6",
            "value": 0.592,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6_x1000000",
            "value": 593675.455,
            "range": "± 591.431 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6",
            "value": 0.594,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6_x1000000",
            "value": 595304.977,
            "range": "± 1151.872 us",
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
            "value": 599163.32,
            "range": "± 875.633 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6",
            "value": 0.604,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6_x1000000",
            "value": 599593.508,
            "range": "± 741.080 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_countones_w64",
            "value": 47389.649,
            "range": "± 925.786 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1",
            "value": 0.62,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1000000",
            "value": 633395.707,
            "range": "± 1308.918 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_std_counter_w32",
            "value": 10213.614,
            "range": "± 331.600 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1",
            "value": 0.007,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1000000",
            "value": 5602.517,
            "range": "± 18.719 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_std_counter_w32_x1000000",
            "value": 27995.347,
            "range": "± 58.547 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_gray_counter_w32",
            "value": 14227.639,
            "range": "± 358.821 us",
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
            "value": 8409.988,
            "range": "± 20.912 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_gray_counter_w32_x1000000",
            "value": 31608.789,
            "range": "± 128.042 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_countones_w64",
            "value": 31340.747,
            "range": "± 582.905 us",
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
            "value": 80543.53,
            "range": "± 256.309 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_linear_sec_p6",
            "value": 144203.057,
            "range": "± 1523.777 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1",
            "value": 0.598,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1000000",
            "value": 605982.192,
            "range": "± 610.408 us",
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
          "id": "5417c4031c8137cb416638da751fd0ef895e41c8",
          "message": "Fix clippy errors in celox crate",
          "timestamp": "2026-03-24T09:36:43Z",
          "tree_id": "9e22d82ae786a6f2f31acbca3bcdb1830f27c51f",
          "url": "https://github.com/celox-sim/celox/commit/5417c4031c8137cb416638da751fd0ef895e41c8"
        },
        "date": 1774346630993,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "rust/simulator_tick_x10000",
            "value": 6419.365,
            "range": "± 107.537 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_step_x20000",
            "value": 8408.717,
            "range": "± 43.700 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_top_n1000",
            "value": 374326.508,
            "range": "± 5732.797 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1",
            "value": 0.642,
            "range": "± 0.002 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1000000",
            "value": 642857.178,
            "range": "± 882.070 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1",
            "value": 0.674,
            "range": "± 0.003 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1000000",
            "value": 676552.049,
            "range": "± 1631.133 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_linear_sec_p6",
            "value": 122307.252,
            "range": "± 1907.406 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1",
            "value": 0.321,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1000000",
            "value": 326947.982,
            "range": "± 2596.654 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_eval_linear_sec_p6_x1000000",
            "value": 327966.333,
            "range": "± 798.498 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6",
            "value": 0.252,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6_x1000000",
            "value": 251434.084,
            "range": "± 322.611 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6",
            "value": 0.252,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6_x1000000",
            "value": 253157.783,
            "range": "± 689.907 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6",
            "value": 0.258,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6_x1000000",
            "value": 258462.412,
            "range": "± 347.714 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6",
            "value": 0.258,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6_x1000000",
            "value": 259106.94,
            "range": "± 219.351 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_countones_w64",
            "value": 26612.246,
            "range": "± 919.409 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1",
            "value": 0.54,
            "range": "± 0.010 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1000000",
            "value": 540463.449,
            "range": "± 2365.147 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_std_counter_w32",
            "value": 6701.624,
            "range": "± 101.008 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1",
            "value": 0.005,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_std_counter_w32_x1000000",
            "value": 5629.811,
            "range": "± 13.418 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_std_counter_w32_x1000000",
            "value": 36786.403,
            "range": "± 1213.858 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_gray_counter_w32",
            "value": 9196.325,
            "range": "± 257.860 us",
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
            "value": 8394.739,
            "range": "± 42.920 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_gray_counter_w32_x1000000",
            "value": 37810.024,
            "range": "± 59.581 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_countones_w64",
            "value": 17446.394,
            "range": "± 720.079 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_countones_w64_x1",
            "value": 0.101,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_countones_w64_x1000000",
            "value": 99980.999,
            "range": "± 511.819 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_linear_sec_p6",
            "value": 117811.098,
            "range": "± 1532.350 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1",
            "value": 0.287,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1000000",
            "value": 288905.318,
            "range": "± 1116.203 us",
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
          "id": "a3d0c9e1394a07f17161ed21aec156bc30dc45ca",
          "message": "Gate verify_assignment with #[cfg(debug_assertions)]\n\nThe function is only called under debug_assertions but wasn't\ngated itself, causing a dead_code error in release builds.",
          "timestamp": "2026-03-31T21:49:37Z",
          "tree_id": "fba0114887846dee4be93235db96591056fea09c",
          "url": "https://github.com/celox-sim/celox/commit/a3d0c9e1394a07f17161ed21aec156bc30dc45ca"
        },
        "date": 1774994940381,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "rust/simulator_tick_x10000",
            "value": 3738.067,
            "range": "± 135.865 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_step_x20000",
            "value": 9292.081,
            "range": "± 192.314 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_top_n1000",
            "value": 212738.5,
            "range": "± 3065.412 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1",
            "value": 0.371,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_tick_top_n1000_x1000000",
            "value": 370514.401,
            "range": "± 981.648 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1",
            "value": 0.418,
            "range": "± 0.003 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_top_n1000_x1000000",
            "value": 418900.472,
            "range": "± 299.269 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_linear_sec_p6",
            "value": 114132.501,
            "range": "± 866.246 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1",
            "value": 0.36,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_linear_sec_p6_x1000000",
            "value": 361774.201,
            "range": "± 473.181 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_eval_linear_sec_p6_x1000000",
            "value": 359224.381,
            "range": "± 956.976 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6",
            "value": 0.28,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_eval_comb_linear_sec_p6_x1000000",
            "value": 280296.414,
            "range": "± 352.906 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6",
            "value": 0.28,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_raw_io_eval_linear_sec_p6_x1000000",
            "value": 281753.865,
            "range": "± 356.292 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6",
            "value": 0.283,
            "range": "± 0.001 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_linear_sec_p6_x1000000",
            "value": 288556.461,
            "range": "± 2101.198 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6",
            "value": 0.291,
            "range": "± 0.002 us",
            "unit": "us"
          },
          {
            "name": "rust/isolation_set_eval_get_as_linear_sec_p6_x1000000",
            "value": 291836.855,
            "range": "± 193.533 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_countones_w64",
            "value": 20999.589,
            "range": "± 829.443 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1",
            "value": 0.298,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_eval_countones_w64_x1000000",
            "value": 300079.984,
            "range": "± 422.250 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_std_counter_w32",
            "value": 4941.014,
            "range": "± 160.105 us",
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
            "value": 8188.315,
            "range": "± 34.181 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_std_counter_w32_x1000000",
            "value": 34449.898,
            "range": "± 118.442 us",
            "unit": "us"
          },
          {
            "name": "rust/simulation_build_gray_counter_w32",
            "value": 7093.429,
            "range": "± 327.795 us",
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
            "value": 11347.609,
            "range": "± 22.801 us",
            "unit": "us"
          },
          {
            "name": "rust/testbench_tick_gray_counter_w32_x1000000",
            "value": 38574.988,
            "range": "± 132.357 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_countones_w64",
            "value": 15413.707,
            "range": "± 706.001 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_countones_w64_x1",
            "value": 0.097,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_countones_w64_x1000000",
            "value": 98342.895,
            "range": "± 167.586 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_build_linear_sec_p6",
            "value": 123188.28,
            "range": "± 1783.262 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1",
            "value": 0.306,
            "range": "± 0.000 us",
            "unit": "us"
          },
          {
            "name": "rust-dse/simulation_eval_linear_sec_p6_x1000000",
            "value": 307090.856,
            "range": "± 697.400 us",
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
        "date": 1772714142125,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ts/simulation_build_top_n1000",
            "value": 775.9956926666667,
            "range": "± 1.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1",
            "value": 0.0005196107908567191,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "962259 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1000000",
            "value": 408.6221483333332,
            "range": "± 2.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1",
            "value": 0.0008608523914450323,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "580820 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1000000",
            "value": 711.2000783333339,
            "range": "± 0.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1",
            "value": 0.0008326574486073262,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "600488 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1000000",
            "value": 697.3080906666655,
            "range": "± 0.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulator_tick_x10000",
            "value": 3.9847070000008293,
            "range": "± 1.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_step_x20000",
            "value": 13.022052000000258,
            "range": "± 1.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_build_top_n1000",
            "value": 773.834500333333,
            "range": "± 0.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_step_x1",
            "value": 0.0007906239213928109,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "632413 samples"
          },
          {
            "name": "ts/simulation_time_step_x1000000",
            "value": 659.6654576666649,
            "range": "± 1.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_runUntil_1000000",
            "value": 99.24350333333375,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/waitForCycles_x1000",
            "value": 0.7095473333344368,
            "range": "± 7.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/manual_step_loop_x2000",
            "value": 0.6979186666624931,
            "range": "± 3.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_fast_path_100000",
            "value": 4.221749666665953,
            "range": "± 0.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_guarded_100000",
            "value": 8.687377666664057,
            "range": "± 1.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_without_optimize",
            "value": 766.055190666666,
            "range": "± 1.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_with_optimize",
            "value": 769.2603089999999,
            "range": "± 2.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_without_optimize",
            "value": 3.9672463333360306,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_with_optimize",
            "value": 3.8693833333381917,
            "range": "± 2.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_linear_sec_p6",
            "value": 157318.42633332903,
            "range": "± 1.8%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1",
            "value": 1.174413961406476,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "425745 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1000000",
            "value": 1042252.1823333325,
            "range": "± 0.0%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_eval_linear_sec_p6_x1000000",
            "value": 1000146.7556666661,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_countones_w64",
            "value": 74529.76900000552,
            "range": "± 3.4%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1",
            "value": 1.27105882964992,
            "range": "± 0.3%",
            "unit": "us",
            "extra": "393373 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1000000",
            "value": 1142382.4269999992,
            "range": "± 0.5%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_std_counter_w32",
            "value": 8276.070999995378,
            "range": "± 1.9%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1",
            "value": 0.2043063290877301,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "2447306 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1000000",
            "value": 107013.37099999849,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_std_counter_w32_x1000000",
            "value": 127433.53933333613,
            "range": "± 0.3%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_gray_counter_w32",
            "value": 11621.47999999676,
            "range": "± 0.4%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1",
            "value": 0.20776375811288852,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "2406580 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1000000",
            "value": 110495.7003333305,
            "range": "± 0.4%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_gray_counter_w32_x1000000",
            "value": 130557.96933333718,
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
          "id": "fe52243e73c8a04f0bae1025b5603d5593e28424",
          "message": "Merge branch 'claude/streamed-strolling-whistle': Add Z signal support",
          "timestamp": "2026-03-05T22:37:13Z",
          "tree_id": "397c2ddc41628122d6e57f6d7bc642175969e0db",
          "url": "https://github.com/celox-sim/celox/commit/fe52243e73c8a04f0bae1025b5603d5593e28424"
        },
        "date": 1772753404986,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ts/simulation_build_top_n1000",
            "value": 759.4479203333327,
            "range": "± 0.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1",
            "value": 0.0005204209090497844,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "960761 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1000000",
            "value": 413.08761200000055,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1",
            "value": 0.0008758459405447359,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "570877 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1000000",
            "value": 736.0204316666677,
            "range": "± 0.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1",
            "value": 0.0008705420254027839,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "574355 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1000000",
            "value": 730.3242696666663,
            "range": "± 0.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulator_tick_x10000",
            "value": 4.123932333333262,
            "range": "± 0.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_step_x20000",
            "value": 13.122441333335397,
            "range": "± 1.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_build_top_n1000",
            "value": 764.7701733333379,
            "range": "± 2.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_step_x1",
            "value": 0.0008026662856657524,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "622925 samples"
          },
          {
            "name": "ts/simulation_time_step_x1000000",
            "value": 655.267736999997,
            "range": "± 0.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_runUntil_1000000",
            "value": 101.24788933332941,
            "range": "± 3.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/waitForCycles_x1000",
            "value": 0.968738333331809,
            "range": "± 48.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/manual_step_loop_x2000",
            "value": 0.7089113333349815,
            "range": "± 10.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_fast_path_100000",
            "value": 5.06367566666692,
            "range": "± 2.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_guarded_100000",
            "value": 9.53293099999913,
            "range": "± 1.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_without_optimize",
            "value": 758.3999566666656,
            "range": "± 0.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_with_optimize",
            "value": 760.3518696666675,
            "range": "± 0.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_without_optimize",
            "value": 4.114292333334258,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_with_optimize",
            "value": 3.900312666669682,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_linear_sec_p6",
            "value": 157737.66033333473,
            "range": "± 1.7%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1",
            "value": 1.2032222398660872,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "415551 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1000000",
            "value": 1076206.1673333326,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_eval_linear_sec_p6_x1000000",
            "value": 1015293.876999994,
            "range": "± 0.0%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_countones_w64",
            "value": 75517.52366666915,
            "range": "± 6.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1",
            "value": 1.3116485650623533,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "381200 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1000000",
            "value": 1169590.4886666636,
            "range": "± 0.9%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_std_counter_w32",
            "value": 8289.071999994727,
            "range": "± 1.5%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1",
            "value": 0.20125927140752295,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "2484358 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1000000",
            "value": 106033.85999999591,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_std_counter_w32_x1000000",
            "value": 133306.6883333328,
            "range": "± 5.9%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_gray_counter_w32",
            "value": 11490.418333332249,
            "range": "± 0.9%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1",
            "value": 0.20417224499427925,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "2448913 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1000000",
            "value": 110499.84499999846,
            "range": "± 0.5%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_gray_counter_w32_x1000000",
            "value": 133369.64966666224,
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
          "id": "b3a4fb2e5f26957bcc2c872e94bcbb903e7ad710",
          "message": "Fix dut tests for new X encoding (v=1, m=1)\n\nZ support changed 4-state encoding from X=(v=0,m=1) to X=(v=1,m=1).\nwriteAllX was updated but three tests still expected the old encoding.\n\nAlso fix biome config to only lint source files (not dist/ build\nartifacts), and suppress clippy::needless_borrow in celox-bench-sv.",
          "timestamp": "2026-03-06T01:15:07Z",
          "tree_id": "83196d261356b3ddcbc709f4f7a807a98e3f186a",
          "url": "https://github.com/celox-sim/celox/commit/b3a4fb2e5f26957bcc2c872e94bcbb903e7ad710"
        },
        "date": 1772761062452,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ts/simulation_build_top_n1000",
            "value": 799.6029080000002,
            "range": "± 10.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1",
            "value": 0.0005722170463325616,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "873795 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1000000",
            "value": 459.6169513333334,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1",
            "value": 0.0009461323728360955,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "528477 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1000000",
            "value": 827.2940303333332,
            "range": "± 2.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1",
            "value": 0.0009292534461336615,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "538067 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1000000",
            "value": 807.048821333335,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulator_tick_x10000",
            "value": 4.200189333336311,
            "range": "± 1.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_step_x20000",
            "value": 14.016287666665448,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_build_top_n1000",
            "value": 766.3591489999988,
            "range": "± 1.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_step_x1",
            "value": 0.0008441444344827775,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "592317 samples"
          },
          {
            "name": "ts/simulation_time_step_x1000000",
            "value": 709.1366989999976,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_runUntil_1000000",
            "value": 100.34878799999812,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/waitForCycles_x1000",
            "value": 0.7792173333339937,
            "range": "± 4.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/manual_step_loop_x2000",
            "value": 0.7855089999987589,
            "range": "± 2.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_fast_path_100000",
            "value": 4.2580756666648085,
            "range": "± 1.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_guarded_100000",
            "value": 10.24296266666594,
            "range": "± 1.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_without_optimize",
            "value": 763.9121709999987,
            "range": "± 1.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_with_optimize",
            "value": 764.1809903333342,
            "range": "± 0.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_without_optimize",
            "value": 4.411054000001362,
            "range": "± 14.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_with_optimize",
            "value": 4.219691666667738,
            "range": "± 3.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_linear_sec_p6",
            "value": 159944.90933333387,
            "range": "± 2.9%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1",
            "value": 1.2506711290585864,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "399786 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1000000",
            "value": 1084995.6603333363,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_eval_linear_sec_p6_x1000000",
            "value": 1034273.2863333415,
            "range": "± 0.0%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_countones_w64",
            "value": 75326.71099999182,
            "range": "± 4.5%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1",
            "value": 1.3201480306187232,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "378746 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1000000",
            "value": 1176262.941333339,
            "range": "± 0.5%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_std_counter_w32",
            "value": 8272.70266666892,
            "range": "± 1.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1",
            "value": 0.25321107716898694,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "1974638 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1000000",
            "value": 157451.8366666695,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_std_counter_w32_x1000000",
            "value": 176510.89733333598,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_gray_counter_w32",
            "value": 11634.77300000765,
            "range": "± 1.8%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1",
            "value": 0.2573593564753082,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "1942809 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1000000",
            "value": 157321.06433333442,
            "range": "± 0.3%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_gray_counter_w32_x1000000",
            "value": 174940.51966666302,
            "range": "± 0.7%",
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
          "id": "1844e23e7b5e07da0d8ee1635e413d4792e88ea3",
          "message": "Fix dead link in gap analysis doc\n\nThe relative path from docs/ts_testbench_gap_analysis.md to\ndocs/guide/parameter-overrides.md should be ./guide/ not ../guide/.",
          "timestamp": "2026-03-06T02:40:56Z",
          "tree_id": "3b1d485b5bf9f4589ff49cd19587ec1963392dea",
          "url": "https://github.com/celox-sim/celox/commit/1844e23e7b5e07da0d8ee1635e413d4792e88ea3"
        },
        "date": 1772766189076,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ts/simulation_build_top_n1000",
            "value": 763.8626209999998,
            "range": "± 3.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1",
            "value": 0.0005129982373399025,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "974663 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1000000",
            "value": 413.72491733333425,
            "range": "± 0.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1",
            "value": 0.0008933253362869369,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "559707 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1000000",
            "value": 762.880634000001,
            "range": "± 2.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1",
            "value": 0.0008815018106035759,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "567214 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1000000",
            "value": 738.955912,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulator_tick_x10000",
            "value": 4.119278999998642,
            "range": "± 0.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_step_x20000",
            "value": 13.131443333331845,
            "range": "± 0.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_build_top_n1000",
            "value": 777.8230623333317,
            "range": "± 9.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_step_x1",
            "value": 0.0007997656162041266,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "625184 samples"
          },
          {
            "name": "ts/simulation_time_step_x1000000",
            "value": 659.0792923333307,
            "range": "± 1.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_runUntil_1000000",
            "value": 100.35507666666672,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/waitForCycles_x1000",
            "value": 0.6943210000002485,
            "range": "± 5.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/manual_step_loop_x2000",
            "value": 0.7261839999991935,
            "range": "± 8.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_fast_path_100000",
            "value": 4.211309333334309,
            "range": "± 0.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_guarded_100000",
            "value": 9.049723000001299,
            "range": "± 8.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_without_optimize",
            "value": 765.6532916666694,
            "range": "± 2.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_with_optimize",
            "value": 757.5773343333325,
            "range": "± 0.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_without_optimize",
            "value": 4.108311999996658,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_with_optimize",
            "value": 4.113765666663919,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_linear_sec_p6",
            "value": 155473.18866667047,
            "range": "± 1.4%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1",
            "value": 1.2277004979595527,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "407266 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1000000",
            "value": 1086479.919333336,
            "range": "± 2.3%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_eval_linear_sec_p6_x1000000",
            "value": 1020615.664333333,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_countones_w64",
            "value": 74501.88799999887,
            "range": "± 1.3%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1",
            "value": 1.304591608357411,
            "range": "± 0.3%",
            "unit": "us",
            "extra": "383262 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1000000",
            "value": 1169647.3280000016,
            "range": "± 0.8%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_std_counter_w32",
            "value": 8250.450666673714,
            "range": "± 1.3%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1",
            "value": 0.207350316975486,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "2411379 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1000000",
            "value": 106141.67633333516,
            "range": "± 0.5%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_std_counter_w32_x1000000",
            "value": 124716.33733333147,
            "range": "± 0.3%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_gray_counter_w32",
            "value": 11597.75966666833,
            "range": "± 0.3%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1",
            "value": 0.21156447258324781,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "2363346 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1000000",
            "value": 107656.76933333452,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_gray_counter_w32_x1000000",
            "value": 126298.45399999856,
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
          "id": "dd3d6e778f1e278917cfa7ace10b9e4143d2b02a",
          "message": "Add pre-push submodule validation and test gate via lefthook\n\nEnsure submodules are initialized, in sync, and their commits exist on\nthe remote before allowing push. Also run the full test suite (pnpm test)\nas a final gate. Use piped execution so failures block subsequent steps.",
          "timestamp": "2026-03-06T10:44:19Z",
          "tree_id": "4cb69d58108de90e3275cc11403552d653222edc",
          "url": "https://github.com/celox-sim/celox/commit/dd3d6e778f1e278917cfa7ace10b9e4143d2b02a"
        },
        "date": 1772795243593,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ts/simulation_build_top_n1000",
            "value": 345.4969733333334,
            "range": "± 1.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1",
            "value": 0.0005300283999712245,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "943346 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1000000",
            "value": 387.01068333333325,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1",
            "value": 0.0009014456852333047,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "554665 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1000000",
            "value": 758.2991080000016,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1",
            "value": 0.0008756951415387159,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "570983 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1000000",
            "value": 707.4658813333335,
            "range": "± 0.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulator_tick_x10000",
            "value": 3.6392653333326357,
            "range": "± 1.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_step_x20000",
            "value": 13.040650666664684,
            "range": "± 0.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_build_top_n1000",
            "value": 338.49417466666637,
            "range": "± 0.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_step_x1",
            "value": 0.0008186353620740275,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "610773 samples"
          },
          {
            "name": "ts/simulation_time_step_x1000000",
            "value": 662.2330820000012,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_runUntil_1000000",
            "value": 98.44608166666633,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/waitForCycles_x1000",
            "value": 0.7175929999987906,
            "range": "± 3.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/manual_step_loop_x2000",
            "value": 0.7392210000010285,
            "range": "± 6.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_fast_path_100000",
            "value": 4.2288273333348725,
            "range": "± 0.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_guarded_100000",
            "value": 9.002784666668353,
            "range": "± 0.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_without_optimize",
            "value": 340.18187966666784,
            "range": "± 0.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_with_optimize",
            "value": 338.92261966666655,
            "range": "± 0.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_without_optimize",
            "value": 3.6385496666650092,
            "range": "± 0.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_with_optimize",
            "value": 3.567011999997097,
            "range": "± 0.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_linear_sec_p6",
            "value": 155243.57900000177,
            "range": "± 4.4%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1",
            "value": 1.2317408592604702,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "405930 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1000000",
            "value": 1067592.2213333372,
            "range": "± 0.3%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_eval_linear_sec_p6_x1000000",
            "value": 1018592.6879999994,
            "range": "± 0.4%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_countones_w64",
            "value": 47485.56999999952,
            "range": "± 3.6%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1",
            "value": 1.3253786868726432,
            "range": "± 0.3%",
            "unit": "us",
            "extra": "377251 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1000000",
            "value": 1165506.145333328,
            "range": "± 0.7%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_std_counter_w32",
            "value": 7838.077000002765,
            "range": "± 2.2%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1",
            "value": 0.19723998977629054,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "2534983 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1000000",
            "value": 105672.39766666414,
            "range": "± 0.7%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_std_counter_w32_x1000000",
            "value": 123817.69599999825,
            "range": "± 0.9%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_gray_counter_w32",
            "value": 11751.961666663798,
            "range": "± 6.2%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1",
            "value": 0.1993137907612966,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "2508608 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1000000",
            "value": 106775.3870000015,
            "range": "± 1.6%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_gray_counter_w32_x1000000",
            "value": 123682.88433333025,
            "range": "± 0.4%",
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
          "id": "cde884593abab6375c496a99b2f80a8d231fadd7",
          "message": "Fix fill-literal ('0/'1/'x/'z) width=0 causing bit loss in always_comb/always_ff\n\nFill-literals have width 0 from the Veryl analyzer (context-dependent).\nThe Factor::Value handler in both comb and FF paths emitted 0-width\nSLTNode::Constant / SIR registers, which caused Mux lowering to produce\n0-bit masks that zeroed out else-arm values in multi-branch if/else if\nalways_comb blocks.\n\nPer IEEE 1800-2023 §5.7.1, fill-literals replicate their single bit\nacross the full context width (1 bit in self-determined contexts).\nExpand both value and mask_xz by replicating bit 0 to the target width.",
          "timestamp": "2026-03-06T14:42:08Z",
          "tree_id": "f5c0dbc897cdabc00cea7cb9a39c94adfdc8af86",
          "url": "https://github.com/celox-sim/celox/commit/cde884593abab6375c496a99b2f80a8d231fadd7"
        },
        "date": 1772809424008,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ts/simulation_build_top_n1000",
            "value": 351.7793803333337,
            "range": "± 1.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1",
            "value": 0.0005082000670822914,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "983865 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1000000",
            "value": 395.6196733333327,
            "range": "± 0.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1",
            "value": 0.0008723338451016865,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "573176 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1000000",
            "value": 737.6125786666659,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1",
            "value": 0.0008655090471605202,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "577695 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1000000",
            "value": 728.9509419999995,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulator_tick_x10000",
            "value": 3.9471236666662057,
            "range": "± 1.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_step_x20000",
            "value": 13.297875666665883,
            "range": "± 1.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_build_top_n1000",
            "value": 344.8331309999994,
            "range": "± 2.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_step_x1",
            "value": 0.0008017859411893983,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "623609 samples"
          },
          {
            "name": "ts/simulation_time_step_x1000000",
            "value": 660.1405953333355,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_runUntil_1000000",
            "value": 101.16288133333244,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/waitForCycles_x1000",
            "value": 0.69868533333162,
            "range": "± 2.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/manual_step_loop_x2000",
            "value": 0.731482666664912,
            "range": "± 7.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_fast_path_100000",
            "value": 4.230819999999464,
            "range": "± 0.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_guarded_100000",
            "value": 8.714949333329665,
            "range": "± 2.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_without_optimize",
            "value": 348.9686476666684,
            "range": "± 3.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_with_optimize",
            "value": 344.6864793333346,
            "range": "± 1.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_without_optimize",
            "value": 3.977276000000226,
            "range": "± 2.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_with_optimize",
            "value": 4.025235666667868,
            "range": "± 8.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_linear_sec_p6",
            "value": 158013.71799999956,
            "range": "± 2.0%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1",
            "value": 1.2528458851618425,
            "range": "± 0.7%",
            "unit": "us",
            "extra": "399092 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1000000",
            "value": 1086953.7636666694,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_eval_linear_sec_p6_x1000000",
            "value": 1045517.7376666736,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_countones_w64",
            "value": 48176.40800000421,
            "range": "± 4.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1",
            "value": 1.3109797794860263,
            "range": "± 0.3%",
            "unit": "us",
            "extra": "381395 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1000000",
            "value": 1182494.928999998,
            "range": "± 0.6%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_std_counter_w32",
            "value": 8055.448000008861,
            "range": "± 4.2%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1",
            "value": 0.2038604775360865,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "2452659 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1000000",
            "value": 104566.74200000027,
            "range": "± 0.4%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_std_counter_w32_x1000000",
            "value": 126925.79633333177,
            "range": "± 5.7%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_gray_counter_w32",
            "value": 11488.146666665367,
            "range": "± 1.0%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1",
            "value": 0.20018234127930618,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "2497723 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1000000",
            "value": 104354.8113333333,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_gray_counter_w32_x1000000",
            "value": 135276.28366666613,
            "range": "± 3.9%",
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
          "id": "fa0347fccb316ac5c78006c28a32cd1965a95b8f",
          "message": "Add untracked file detection to pre-push clean-tree check\n\nPrevents pushing when untracked files exist, catching cases where\ntests reference local files (e.g. via include_str!) that aren't\ncommitted. Previously only tracked file changes were detected.",
          "timestamp": "2026-03-06T15:14:47Z",
          "tree_id": "82e54feef12a5e1c65eba78a3e33bd9bf9d22e1e",
          "url": "https://github.com/celox-sim/celox/commit/fa0347fccb316ac5c78006c28a32cd1965a95b8f"
        },
        "date": 1772811504048,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ts/simulation_build_top_n1000",
            "value": 341.8140853333337,
            "range": "± 1.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1",
            "value": 0.0005086960268834903,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "982906 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1000000",
            "value": 412.22199900000004,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1",
            "value": 0.0008882627763156802,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "562897 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1000000",
            "value": 739.5596399999995,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1",
            "value": 0.0008682799816627325,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "575852 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1000000",
            "value": 740.5993343333345,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulator_tick_x10000",
            "value": 3.984836333333078,
            "range": "± 1.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_step_x20000",
            "value": 13.277038666664643,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_build_top_n1000",
            "value": 343.31975366666785,
            "range": "± 1.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_step_x1",
            "value": 0.0008127459179879523,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "615199 samples"
          },
          {
            "name": "ts/simulation_time_step_x1000000",
            "value": 669.510808999997,
            "range": "± 0.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_runUntil_1000000",
            "value": 101.21958466666547,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/waitForCycles_x1000",
            "value": 0.6957856666679921,
            "range": "± 4.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/manual_step_loop_x2000",
            "value": 0.7063320000015665,
            "range": "± 6.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_fast_path_100000",
            "value": 4.999579666667462,
            "range": "± 0.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_guarded_100000",
            "value": 9.557453666665728,
            "range": "± 1.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_without_optimize",
            "value": 341.7062446666653,
            "range": "± 0.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_with_optimize",
            "value": 341.32074499999726,
            "range": "± 1.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_without_optimize",
            "value": 3.995954666667482,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_with_optimize",
            "value": 3.9842523333306112,
            "range": "± 2.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_linear_sec_p6",
            "value": 157895.69833333613,
            "range": "± 1.8%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1",
            "value": 1.2110213333339352,
            "range": "± 0.3%",
            "unit": "us",
            "extra": "412875 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1000000",
            "value": 1073876.4719999987,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_eval_linear_sec_p6_x1000000",
            "value": 1025866.1623333319,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_countones_w64",
            "value": 48891.8266666636,
            "range": "± 5.2%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1",
            "value": 1.2949644299189054,
            "range": "± 0.4%",
            "unit": "us",
            "extra": "386111 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1000000",
            "value": 1170641.200666665,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_std_counter_w32",
            "value": 8255.441000001156,
            "range": "± 4.0%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1",
            "value": 0.20528040151316843,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "2435693 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1000000",
            "value": 106192.00166666512,
            "range": "± 0.3%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_std_counter_w32_x1000000",
            "value": 131842.01399999924,
            "range": "± 0.6%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_gray_counter_w32",
            "value": 11523.29266666493,
            "range": "± 3.6%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1",
            "value": 0.2152590263464065,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "2322783 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1000000",
            "value": 108628.94300000335,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_gray_counter_w32_x1000000",
            "value": 139552.21266666194,
            "range": "± 0.4%",
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
          "id": "9a6ef5d0cc81ed4bf680c5b6fad3d2a21554bb41",
          "message": "Box SimBackend::Full to fix clippy large_enum_variant warning",
          "timestamp": "2026-03-07T10:39:04Z",
          "tree_id": "324129867e926b82150e673d111e12a1fe763c36",
          "url": "https://github.com/celox-sim/celox/commit/9a6ef5d0cc81ed4bf680c5b6fad3d2a21554bb41"
        },
        "date": 1772881478173,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ts/simulation_build_top_n1000",
            "value": 346.85145033333356,
            "range": "± 0.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1",
            "value": 0.0005985350735543089,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "835373 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1000000",
            "value": 474.0147596666666,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1",
            "value": 0.0009552206537526302,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "523440 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1000000",
            "value": 837.3596613333333,
            "range": "± 0.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1",
            "value": 0.0009510897771233929,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "525713 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1000000",
            "value": 813.7310350000007,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulator_tick_x10000",
            "value": 4.818196666664638,
            "range": "± 0.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_step_x20000",
            "value": 13.170712666666077,
            "range": "± 1.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_build_top_n1000",
            "value": 341.38262699999905,
            "range": "± 0.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_step_x1",
            "value": 0.0007999420954593906,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "625046 samples"
          },
          {
            "name": "ts/simulation_time_step_x1000000",
            "value": 661.1668613333338,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_runUntil_1000000",
            "value": 101.80129400000199,
            "range": "± 2.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/waitForCycles_x1000",
            "value": 0.8058973333342389,
            "range": "± 49.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/manual_step_loop_x2000",
            "value": 0.716296999999031,
            "range": "± 4.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_fast_path_100000",
            "value": 4.305474666667578,
            "range": "± 0.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_guarded_100000",
            "value": 8.859893000000739,
            "range": "± 4.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_without_optimize",
            "value": 344.77716500000196,
            "range": "± 5.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_with_optimize",
            "value": 341.60165033333277,
            "range": "± 0.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_without_optimize",
            "value": 4.801525666664626,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_with_optimize",
            "value": 4.730777333332905,
            "range": "± 0.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_linear_sec_p6",
            "value": 152702.19666666526,
            "range": "± 0.8%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1",
            "value": 1.198228509188789,
            "range": "± 0.3%",
            "unit": "us",
            "extra": "417283 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1000000",
            "value": 1076550.768999999,
            "range": "± 1.0%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_eval_linear_sec_p6_x1000000",
            "value": 1014528.2639999981,
            "range": "± 0.5%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_countones_w64",
            "value": 47629.19099999999,
            "range": "± 1.7%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1",
            "value": 1.3004784666779066,
            "range": "± 0.3%",
            "unit": "us",
            "extra": "384474 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1000000",
            "value": 1172666.7166666691,
            "range": "± 0.3%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_std_counter_w32",
            "value": 8010.8563333293805,
            "range": "± 1.2%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1",
            "value": 0.210729705466164,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "2372708 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1000000",
            "value": 107464.6359999994,
            "range": "± 0.8%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_std_counter_w32_x1000000",
            "value": 129339.87266666372,
            "range": "± 0.3%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_gray_counter_w32",
            "value": 11374.735333331046,
            "range": "± 1.5%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1",
            "value": 0.207914278213567,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "2404838 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1000000",
            "value": 111652.8226666657,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_gray_counter_w32_x1000000",
            "value": 132484.06833333138,
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
          "id": "308dd9af7e68375c3a1406b1a0f074926a7ce657",
          "message": "Revert Box<Simulator> — use allow(clippy::large_enum_variant) instead\n\nBoxing adds indirection on every tick call in the hot path.",
          "timestamp": "2026-03-07T11:28:14Z",
          "tree_id": "9d27eac47590a27fba99f8bf9d04e19473b9ed19",
          "url": "https://github.com/celox-sim/celox/commit/308dd9af7e68375c3a1406b1a0f074926a7ce657"
        },
        "date": 1772884297767,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ts/simulation_build_top_n1000",
            "value": 336.2240693333333,
            "range": "± 1.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1",
            "value": 0.0005928979836742116,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "843316 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1000000",
            "value": 469.3728543333327,
            "range": "± 1.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1",
            "value": 0.0010161767047880681,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "492041 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1000000",
            "value": 890.805851000001,
            "range": "± 0.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1",
            "value": 0.0010001775432497172,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "499912 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1000000",
            "value": 867.1293063333336,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulator_tick_x10000",
            "value": 4.678434333333523,
            "range": "± 1.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_step_x20000",
            "value": 13.352347666666901,
            "range": "± 1.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_build_top_n1000",
            "value": 371.3178560000015,
            "range": "± 3.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_step_x1",
            "value": 0.0008378285621906075,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "596782 samples"
          },
          {
            "name": "ts/simulation_time_step_x1000000",
            "value": 658.6679510000007,
            "range": "± 1.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_runUntil_1000000",
            "value": 100.22004900000563,
            "range": "± 0.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/waitForCycles_x1000",
            "value": 0.6916710000004969,
            "range": "± 5.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/manual_step_loop_x2000",
            "value": 0.7048856666660868,
            "range": "± 5.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_fast_path_100000",
            "value": 4.256928666664559,
            "range": "± 1.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_guarded_100000",
            "value": 8.731353333333876,
            "range": "± 0.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_without_optimize",
            "value": 336.89118533333385,
            "range": "± 1.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_with_optimize",
            "value": 344.98017766666936,
            "range": "± 2.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_without_optimize",
            "value": 4.681327000000844,
            "range": "± 0.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_with_optimize",
            "value": 4.655962666668832,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_linear_sec_p6",
            "value": 152816.37200000114,
            "range": "± 4.4%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1",
            "value": 1.2064801980590927,
            "range": "± 0.3%",
            "unit": "us",
            "extra": "414429 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1000000",
            "value": 1077657.7353333342,
            "range": "± 0.7%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_eval_linear_sec_p6_x1000000",
            "value": 1025420.7639999997,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_countones_w64",
            "value": 45831.31233333552,
            "range": "± 0.4%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1",
            "value": 1.2929959891355305,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "386699 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1000000",
            "value": 1161787.6696666596,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_std_counter_w32",
            "value": 8046.097666675147,
            "range": "± 1.4%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1",
            "value": 0.2163964082514835,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "2310575 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1000000",
            "value": 104241.0499999969,
            "range": "± 1.5%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_std_counter_w32_x1000000",
            "value": 127158.81433333561,
            "range": "± 0.4%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_gray_counter_w32",
            "value": 11329.1133333308,
            "range": "± 2.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1",
            "value": 0.22295825197233643,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "2242573 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1000000",
            "value": 104643.87533333502,
            "range": "± 0.3%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_gray_counter_w32_x1000000",
            "value": 127268.02166666312,
            "range": "± 0.4%",
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
          "id": "1c00e87c78fbe6c21c4d495439d93a582bd9c581",
          "message": "Remove unnecessary cast flagged by clippy",
          "timestamp": "2026-03-07T11:54:17Z",
          "tree_id": "8f4484551df424cac5506ba3d1ba0094442d2a50",
          "url": "https://github.com/celox-sim/celox/commit/1c00e87c78fbe6c21c4d495439d93a582bd9c581"
        },
        "date": 1772886000010,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ts/simulation_build_top_n1000",
            "value": 350.51064999999994,
            "range": "± 2.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1",
            "value": 0.000592001996229863,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "844592 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1000000",
            "value": 476.3866793333327,
            "range": "± 0.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1",
            "value": 0.0009468109674672598,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "528089 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1000000",
            "value": 826.314738333332,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1",
            "value": 0.0009499438507282068,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "526347 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1000000",
            "value": 815.4977566666676,
            "range": "± 2.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulator_tick_x10000",
            "value": 4.747105333333214,
            "range": "± 1.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_step_x20000",
            "value": 13.37351866666601,
            "range": "± 1.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_build_top_n1000",
            "value": 340.74134833333306,
            "range": "± 1.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_step_x1",
            "value": 0.0008038664102898627,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "621994 samples"
          },
          {
            "name": "ts/simulation_time_step_x1000000",
            "value": 671.9267570000035,
            "range": "± 0.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_runUntil_1000000",
            "value": 102.02780933333513,
            "range": "± 0.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/waitForCycles_x1000",
            "value": 0.7216330000010203,
            "range": "± 4.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/manual_step_loop_x2000",
            "value": 0.7086893333325861,
            "range": "± 3.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_fast_path_100000",
            "value": 4.405046333335728,
            "range": "± 0.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_guarded_100000",
            "value": 9.081183999997544,
            "range": "± 3.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_without_optimize",
            "value": 342.552714666667,
            "range": "± 0.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_with_optimize",
            "value": 348.11303633333347,
            "range": "± 0.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_without_optimize",
            "value": 4.769771999999648,
            "range": "± 0.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_with_optimize",
            "value": 4.793867000002744,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_linear_sec_p6",
            "value": 165965.75499999744,
            "range": "± 3.2%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1",
            "value": 1.2178348665857457,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "410565 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1000000",
            "value": 1070165.505000002,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_eval_linear_sec_p6_x1000000",
            "value": 1030044.8163333349,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_countones_w64",
            "value": 48635.857000001124,
            "range": "± 0.5%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1",
            "value": 1.2957562973669667,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "385876 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1000000",
            "value": 1181586.7573333331,
            "range": "± 0.7%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_std_counter_w32",
            "value": 8441.709000006085,
            "range": "± 2.8%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1",
            "value": 0.21436945930492907,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "2332422 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1000000",
            "value": 110475.3273333287,
            "range": "± 0.5%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_std_counter_w32_x1000000",
            "value": 130054.6133333361,
            "range": "± 0.7%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_gray_counter_w32",
            "value": 11495.252999996106,
            "range": "± 2.8%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1",
            "value": 0.21776901695789874,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "2296030 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1000000",
            "value": 120273.55133333185,
            "range": "± 0.8%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_gray_counter_w32_x1000000",
            "value": 144897.67133333467,
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
          "id": "1c00e87c78fbe6c21c4d495439d93a582bd9c581",
          "message": "Remove unnecessary cast flagged by clippy",
          "timestamp": "2026-03-07T11:54:17Z",
          "tree_id": "8f4484551df424cac5506ba3d1ba0094442d2a50",
          "url": "https://github.com/celox-sim/celox/commit/1c00e87c78fbe6c21c4d495439d93a582bd9c581"
        },
        "date": 1772894745024,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ts/simulation_build_top_n1000",
            "value": 350.51064999999994,
            "range": "± 2.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1",
            "value": 0.000592001996229863,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "844592 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1000000",
            "value": 476.3866793333327,
            "range": "± 0.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1",
            "value": 0.0009468109674672598,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "528089 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1000000",
            "value": 826.314738333332,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1",
            "value": 0.0009499438507282068,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "526347 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1000000",
            "value": 815.4977566666676,
            "range": "± 2.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulator_tick_x10000",
            "value": 4.747105333333214,
            "range": "± 1.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_step_x20000",
            "value": 13.37351866666601,
            "range": "± 1.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_build_top_n1000",
            "value": 340.74134833333306,
            "range": "± 1.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_step_x1",
            "value": 0.0008038664102898627,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "621994 samples"
          },
          {
            "name": "ts/simulation_time_step_x1000000",
            "value": 671.9267570000035,
            "range": "± 0.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_runUntil_1000000",
            "value": 102.02780933333513,
            "range": "± 0.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/waitForCycles_x1000",
            "value": 0.7216330000010203,
            "range": "± 4.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/manual_step_loop_x2000",
            "value": 0.7086893333325861,
            "range": "± 3.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_fast_path_100000",
            "value": 4.405046333335728,
            "range": "± 0.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_guarded_100000",
            "value": 9.081183999997544,
            "range": "± 3.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_without_optimize",
            "value": 342.552714666667,
            "range": "± 0.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_with_optimize",
            "value": 348.11303633333347,
            "range": "± 0.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_without_optimize",
            "value": 4.769771999999648,
            "range": "± 0.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_with_optimize",
            "value": 4.793867000002744,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_linear_sec_p6",
            "value": 165965.75499999744,
            "range": "± 3.2%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1",
            "value": 1.2178348665857457,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "410565 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1000000",
            "value": 1070165.505000002,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_eval_linear_sec_p6_x1000000",
            "value": 1030044.8163333349,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_countones_w64",
            "value": 48635.857000001124,
            "range": "± 0.5%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1",
            "value": 1.2957562973669667,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "385876 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1000000",
            "value": 1181586.7573333331,
            "range": "± 0.7%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_std_counter_w32",
            "value": 8441.709000006085,
            "range": "± 2.8%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1",
            "value": 0.21436945930492907,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "2332422 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1000000",
            "value": 110475.3273333287,
            "range": "± 0.5%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_std_counter_w32_x1000000",
            "value": 130054.6133333361,
            "range": "± 0.7%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_gray_counter_w32",
            "value": 11495.252999996106,
            "range": "± 2.8%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1",
            "value": 0.21776901695789874,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "2296030 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1000000",
            "value": 120273.55133333185,
            "range": "± 0.8%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_gray_counter_w32_x1000000",
            "value": 144897.67133333467,
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
          "id": "b71d5fa481a9704008d413cdb84169eefd93b188",
          "message": "Fix .at() returning wrong values for non-byte-aligned array elements\n\nThe JIT stores all array elements bit-packed (element i starts at bit\ni*W), but createArrayDut assumed byte-aligned stride for elements >= 8\nbits wide. For widths not divisible by 8 (e.g. logic<34>[4]), this\ncaused a cumulative bit shift on every element after index 0.\n\nAdd readBitPackedWide/writeBitPackedWide for BigInt-based bit-level\narray access, and split createArrayDut into three paths: sub-byte,\nnon-byte-aligned (>= 8 bits), and byte-aligned.",
          "timestamp": "2026-03-07T14:55:05Z",
          "tree_id": "6a0ddef2b7369e4a710afc1b0bb9f7c1206d3b12",
          "url": "https://github.com/celox-sim/celox/commit/b71d5fa481a9704008d413cdb84169eefd93b188"
        },
        "date": 1772896717829,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ts/simulation_build_top_n1000",
            "value": 345.71863333333323,
            "range": "± 2.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1",
            "value": 0.0006415035122313878,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "779419 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1000000",
            "value": 520.8493256666674,
            "range": "± 2.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1",
            "value": 0.0010234320779919884,
            "range": "± 0.8%",
            "unit": "ms",
            "extra": "488553 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1000000",
            "value": 876.7990283333335,
            "range": "± 0.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1",
            "value": 0.0009772711105100575,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "511629 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1000000",
            "value": 863.4825859999995,
            "range": "± 1.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulator_tick_x10000",
            "value": 5.142451999998836,
            "range": "± 2.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_step_x20000",
            "value": 13.219941999998506,
            "range": "± 0.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_build_top_n1000",
            "value": 342.3465830000011,
            "range": "± 2.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_step_x1",
            "value": 0.0008165878632151299,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "612304 samples"
          },
          {
            "name": "ts/simulation_time_step_x1000000",
            "value": 667.1601636666675,
            "range": "± 0.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_runUntil_1000000",
            "value": 101.95538733333524,
            "range": "± 0.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/waitForCycles_x1000",
            "value": 0.6939020000014958,
            "range": "± 2.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/manual_step_loop_x2000",
            "value": 0.7419450000015786,
            "range": "± 2.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_fast_path_100000",
            "value": 4.402905333331243,
            "range": "± 1.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_guarded_100000",
            "value": 8.851311333331978,
            "range": "± 2.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_without_optimize",
            "value": 346.09966466666566,
            "range": "± 3.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_with_optimize",
            "value": 345.0985806666674,
            "range": "± 0.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_without_optimize",
            "value": 5.141449666666934,
            "range": "± 0.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_with_optimize",
            "value": 5.1743009999991045,
            "range": "± 0.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_linear_sec_p6",
            "value": 167718.02966666778,
            "range": "± 5.6%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1",
            "value": 1.2196039563790861,
            "range": "± 0.3%",
            "unit": "us",
            "extra": "409970 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1000000",
            "value": 1063666.7856666658,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_eval_linear_sec_p6_x1000000",
            "value": 1021968.4970000089,
            "range": "± 0.3%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_countones_w64",
            "value": 47955.93233333784,
            "range": "± 0.4%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1",
            "value": 1.3046926725939412,
            "range": "± 0.3%",
            "unit": "us",
            "extra": "383233 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1000000",
            "value": 1151466.5370000002,
            "range": "± 0.4%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_std_counter_w32",
            "value": 8148.0236666684505,
            "range": "± 1.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1",
            "value": 0.2506978019823283,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "1994434 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1000000",
            "value": 148211.85100000972,
            "range": "± 0.4%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_std_counter_w32_x1000000",
            "value": 169242.91300000428,
            "range": "± 1.6%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_gray_counter_w32",
            "value": 11632.397666665687,
            "range": "± 3.2%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1",
            "value": 0.26671487233510727,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "1874662 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1000000",
            "value": 156043.1113333325,
            "range": "± 2.9%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_gray_counter_w32_x1000000",
            "value": 178976.84166667264,
            "range": "± 1.5%",
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
          "id": "3acefb671902ffb2d55a66293508db14c5264ec7",
          "message": "Cache comb_func on JitBackend to avoid Arc dereference on hot path\n\neval_comb() is called on every tick. Accessing the function pointer\nthrough self.shared.comb_func requires an Arc pointer chase that\npenalizes performance (~8% locally, potentially worse under cache\npressure in CI). Copy the SimFunc pointer directly onto JitBackend\nat construction time.",
          "timestamp": "2026-03-07T15:48:17Z",
          "tree_id": "d1d1c439a60294d15f20663a261f3674424505b3",
          "url": "https://github.com/celox-sim/celox/commit/3acefb671902ffb2d55a66293508db14c5264ec7"
        },
        "date": 1772899925043,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ts/simulation_build_top_n1000",
            "value": 343.3434690000001,
            "range": "± 1.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1",
            "value": 0.0006330765174209834,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "789794 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1000000",
            "value": 514.4269806666665,
            "range": "± 1.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1",
            "value": 0.001007380447155663,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "496337 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1000000",
            "value": 886.9585143333352,
            "range": "± 0.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1",
            "value": 0.0010092317314176074,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "495427 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1000000",
            "value": 881.523030666666,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulator_tick_x10000",
            "value": 5.138864333335126,
            "range": "± 0.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_step_x20000",
            "value": 13.092378333334636,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_build_top_n1000",
            "value": 339.14361199999985,
            "range": "± 1.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_step_x1",
            "value": 0.0008123222381099235,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "615520 samples"
          },
          {
            "name": "ts/simulation_time_step_x1000000",
            "value": 664.9931679999985,
            "range": "± 0.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_runUntil_1000000",
            "value": 102.07797066666778,
            "range": "± 1.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/waitForCycles_x1000",
            "value": 0.7198170000022704,
            "range": "± 20.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/manual_step_loop_x2000",
            "value": 0.7276116666689632,
            "range": "± 1.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_fast_path_100000",
            "value": 4.327301333330979,
            "range": "± 2.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_guarded_100000",
            "value": 8.794084999998935,
            "range": "± 1.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_without_optimize",
            "value": 345.21352700000233,
            "range": "± 1.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_with_optimize",
            "value": 348.0039243333328,
            "range": "± 1.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_without_optimize",
            "value": 4.780402333332556,
            "range": "± 1.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_with_optimize",
            "value": 5.076049333333988,
            "range": "± 0.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_linear_sec_p6",
            "value": 166265.59966666537,
            "range": "± 2.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1",
            "value": 1.1947591154043702,
            "range": "± 0.3%",
            "unit": "us",
            "extra": "418495 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1000000",
            "value": 1054753.3879999975,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_eval_linear_sec_p6_x1000000",
            "value": 1009687.5863333311,
            "range": "± 0.3%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_countones_w64",
            "value": 49791.4946666618,
            "range": "± 6.3%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1",
            "value": 1.2828996851667853,
            "range": "± 0.4%",
            "unit": "us",
            "extra": "389743 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1000000",
            "value": 1142580.1386666668,
            "range": "± 0.4%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_std_counter_w32",
            "value": 8136.9469999966295,
            "range": "± 1.3%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1",
            "value": 0.2644348620335953,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "1890825 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1000000",
            "value": 150441.05766666567,
            "range": "± 0.3%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_std_counter_w32_x1000000",
            "value": 177359.54666667385,
            "range": "± 0.6%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_gray_counter_w32",
            "value": 11434.246666671243,
            "range": "± 1.2%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1",
            "value": 0.26742746939769846,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "1869666 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1000000",
            "value": 164274.37199999986,
            "range": "± 0.4%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_gray_counter_w32_x1000000",
            "value": 182167.85333333732,
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
          "id": "c03af0f45dbf2eab7f759d48d1500d6023bce397",
          "message": "Fix AsyncLow reset polarity in Verilator top_n1000 benchmark\n\nThe reset sequence had rst=1 (release) before rst=0 (assert), leaving\nthe counters in reset during the entire benchmark. This matches the\nsame bug that was fixed for the Celox Rust benchmarks in 20cb7a2.",
          "timestamp": "2026-03-07T16:55:34Z",
          "tree_id": "965a33644531d48055c3acce1c6a9e10127a5ab2",
          "url": "https://github.com/celox-sim/celox/commit/c03af0f45dbf2eab7f759d48d1500d6023bce397"
        },
        "date": 1772903993009,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ts/simulation_build_top_n1000",
            "value": 345.88438266666685,
            "range": "± 1.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1",
            "value": 0.0005933105648431809,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "842729 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1000000",
            "value": 474.42710833333393,
            "range": "± 1.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1",
            "value": 0.00095915141330695,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "521295 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1000000",
            "value": 806.8028376666686,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1",
            "value": 0.0009435967713931137,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "529888 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1000000",
            "value": 794.6615053333322,
            "range": "± 0.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulator_tick_x10000",
            "value": 4.75364633333326,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_step_x20000",
            "value": 13.173205999998268,
            "range": "± 0.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_build_top_n1000",
            "value": 345.01301699999993,
            "range": "± 1.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_step_x1",
            "value": 0.0008051144161873225,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "621031 samples"
          },
          {
            "name": "ts/simulation_time_step_x1000000",
            "value": 661.6732269999993,
            "range": "± 0.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_runUntil_1000000",
            "value": 100.41707066666761,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/waitForCycles_x1000",
            "value": 0.6958383333330858,
            "range": "± 3.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/manual_step_loop_x2000",
            "value": 0.7086226666676035,
            "range": "± 3.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_fast_path_100000",
            "value": 4.307157666664959,
            "range": "± 1.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_guarded_100000",
            "value": 8.852080333335229,
            "range": "± 0.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_without_optimize",
            "value": 343.7454793333357,
            "range": "± 0.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_with_optimize",
            "value": 344.61316299999936,
            "range": "± 1.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_without_optimize",
            "value": 4.713513666664464,
            "range": "± 2.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_with_optimize",
            "value": 4.666792999999113,
            "range": "± 0.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_linear_sec_p6",
            "value": 169987.79033333267,
            "range": "± 3.0%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1",
            "value": 1.2129709057564262,
            "range": "± 0.8%",
            "unit": "us",
            "extra": "412212 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1000000",
            "value": 1060570.248000001,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_eval_linear_sec_p6_x1000000",
            "value": 1005300.125000004,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_countones_w64",
            "value": 48408.56833333479,
            "range": "± 0.6%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1",
            "value": 1.282832328867181,
            "range": "± 0.3%",
            "unit": "us",
            "extra": "389763 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1000000",
            "value": 1151142.115333336,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_std_counter_w32",
            "value": 8425.314000007347,
            "range": "± 0.7%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1",
            "value": 0.22403414638669428,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "2231803 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1000000",
            "value": 108772.11733333145,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_std_counter_w32_x1000000",
            "value": 133745.65000000197,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_gray_counter_w32",
            "value": 11577.290999998999,
            "range": "± 2.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1",
            "value": 0.22168960666183263,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "2255406 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1000000",
            "value": 111514.61833333208,
            "range": "± 0.7%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_gray_counter_w32_x1000000",
            "value": 133093.56766667042,
            "range": "± 0.9%",
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
          "id": "d7529d58a27f5681d9ef8751d5a49c32b27d8e91",
          "message": "Share Veryl benchmark sources between Celox and Verilator benchmarks\n\nExtract wrapper module sources into benches/veryl/*.veryl and reference\nthem via include_str! from both simulation.rs and celox-bench-sv.\nThis guarantees the Celox JIT and Verilator benchmarks run identical\ncircuits compiled from the same Veryl source.\n\nAlso unify the LinearSec top module name to \"Top\" (was \"LinearSecTop\"\nin the Verilator bench only), matching the Celox Rust benchmarks.",
          "timestamp": "2026-03-07T18:11:59Z",
          "tree_id": "ecdb903854ea4d42d06881cf4994785e9f7f96aa",
          "url": "https://github.com/celox-sim/celox/commit/d7529d58a27f5681d9ef8751d5a49c32b27d8e91"
        },
        "date": 1772908579061,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ts/simulation_build_top_n1000",
            "value": 343.406177666667,
            "range": "± 1.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1",
            "value": 0.0006230203902101892,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "802542 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1000000",
            "value": 511.97209399999946,
            "range": "± 0.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1",
            "value": 0.0009625840064997502,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "519436 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1000000",
            "value": 844.7517310000003,
            "range": "± 0.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1",
            "value": 0.0009505313028734235,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "526022 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1000000",
            "value": 818.3884506666654,
            "range": "± 0.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulator_tick_x10000",
            "value": 5.20868033333439,
            "range": "± 1.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_step_x20000",
            "value": 13.076475333332686,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_build_top_n1000",
            "value": 336.9059446666676,
            "range": "± 3.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_step_x1",
            "value": 0.0008013673882235562,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "623934 samples"
          },
          {
            "name": "ts/simulation_time_step_x1000000",
            "value": 668.1448016666642,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_runUntil_1000000",
            "value": 102.25934166666654,
            "range": "± 0.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/waitForCycles_x1000",
            "value": 0.7179256666689374,
            "range": "± 10.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/manual_step_loop_x2000",
            "value": 0.7043299999980567,
            "range": "± 2.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_fast_path_100000",
            "value": 4.292438666666082,
            "range": "± 0.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_guarded_100000",
            "value": 8.959239666667903,
            "range": "± 2.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_without_optimize",
            "value": 340.6442139999999,
            "range": "± 1.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_with_optimize",
            "value": 341.85080599999975,
            "range": "± 0.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_without_optimize",
            "value": 5.122935666668734,
            "range": "± 1.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_with_optimize",
            "value": 5.238415333333251,
            "range": "± 8.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_linear_sec_p6",
            "value": 168515.89599999716,
            "range": "± 1.0%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1",
            "value": 1.2036485228846774,
            "range": "± 0.3%",
            "unit": "us",
            "extra": "415404 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1000000",
            "value": 1068679.1803333326,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_eval_linear_sec_p6_x1000000",
            "value": 1012876.8346666669,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_countones_w64",
            "value": 49872.9059999993,
            "range": "± 4.3%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1",
            "value": 1.2948209315457304,
            "range": "± 0.3%",
            "unit": "us",
            "extra": "386154 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1000000",
            "value": 1152874.5263333356,
            "range": "± 0.9%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_std_counter_w32",
            "value": 8271.804333329783,
            "range": "± 8.3%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1",
            "value": 0.2550727416082087,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "1960226 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1000000",
            "value": 149892.27299999524,
            "range": "± 0.3%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_std_counter_w32_x1000000",
            "value": 170420.9173333317,
            "range": "± 0.3%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_gray_counter_w32",
            "value": 11673.059000003073,
            "range": "± 2.5%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1",
            "value": 0.2599520731714961,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "1923432 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1000000",
            "value": 157749.0020000017,
            "range": "± 0.6%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_gray_counter_w32_x1000000",
            "value": 181367.8973333299,
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
          "id": "0bd3099f2bfb1978b820362773a3ae7d94e154f4",
          "message": "Fix eval_constexpr to recursively evaluate param expressions\n\nWhen the Veryl analyzer marks compound expressions like `N - 1` as\nis_const=true but leaves the top-level value as Unknown, eval_constexpr\nreturned None. This broke constant folding in parse_if_statement (param-\nbased conditions not folded), parse_binary for Pow (param exponents\nrejected), and concatenation replication counts.\n\nRecursively evaluate Binary/Unary sub-expressions when the top-level\ncomptime value is missing, supporting arithmetic, bitwise, shift, and\ncomparison operators.",
          "timestamp": "2026-03-07T22:59:04Z",
          "tree_id": "56f11d3c48848207e5260ac40b901a45e89f63a5",
          "url": "https://github.com/celox-sim/celox/commit/0bd3099f2bfb1978b820362773a3ae7d94e154f4"
        },
        "date": 1772925814346,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ts/simulation_build_top_n1000",
            "value": 343.07685399999997,
            "range": "± 1.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1",
            "value": 0.0005998744031259445,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "833508 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1000000",
            "value": 476.8448059999998,
            "range": "± 4.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1",
            "value": 0.0009739815917585723,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "513357 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1000000",
            "value": 792.402582666667,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1",
            "value": 0.0009271158134408679,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "539307 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1000000",
            "value": 791.4335113333327,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulator_tick_x10000",
            "value": 4.656829666665241,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_step_x20000",
            "value": 13.256197333333452,
            "range": "± 1.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_build_top_n1000",
            "value": 342.7277646666662,
            "range": "± 1.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_step_x1",
            "value": 0.0008049038575825557,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "621193 samples"
          },
          {
            "name": "ts/simulation_time_step_x1000000",
            "value": 664.4176423333339,
            "range": "± 4.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_runUntil_1000000",
            "value": 101.3272733333312,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/waitForCycles_x1000",
            "value": 0.9617680000010296,
            "range": "± 86.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/manual_step_loop_x2000",
            "value": 0.722325666664498,
            "range": "± 3.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_fast_path_100000",
            "value": 4.452333999999003,
            "range": "± 11.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_guarded_100000",
            "value": 9.00447533333257,
            "range": "± 3.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_without_optimize",
            "value": 344.8350193333342,
            "range": "± 0.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_with_optimize",
            "value": 343.0339936666666,
            "range": "± 1.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_without_optimize",
            "value": 4.643793999998404,
            "range": "± 0.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_with_optimize",
            "value": 4.624791666666472,
            "range": "± 0.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_linear_sec_p6",
            "value": 166392.8233333354,
            "range": "± 3.8%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1",
            "value": 1.2184354876072025,
            "range": "± 0.3%",
            "unit": "us",
            "extra": "410363 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1000000",
            "value": 1077586.4450000008,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_eval_linear_sec_p6_x1000000",
            "value": 1021239.2009999991,
            "range": "± 0.4%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_countones_w64",
            "value": 49868.290666665416,
            "range": "± 5.3%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1",
            "value": 1.2826239860833055,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "389826 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1000000",
            "value": 1149252.6253333343,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_std_counter_w32",
            "value": 8094.630666668914,
            "range": "± 1.6%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1",
            "value": 0.20657217836814434,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "2420462 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1000000",
            "value": 115898.32966666533,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_std_counter_w32_x1000000",
            "value": 138660.3976666714,
            "range": "± 1.4%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_gray_counter_w32",
            "value": 11684.301333337013,
            "range": "± 2.8%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1",
            "value": 0.22140786068519366,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "2258276 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1000000",
            "value": 120978.24900000221,
            "range": "± 0.5%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_gray_counter_w32_x1000000",
            "value": 158352.16099999766,
            "range": "± 4.8%",
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
          "id": "8f74385654dddb948f9a9cf6e599d76456038be9",
          "message": "Prevent use-after-free on DUT access after dispose\n\nshared_memory() passes a raw pointer to Vec<u64> via\nUint8Array::with_external_data with a no-op finalizer. When dispose()\ndrops the JitBackend, the Vec is freed but the JS ArrayBuffer/DataView\nremain alive, allowing DUT getter/setters to read/write freed memory.\n\nAdd a disposed flag to DirtyState and check it at the top of every DUT\naccessor (defineSignalProperty get/set, createArrayDut at/set for all\nthree paths). Set the flag BEFORE calling _handle.dispose() so the\nguard is active before the Rust memory is freed.",
          "timestamp": "2026-03-08T00:03:06Z",
          "tree_id": "6b6ec3238177dc7f8098542a9f829934d59ddf44",
          "url": "https://github.com/celox-sim/celox/commit/8f74385654dddb948f9a9cf6e599d76456038be9"
        },
        "date": 1772929644052,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ts/simulation_build_top_n1000",
            "value": 346.26264666666674,
            "range": "± 2.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1",
            "value": 0.0005984438163452333,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "835501 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1000000",
            "value": 468.7731776666672,
            "range": "± 0.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1",
            "value": 0.0009601889804926797,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "520731 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1000000",
            "value": 816.7810379999997,
            "range": "± 0.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1",
            "value": 0.0009468385188419714,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "528074 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1000000",
            "value": 793.6712026666671,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulator_tick_x10000",
            "value": 4.6560686666671245,
            "range": "± 1.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_step_x20000",
            "value": 13.449174000000008,
            "range": "± 1.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_build_top_n1000",
            "value": 341.83427266666575,
            "range": "± 2.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_step_x1",
            "value": 0.0007999746072535714,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "625021 samples"
          },
          {
            "name": "ts/simulation_time_step_x1000000",
            "value": 665.4777103333303,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_runUntil_1000000",
            "value": 101.32342066666509,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/waitForCycles_x1000",
            "value": 0.7302236666655517,
            "range": "± 3.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/manual_step_loop_x2000",
            "value": 0.819526333330335,
            "range": "± 7.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_fast_path_100000",
            "value": 4.316066333332856,
            "range": "± 0.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_guarded_100000",
            "value": 9.45359633333525,
            "range": "± 4.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_without_optimize",
            "value": 343.8372699999988,
            "range": "± 3.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_with_optimize",
            "value": 342.97410533333215,
            "range": "± 1.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_without_optimize",
            "value": 4.8521869999992004,
            "range": "± 1.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_with_optimize",
            "value": 4.708379333334354,
            "range": "± 1.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_linear_sec_p6",
            "value": 170070.28533333263,
            "range": "± 1.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1",
            "value": 1.2082078233661542,
            "range": "± 0.5%",
            "unit": "us",
            "extra": "413837 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1000000",
            "value": 1062298.6873333333,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_eval_linear_sec_p6_x1000000",
            "value": 1003519.034666669,
            "range": "± 0.5%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_countones_w64",
            "value": 50221.570999997006,
            "range": "± 4.3%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1",
            "value": 1.2799076497367454,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "390654 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1000000",
            "value": 1150746.180333328,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_std_counter_w32",
            "value": 8328.905333333145,
            "range": "± 3.9%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1",
            "value": 0.21441704657663577,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "2331905 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1000000",
            "value": 112565.63266666974,
            "range": "± 0.3%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_std_counter_w32_x1000000",
            "value": 131931.42900000385,
            "range": "± 0.7%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_gray_counter_w32",
            "value": 11871.974666665968,
            "range": "± 4.0%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1",
            "value": 0.2208707071617086,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "2263768 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1000000",
            "value": 116323.06033333589,
            "range": "± 0.6%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_gray_counter_w32_x1000000",
            "value": 138608.11700000582,
            "range": "± 0.4%",
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
          "id": "5c568535f3416422e789135decda5a56a235b296",
          "message": "test: increase absolute time limits for sorter_tree CI\n\nCI servers are significantly slower than dev machines. Increase limits:\n- N=64: 60s → 120s\n- N=128: 180s → 360s\n\nThe ratio-based scaling tests are the real regression guards;\nabsolute time limits just prevent catastrophic blowup.",
          "timestamp": "2026-03-10T00:08:58Z",
          "tree_id": "4d299b9f61314e7d896f290ddca99160e3447598",
          "url": "https://github.com/celox-sim/celox/commit/5c568535f3416422e789135decda5a56a235b296"
        },
        "date": 1773102838193,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ts/simulation_build_top_n1000",
            "value": 403.38775200000026,
            "range": "± 0.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1",
            "value": 0.0005987772646923047,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "835036 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1000000",
            "value": 477.10304499999984,
            "range": "± 0.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1",
            "value": 0.000988774834530069,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "505677 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1000000",
            "value": 844.2689849999975,
            "range": "± 0.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1",
            "value": 0.0009577274043707507,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "522070 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1000000",
            "value": 819.776769666667,
            "range": "± 0.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulator_tick_x10000",
            "value": 4.8315669999998745,
            "range": "± 2.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_step_x20000",
            "value": 16.300585666666546,
            "range": "± 5.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_build_top_n1000",
            "value": 399.1437753333351,
            "range": "± 2.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_step_x1",
            "value": 0.0009434954759492148,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "529945 samples"
          },
          {
            "name": "ts/simulation_time_step_x1000000",
            "value": 812.8382869999987,
            "range": "± 0.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_runUntil_1000000",
            "value": 130.89332333333246,
            "range": "± 0.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/waitForCycles_x1000",
            "value": 0.7053450000021257,
            "range": "± 2.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/manual_step_loop_x2000",
            "value": 0.6928420000040205,
            "range": "± 6.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_fast_path_100000",
            "value": 4.245171666669194,
            "range": "± 1.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_guarded_100000",
            "value": 8.80728700000327,
            "range": "± 0.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_without_optimize",
            "value": 412.38957966666686,
            "range": "± 0.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_with_optimize",
            "value": 414.2747180000006,
            "range": "± 2.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_without_optimize",
            "value": 4.86438199999975,
            "range": "± 6.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_with_optimize",
            "value": 4.722526666664635,
            "range": "± 0.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_linear_sec_p6",
            "value": 180305.88100000264,
            "range": "± 2.4%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1",
            "value": 1.2325521947987927,
            "range": "± 0.3%",
            "unit": "us",
            "extra": "405663 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1000000",
            "value": 1087656.4180000036,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_eval_linear_sec_p6_x1000000",
            "value": 1036263.8996666694,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_countones_w64",
            "value": 50012.0339999994,
            "range": "± 2.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1",
            "value": 1.3101815726038126,
            "range": "± 0.4%",
            "unit": "us",
            "extra": "381627 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1000000",
            "value": 1168332.9160000042,
            "range": "± 0.3%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_std_counter_w32",
            "value": 10955.737999999352,
            "range": "± 5.7%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1",
            "value": 0.2015477986558468,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "2480802 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1000000",
            "value": 109633.6216666629,
            "range": "± 0.3%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_std_counter_w32_x1000000",
            "value": 133408.45399999912,
            "range": "± 1.0%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_gray_counter_w32",
            "value": 15689.616666665339,
            "range": "± 2.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1",
            "value": 0.2114983204838933,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "2364085 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1000000",
            "value": 118763.50133333956,
            "range": "± 0.6%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_gray_counter_w32_x1000000",
            "value": 139709.388999996,
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
          "id": "1cf64ff1f6060c361a0052a8fc396b51cac25b0a",
          "message": "style: cargo fmt",
          "timestamp": "2026-03-10T02:50:52Z",
          "tree_id": "789fadd993f9c630883a5e6b3f4e16125f2db617",
          "url": "https://github.com/celox-sim/celox/commit/1cf64ff1f6060c361a0052a8fc396b51cac25b0a"
        },
        "date": 1773112872034,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ts/simulation_build_top_n1000",
            "value": 398.13489633333364,
            "range": "± 2.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1",
            "value": 0.0005833374839731324,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "857137 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1000000",
            "value": 455.1533646666667,
            "range": "± 3.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1",
            "value": 0.0009344134286178562,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "535096 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1000000",
            "value": 841.5273573333337,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1",
            "value": 0.0009387486895060174,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "532624 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1000000",
            "value": 825.8262503333318,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulator_tick_x10000",
            "value": 4.490598666665998,
            "range": "± 0.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_step_x20000",
            "value": 12.25620866666577,
            "range": "± 1.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_build_top_n1000",
            "value": 387.7186836666685,
            "range": "± 1.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_step_x1",
            "value": 0.0007816436056203588,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "639679 samples"
          },
          {
            "name": "ts/simulation_time_step_x1000000",
            "value": 676.8307546666645,
            "range": "± 2.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_runUntil_1000000",
            "value": 92.57775766666843,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/waitForCycles_x1000",
            "value": 0.9306509999999738,
            "range": "± 3.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/manual_step_loop_x2000",
            "value": 1.0187823333326378,
            "range": "± 28.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_fast_path_100000",
            "value": 4.6975283333304105,
            "range": "± 0.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_guarded_100000",
            "value": 11.210568333333262,
            "range": "± 1.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_without_optimize",
            "value": 397.395589333338,
            "range": "± 2.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_with_optimize",
            "value": 400.85286300000007,
            "range": "± 2.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_without_optimize",
            "value": 4.476772666666268,
            "range": "± 0.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_with_optimize",
            "value": 4.478224000001016,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_linear_sec_p6",
            "value": 166698.42666666713,
            "range": "± 0.4%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1",
            "value": 1.0442862471737782,
            "range": "± 0.3%",
            "unit": "us",
            "extra": "478796 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1000000",
            "value": 912811.526333336,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_eval_linear_sec_p6_x1000000",
            "value": 879420.1786666721,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_countones_w64",
            "value": 51632.11233333277,
            "range": "± 10.2%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1",
            "value": 1.1844186081208397,
            "range": "± 0.5%",
            "unit": "us",
            "extra": "422149 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1000000",
            "value": 1064827.8116666672,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_std_counter_w32",
            "value": 10716.702999998233,
            "range": "± 2.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1",
            "value": 0.23758902291714254,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "2104475 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1000000",
            "value": 106131.15933333272,
            "range": "± 1.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_std_counter_w32_x1000000",
            "value": 133042.941333338,
            "range": "± 0.5%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_gray_counter_w32",
            "value": 14974.695999999918,
            "range": "± 2.6%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1",
            "value": 0.24534562808233565,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "2037942 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1000000",
            "value": 111950.13300000574,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_gray_counter_w32_x1000000",
            "value": 145715.06399999876,
            "range": "± 3.5%",
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
          "id": "ce4ef3314df71f871eceff94ba337ccd4a840636",
          "message": "Fix clippy manual_unwrap_or lint in ff.rs",
          "timestamp": "2026-03-10T18:27:56Z",
          "tree_id": "0b42ffc5654ef34d1899c4e8294d442269cb4b8f",
          "url": "https://github.com/celox-sim/celox/commit/ce4ef3314df71f871eceff94ba337ccd4a840636"
        },
        "date": 1773168791436,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ts/simulation_build_top_n1000",
            "value": 405.34182233333314,
            "range": "± 2.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1",
            "value": 0.0005973397451749329,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "837045 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1000000",
            "value": 470.1896580000005,
            "range": "± 1.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1",
            "value": 0.0010139639069993377,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "493115 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1000000",
            "value": 881.5379796666663,
            "range": "± 0.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1",
            "value": 0.0009964664171545077,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "501774 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1000000",
            "value": 874.4948856666657,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulator_tick_x10000",
            "value": 4.676844000000226,
            "range": "± 2.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_step_x20000",
            "value": 15.442678666666325,
            "range": "± 40.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_build_top_n1000",
            "value": 411.3280303333352,
            "range": "± 4.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_step_x1",
            "value": 0.0007990695157173805,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "625729 samples"
          },
          {
            "name": "ts/simulation_time_step_x1000000",
            "value": 657.522961,
            "range": "± 0.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_runUntil_1000000",
            "value": 101.1306776666679,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/waitForCycles_x1000",
            "value": 0.7072020000002036,
            "range": "± 2.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/manual_step_loop_x2000",
            "value": 0.7385639999993145,
            "range": "± 4.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_fast_path_100000",
            "value": 4.23503399999754,
            "range": "± 1.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_guarded_100000",
            "value": 9.035206666667364,
            "range": "± 2.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_without_optimize",
            "value": 401.88213833333674,
            "range": "± 3.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_with_optimize",
            "value": 408.5347233333329,
            "range": "± 6.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_without_optimize",
            "value": 4.73793399999704,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_with_optimize",
            "value": 4.678353333333992,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_linear_sec_p6",
            "value": 172987.28466666216,
            "range": "± 3.0%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1",
            "value": 1.2144513454888777,
            "range": "± 0.3%",
            "unit": "us",
            "extra": "411709 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1000000",
            "value": 1081856.1199999943,
            "range": "± 0.0%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_eval_linear_sec_p6_x1000000",
            "value": 1030885.3673333313,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_countones_w64",
            "value": 52430.56800000098,
            "range": "± 3.8%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1",
            "value": 1.3212511772321605,
            "range": "± 0.5%",
            "unit": "us",
            "extra": "378430 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1000000",
            "value": 1166156.2139999976,
            "range": "± 0.3%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_std_counter_w32",
            "value": 10950.60133333512,
            "range": "± 4.7%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1",
            "value": 0.21738544830637394,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "2300062 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1000000",
            "value": 107404.74466667122,
            "range": "± 1.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_std_counter_w32_x1000000",
            "value": 131751.94899999283,
            "range": "± 1.2%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_gray_counter_w32",
            "value": 15059.516666670484,
            "range": "± 5.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1",
            "value": 0.2322099888324493,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "2153224 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1000000",
            "value": 117088.48500000506,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_gray_counter_w32_x1000000",
            "value": 137804.58233333775,
            "range": "± 1.0%",
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
          "id": "d77ad012b5282e8fc5790f128b895d3ac3836f79",
          "message": "Add optimization tuning guide and benchmark tool\n\n- Add docs/guide/optimization-tuning.md (EN) and docs/ja/ (JA) with\n  benchmark-backed guidance on SIRT pass interactions, Cranelift backend\n  options, and design-dependent behavior\n- Add pass_benchmark example for measuring per-pass compile/sim impact\n- Add sidebar entries in VitePress config\n- Link guide from CLAUDE.md optimizer section",
          "timestamp": "2026-03-10T19:23:33Z",
          "tree_id": "fcdafce122d42d04e3114380629ad5c1936d20a4",
          "url": "https://github.com/celox-sim/celox/commit/d77ad012b5282e8fc5790f128b895d3ac3836f79"
        },
        "date": 1773172108884,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ts/simulation_build_top_n1000",
            "value": 384.12591233333325,
            "range": "± 1.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1",
            "value": 0.0005793659759467409,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "863013 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1000000",
            "value": 458.11920599999945,
            "range": "± 0.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1",
            "value": 0.0009501312699062927,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "526244 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1000000",
            "value": 865.8678459999998,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1",
            "value": 0.0009929732175635961,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "503539 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1000000",
            "value": 854.7156146666675,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulator_tick_x10000",
            "value": 4.481591666666645,
            "range": "± 0.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_step_x20000",
            "value": 12.461713666665068,
            "range": "± 9.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_build_top_n1000",
            "value": 389.93810900000244,
            "range": "± 4.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_step_x1",
            "value": 0.000751545526276814,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "665297 samples"
          },
          {
            "name": "ts/simulation_time_step_x1000000",
            "value": 612.4516006666697,
            "range": "± 2.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_runUntil_1000000",
            "value": 91.1382426666678,
            "range": "± 0.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/waitForCycles_x1000",
            "value": 0.7421313333349341,
            "range": "± 4.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/manual_step_loop_x2000",
            "value": 0.7467996666673571,
            "range": "± 0.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_fast_path_100000",
            "value": 4.717261000000387,
            "range": "± 2.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_guarded_100000",
            "value": 9.4540873333317,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_without_optimize",
            "value": 386.3783506666684,
            "range": "± 0.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_with_optimize",
            "value": 389.0496493333315,
            "range": "± 0.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_without_optimize",
            "value": 4.466476333330017,
            "range": "± 0.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_with_optimize",
            "value": 4.57532366666904,
            "range": "± 2.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_linear_sec_p6",
            "value": 165679.4310000017,
            "range": "± 2.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1",
            "value": 1.0505022071099663,
            "range": "± 0.3%",
            "unit": "us",
            "extra": "475963 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1000000",
            "value": 932551.4066666656,
            "range": "± 0.5%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_eval_linear_sec_p6_x1000000",
            "value": 887156.9079999948,
            "range": "± 0.3%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_countones_w64",
            "value": 50988.44233333754,
            "range": "± 2.0%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1",
            "value": 1.2052155887198255,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "414864 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1000000",
            "value": 1167372.814000002,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_std_counter_w32",
            "value": 10314.983666670742,
            "range": "± 2.3%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1",
            "value": 0.23771168576077345,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "2103389 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1000000",
            "value": 103142.99233332956,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_std_counter_w32_x1000000",
            "value": 159618.76466667067,
            "range": "± 3.5%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_gray_counter_w32",
            "value": 14976.96466666821,
            "range": "± 3.7%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1",
            "value": 0.2510003664689491,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "1992029 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1000000",
            "value": 110301.83600000358,
            "range": "± 1.5%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_gray_counter_w32_x1000000",
            "value": 166082.05633333515,
            "range": "± 3.8%",
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
          "id": "43dde93cf46460d92f9c9bea8fa96f40bd3ffaa4",
          "message": "Add bench example files to .gitignore",
          "timestamp": "2026-03-11T02:13:10Z",
          "tree_id": "146a6f835fb3dd473780e89c89774f67c61af396",
          "url": "https://github.com/celox-sim/celox/commit/43dde93cf46460d92f9c9bea8fa96f40bd3ffaa4"
        },
        "date": 1773196665281,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ts/simulation_build_top_n1000",
            "value": 353.64343966666667,
            "range": "± 1.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1",
            "value": 0.0006009729322041241,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "831985 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1000000",
            "value": 465.60836866666614,
            "range": "± 0.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1",
            "value": 0.0009495542299762105,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "526563 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1000000",
            "value": 812.2968346666663,
            "range": "± 0.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1",
            "value": 0.0009315285801843487,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "536753 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1000000",
            "value": 791.9629363333333,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulator_tick_x10000",
            "value": 4.681205666668878,
            "range": "± 1.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_step_x20000",
            "value": 13.209034999999858,
            "range": "± 1.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_build_top_n1000",
            "value": 347.78413466666825,
            "range": "± 2.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_step_x1",
            "value": 0.0008098806057891588,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "617375 samples"
          },
          {
            "name": "ts/simulation_time_step_x1000000",
            "value": 668.3042126666672,
            "range": "± 2.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_runUntil_1000000",
            "value": 100.40333399999993,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/waitForCycles_x1000",
            "value": 0.6908296666685297,
            "range": "± 2.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/manual_step_loop_x2000",
            "value": 0.7128816666663624,
            "range": "± 6.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_fast_path_100000",
            "value": 4.208282999997512,
            "range": "± 0.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_guarded_100000",
            "value": 9.003979999998895,
            "range": "± 1.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_without_optimize",
            "value": 346.37731933333515,
            "range": "± 1.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_with_optimize",
            "value": 346.43733200000133,
            "range": "± 4.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_without_optimize",
            "value": 4.702202666664864,
            "range": "± 5.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_with_optimize",
            "value": 4.814381333334798,
            "range": "± 18.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_linear_sec_p6",
            "value": 168345.51300000263,
            "range": "± 1.3%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1",
            "value": 1.2246810419637053,
            "range": "± 0.5%",
            "unit": "us",
            "extra": "408270 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1000000",
            "value": 1074577.7160000023,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_eval_linear_sec_p6_x1000000",
            "value": 1013093.133000007,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_countones_w64",
            "value": 50177.34533333472,
            "range": "± 5.6%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1",
            "value": 1.2842531489863473,
            "range": "± 0.4%",
            "unit": "us",
            "extra": "389332 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1000000",
            "value": 1152460.8599999968,
            "range": "± 0.5%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_std_counter_w32",
            "value": 10249.75366666816,
            "range": "± 3.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1",
            "value": 0.20901086019290854,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "2392221 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1000000",
            "value": 108540.81466667412,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_std_counter_w32_x1000000",
            "value": 134363.68166666458,
            "range": "± 1.6%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_gray_counter_w32",
            "value": 14741.611999997986,
            "range": "± 3.3%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1",
            "value": 0.21307676897141556,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "2346573 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1000000",
            "value": 120558.61500000658,
            "range": "± 0.4%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_gray_counter_w32_x1000000",
            "value": 139539.26400000637,
            "range": "± 0.7%",
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
          "id": "c8d3bae50ae1ef6a0e4adbff0c6162aae08a8b93",
          "message": "Fix formatting (cargo fmt)",
          "timestamp": "2026-03-14T22:15:00Z",
          "tree_id": "1664e162817ac93ebcbbb18e6ce72b5db813dfa3",
          "url": "https://github.com/celox-sim/celox/commit/c8d3bae50ae1ef6a0e4adbff0c6162aae08a8b93"
        },
        "date": 1773528253233,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ts/simulation_build_top_n1000",
            "value": 331.2369876666665,
            "range": "± 2.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1",
            "value": 0.0006430721230894313,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "777518 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1000000",
            "value": 514.938127,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1",
            "value": 0.0009817948679683062,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "509272 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1000000",
            "value": 853.4765653333343,
            "range": "± 0.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1",
            "value": 0.0009634199447028175,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "518985 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1000000",
            "value": 841.2073196666666,
            "range": "± 0.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulator_tick_x10000",
            "value": 5.162764333332841,
            "range": "± 1.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_step_x20000",
            "value": 13.10337833333324,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_build_top_n1000",
            "value": 322.61946066666616,
            "range": "± 0.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_step_x1",
            "value": 0.0008067241134520165,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "619791 samples"
          },
          {
            "name": "ts/simulation_time_step_x1000000",
            "value": 658.139800666669,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_runUntil_1000000",
            "value": 101.52287866666786,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/waitForCycles_x1000",
            "value": 0.7333063333353493,
            "range": "± 1.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/manual_step_loop_x2000",
            "value": 0.7230976666687638,
            "range": "± 4.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_fast_path_100000",
            "value": 4.241058666666504,
            "range": "± 2.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_guarded_100000",
            "value": 9.290480999998787,
            "range": "± 1.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_without_optimize",
            "value": 329.28394366666663,
            "range": "± 1.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_with_optimize",
            "value": 333.39537466666417,
            "range": "± 0.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_without_optimize",
            "value": 5.1275176666677,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_with_optimize",
            "value": 5.13905966666789,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_linear_sec_p6",
            "value": 207408.33199999665,
            "range": "± 78.9%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1",
            "value": 1.2071265671524674,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "414207 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1000000",
            "value": 1076315.1733333336,
            "range": "± 3.5%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_eval_linear_sec_p6_x1000000",
            "value": 1008599.4810000024,
            "range": "± 0.0%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_countones_w64",
            "value": 49462.695666666456,
            "range": "± 0.7%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1",
            "value": 1.2922948083428207,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "386909 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1000000",
            "value": 1143501.873000001,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_std_counter_w32",
            "value": 9909.485000001345,
            "range": "± 0.5%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1",
            "value": 0.25320472155969115,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "1974687 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1000000",
            "value": 158778.6583333315,
            "range": "± 0.3%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_std_counter_w32_x1000000",
            "value": 177013.64166665977,
            "range": "± 0.5%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_gray_counter_w32",
            "value": 14029.71766666451,
            "range": "± 0.7%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1",
            "value": 0.2570091351261229,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "1945457 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1000000",
            "value": 170534.46599999734,
            "range": "± 0.4%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_gray_counter_w32_x1000000",
            "value": 185190.36966667045,
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
          "id": "be71b640110460211405c51067bd7b3c4a40a9ca",
          "message": "Fix FF parser widening shift result by RHS width\n\nThe FF path used max(lhs_width, rhs_width) for shift/pow operations,\ncausing the result to be incorrectly widened when the shift amount\n(e.g. a 32-bit for-loop constant) was wider than the value being\nshifted. This matches the comb path which already uses LHS-only width\nper IEEE 1800-2023 §11.4.10.",
          "timestamp": "2026-03-17T00:40:23Z",
          "tree_id": "99362c5f19276293e0f62fd00af147e277202f7a",
          "url": "https://github.com/celox-sim/celox/commit/be71b640110460211405c51067bd7b3c4a40a9ca"
        },
        "date": 1773710131063,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ts/simulation_build_top_n1000",
            "value": 352.3291616666669,
            "range": "± 15.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1",
            "value": 0.0006177998020571356,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "809324 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1000000",
            "value": 464.2107903333338,
            "range": "± 0.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1",
            "value": 0.0009950316459613807,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "502497 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1000000",
            "value": 844.4019710000006,
            "range": "± 2.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1",
            "value": 0.0009682097060908913,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "516418 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1000000",
            "value": 827.2221153333327,
            "range": "± 0.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulator_tick_x10000",
            "value": 4.993771000000076,
            "range": "± 5.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_step_x20000",
            "value": 12.993632333333759,
            "range": "± 1.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_build_top_n1000",
            "value": 342.2745243333363,
            "range": "± 4.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_step_x1",
            "value": 0.0007967329834068072,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "627564 samples"
          },
          {
            "name": "ts/simulation_time_step_x1000000",
            "value": 645.3840316666684,
            "range": "± 0.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_runUntil_1000000",
            "value": 100.42200233333521,
            "range": "± 3.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/waitForCycles_x1000",
            "value": 0.7026153333354159,
            "range": "± 6.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/manual_step_loop_x2000",
            "value": 0.7153720000011768,
            "range": "± 9.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_fast_path_100000",
            "value": 4.249357999998513,
            "range": "± 0.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_guarded_100000",
            "value": 8.813943333334464,
            "range": "± 0.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_without_optimize",
            "value": 345.88709500000067,
            "range": "± 3.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_with_optimize",
            "value": 351.1817066666699,
            "range": "± 4.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_without_optimize",
            "value": 4.873624666669154,
            "range": "± 0.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_with_optimize",
            "value": 4.82658966666592,
            "range": "± 1.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_linear_sec_p6",
            "value": 173905.15033333455,
            "range": "± 1.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1",
            "value": 1.2615314701416231,
            "range": "± 0.7%",
            "unit": "us",
            "extra": "396344 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1000000",
            "value": 1097690.3233333312,
            "range": "± 0.4%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_eval_linear_sec_p6_x1000000",
            "value": 1052985.3663333342,
            "range": "± 0.4%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_countones_w64",
            "value": 50461.23900000142,
            "range": "± 1.3%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1",
            "value": 1.3636650293608281,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "366659 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1000000",
            "value": 1210976.508666669,
            "range": "± 1.6%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_std_counter_w32",
            "value": 11263.632333332984,
            "range": "± 4.7%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1",
            "value": 0.24250565668491372,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "2061808 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1000000",
            "value": 123841.79166666097,
            "range": "± 1.4%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_std_counter_w32_x1000000",
            "value": 137500.04033333002,
            "range": "± 0.7%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_gray_counter_w32",
            "value": 16144.883999998758,
            "range": "± 1.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1",
            "value": 0.23578504990971003,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "2120576 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1000000",
            "value": 126094.90700000606,
            "range": "± 0.7%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_gray_counter_w32_x1000000",
            "value": 144247.92733333985,
            "range": "± 1.0%",
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
          "id": "d4fafe4a3e0a8a2177145268e98fad53b233108c",
          "message": "Add verilator obj_dir and debug trace files to .gitignore",
          "timestamp": "2026-03-23T12:02:39Z",
          "tree_id": "f0c7fa2e0db8a165397d2366eb7a6ccea2c8bc51",
          "url": "https://github.com/celox-sim/celox/commit/d4fafe4a3e0a8a2177145268e98fad53b233108c"
        },
        "date": 1774268859338,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ts/simulation_build_top_n1000",
            "value": 352.67191633333306,
            "range": "± 2.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1",
            "value": 0.0006070872675614906,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "823605 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1000000",
            "value": 476.20644166666597,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1",
            "value": 0.0009648710025403143,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "518204 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1000000",
            "value": 829.5106743333333,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1",
            "value": 0.0009381014555541818,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "532992 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1000000",
            "value": 822.816021,
            "range": "± 0.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulator_tick_x10000",
            "value": 4.838465666666404,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_step_x20000",
            "value": 13.13674033333397,
            "range": "± 0.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_build_top_n1000",
            "value": 345.1130439999967,
            "range": "± 2.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_step_x1",
            "value": 0.0008008442632421223,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "624342 samples"
          },
          {
            "name": "ts/simulation_time_step_x1000000",
            "value": 656.1683953333364,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_runUntil_1000000",
            "value": 102.163460333332,
            "range": "± 2.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/waitForCycles_x1000",
            "value": 0.7095443333358465,
            "range": "± 5.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/manual_step_loop_x2000",
            "value": 0.7270536666668098,
            "range": "± 7.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_fast_path_100000",
            "value": 4.3074140000001835,
            "range": "± 1.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_guarded_100000",
            "value": 8.749581333332268,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_without_optimize",
            "value": 352.84038466666726,
            "range": "± 6.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_with_optimize",
            "value": 351.0859143333316,
            "range": "± 4.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_without_optimize",
            "value": 4.712634666665205,
            "range": "± 0.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_with_optimize",
            "value": 4.809304666664199,
            "range": "± 3.9%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_linear_sec_p6",
            "value": 171082.73099999983,
            "range": "± 1.9%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1",
            "value": 1.2157649295838322,
            "range": "± 0.3%",
            "unit": "us",
            "extra": "411264 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1000000",
            "value": 1074497.8653333334,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_eval_linear_sec_p6_x1000000",
            "value": 1020358.991000001,
            "range": "± 0.5%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_countones_w64",
            "value": 49874.06333333153,
            "range": "± 1.3%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1",
            "value": 1.1361832935657499,
            "range": "± 0.4%",
            "unit": "us",
            "extra": "440070 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1000000",
            "value": 985249.921666662,
            "range": "± 0.4%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_std_counter_w32",
            "value": 10209.429000004699,
            "range": "± 5.9%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1",
            "value": 0.20959634556308526,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "2385538 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1000000",
            "value": 121060.83933333382,
            "range": "± 1.9%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_std_counter_w32_x1000000",
            "value": 140549.41166666686,
            "range": "± 0.5%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_gray_counter_w32",
            "value": 14489.663333336162,
            "range": "± 3.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1",
            "value": 0.21783096101326208,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "2295358 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1000000",
            "value": 125131.18566666769,
            "range": "± 1.8%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_gray_counter_w32_x1000000",
            "value": 153045.08133333488,
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
          "id": "6b01130bda6cbb89e98aff018061ecbdd262d23a",
          "message": "Add ts-gen tests for duplicate scoped variables and generate-for\n\nVerify that TypeScript type generation handles modules with:\n- Duplicate-named scoped variables in always_comb for-loop scopes\n- Generate-for blocks with scoped variables in always_comb\n- Complex patterns combining scoped vars with for-loop instances",
          "timestamp": "2026-03-23T13:01:54Z",
          "tree_id": "c5a099565af6cf5437a9eacbebe62bd6b7bdecb1",
          "url": "https://github.com/celox-sim/celox/commit/6b01130bda6cbb89e98aff018061ecbdd262d23a"
        },
        "date": 1774272476334,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ts/simulation_build_top_n1000",
            "value": 345.5725089999999,
            "range": "± 5.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1",
            "value": 0.0006020209707951955,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "830536 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1000000",
            "value": 481.6411116666665,
            "range": "± 0.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1",
            "value": 0.0009971765460209056,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "501416 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1000000",
            "value": 870.7647523333326,
            "range": "± 0.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1",
            "value": 0.0009794575552362242,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "510487 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1000000",
            "value": 848.0730616666673,
            "range": "± 0.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulator_tick_x10000",
            "value": 4.8155526666669175,
            "range": "± 1.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_step_x20000",
            "value": 12.972439333332053,
            "range": "± 1.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_build_top_n1000",
            "value": 339.4765416666657,
            "range": "± 2.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_step_x1",
            "value": 0.0008007999589964509,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "624377 samples"
          },
          {
            "name": "ts/simulation_time_step_x1000000",
            "value": 655.1100056666693,
            "range": "± 0.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_runUntil_1000000",
            "value": 100.2424540000017,
            "range": "± 0.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/waitForCycles_x1000",
            "value": 0.6891276666647173,
            "range": "± 7.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/manual_step_loop_x2000",
            "value": 0.6895550000020497,
            "range": "± 5.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_fast_path_100000",
            "value": 4.352502333332571,
            "range": "± 0.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_guarded_100000",
            "value": 8.95874799999971,
            "range": "± 1.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_without_optimize",
            "value": 342.4317806666659,
            "range": "± 2.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_with_optimize",
            "value": 344.3912170000015,
            "range": "± 1.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_without_optimize",
            "value": 4.915204333333047,
            "range": "± 8.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_with_optimize",
            "value": 4.848543666666956,
            "range": "± 0.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_linear_sec_p6",
            "value": 171586.35933333426,
            "range": "± 3.3%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1",
            "value": 1.2228702390715946,
            "range": "± 0.4%",
            "unit": "us",
            "extra": "408875 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1000000",
            "value": 1081908.0003333292,
            "range": "± 0.3%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_eval_linear_sec_p6_x1000000",
            "value": 1026118.9033333358,
            "range": "± 0.4%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_countones_w64",
            "value": 51389.2130000022,
            "range": "± 4.7%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1",
            "value": 1.1391353541447755,
            "range": "± 0.5%",
            "unit": "us",
            "extra": "438930 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1000000",
            "value": 998530.0473333336,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_std_counter_w32",
            "value": 10682.627000006809,
            "range": "± 6.3%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1",
            "value": 0.22589359419747865,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "2213432 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1000000",
            "value": 125248.51266667247,
            "range": "± 2.1%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_std_counter_w32_x1000000",
            "value": 146054.38666666546,
            "range": "± 0.4%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_gray_counter_w32",
            "value": 15230.581333332035,
            "range": "± 1.7%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1",
            "value": 0.2229824963277864,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "2242329 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1000000",
            "value": 134574.0456666681,
            "range": "± 1.4%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_gray_counter_w32_x1000000",
            "value": 152160.6909999973,
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
          "id": "5417c4031c8137cb416638da751fd0ef895e41c8",
          "message": "Fix clippy errors in celox crate",
          "timestamp": "2026-03-24T09:36:43Z",
          "tree_id": "9e22d82ae786a6f2f31acbca3bcdb1830f27c51f",
          "url": "https://github.com/celox-sim/celox/commit/5417c4031c8137cb416638da751fd0ef895e41c8"
        },
        "date": 1774346634092,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ts/simulation_build_top_n1000",
            "value": 388.52296033333306,
            "range": "± 1.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1",
            "value": 0.0008544376611676945,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "585181 samples"
          },
          {
            "name": "ts/simulation_tick_top_n1000_x1000000",
            "value": 759.5280556666664,
            "range": "± 0.0%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1",
            "value": 0.001232733781556448,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "405603 samples"
          },
          {
            "name": "ts/testbench_tick_top_n1000_x1000000",
            "value": 1095.7094709999994,
            "range": "± 2.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1",
            "value": 0.0012214389135070663,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "409354 samples"
          },
          {
            "name": "ts/testbench_array_tick_top_n1000_x1000000",
            "value": 1084.8825243333315,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulator_tick_x10000",
            "value": 7.564880666664976,
            "range": "± 1.6%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_step_x20000",
            "value": 11.532978333331508,
            "range": "± 0.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_build_top_n1000",
            "value": 384.54905400000035,
            "range": "± 1.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_step_x1",
            "value": 0.000730919382521822,
            "range": "± 0.1%",
            "unit": "ms",
            "extra": "684070 samples"
          },
          {
            "name": "ts/simulation_time_step_x1000000",
            "value": 582.8715166666661,
            "range": "± 0.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_time_runUntil_1000000",
            "value": 84.82154499999645,
            "range": "± 0.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/waitForCycles_x1000",
            "value": 0.7079853333343635,
            "range": "± 2.2%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/manual_step_loop_x2000",
            "value": 0.7175200000007559,
            "range": "± 4.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_fast_path_100000",
            "value": 4.264851333333354,
            "range": "± 2.5%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/runUntil_guarded_100000",
            "value": 8.857656666667026,
            "range": "± 0.4%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_without_optimize",
            "value": 391.23062799999997,
            "range": "± 1.3%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/build_with_optimize",
            "value": 460.7302460000016,
            "range": "± 2.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_without_optimize",
            "value": 7.60067166666704,
            "range": "± 0.8%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/tick_x10000_with_optimize",
            "value": 7.911209666667371,
            "range": "± 0.7%",
            "unit": "ms",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_linear_sec_p6",
            "value": 148554.06199999803,
            "range": "± 3.4%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1",
            "value": 0.8915569784789679,
            "range": "± 0.3%",
            "unit": "us",
            "extra": "560817 samples"
          },
          {
            "name": "ts/simulation_eval_linear_sec_p6_x1000000",
            "value": 770737.9396666705,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_eval_linear_sec_p6_x1000000",
            "value": 711288.9896666651,
            "range": "± 1.8%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_countones_w64",
            "value": 34595.16266666469,
            "range": "± 4.6%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1",
            "value": 1.0109314872478383,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "494594 samples"
          },
          {
            "name": "ts/simulation_eval_countones_w64_x1000000",
            "value": 904110.0016666605,
            "range": "± 3.0%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_std_counter_w32",
            "value": 7488.109333333948,
            "range": "± 4.0%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1",
            "value": 0.20512216458695984,
            "range": "± 0.1%",
            "unit": "us",
            "extra": "2437572 samples"
          },
          {
            "name": "ts/simulation_tick_std_counter_w32_x1000000",
            "value": 112613.45699999947,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_std_counter_w32_x1000000",
            "value": 139137.22866667376,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_build_gray_counter_w32",
            "value": 10224.673999997322,
            "range": "± 3.0%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1",
            "value": 0.2144201468142026,
            "range": "± 0.2%",
            "unit": "us",
            "extra": "2331892 samples"
          },
          {
            "name": "ts/simulation_tick_gray_counter_w32_x1000000",
            "value": 119157.23099999984,
            "range": "± 0.8%",
            "unit": "us",
            "extra": "3 samples"
          },
          {
            "name": "ts/testbench_tick_gray_counter_w32_x1000000",
            "value": 137042.77233332687,
            "range": "± 0.4%",
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
          "id": "fe52243e73c8a04f0bae1025b5603d5593e28424",
          "message": "Merge branch 'claude/streamed-strolling-whistle': Add Z signal support",
          "timestamp": "2026-03-05T22:37:13Z",
          "tree_id": "397c2ddc41628122d6e57f6d7bc642175969e0db",
          "url": "https://github.com/celox-sim/celox/commit/fe52243e73c8a04f0bae1025b5603d5593e28424"
        },
        "date": 1772753404298,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "verilator/simulation_build_top_n1000",
            "value": 5755244.111,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_linear_sec_p6",
            "value": 1107022.216,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_countones_w64",
            "value": 1053951.05,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_std_counter_w32",
            "value": 1047864.362,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_gray_counter_w32",
            "value": 1068226.75,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1",
            "value": 0.3402752737277644,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1000000",
            "value": 341259.01399999997,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1",
            "value": 0.34287407482133236,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1000000",
            "value": 351678.16133333335,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1",
            "value": 0.035097460283119865,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1000000",
            "value": 35160.97033333333,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_eval_linear_sec_p6_x1000000",
            "value": 35584.43033333334,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1",
            "value": 0.034556008576848624,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1000000",
            "value": 34916.01966666667,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1",
            "value": 0.034523600057236876,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1000000",
            "value": 34545.48966666667,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_std_counter_w32_x1000000",
            "value": 34561.94033333334,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1",
            "value": 0.0378752390041611,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1000000",
            "value": 37957.40033333334,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_gray_counter_w32_x1000000",
            "value": 38306.575333333334,
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
          "id": "b3a4fb2e5f26957bcc2c872e94bcbb903e7ad710",
          "message": "Fix dut tests for new X encoding (v=1, m=1)\n\nZ support changed 4-state encoding from X=(v=0,m=1) to X=(v=1,m=1).\nwriteAllX was updated but three tests still expected the old encoding.\n\nAlso fix biome config to only lint source files (not dist/ build\nartifacts), and suppress clippy::needless_borrow in celox-bench-sv.",
          "timestamp": "2026-03-06T01:15:07Z",
          "tree_id": "83196d261356b3ddcbc709f4f7a807a98e3f186a",
          "url": "https://github.com/celox-sim/celox/commit/b3a4fb2e5f26957bcc2c872e94bcbb903e7ad710"
        },
        "date": 1772761061177,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "verilator/simulation_build_top_n1000",
            "value": 14123509.244,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_linear_sec_p6",
            "value": 1175797.109,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_countones_w64",
            "value": 1088597.778,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_std_counter_w32",
            "value": 1086620.081,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_gray_counter_w32",
            "value": 1107731.961,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1",
            "value": 0.3406304745853601,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1000000",
            "value": 340762.69000000006,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1",
            "value": 0.3397841608330015,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1000000",
            "value": 353708.61699999997,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1",
            "value": 0.03579884120484679,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1000000",
            "value": 35519.72466666666,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_eval_linear_sec_p6_x1000000",
            "value": 35592.05266666666,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1",
            "value": 0.03452886691144083,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1000000",
            "value": 35039.30066666666,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1",
            "value": 0.034590552463016504,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1000000",
            "value": 34632.91133333334,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_std_counter_w32_x1000000",
            "value": 34681.576,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1",
            "value": 0.035463479562273935,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1000000",
            "value": 35030.10933333333,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_gray_counter_w32_x1000000",
            "value": 35089.78033333333,
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
          "id": "1844e23e7b5e07da0d8ee1635e413d4792e88ea3",
          "message": "Fix dead link in gap analysis doc\n\nThe relative path from docs/ts_testbench_gap_analysis.md to\ndocs/guide/parameter-overrides.md should be ./guide/ not ../guide/.",
          "timestamp": "2026-03-06T02:40:56Z",
          "tree_id": "3b1d485b5bf9f4589ff49cd19587ec1963392dea",
          "url": "https://github.com/celox-sim/celox/commit/1844e23e7b5e07da0d8ee1635e413d4792e88ea3"
        },
        "date": 1772766188358,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "verilator/simulation_build_top_n1000",
            "value": 11764269.922,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_linear_sec_p6",
            "value": 1158755.852,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_countones_w64",
            "value": 1099554.367,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_std_counter_w32",
            "value": 1127316.184,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_gray_counter_w32",
            "value": 1118085.558,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1",
            "value": 0.3403122666544726,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1000000",
            "value": 340537.7443333334,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1",
            "value": 0.34174340544192383,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1000000",
            "value": 352507.9536666667,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1",
            "value": 0.03525262408363268,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1000000",
            "value": 35964.384333333335,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_eval_linear_sec_p6_x1000000",
            "value": 35561.29466666666,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1",
            "value": 0.03455657372954888,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1000000",
            "value": 34844.89599999999,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1",
            "value": 0.034543323062538614,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1000000",
            "value": 34574.167,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_std_counter_w32_x1000000",
            "value": 34966.45999999999,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1",
            "value": 0.03495711993202746,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1000000",
            "value": 34965.905333333336,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_gray_counter_w32_x1000000",
            "value": 35055.73,
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
          "id": "dd3d6e778f1e278917cfa7ace10b9e4143d2b02a",
          "message": "Add pre-push submodule validation and test gate via lefthook\n\nEnsure submodules are initialized, in sync, and their commits exist on\nthe remote before allowing push. Also run the full test suite (pnpm test)\nas a final gate. Use piped execution so failures block subsequent steps.",
          "timestamp": "2026-03-06T10:44:19Z",
          "tree_id": "4cb69d58108de90e3275cc11403552d653222edc",
          "url": "https://github.com/celox-sim/celox/commit/dd3d6e778f1e278917cfa7ace10b9e4143d2b02a"
        },
        "date": 1772795243036,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "verilator/simulation_build_top_n1000",
            "value": 11222778.981,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_linear_sec_p6",
            "value": 1197721.746,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_countones_w64",
            "value": 1181622.266,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_std_counter_w32",
            "value": 1165894.613,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_gray_counter_w32",
            "value": 1136516.136,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1",
            "value": 0.34147554213694037,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1000000",
            "value": 338631.00533333333,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1",
            "value": 0.33960386393562975,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1000000",
            "value": 351557.7043333333,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1",
            "value": 0.03526743376980573,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1000000",
            "value": 35631.094333333334,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_eval_linear_sec_p6_x1000000",
            "value": 35439.454666666665,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1",
            "value": 0.03545312562200218,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1000000",
            "value": 35676.12599999999,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1",
            "value": 0.0345905765952189,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1000000",
            "value": 34531.257333333335,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_std_counter_w32_x1000000",
            "value": 34682.785,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1",
            "value": 0.03501534641012118,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1000000",
            "value": 35047.603,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_gray_counter_w32_x1000000",
            "value": 35004.54899999999,
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
          "id": "cde884593abab6375c496a99b2f80a8d231fadd7",
          "message": "Fix fill-literal ('0/'1/'x/'z) width=0 causing bit loss in always_comb/always_ff\n\nFill-literals have width 0 from the Veryl analyzer (context-dependent).\nThe Factor::Value handler in both comb and FF paths emitted 0-width\nSLTNode::Constant / SIR registers, which caused Mux lowering to produce\n0-bit masks that zeroed out else-arm values in multi-branch if/else if\nalways_comb blocks.\n\nPer IEEE 1800-2023 §5.7.1, fill-literals replicate their single bit\nacross the full context width (1 bit in self-determined contexts).\nExpand both value and mask_xz by replicating bit 0 to the target width.",
          "timestamp": "2026-03-06T14:42:08Z",
          "tree_id": "f5c0dbc897cdabc00cea7cb9a39c94adfdc8af86",
          "url": "https://github.com/celox-sim/celox/commit/cde884593abab6375c496a99b2f80a8d231fadd7"
        },
        "date": 1772809423272,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "verilator/simulation_build_top_n1000",
            "value": 5958300.731,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_linear_sec_p6",
            "value": 1128504.452,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_countones_w64",
            "value": 1092267.883,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_std_counter_w32",
            "value": 1103763.135,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_gray_counter_w32",
            "value": 1113826.848,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1",
            "value": 0.340234259688,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1000000",
            "value": 340554.236,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1",
            "value": 0.34172054737840357,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1000000",
            "value": 352519.2036666667,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1",
            "value": 0.03509927829638506,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1000000",
            "value": 35411.335999999996,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_eval_linear_sec_p6_x1000000",
            "value": 35697.22666666666,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1",
            "value": 0.034554579144076614,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1000000",
            "value": 34923.682,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1",
            "value": 0.03452876050744383,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1000000",
            "value": 34601.50033333334,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_std_counter_w32_x1000000",
            "value": 34607.26433333333,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1",
            "value": 0.034971870014764914,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1000000",
            "value": 35032.761666666665,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_gray_counter_w32_x1000000",
            "value": 35004.585333333336,
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
          "id": "fa0347fccb316ac5c78006c28a32cd1965a95b8f",
          "message": "Add untracked file detection to pre-push clean-tree check\n\nPrevents pushing when untracked files exist, catching cases where\ntests reference local files (e.g. via include_str!) that aren't\ncommitted. Previously only tracked file changes were detected.",
          "timestamp": "2026-03-06T15:14:47Z",
          "tree_id": "82e54feef12a5e1c65eba78a3e33bd9bf9d22e1e",
          "url": "https://github.com/celox-sim/celox/commit/fa0347fccb316ac5c78006c28a32cd1965a95b8f"
        },
        "date": 1772811503264,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "verilator/simulation_build_top_n1000",
            "value": 7252079.355,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_linear_sec_p6",
            "value": 1161655.176,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_countones_w64",
            "value": 1110417.695,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_std_counter_w32",
            "value": 1122029.292,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_gray_counter_w32",
            "value": 1090374.96,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1",
            "value": 0.3400432829629221,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1000000",
            "value": 343736.70633333334,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1",
            "value": 0.35687114826513516,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1000000",
            "value": 371091.291,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1",
            "value": 0.0351966526569674,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1000000",
            "value": 37217.86833333333,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_eval_linear_sec_p6_x1000000",
            "value": 36117.94566666667,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1",
            "value": 0.03457254395960618,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1000000",
            "value": 35159.44400000001,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1",
            "value": 0.034509688617196325,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1000000",
            "value": 34603.13566666666,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_std_counter_w32_x1000000",
            "value": 34649.078,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1",
            "value": 0.0350620170439004,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1000000",
            "value": 35047.469999999994,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_gray_counter_w32_x1000000",
            "value": 34965.922,
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
          "id": "9a6ef5d0cc81ed4bf680c5b6fad3d2a21554bb41",
          "message": "Box SimBackend::Full to fix clippy large_enum_variant warning",
          "timestamp": "2026-03-07T10:39:04Z",
          "tree_id": "324129867e926b82150e673d111e12a1fe763c36",
          "url": "https://github.com/celox-sim/celox/commit/9a6ef5d0cc81ed4bf680c5b6fad3d2a21554bb41"
        },
        "date": 1772881477397,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "verilator/simulation_build_top_n1000",
            "value": 6623402.214,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_linear_sec_p6",
            "value": 1159824.438,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_countones_w64",
            "value": 1105064.763,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_std_counter_w32",
            "value": 1105792.525,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_gray_counter_w32",
            "value": 1102314.127,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1",
            "value": 0.34007475107200924,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1000000",
            "value": 366774.665,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1",
            "value": 0.34218348598224946,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1000000",
            "value": 352508.2626666667,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1",
            "value": 0.03543348758159475,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1000000",
            "value": 35382.13399999999,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_eval_linear_sec_p6_x1000000",
            "value": 35658.533,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1",
            "value": 0.034564898756824246,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1000000",
            "value": 34928.240333333335,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1",
            "value": 0.03455639087568662,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1000000",
            "value": 34584.112,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_std_counter_w32_x1000000",
            "value": 35019.60433333334,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1",
            "value": 0.035020315216333094,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1000000",
            "value": 34994.00666666666,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_gray_counter_w32_x1000000",
            "value": 34981.44966666667,
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
          "id": "308dd9af7e68375c3a1406b1a0f074926a7ce657",
          "message": "Revert Box<Simulator> — use allow(clippy::large_enum_variant) instead\n\nBoxing adds indirection on every tick call in the hot path.",
          "timestamp": "2026-03-07T11:28:14Z",
          "tree_id": "9d27eac47590a27fba99f8bf9d04e19473b9ed19",
          "url": "https://github.com/celox-sim/celox/commit/308dd9af7e68375c3a1406b1a0f074926a7ce657"
        },
        "date": 1772884296997,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "verilator/simulation_build_top_n1000",
            "value": 9594943.625,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_linear_sec_p6",
            "value": 1113076.443,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_countones_w64",
            "value": 1061032.028,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_std_counter_w32",
            "value": 1056018.029,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_gray_counter_w32",
            "value": 1066201.185,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1",
            "value": 0.3404882023931267,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1000000",
            "value": 338805.7953333334,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1",
            "value": 0.34021252314538697,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1000000",
            "value": 351425.9063333333,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1",
            "value": 0.03513416780026873,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1000000",
            "value": 35441.954666666665,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_eval_linear_sec_p6_x1000000",
            "value": 36611.326,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1",
            "value": 0.03453942575062948,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1000000",
            "value": 34895.36533333333,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1",
            "value": 0.03538608431636696,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1000000",
            "value": 35468.88166666666,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_std_counter_w32_x1000000",
            "value": 35356.861666666664,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1",
            "value": 0.0357102801512631,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1000000",
            "value": 35317.43133333333,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_gray_counter_w32_x1000000",
            "value": 35503.11633333333,
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
          "id": "1c00e87c78fbe6c21c4d495439d93a582bd9c581",
          "message": "Remove unnecessary cast flagged by clippy",
          "timestamp": "2026-03-07T11:54:17Z",
          "tree_id": "8f4484551df424cac5506ba3d1ba0094442d2a50",
          "url": "https://github.com/celox-sim/celox/commit/1c00e87c78fbe6c21c4d495439d93a582bd9c581"
        },
        "date": 1772885999254,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "verilator/simulation_build_top_n1000",
            "value": 6574051.025,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_linear_sec_p6",
            "value": 1109491.47,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_countones_w64",
            "value": 1050300.895,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_std_counter_w32",
            "value": 1053334.587,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_gray_counter_w32",
            "value": 1053351.162,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1",
            "value": 0.3376952226759396,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1000000",
            "value": 337222.716,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1",
            "value": 0.3396091669006207,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1000000",
            "value": 351756.01333333337,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1",
            "value": 0.035021258238178345,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1000000",
            "value": 35485.34300000001,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_eval_linear_sec_p6_x1000000",
            "value": 35386.45266666667,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1",
            "value": 0.03408691503600048,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1000000",
            "value": 34459.66166666667,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1",
            "value": 0.0334406897970338,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1000000",
            "value": 33348.073333333334,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_std_counter_w32_x1000000",
            "value": 33922.388999999996,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1",
            "value": 0.03432931236679608,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1000000",
            "value": 35015.53966666667,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_gray_counter_w32_x1000000",
            "value": 34675.338,
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
          "id": "1c00e87c78fbe6c21c4d495439d93a582bd9c581",
          "message": "Remove unnecessary cast flagged by clippy",
          "timestamp": "2026-03-07T11:54:17Z",
          "tree_id": "8f4484551df424cac5506ba3d1ba0094442d2a50",
          "url": "https://github.com/celox-sim/celox/commit/1c00e87c78fbe6c21c4d495439d93a582bd9c581"
        },
        "date": 1772894744262,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "verilator/simulation_build_top_n1000",
            "value": 6574051.025,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_linear_sec_p6",
            "value": 1109491.47,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_countones_w64",
            "value": 1050300.895,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_std_counter_w32",
            "value": 1053334.587,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_gray_counter_w32",
            "value": 1053351.162,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1",
            "value": 0.3376952226759396,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1000000",
            "value": 337222.716,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1",
            "value": 0.3396091669006207,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1000000",
            "value": 351756.01333333337,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1",
            "value": 0.035021258238178345,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1000000",
            "value": 35485.34300000001,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_eval_linear_sec_p6_x1000000",
            "value": 35386.45266666667,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1",
            "value": 0.03408691503600048,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1000000",
            "value": 34459.66166666667,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1",
            "value": 0.0334406897970338,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1000000",
            "value": 33348.073333333334,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_std_counter_w32_x1000000",
            "value": 33922.388999999996,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1",
            "value": 0.03432931236679608,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1000000",
            "value": 35015.53966666667,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_gray_counter_w32_x1000000",
            "value": 34675.338,
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
          "id": "b71d5fa481a9704008d413cdb84169eefd93b188",
          "message": "Fix .at() returning wrong values for non-byte-aligned array elements\n\nThe JIT stores all array elements bit-packed (element i starts at bit\ni*W), but createArrayDut assumed byte-aligned stride for elements >= 8\nbits wide. For widths not divisible by 8 (e.g. logic<34>[4]), this\ncaused a cumulative bit shift on every element after index 0.\n\nAdd readBitPackedWide/writeBitPackedWide for BigInt-based bit-level\narray access, and split createArrayDut into three paths: sub-byte,\nnon-byte-aligned (>= 8 bits), and byte-aligned.",
          "timestamp": "2026-03-07T14:55:05Z",
          "tree_id": "6a0ddef2b7369e4a710afc1b0bb9f7c1206d3b12",
          "url": "https://github.com/celox-sim/celox/commit/b71d5fa481a9704008d413cdb84169eefd93b188"
        },
        "date": 1772896716494,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "verilator/simulation_build_top_n1000",
            "value": 11167690.879,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_linear_sec_p6",
            "value": 1134150.484,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_countones_w64",
            "value": 1078342.283,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_std_counter_w32",
            "value": 1068791.353,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_gray_counter_w32",
            "value": 1053179.087,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1",
            "value": 0.34009667960871565,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1000000",
            "value": 341149.4776666667,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1",
            "value": 0.34223886408980103,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1000000",
            "value": 352490.995,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1",
            "value": 0.03505134146105367,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1000000",
            "value": 35291.977,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_eval_linear_sec_p6_x1000000",
            "value": 35423.80599999999,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1",
            "value": 0.03455445748178606,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1000000",
            "value": 34564.43933333333,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1",
            "value": 0.0759196645547509,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1000000",
            "value": 38421.59066666666,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_std_counter_w32_x1000000",
            "value": 39385.69633333333,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1",
            "value": 0.034946094019887394,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1000000",
            "value": 35182.40233333333,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_gray_counter_w32_x1000000",
            "value": 35011.48733333334,
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
          "id": "3acefb671902ffb2d55a66293508db14c5264ec7",
          "message": "Cache comb_func on JitBackend to avoid Arc dereference on hot path\n\neval_comb() is called on every tick. Accessing the function pointer\nthrough self.shared.comb_func requires an Arc pointer chase that\npenalizes performance (~8% locally, potentially worse under cache\npressure in CI). Copy the SimFunc pointer directly onto JitBackend\nat construction time.",
          "timestamp": "2026-03-07T15:48:17Z",
          "tree_id": "d1d1c439a60294d15f20663a261f3674424505b3",
          "url": "https://github.com/celox-sim/celox/commit/3acefb671902ffb2d55a66293508db14c5264ec7"
        },
        "date": 1772899924268,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "verilator/simulation_build_top_n1000",
            "value": 6502624.654,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_linear_sec_p6",
            "value": 1162666.463,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_countones_w64",
            "value": 1111302.119,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_std_counter_w32",
            "value": 1094816.223,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_gray_counter_w32",
            "value": 1104431.42,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1",
            "value": 0.34167000514286383,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1000000",
            "value": 340349.3670000001,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1",
            "value": 0.34149235732688543,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1000000",
            "value": 352432.64699999994,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1",
            "value": 0.0351916208679375,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1000000",
            "value": 35436.27333333334,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_eval_linear_sec_p6_x1000000",
            "value": 35528.50866666666,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1",
            "value": 0.034534725007101336,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1000000",
            "value": 34975.03666666667,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1",
            "value": 0.034572827714391866,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1000000",
            "value": 34570.33266666666,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_std_counter_w32_x1000000",
            "value": 34574.662333333334,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1",
            "value": 0.03494381030174843,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1000000",
            "value": 35053.04833333333,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_gray_counter_w32_x1000000",
            "value": 35087.42900000001,
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
          "id": "c03af0f45dbf2eab7f759d48d1500d6023bce397",
          "message": "Fix AsyncLow reset polarity in Verilator top_n1000 benchmark\n\nThe reset sequence had rst=1 (release) before rst=0 (assert), leaving\nthe counters in reset during the entire benchmark. This matches the\nsame bug that was fixed for the Celox Rust benchmarks in 20cb7a2.",
          "timestamp": "2026-03-07T16:55:34Z",
          "tree_id": "965a33644531d48055c3acce1c6a9e10127a5ab2",
          "url": "https://github.com/celox-sim/celox/commit/c03af0f45dbf2eab7f759d48d1500d6023bce397"
        },
        "date": 1772903992238,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "verilator/simulation_build_top_n1000",
            "value": 12695982.483,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_linear_sec_p6",
            "value": 1129643.675,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_countones_w64",
            "value": 1067482.051,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_std_counter_w32",
            "value": 1068464.817,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_gray_counter_w32",
            "value": 1072261.772,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1",
            "value": 0.29532071188480835,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1000000",
            "value": 295777.7506666666,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1",
            "value": 0.29596595019020316,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1000000",
            "value": 295751.585,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1",
            "value": 0.0352117669428569,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1000000",
            "value": 35363.76433333333,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_eval_linear_sec_p6_x1000000",
            "value": 35652.28633333334,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1",
            "value": 0.03456632217317816,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1000000",
            "value": 34671.691,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1",
            "value": 0.03452669843741434,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1000000",
            "value": 34481.255333333334,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_std_counter_w32_x1000000",
            "value": 34562.827333333335,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1",
            "value": 0.03518017853601984,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1000000",
            "value": 35451.616,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_gray_counter_w32_x1000000",
            "value": 35238.08000000001,
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
          "id": "d7529d58a27f5681d9ef8751d5a49c32b27d8e91",
          "message": "Share Veryl benchmark sources between Celox and Verilator benchmarks\n\nExtract wrapper module sources into benches/veryl/*.veryl and reference\nthem via include_str! from both simulation.rs and celox-bench-sv.\nThis guarantees the Celox JIT and Verilator benchmarks run identical\ncircuits compiled from the same Veryl source.\n\nAlso unify the LinearSec top module name to \"Top\" (was \"LinearSecTop\"\nin the Verilator bench only), matching the Celox Rust benchmarks.",
          "timestamp": "2026-03-07T18:11:59Z",
          "tree_id": "ecdb903854ea4d42d06881cf4994785e9f7f96aa",
          "url": "https://github.com/celox-sim/celox/commit/d7529d58a27f5681d9ef8751d5a49c32b27d8e91"
        },
        "date": 1772908578259,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "verilator/simulation_build_top_n1000",
            "value": 10117844.668,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_linear_sec_p6",
            "value": 1163805.219,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_countones_w64",
            "value": 1105509.272,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_std_counter_w32",
            "value": 1116454.017,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_gray_counter_w32",
            "value": 1104063.806,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1",
            "value": 0.3543942935769048,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1000000",
            "value": 354633.2303333333,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1",
            "value": 0.3546745268901311,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1000000",
            "value": 355450.4593333333,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1",
            "value": 0.03559146779255526,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1000000",
            "value": 34702.821333333326,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_eval_linear_sec_p6_x1000000",
            "value": 35220.781333333325,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1",
            "value": 0.034574276081296776,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1000000",
            "value": 35162.367666666665,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1",
            "value": 0.03585246375909516,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1000000",
            "value": 36059.293333333335,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_std_counter_w32_x1000000",
            "value": 36264.42,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1",
            "value": 0.03509780424381059,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1000000",
            "value": 35061.530666666666,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_gray_counter_w32_x1000000",
            "value": 35146.82299999999,
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
          "id": "0bd3099f2bfb1978b820362773a3ae7d94e154f4",
          "message": "Fix eval_constexpr to recursively evaluate param expressions\n\nWhen the Veryl analyzer marks compound expressions like `N - 1` as\nis_const=true but leaves the top-level value as Unknown, eval_constexpr\nreturned None. This broke constant folding in parse_if_statement (param-\nbased conditions not folded), parse_binary for Pow (param exponents\nrejected), and concatenation replication counts.\n\nRecursively evaluate Binary/Unary sub-expressions when the top-level\ncomptime value is missing, supporting arithmetic, bitwise, shift, and\ncomparison operators.",
          "timestamp": "2026-03-07T22:59:04Z",
          "tree_id": "56f11d3c48848207e5260ac40b901a45e89f63a5",
          "url": "https://github.com/celox-sim/celox/commit/0bd3099f2bfb1978b820362773a3ae7d94e154f4"
        },
        "date": 1772925813624,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "verilator/simulation_build_top_n1000",
            "value": 9120881.445,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_linear_sec_p6",
            "value": 1161274.925,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_countones_w64",
            "value": 1207065.278,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_std_counter_w32",
            "value": 1168273.552,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_gray_counter_w32",
            "value": 1219520.805,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1",
            "value": 0.35560952126686324,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1000000",
            "value": 354422.8956666666,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1",
            "value": 0.3557927701766133,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1000000",
            "value": 355175.9803333333,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1",
            "value": 0.03306073465106486,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1000000",
            "value": 35789.46833333334,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_eval_linear_sec_p6_x1000000",
            "value": 35484.218,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1",
            "value": 0.0345629816410489,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1000000",
            "value": 35034.52566666666,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1",
            "value": 0.035901435827338846,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1000000",
            "value": 35515.262,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_std_counter_w32_x1000000",
            "value": 35020.30333333334,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1",
            "value": 0.03498613261392444,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1000000",
            "value": 34993.16966666666,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_gray_counter_w32_x1000000",
            "value": 35059.666333333334,
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
          "id": "8f74385654dddb948f9a9cf6e599d76456038be9",
          "message": "Prevent use-after-free on DUT access after dispose\n\nshared_memory() passes a raw pointer to Vec<u64> via\nUint8Array::with_external_data with a no-op finalizer. When dispose()\ndrops the JitBackend, the Vec is freed but the JS ArrayBuffer/DataView\nremain alive, allowing DUT getter/setters to read/write freed memory.\n\nAdd a disposed flag to DirtyState and check it at the top of every DUT\naccessor (defineSignalProperty get/set, createArrayDut at/set for all\nthree paths). Set the flag BEFORE calling _handle.dispose() so the\nguard is active before the Rust memory is freed.",
          "timestamp": "2026-03-08T00:03:06Z",
          "tree_id": "6b6ec3238177dc7f8098542a9f829934d59ddf44",
          "url": "https://github.com/celox-sim/celox/commit/8f74385654dddb948f9a9cf6e599d76456038be9"
        },
        "date": 1772929643249,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "verilator/simulation_build_top_n1000",
            "value": 11926727.916,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_linear_sec_p6",
            "value": 1175183.852,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_countones_w64",
            "value": 1106782.572,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_std_counter_w32",
            "value": 1116444.408,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_gray_counter_w32",
            "value": 1122821.582,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1",
            "value": 0.35504965134801897,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1000000",
            "value": 354167.64,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1",
            "value": 0.355423318730564,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1000000",
            "value": 355153.7153333333,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1",
            "value": 0.03306647043007275,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1000000",
            "value": 33044.272000000004,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_eval_linear_sec_p6_x1000000",
            "value": 33508.72466666667,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1",
            "value": 0.0345949423175295,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1000000",
            "value": 35246.95,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1",
            "value": 0.03654678027325632,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1000000",
            "value": 36501.704000000005,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_std_counter_w32_x1000000",
            "value": 36433.19666666666,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1",
            "value": 0.03575203789839826,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1000000",
            "value": 35935.992333333335,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_gray_counter_w32_x1000000",
            "value": 36358.15666666666,
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
          "id": "5c568535f3416422e789135decda5a56a235b296",
          "message": "test: increase absolute time limits for sorter_tree CI\n\nCI servers are significantly slower than dev machines. Increase limits:\n- N=64: 60s → 120s\n- N=128: 180s → 360s\n\nThe ratio-based scaling tests are the real regression guards;\nabsolute time limits just prevent catastrophic blowup.",
          "timestamp": "2026-03-10T00:08:58Z",
          "tree_id": "4d299b9f61314e7d896f290ddca99160e3447598",
          "url": "https://github.com/celox-sim/celox/commit/5c568535f3416422e789135decda5a56a235b296"
        },
        "date": 1773102837382,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "verilator/simulation_build_top_n1000",
            "value": 21334552.275,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_linear_sec_p6",
            "value": 1171742.626,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_countones_w64",
            "value": 1092363.789,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_std_counter_w32",
            "value": 1131297.096,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_gray_counter_w32",
            "value": 1094358.621,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1",
            "value": 0.3555840106116566,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1000000",
            "value": 354685.434,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1",
            "value": 0.35597638167218026,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1000000",
            "value": 355711.241,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1",
            "value": 0.033078398948999145,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1000000",
            "value": 33016.094000000005,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_eval_linear_sec_p6_x1000000",
            "value": 32757.271333333327,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1",
            "value": 0.03456850495845002,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1000000",
            "value": 34956.993,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1",
            "value": 0.03458378619262492,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1000000",
            "value": 34609.55066666666,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_std_counter_w32_x1000000",
            "value": 34560.09133333333,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1",
            "value": 0.034987670457714816,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1000000",
            "value": 34974.82333333333,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_gray_counter_w32_x1000000",
            "value": 35062.72866666666,
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
          "id": "1cf64ff1f6060c361a0052a8fc396b51cac25b0a",
          "message": "style: cargo fmt",
          "timestamp": "2026-03-10T02:50:52Z",
          "tree_id": "789fadd993f9c630883a5e6b3f4e16125f2db617",
          "url": "https://github.com/celox-sim/celox/commit/1cf64ff1f6060c361a0052a8fc396b51cac25b0a"
        },
        "date": 1773112871224,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "verilator/simulation_build_top_n1000",
            "value": 12558196.561,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_linear_sec_p6",
            "value": 1199589.175,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_countones_w64",
            "value": 1107744.825,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_std_counter_w32",
            "value": 1173951.265,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_gray_counter_w32",
            "value": 1134370.235,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1",
            "value": 0.3567388479512559,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1000000",
            "value": 356715.35,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1",
            "value": 0.35737375750709666,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1000000",
            "value": 357506.7103333333,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1",
            "value": 0.03300314424789922,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1000000",
            "value": 32696.68633333333,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_eval_linear_sec_p6_x1000000",
            "value": 34394.58699999999,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1",
            "value": 0.03454839067242975,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1000000",
            "value": 35090.85833333334,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1",
            "value": 0.03502284042766204,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1000000",
            "value": 35809.12033333333,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_std_counter_w32_x1000000",
            "value": 35195.079666666665,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1",
            "value": 0.0353705399233271,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1000000",
            "value": 35454.779,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_gray_counter_w32_x1000000",
            "value": 35673.09766666666,
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
          "id": "ce4ef3314df71f871eceff94ba337ccd4a840636",
          "message": "Fix clippy manual_unwrap_or lint in ff.rs",
          "timestamp": "2026-03-10T18:27:56Z",
          "tree_id": "0b42ffc5654ef34d1899c4e8294d442269cb4b8f",
          "url": "https://github.com/celox-sim/celox/commit/ce4ef3314df71f871eceff94ba337ccd4a840636"
        },
        "date": 1773168790806,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "verilator/simulation_build_top_n1000",
            "value": 5537920.883,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_linear_sec_p6",
            "value": 1147213.174,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_countones_w64",
            "value": 1085494.885,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_std_counter_w32",
            "value": 1083071.679,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_gray_counter_w32",
            "value": 1081537.998,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1",
            "value": 0.35450763658675427,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1000000",
            "value": 354584.40566666663,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1",
            "value": 0.3553084971754643,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1000000",
            "value": 355652.6436666667,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1",
            "value": 0.03309210001824177,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1000000",
            "value": 33458.992333333335,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_eval_linear_sec_p6_x1000000",
            "value": 34592.06633333333,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1",
            "value": 0.03479145278108926,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1000000",
            "value": 34858.16599999999,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1",
            "value": 0.03450088765202563,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1000000",
            "value": 34601.723999999995,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_std_counter_w32_x1000000",
            "value": 34620.83933333334,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1",
            "value": 0.03507559289391111,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1000000",
            "value": 35149.804333333326,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_gray_counter_w32_x1000000",
            "value": 34976.299999999996,
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
          "id": "d77ad012b5282e8fc5790f128b895d3ac3836f79",
          "message": "Add optimization tuning guide and benchmark tool\n\n- Add docs/guide/optimization-tuning.md (EN) and docs/ja/ (JA) with\n  benchmark-backed guidance on SIRT pass interactions, Cranelift backend\n  options, and design-dependent behavior\n- Add pass_benchmark example for measuring per-pass compile/sim impact\n- Add sidebar entries in VitePress config\n- Link guide from CLAUDE.md optimizer section",
          "timestamp": "2026-03-10T19:23:33Z",
          "tree_id": "fcdafce122d42d04e3114380629ad5c1936d20a4",
          "url": "https://github.com/celox-sim/celox/commit/d77ad012b5282e8fc5790f128b895d3ac3836f79"
        },
        "date": 1773172108021,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "verilator/simulation_build_top_n1000",
            "value": 10753673.374,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_linear_sec_p6",
            "value": 1145987.197,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_countones_w64",
            "value": 1097554.226,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_std_counter_w32",
            "value": 1062704.304,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_gray_counter_w32",
            "value": 1065331.351,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1",
            "value": 0.3544259097263654,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1000000",
            "value": 353907.9206666667,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1",
            "value": 0.355208306914588,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1000000",
            "value": 355311.66266666667,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1",
            "value": 0.03300220881116341,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1000000",
            "value": 32684.390666666666,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_eval_linear_sec_p6_x1000000",
            "value": 34255.85733333333,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1",
            "value": 0.034555781197793865,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1000000",
            "value": 34959.90266666666,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1",
            "value": 0.034517759269971006,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1000000",
            "value": 34499.906,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_std_counter_w32_x1000000",
            "value": 34563.193333333336,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1",
            "value": 0.03510825021261909,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1000000",
            "value": 35233.53166666667,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_gray_counter_w32_x1000000",
            "value": 35263.718,
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
          "id": "43dde93cf46460d92f9c9bea8fa96f40bd3ffaa4",
          "message": "Add bench example files to .gitignore",
          "timestamp": "2026-03-11T02:13:10Z",
          "tree_id": "146a6f835fb3dd473780e89c89774f67c61af396",
          "url": "https://github.com/celox-sim/celox/commit/43dde93cf46460d92f9c9bea8fa96f40bd3ffaa4"
        },
        "date": 1773196664533,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "verilator/simulation_build_top_n1000",
            "value": 5876185.034,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_linear_sec_p6",
            "value": 1149911.33,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_countones_w64",
            "value": 1081750.44,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_std_counter_w32",
            "value": 1107029.432,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_gray_counter_w32",
            "value": 1074409.713,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1",
            "value": 0.35506600368817803,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1000000",
            "value": 354770.0576666667,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1",
            "value": 0.3561095958527725,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1000000",
            "value": 355277.8926666667,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1",
            "value": 0.03289875069580627,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1000000",
            "value": 34118.11,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_eval_linear_sec_p6_x1000000",
            "value": 33365.00566666667,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1",
            "value": 0.03510925895651572,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1000000",
            "value": 35116.19033333334,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1",
            "value": 0.03453924009543022,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1000000",
            "value": 34630.32266666667,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_std_counter_w32_x1000000",
            "value": 34666.996333333336,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1",
            "value": 0.03499437510824189,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1000000",
            "value": 35070.518333333326,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_gray_counter_w32_x1000000",
            "value": 35098.420333333335,
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
          "id": "c8d3bae50ae1ef6a0e4adbff0c6162aae08a8b93",
          "message": "Fix formatting (cargo fmt)",
          "timestamp": "2026-03-14T22:15:00Z",
          "tree_id": "1664e162817ac93ebcbbb18e6ce72b5db813dfa3",
          "url": "https://github.com/celox-sim/celox/commit/c8d3bae50ae1ef6a0e4adbff0c6162aae08a8b93"
        },
        "date": 1773528252377,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "verilator/simulation_build_top_n1000",
            "value": 6848171.133,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_linear_sec_p6",
            "value": 1170140.761,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_countones_w64",
            "value": 1107478.793,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_std_counter_w32",
            "value": 1096458.182,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_gray_counter_w32",
            "value": 1119733.038,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1",
            "value": 0.35445456199471315,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1000000",
            "value": 354126.1126666667,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1",
            "value": 0.3549879508598555,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1000000",
            "value": 356022.87266666663,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1",
            "value": 0.0330608034968279,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1000000",
            "value": 32809.82399999999,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_eval_linear_sec_p6_x1000000",
            "value": 34279.47666666666,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1",
            "value": 0.03465852026861647,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1000000",
            "value": 35481.38233333333,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1",
            "value": 0.03459945679175872,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1000000",
            "value": 34793.743,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_std_counter_w32_x1000000",
            "value": 34897.219,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1",
            "value": 0.03495874217981521,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1000000",
            "value": 35104.035333333326,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_gray_counter_w32_x1000000",
            "value": 35178.33,
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
          "id": "be71b640110460211405c51067bd7b3c4a40a9ca",
          "message": "Fix FF parser widening shift result by RHS width\n\nThe FF path used max(lhs_width, rhs_width) for shift/pow operations,\ncausing the result to be incorrectly widened when the shift amount\n(e.g. a 32-bit for-loop constant) was wider than the value being\nshifted. This matches the comb path which already uses LHS-only width\nper IEEE 1800-2023 §11.4.10.",
          "timestamp": "2026-03-17T00:40:23Z",
          "tree_id": "99362c5f19276293e0f62fd00af147e277202f7a",
          "url": "https://github.com/celox-sim/celox/commit/be71b640110460211405c51067bd7b3c4a40a9ca"
        },
        "date": 1773710130173,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "verilator/simulation_build_top_n1000",
            "value": 12107335.988,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_linear_sec_p6",
            "value": 1139787.098,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_countones_w64",
            "value": 1061108.933,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_std_counter_w32",
            "value": 1059187.834,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_gray_counter_w32",
            "value": 1073247.426,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1",
            "value": 0.35405087473525954,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1000000",
            "value": 356418.021,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1",
            "value": 0.35561926954671247,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1000000",
            "value": 355547.4483333333,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1",
            "value": 0.03367043092927459,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1000000",
            "value": 32388.071,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_eval_linear_sec_p6_x1000000",
            "value": 33826.487,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1",
            "value": 0.03454008169958074,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1000000",
            "value": 35095.63733333333,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1",
            "value": 0.03453507374355745,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1000000",
            "value": 34590.054,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_std_counter_w32_x1000000",
            "value": 34569.73266666666,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1",
            "value": 0.03505917599948844,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1000000",
            "value": 34958.02300000001,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_gray_counter_w32_x1000000",
            "value": 35062.725666666665,
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
          "id": "d4fafe4a3e0a8a2177145268e98fad53b233108c",
          "message": "Add verilator obj_dir and debug trace files to .gitignore",
          "timestamp": "2026-03-23T12:02:39Z",
          "tree_id": "f0c7fa2e0db8a165397d2366eb7a6ccea2c8bc51",
          "url": "https://github.com/celox-sim/celox/commit/d4fafe4a3e0a8a2177145268e98fad53b233108c"
        },
        "date": 1774268858633,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "verilator/simulation_build_top_n1000",
            "value": 10664703.768,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_linear_sec_p6",
            "value": 1143186.7,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_countones_w64",
            "value": 1084673.165,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_std_counter_w32",
            "value": 1088831.042,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_gray_counter_w32",
            "value": 1090995.658,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1",
            "value": 0.3545733259431274,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1000000",
            "value": 354437.5246666667,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1",
            "value": 0.3552544754320004,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1000000",
            "value": 355242.81533333333,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1",
            "value": 0.03348297354565083,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1000000",
            "value": 33008.299,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_eval_linear_sec_p6_x1000000",
            "value": 33150.77433333333,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1",
            "value": 0.034590275849606035,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1000000",
            "value": 35087.08866666666,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1",
            "value": 0.03468250013870587,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1000000",
            "value": 34684.34533333333,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_std_counter_w32_x1000000",
            "value": 35805.316666666666,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1",
            "value": 0.034997191141895916,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1000000",
            "value": 35006.26466666666,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_gray_counter_w32_x1000000",
            "value": 35229.27366666666,
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
          "id": "6b01130bda6cbb89e98aff018061ecbdd262d23a",
          "message": "Add ts-gen tests for duplicate scoped variables and generate-for\n\nVerify that TypeScript type generation handles modules with:\n- Duplicate-named scoped variables in always_comb for-loop scopes\n- Generate-for blocks with scoped variables in always_comb\n- Complex patterns combining scoped vars with for-loop instances",
          "timestamp": "2026-03-23T13:01:54Z",
          "tree_id": "c5a099565af6cf5437a9eacbebe62bd6b7bdecb1",
          "url": "https://github.com/celox-sim/celox/commit/6b01130bda6cbb89e98aff018061ecbdd262d23a"
        },
        "date": 1774272475477,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "verilator/simulation_build_top_n1000",
            "value": 6130274.747,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_linear_sec_p6",
            "value": 1130179.435,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_countones_w64",
            "value": 1071979.351,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_std_counter_w32",
            "value": 1064191.504,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_gray_counter_w32",
            "value": 1073344.907,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1",
            "value": 0.3563841419463459,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1000000",
            "value": 354156.03366666666,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1",
            "value": 0.35501485912079284,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1000000",
            "value": 355496.701,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1",
            "value": 0.03298126867381352,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1000000",
            "value": 32459.33033333333,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_eval_linear_sec_p6_x1000000",
            "value": 34399.49133333334,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1",
            "value": 0.03452490893838728,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1000000",
            "value": 34807.01933333334,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1",
            "value": 0.03456162408168062,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1000000",
            "value": 34499.338,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_std_counter_w32_x1000000",
            "value": 34694.025,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1",
            "value": 0.03498688082488575,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1000000",
            "value": 35004.65833333333,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_gray_counter_w32_x1000000",
            "value": 35127.645,
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
          "id": "5417c4031c8137cb416638da751fd0ef895e41c8",
          "message": "Fix clippy errors in celox crate",
          "timestamp": "2026-03-24T09:36:43Z",
          "tree_id": "9e22d82ae786a6f2f31acbca3bcdb1830f27c51f",
          "url": "https://github.com/celox-sim/celox/commit/5417c4031c8137cb416638da751fd0ef895e41c8"
        },
        "date": 1774346633192,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "verilator/simulation_build_top_n1000",
            "value": 11392370.934,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_linear_sec_p6",
            "value": 1189231.448,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_countones_w64",
            "value": 1160233.426,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_std_counter_w32",
            "value": 1153646.45,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_build_gray_counter_w32",
            "value": 1149788.434,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1",
            "value": 0.3596721894019808,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_top_n1000_x1000000",
            "value": 370028.8896666667,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1",
            "value": 0.3694466478242872,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_top_n1000_x1000000",
            "value": 361542.243,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1",
            "value": 0.033315357121138596,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_linear_sec_p6_x1000000",
            "value": 32898.206666666665,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_eval_linear_sec_p6_x1000000",
            "value": 33168.039,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1",
            "value": 0.03464227018581836,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_eval_countones_w64_x1000000",
            "value": 35136.61266666667,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1",
            "value": 0.03462457439661244,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_std_counter_w32_x1000000",
            "value": 34627.414333333334,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_std_counter_w32_x1000000",
            "value": 34719.121666666666,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1",
            "value": 0.035061752778243395,
            "unit": "us"
          },
          {
            "name": "verilator/simulation_tick_gray_counter_w32_x1000000",
            "value": 35070.534,
            "unit": "us"
          },
          {
            "name": "verilator/testbench_tick_gray_counter_w32_x1000000",
            "value": 35045.89833333334,
            "unit": "us"
          }
        ]
      }
    ]
  }
}