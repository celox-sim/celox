# Rica native testbench

`native.veryl.in` is the runtime stimulus and oracle for the pinned Rica
`HardwareCoreTestTop`. `celox-rica --emit-native-tb DIRECTORY` reads the ELF and
board profile, then emits a standalone Veryl project per kernel. The source DUT
is unchanged. Rust only prepares the project; clock generation, byte writes to
board memory, CPU execution and the `tohost == 1` assertion run in the Veryl TB.
Independent TB processes continually wait on each clock so both domains advance
on both runtimes, including during ELF loading. Both a full 64-bit mailbox store and two 32-bit stores are supported. Exhausting
the cycle budget is an assertion failure.

`scripts/run-rica-bench.sh run` uses this path by default (`RICA_HARNESS=tiered`) and compares
Celox tiered with the Veryl simulator’s asynchronous C backend. Both receive the
same generated project, and each must pass the kernel and reject a broken mailbox
assertion before measurement. `RICA_HARNESS=native-tb` retains Celox-only native
execution for diagnostics.
Each repetition invokes `celox run` in a fresh process with a cold tiered backend. Celox uses two-state simulation and O2; Veryl
uses its default Cranelift/C tiering. Simulation uses one thread and compiler
parallelism follows each implementation’s defaults on the same host. `CELOX_TB_RUNNER_BIN` selects that CLI;
`CELOX_RUNNER_BIN` selects the ELF-to-project exporter. `VERYL_TIMED_RUNNER_BIN` selects
`veryl-heliodor`, the shared Veryl 0.22.0 simulator runner. Paired logs, hashes,
qualification outcomes and individual samples are stored in `comparison/results.json`.
These Rica results remain local; the circuit publication workflow covers SHA, FFT and LZ77. The old `resolved` and
`rica` host harnesses remain available for API diagnostics.

The paired workload contract is `board-kernel-tiered-tb-v1`, with
`cold-tiered-process-v1` timing. `processNs` measures launch through exit and is
the primary comparison. Both runtimes may finish before promotion; the actual
tiered dispatch statistics are retained. Each process gets a fresh AOT cache.
The Celox-only native diagnostic uses `board-kernel-native-tb-v2` (explicit clock drivers). `compile_ns` is separate
from `execute_ns`; execution includes reset, loading the ELF bytes into board
memory, stimulus, scheduling and assertions. `jit_execute_ns` only counts time
inside generated DUT functions (with instrumentation overhead). Old host-loop
measurements excluded memory loading and reused a compiled model, so they must
not be combined with this series. A completion marker and a passing test result
are both required; failed or incomplete runs are not performance samples.

The TB is compiled to Celox testbench bytecode and executed by its Rust runtime;
`native` denotes the DUT backend, not machine-code compilation of the whole TB.
For a future SystemVerilog frontend comparison, preserve this workload's ELF,
clock relation, memory writes and mailbox assertion when adopting an SV TB.
This template is not a claim that an equivalent SV TB has already been qualified.
