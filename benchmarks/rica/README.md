# Rica native testbench

`native.veryl.in` is the runtime stimulus and oracle for the pinned Rica
`HardwareCoreTestTop`. `celox-rica --emit-native-tb DIRECTORY` reads the ELF and
board profile, then emits a standalone Veryl project per kernel. The source DUT
is unchanged. Rust only prepares the project; clock generation, byte writes to
board memory, CPU execution and the `tohost == 1` assertion run in the Veryl TB.
Both a full 64-bit mailbox store and two 32-bit stores are supported. Exhausting
the cycle budget is an assertion failure.

`scripts/run-rica-bench.sh run` uses this path by default (`RICA_HARNESS=native-tb`).
Each repetition invokes `celox run` in a fresh process with the native backend,
two-state simulation and O2 by default. `CELOX_TB_RUNNER_BIN` selects that CLI;
`CELOX_RUNNER_BIN` selects the ELF-to-project exporter. The old `resolved` and
`rica` host harnesses remain available for API diagnostics.

The workload contract is `board-kernel-native-tb-v1`. `compile_ns` is separate
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
