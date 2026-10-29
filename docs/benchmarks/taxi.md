# Taxi / cocotb comparison

The **Taxi / cocotb** tab on the [benchmark dashboard](./index.md) compares Celox
with Verilator using unmodified, self-checking upstream SystemVerilog designs
and Python testbenches. The `Taxi Benchmark` workflow runs daily at 03:17 UTC
(12:17 JST) and supports manual dispatch. Only a successful scheduled or manual
`master` run publishes history.

The [manifest](https://github.com/celox-sim/celox/blob/master/benchmarks/taxi/manifest.json)
pins [fpganinja/taxi](https://github.com/fpganinja/taxi) to
`cc70b270b910d369ab1ad7b3855e76399fd461f1`, including sources, tops, parameters,
expected test counts and random seed 12345. The Python requirements are pinned;
CI uses Ubuntu 24.04, Python 3.13, cocotb 2.0.1 and Verilator 5.052.

| Configuration | Design | Tests per simulator per sample |
| --- | --- | ---: |
| lfsr64 | 64-bit self-synchronous LFSR descrambler | 1 |
| phy64 | 64-bit 10G Ethernet PHY | 3 |
| rx16 | 16-bit BASE-X receive, gearbox/SGMII/PTP enabled | 78 |
| rx8 | 8-bit BASE-X receive, gearbox/SGMII/PTP enabled | 78 |
| rx32 | 32-bit BASE-R receive, gearbox/USXGMII/PTP enabled | 312 |

These are five fixed configurations, totaling **472 tests per simulator per
sample**, rather than the entire Taxi project's parameter matrix. Each daily
run executes three samples of each configuration for each simulator.

## Measurement

Each configuration gets one VM. Both engines compile and run sequentially on the same logical
CPU, alternating Celox/Verilator then Verilator/Celox. The third pair runs in the
first order again. Different configurations can use different CPU models. CPU
model, runner, boot ID, tool versions, CLI binary hash, source hashes, parameters,
commands, logs, XML and every raw timing are retained in the `taxi-*` artifacts
for 30 days. The dashboard separates CPU models.

The charts show three distinct measurements:

- **Test body:** median of three sums of cocotb XML test times. This includes
  Python stimulus, resets, simulator interaction and assertions. A shared GPI
  startup hook substitutes `perf_counter()` only for the pinned regression
  manager's elapsed-time clock, retaining cocotb's measurement boundaries and
  XML writer. Other Python clocks and upstream tests are unchanged. This prevents
  wall-clock corrections from corrupting elapsed times. It measures
  the usable cocotb workload, including Python overhead.
- **Whole process:** median of three fresh executable processes, from spawn to
  exit. Celox's Python discovery and result checks are inside this interval;
  Verilator's Python environment is configured beforehand. Both retain Python
  initialization and simulator startup. Compiling the RTL is excluded.
- **HDL compilation:** one fresh build per engine, before any runtime samples.
  Celox includes frontend analysis, native code generation and attached executable
  writing. Verilator includes RTL-to-C++ generation and serial C++ compilation.
  C++ compiler caching is disabled. Building Celox itself and installing tools
  are excluded.

Celox uses native O1 and two-state execution. Verilator uses cocotb's standard
`--vpi --public-flat-rw` adapter and its default C++ `-Os` setting. Tracing is off
for both. RX32 explicitly uses `1ns/1fs` in both builds. Diagnostic relaxations
are explicit in the runner. Both simulators receive the same unchanged source
bytes and parameter values.

Every sample must contain the expected number of unique, passing tests. Skips,
missing XML, duplicate identities, invalid timings, nonzero exit, timeout, or
unequal per-test simulated durations fail the run. Any failed configuration
prevents the daily set from publishing. A failed run cannot add zero or partial
performance points. Prior successful history remains visible with its original
date. The raw report also records the median of paired Celox/Verilator body-time
ratios; values below one favor Celox. This differs from dividing the two medians.
Shared CI machines introduce noise; repeat small differences before drawing
performance conclusions.

## Run locally

```bash
python3.13 -m venv work/taxi-venv
work/taxi-venv/bin/pip install -r benchmarks/taxi/requirements.txt
cargo build --locked --release -p celox-vpi --features systemverilog --bin celox
work/taxi-venv/bin/python scripts/run-taxi-bench.py prepare work/taxi-source
work/taxi-venv/bin/python scripts/run-taxi-bench.py run phy64 work/taxi-source target/release/celox work/taxi-phy64
```

Install Verilator 5.052 and a C++ compiler first. Choose a fresh output directory
for every run; existing results are never reused as a new measurement. Use
`--samples 1` for local integration checks; daily publication requires all three
samples. Results from a locally modified checkout are diagnostics, not published
benchmark evidence.
