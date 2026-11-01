# RTLMeter

[Open the RTLMeter dashboard](./index.md?tab=rtlmeter).

The comparison runs the unmodified **XuanTie-E902**, **XuanTie-E906** and
**HummingbirdV2-E203** `hello` programs from [RTLMeter](https://github.com/verilator/rtlmeter), fixed at commit
`4fdd6c3fd154e9b99d3a9d713cf3f4977e284f94`. All tools receive the same ordered
source list, headers, defines, testbench and firmware (`case.pat` for E902/E906;
`program.hex` for E203). E203 uses the upstream `default` configuration, including
its `#1` flip-flop delays, with assertions enabled and standard shared-variable
semantics. Input hashes, tool
versions, executable hashes and raw output are retained with each run.

## State models

2-state and 4-state have separate charts and qualification tables. Every
supported configuration must first pass a probe for uninitialized logic and
an undriven wire. The 2-state probe expects zero; the 4-state probe expects X/Z.

| Simulator | 2-state | 4-state |
| --- | --- | --- |
| Celox native O2, one thread | `--two-state` | `--four-state` |
| Verilator 5.052, C++ -O3, one build worker | `--no-fourstate --x-initial 0 --x-assign 0` | `--fourstate`, experimental |
| xezim, stable release pinned by Nix | Unsupported: no global 2-state mode | Default VM, persistent caches disabled |

Verilator documents `--fourstate` as experimental and for developer use.
See its [option reference](https://verilator.org/guide/latest/exe_verilator.html#cmdoption-fourstate).
xezim's [zero-initialization settings](https://github.com/aionhw/xezim/blob/0.11.4/docs/command-line.md)
change initial values, rather than selecting
2-state semantics; they are not used as a substitute.

## Measurement and acceptance

Each point is the median of three fresh **cold source-to-finish** measurements.
The timer includes HDL parsing, elaboration, compilation, startup and execution.
For Verilator it includes both C++ generation/build and the resulting executable.
Every sample uses a fresh build directory; persistent simulator/compiler caches
are disabled. Temporary C++ artifacts are deleted after each measurement.
Simulator installation and building Celox itself are excluded.
These short programs measure iteration latency, **not steady-state simulation
throughput**.

All configurations for one design run sequentially on the same VM and logical
CPU, with tool order rotated between repetitions. Different designs may run on
different hosts. Compare results within one design, state model and host.

A successful sample requires a zero exit status, the expected program output
and success marker exactly once, no failure marker, and a positive final
RTLMeter clock count. E902/E906 print `Hello Friend!` and `TEST PASSED`; E203
prints `Hello World from HummingbirdV2 E203` and `TEST_PASSED`. Counts must be stable across repetitions and differ by at most one clock
between tools, allowing for `$finish` and the counter on the same active edge.
A separate one-cycle negative control must stop before program success.
All tools use the same upstream `+max_cycles` limit (16,384 for E902/E203; 32,768
for E906), so a stalled UART or CPU cannot count as a completed workload.
Unsupported modes, probe failures, simulation failures, timeouts and clock
mismatches appear in the table and contribute no performance points.

The **RTLMeter Benchmark** workflow runs daily at 04:37 UTC and by manual
dispatch. Only scheduled/manual `master` runs publish history. Raw logs remain
available in workflow artifacts for 30 days.

## Reproduce

Build the CLI in the repository development shell and use the xezim shell:

```sh
cargo build --locked --release -p celox-vpi --features systemverilog --bin celox
mkdir -p work/rtlmeter-bin
cp target/release/celox work/rtlmeter-bin/celox
python -m venv work/rtlmeter-python
work/rtlmeter-python/bin/pip install -r benchmarks/rtlmeter/requirements.txt
work/rtlmeter-python/bin/python scripts/run-rtlmeter-bench.py prepare work/rtlmeter-source
nix develop .#xezim --command work/rtlmeter-python/bin/python scripts/run-rtlmeter-bench.py run e902 work/rtlmeter-source work/rtlmeter-bin/celox work/rtlmeter-report
```

Repeat with `e906` or `e203` and a fresh output directory. Local dirty-checkout results
are useful for investigation and cannot be published as release measurements.
