window.BENCHMARK_DATA = {
  "lastUpdate": 1772355882999,
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
      }
    ]
  }
}