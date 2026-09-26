# veryl-test-suite

A reusable corpus of 648 Veryl language tests for compiler and simulator
implementations. Sources, input sequences, and assertions live together in this
crate. The default dependency graph contains numeric support and Veryl standard
library sources, with no Celox, parser, or simulator dependency.

## Use from another project

Until this crate is published, use a path dependency on this directory, or a Git
dependency on the revision containing it:

```toml
[dev-dependencies]
veryl-test-suite = { path = "../celox/crates/veryl-test-suite" }
```

Implement `Backend` for a wrapper around your simulator. A compiler factory
receives `Design` (source files, top module, and two/four-state mode) and returns
a fresh `Box<dyn Backend>`. The four required operations are `write`, `read`,
`eval_comb`, and `tick`; the shared driver handles integer conversions, batched
writes, and stable signal handles.

```rust
use veryl_test_suite::{Category, Factory, case, cases};

fn check_operators(compile: &mut Factory<'_>) {
    for case in cases().filter(|case| case.category == Category::Operators) {
        case.run(compile);
    }
}

fn reproduce_one(compile: &mut Factory<'_>) {
    case("operators::test_bitwise_operations").unwrap().run(compile);
}
```

A factory can capture compiler configuration or connections to an external
process. Integrate each `TestCase` into your preferred test harness using its
stable `group::test_name` identifier. Failed assertions panic; compiler and
adapter errors also fail simulation tests. Eight cases instead specify
`Expectation::CompilationError`: their designs must be rejected by the compiler.
Factories must distinguish language diagnostics from unavailable tools or other
infrastructure failures. The crate never silently ignores cases.
`tests/external_adapter.rs` is an executable example using a small independent
behavioral model and verifies that incorrect results are rejected.

## Adapter contract

- All signal values are arbitrary-width unsigned bit patterns (`BigUint`).
  Signed scalar writes preserve the scalar's two's-complement representation.
- Four-state values use `(payload, mask)`: 0 = `(0,0)`, 1 = `(1,0)`,
  X = `(1,1)`, Z = `(0,1)` per bit. Translate your simulator's encoding at the
  adapter boundary. Two-state reads return a zero mask.
- Use the compiled signal's width to truncate/extend writes. Array element zero
  occupies the least-significant bits; successive elements occupy higher bits.
- `write` stages a value without settling. `eval_comb` settles all pending
  changes. `read` observes settled values, including after a direct write.
  `tick` executes the named event once and settles its consequences. Event names
  can refer to clocks or asynchronous resets; use the declaration's polarity.
- Start each design with fresh storage: zero for two-state signals and X for
  four-state signals. Initial combinational outputs must be readable before an
  input is written. Generic clock/reset types use Veryl's defaults (positive
  edge / asynchronous active-low reset).
- `SignalPath` distinguishes top-level names from hierarchical instance paths
  and includes each instance-array index. Internal signals are observable in
  some cases; do not optimize away required observations.

These are the stimulus conventions inherited from the original test suite.
Simulator agreement alone does not establish language conformance. A compiler
that deliberately uses different initialization or scheduling must account for
that in its runner, with explicit exclusions and reasons.

The two aliased function-output cases accept either copy-out order: IEEE
1800-2023 specifies blocking copy-out but does not order different output
formals. Each case also checks named binding using distinct destinations with
exact expected values. Their legacy case IDs mentioning argument/source order
are retained for compatibility; they no longer require that order.

