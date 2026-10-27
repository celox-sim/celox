# Circuit benchmark collection

The main dashboard measures complete hardware workloads. API calls, counters,
and standard-library microbenchmarks remain available in the
[diagnostic dashboard](./diagnostics.md).

| Design | Workload | Role |
| --- | --- | --- |
| Heliodor | Pinned Linux boot workloads | Large CPU systems and startup |
| rica | Self-checking ELF kernels with board memory | CPU and dual-clock memory |
| logikbench SHA-256 | 8,192 checked hashes | Cryptographic datapath |
| logikbench FFT | 3,072 checked 64-point frames | Fixed-point signal processing |
| logikbench LZ77 | 2,944 reconstructed bytes | Compression and history memory |
| tarunes | Headless video/audio workload under qualification | CPU, video and audio together |

Heliodor retains its [existing comparison suite](./heliodor.md). rica retains
`scripts/run-rica-bench.sh`; its results are currently local, not a published
collection history. tarunes has no accepted performance samples yet: its synthetic-ROM
qualification probe agrees with Verilator on CPU, PPU and audio, but HDMI RGB differs.
The paired Veryl/SV probes and reproduction instructions live in `benchmarks/tarunes/`.
The collection does not combine these designs into an overall score.

## Reproduce the representative circuits

The machine-readable selection, source revisions, testbench provenance and
workload recipes are in `benchmarks/collection.json`. The logikbench Veryl port
is pinned to `6179df1cd15e13600a36621fc6b57de521a93d9f`.

```sh
cargo build --release --locked -p celox-vpi --bin celox
node scripts/benchmark-collection.mjs list
node scripts/benchmark-collection.mjs prepare all work/circuits/sources
node scripts/benchmark-collection.mjs qualify all work/circuits/sources work/circuits/qualification
node scripts/benchmark-collection.mjs measure all work/circuits/sources work/circuits/measurement
```

`all` selects implemented collection runners; `list` also shows existing standalone
runners and pending candidates. A comma-separated selection such as `sha256,fft`
is supported. Use a fresh output directory for each invocation. The last optional
argument to `qualify` or `measure` selects a prebuilt Celox binary; the default is
`target/release/celox`. Debug runs are useful for validation, not release comparisons.
The tool does not build Cargo projects or create `target/` itself.

Each run verifies the pinned checkout and materializes tracked project bytes in
a separate directory. The originals stay unchanged. It first executes the
upstream self-checking TB, then corrupts an expected result in a separate working
copy and requires an assertion failure. Exit zero alone is insufficient. A
compile error, timeout, signal, missing timing record or incomplete workload
cannot become a performance sample. Qualification timings are never published.

Measurement repeats each complete upstream scenario inside one simulation:

- SHA-256: 4,096 pairs of the `abc` and empty-message known-answer tests.
- FFT: 1,024 groups of the DC, Nyquist and bin-16 exact fixed-point tests.
- LZ77: 16 groups of the periodic, small-alphabet and LFSR streams. Each phase
  stops once its expected byte count is received; the original cycle limit and
  every byte comparison remain. This avoids measuring the smoke TB's idle tail.

Resets, stimulus and assertions are included in execution time. Every measured
process must complete the entire fixed workload and print its completion marker.
Three fresh processes run sequentially; the dashboard publishes the median for
compilation, execution and total time separately. Total starts before design
analysis and excludes building or launching the CLI, matching the runner's
reported timing boundary. Process wall time is retained separately. Build caches
and waveform output are not enabled.

`results.json` retains the source revision and content hash, TB hash, work count,
frontend, backend, state model, optimization level, thread count, host CPU and
architecture, exact runner binary hash, logs, and individual samples. A changed
source, TB or workload contract starts a new chart. The Circuit benchmarks workflow
runs daily at 04:17 UTC and on manual dispatch. Only successful scheduled/manual
runs on master publish history; failed logs are retained as artifacts.

## SystemVerilog frontend phase

A circuit, its stimulus/testbench, its source frontend and its execution backend
are separate identities. A Verilator run over emitted SV does not itself test
Celox's SV frontend. Future comparisons should use one pinned SV DUT/TB set for
both Celox SV and Verilator, and retain equivalent reset, seed, completion and
output-check contracts.

The catalog records existing upstream SV TB paths now, including the original
logikbench repository pinned to `e8c7c4388c66302c6adb235678512ce62b79968a`:

- `logikbench/benchmarks/{blocks/sha256,blocks/fft,large/lz77}/testbench/`
  contains the original self-checking Verilog TBs. Some report `FAILED` and still
  call `$finish`; adapters must require `PASSED` and reject failures explicitly.
- The Veryl port's `equiv/equiv_tb.v` files instantiate both the original Verilog
  and the emitted SV DUT. They are equivalence checks, not single-DUT performance
  measurements.
- The original and ported FFT/SHA TBs do not use identical inputs or oracles.
  Their results must not be joined as a frontend-only comparison without a common
  workload contract.
- tarunes has `src/tb_top.sv`, but it merely prints PC after a fixed delay and
  enables waveform dumping. It needs a completion/output oracle before adoption
  as a published performance workload.

The candidate selection is based on circuit diversity, before measuring speed.
An unsupported or incorrect run remains a visible qualification issue, never a
zero-time result or an omitted slow sample.
