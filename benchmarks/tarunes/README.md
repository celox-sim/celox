# tarunes qualification probe

This candidate is **not qualified** and is not included in the performance runner.
The original MIT-licensed ROM generator exercises CPU, PPU, HDMI and APU without
commercial ROMs. Paired headless Veryl and SV testbenches run three NES frames
(268,029 clocks after reset), check output hashes and enforce a cycle limit.
The upstream `src/tb_top.sv` lacks an output oracle, so it is recorded in the
catalogue but not used to accept performance results.

At tarunes revision `5aeb5048cc82a52ef3ec15489970a51537965a35`, Veryl 0.22.0
emitted SV run with Verilator produces:

| Output | Reference hash |
| --- | --- |
| PPU pixel indices | `745d1730` |
| HDMI RGB | `f11b12d6` |
| Audio samples | `76363624` |
| I2S data XOR HDMI DE | `85e85e85` |
| Combined output | `e8ccbe50` |

The final CPU PC is 32867. Celox native agrees on PC, PPU, audio and I2S/DE,
but gives HDMI `144fb851` with this probe. The first RGB difference in an
instrumented run is at step 61882. Both O0 and O2 reproduce the discrepancy;
its cause is still unconfirmed. The expected assertions remain unchanged and
Celox fails qualification. A successful timing line must never override that
failure.

## Reproduce

Run from the repository root, with `veryl` **0.22.0** and Verilator on PATH.
Use a fresh directory. Cargo builds must follow the repository's build-output
rules; these commands use an already-built release CLI.

```sh
git clone https://github.com/tarusake/tarunes work/tarunes-source
git -C work/tarunes-source checkout --detach 5aeb5048cc82a52ef3ec15489970a51537965a35
mkdir -p work/tarunes-probe
cp work/tarunes-source/Veryl.toml work/tarunes-probe/
cp -r work/tarunes-source/src work/tarunes-probe/
cp benchmarks/tarunes/frames.veryl work/tarunes-probe/src/
node benchmarks/tarunes/rom.mjs work/tarunes-probe/src
node benchmarks/tarunes/rom.mjs work/tarunes-probe
RUST_MIN_STACK=33554432 target/release/celox run \
  --project work/tarunes-probe --top benchmark_frames --backend native \
  --report --unlimited --two-state --threads 1 -O O2
```

Celox currently needs the larger compiler thread stack for this design. With
the default stack it overflows before simulation. This is a separate unresolved
qualification issue, not a reason to alter the golden outputs.

Then run the independent SV reference:

```sh
(cd work/tarunes-probe && veryl build --out-dir generated)
(cd work/tarunes-probe && verilator --binary --timing \
  --top-module benchmark_frames -Wno-fatal -j 2 --Mdir obj_benchmark \
  -f generated/tarunes.f ../../benchmarks/tarunes/frames.sv)
(cd work/tarunes-probe && ./obj_benchmark/Vbenchmark_frames)
```

Require exit zero and `COLLECTION_REFERENCE_PASS frames-v1` from the SV run.
The duplicate ROM files accommodate the native TB's source-relative paths and
the SV simulator's working-directory-relative paths. This reference checks
emitted SV; it does not constitute Celox SV frontend coverage. Reusing this SV
TB with a pinned emitted DUT is a possible next-phase comparison after the
qualification discrepancy is resolved.