The `$bits` signedness and function-input width cases use the checked SV
requirements as their oracle, even where current Veryl or Celox produces a
different result. Those implementations fail the shared assertions; the suite
does not substitute their current values. Celox's test harness ignores only
the affected backend variants until their implementations are fixed, with the
reasons and observed failures retained in [the review](MISMATCH_REVIEW.md#2-bits-and-function-argument-sizing-require-the-sv-results).
The passing SV frontend variant and both external simulators remain enabled.

## Corpus organization

| Category | Coverage |
| --- | --- |
| `Combinational` | assignments, dependencies, procedural observations |
| `Operators` | arithmetic, comparisons, shifts, concatenation, wide values |
| `Types` | signedness, casts, context widths, enums, structs, parameters |
| `Arrays` | literals, indexing, writes, nonblocking array assignments |
| `Hierarchy` | instances, generics, packages, interfaces |
| `Sequential` | clocks, resets, flip-flops, counters, nonblocking assignments |
| `FourState` | X/Z propagation, comparisons, arithmetic and storage |
| `Functions` | function arguments and system functions |
| `ControlFlow` | branches, loops, dynamic bounds |
| `StandardLibrary` | RAM, FIFO, codecs, delays, edge detection, muxes, LFSR |
| `Regression` | complete reproductions of previously failing designs |

A case's category is its main topic, not a complete capability list. For
example, a sequential case may request four-state mode through `Design`.
The corpus was extracted against Veryl 0.21.0; the optional emitter and the
standard library dependency follow this workspace's Veryl version.

`src/cases/` contains the canonical cases, grouped by their original topics.
Add cases to a group's private `cases!` declaration with optional `@setup`, a
`@build Design::new(...)`, and ordinary Rust assertions using the shared driver.
For an invalid design, put `@expect reject;` before `@build` and omit simulation
assertions. `TestCase::expectation` lets consumers select these separately.
For a new group, add it to `GROUPS` in `src/cases/mod.rs`. Keep implementation
specific optimization, diagnostics, tracing, runtime-event, and API tests in
the implementing project. Celox retains its original test names, backend matrix,
and known exclusions in `crates/celox/tests`; those tests now call this corpus.

## Independent verification

The optional `verilator` and `icarus` features provide reusable process adapters
and CLI runners. Both compile Veryl to SystemVerilog and run the **shared
assertions against the external simulator**, or verify compilation rejection. No expected outputs are recorded
from Celox. Compiler diagnostics are not suppressed and emitted SV is not
rewritten to fit a simulator.

```sh
cargo run -p veryl-test-suite --features verilator --bin verify-verilator -- \
  --jobs 8 --output /tmp/veryl-suite-verilator \
  --report crates/veryl-test-suite/verification/verilator.json
cargo run -p veryl-test-suite --features icarus --bin verify-icarus -- \
  --jobs 8 --output /tmp/veryl-suite-icarus \
  --report crates/veryl-test-suite/verification/icarus.json
python3 crates/veryl-test-suite/scripts/summarize.py

# Reproduce one case; omit --report to preserve the complete retained report:
cargo run -p veryl-test-suite --features icarus --bin verify-icarus -- \
  --filter four_state::test_four_state_initial_and_set --jobs 1

# Recheck an ignored discrepancy against its unchanged assertions:
cargo run -p veryl-test-suite --features verilator --bin verify-verilator -- \
  --include-ignored --filter signed_divrem::signed_divrem_i64 --jobs 1

# Verify the adapters themselves (requires both tools):
cargo test -p veryl-test-suite --all-features --test oracles -- --ignored
```

Normal verification excludes the reviewed limitations and succeeds for the
retained tool versions. New failures still produce a nonzero exit. Run the two
commands independently so a failure in one does not prevent the other running.

Both require a C++ compiler and GNU `timeout` on `PATH`. Verilator additionally
needs GNU make; Icarus needs `iverilog`, `iverilog-vpi`, and `vvp`. The Nix dev
shell includes both simulators. Verilator honors `OBJCACHE=ccache` when ccache
is installed, which speeds up repeated model builds. The `emit` feature alone
exposes the shared Veryl-to-SystemVerilog helper. No optional feature adds a
Celox dependency.

The runner distinguishes `passed`, `mismatch`, `emission_error`,
`compile_error`, `runtime_error`, `unsupported`, and `ignored`, plus `rejected` and
`unexpected_accept` for negative cases. `passed` means every simulation assertion
succeeded; `rejected` means the HDL compiler rejected a negative case. An emitter
panic, missing tool, timeout, or C++ harness failure is not successful rejection.
Only assertion disagreements are mismatches. Every status other than
`passed`/`rejected`/`unsupported`/`ignored` makes the command exit nonzero. Verilator
reports four-state designs as unsupported; Icarus runs their X/Z assertions.

The external runners default to ignoring ten reviewed conformance discrepancies:
Verilator's read-before-write scheduling and signed 64-bit division overflow,
its acceptance of seven invalid dynamic output connections, and Icarus's
input-port expression width. These are exclusions for the affected tool only;
`TestCase::run` and the excluded cases' expectations remain unchanged. An `ignored`
case is not compiled or executed, and is never counted as a pass. Its console
message and JSON record explain the reason; `known_issue` metadata retains the
checked SV clauses, observed tool version, and upstream links or retained local
observations. No direct upstream decision was found for the seven dynamic
output connections; their pre-exclusion failures are retained. Icarus's source
comment is recorded separately from a direct developer response, which was not
found. See the [upstream findings](MISMATCH_REVIEW.md).

An additional [reviewed limitation manifest](LIMITATIONS.md) lists exact case
IDs blocked by Veryl emission, simulator support, a known compiler timeout, or
a runtime crash. Those rows also report `ignored`, with their original stage,
category, reason, and retained diagnostic evidence. This is not a blanket
exception for compilation or runtime errors. No new case is excluded merely
because it fails, and these limitations do not assert a standards violation.

Two-state zero division follows Verilator's zero result. Icarus's four-state
execution of the emitted `logic` ports produces X and cannot validate that
two-state contract, so that case is excluded on Icarus only. The separate
four-state zero-divisor case requires all X in accordance with SV 11.4.3.

`--include-ignored` executes these cases normally and retains that metadata.
Any resulting mismatch or other failure makes the command fail; a corrected
tool can pass. Use this option when checking a new tool version, then remove
resolved exclusions from `src/verification/known_issues.rs`. These exclusions
are not version-gated promises about future releases.

Builds have a 120-second timeout and each simulator has a 30-second timeout.
The adapters stage the first input batch before evaluation, settle pending
writes before reads, use declared event polarity, restore the driven event
level after a tick, and flatten array element zero into the least-significant
bits. Icarus runs in four-state mode. For two-state designs it initializes
storage to zero, but rejects a read if execution subsequently produces X/Z;
silently converting such a result to zero would hide an unverified expectation.
Icarus uses `-g2012 -gstrict-expr-width` (see its
[official flag documentation](https://steveicarus.github.io/iverilog/usage/command_line_flags.html)).

Each output folder retains Veryl sources, emitted `.sv`, build/runtime logs,
protocol transcripts, full panic diagnostics, and per-case results. The runner
also writes `results.json`. `--report` writes a portable copy with tool versions,
counts, every case, and bounded diagnostics, independently of disposable build
caches. Report schema 2 adds `ignored`, optional per-case `known_issue`, and the
top-level `include_ignored` setting. Ignored cases write their result and reason
only; any other files already in their artifact directory are from an earlier
execution. Multiple designs in one case receive separate artifact folders.

The retained [result matrix](verification/README.md) and
[investigation notes](VERIFICATION_REPORT.md), and
[detailed mismatch review](MISMATCH_REVIEW.md) describe what has been validated
and what remains unresolved. These extracted regressions include behavior
specific to Celox; the entire catalogue is not yet a verified Veryl conformance
suite. Keep the distinctions when reusing it with another compiler.
