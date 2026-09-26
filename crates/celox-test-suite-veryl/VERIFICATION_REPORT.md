# Independent expectation verification

Verified on 2026-09-26 with Veryl 0.21.0, Verilator 5.052, and Icarus Verilog
13.0 on x86_64 Linux. The current corpus has **648 cases: 640 simulation cases
and 8 compilation-rejection cases**. The original 646-case reports remain in
[verification/baseline](verification/baseline/README.md).
The [expectation review](MISMATCH_REVIEW.md) records the specification clauses,
allowed-result questions, and independent reproductions for every disagreement.
The [generated table](verification/README.md), [CSV matrix](verification/matrix.csv),
and both JSON reports are retained in the crate.

| Outcome | Verilator | Icarus |
| --- | ---: | ---: |
| Simulation assertions passed | 456 | 455 |
| Expected compilation rejection | 1 | 8 |
| Invalid design unexpectedly accepted | 0 | 0 |
| Assertion disagreement | 0 | 0 |
| Reviewed discrepancy / limitation ignored | 82 | 185 |
| Veryl emission blocked | 0 | 0 |
| SystemVerilog compilation blocked | 0 | 0 |
| Execution error / unrepresentable result | 0 | 0 |
| Unsupported four-state design | 109 | 0 |
| Total | 648 | 648 |

350 simulation cases pass both tools and 561 pass at least one. This does not
certify portability: a pass in one tool can coexist with a disagreement in the
other. Icarus validates 101 of the 109 four-state cases that Verilator cannot
execute; the remaining eight have five emission and three compilation errors.

## Corrections since the baseline

- The two inclusive-loop fixtures now explicitly declare their intended negative
  bounds as `signed logic<32>`. Both tools pass the unchanged numerical assertions.
- The wide shift expectation now preserves the Z bits in its alternating X/Z
  stimulus. Icarus passes it. Celox's backends and interpreter were fixed too,
  including mask-preserving narrowing. An additional shared case covers
  8/64/65/128/256-bit values, left/right shifts, constant/runtime counts, word
  boundaries, and X/Z shift counts with a known-one bit; Icarus also passes it.
  Both cases also pass Veryl's reference simulator, so their exclusions in the
  Celox runner were removed.
- Eight dynamic output connection cases now require compilation rejection.
  Celox's Veryl frontend rejects these destinations, and the old extension was
  removed. Its SV frontend checks the forms it supports; cases blocked by
  earlier unsupported constructs remain excluded (see the review checks below).
  Icarus rejects all eight; Verilator rejects one and accepts seven.
  Six of those seven previously counted as passes. Accepting an invalid design
  no longer counts as numerical validation. The seven acceptances are now
  ignored on Verilator only, with their pre-exclusion failures retained in
  [a separate report](verification/repros/dynamic_output_acceptance.json).
- The two aliased function-output cases now accept either copy-out order,
  because SV does not prescribe the relative order of the blocking output
  copies. Both cases also verify named binding with distinct destinations and
  exact expected values. Their legacy case IDs remain stable. Celox warns about
  statically overlapping output arguments with `unspecified_output_copy_order`.
- The `$bits` signedness case now requires `0xffffffff`; the function-input
  width case requires bit 2 = `1`. Their checked SV semantics determine the
  expectations. Veryl/Celox implementation errors are retained in the failure
  report, and the 11 affected backend variants are ignored until the deferred
  fixes are implemented. The passing SV function-width variant stays enabled.
- Two-state zero division retains the Verilator-compatible zero result.
  A separate four-state case requires all-X quotient and remainder, including
  positive/negative numerators and 0 / 0. Icarus, Veryl's reference simulator,
  and all Celox paths now pass. Celox's four Veryl-path backends were fixed,
  and their four temporary ignores were removed. The expanded case also
  covers mixed bit widths and the resolved x86 SIMD lifetime failure.

The new `rejected` status means an HDL compiler rejected a negative fixture;
its actual diagnostic remains in the report. It does not establish support for
all other constructs in that fixture. Missing tools, timeouts, emitter panics,
and C++ harness failures cannot satisfy the rejection expectation. The suite
keeps stable case IDs even where the former simulation contract became a
compilation-rejection contract.

## Numerical disagreements and implementation failures

Of the seven distinct numerical disagreements after the fixture fixes, three
default to `ignored` on the affected tool and two now accept either allowed
copy-out order. The remaining two cases now require the SV-specified values and
pass Verilator and Icarus. Current Veryl/Celox failures remain recorded; the
implementation fixes are deferred. Neither external runner has an enabled
assertion mismatch after these expectation corrections.

| Finding | Cases | Treatment |
| --- | ---: | --- |
| Simulator differs from the checked SV requirement | 3 | Retain expectations; ignore read-before-write and signed division on Verilator, input-port width on Icarus |
| Veryl constant evaluator disagrees with emitted SV | 2 | Require SV results; retain failures and ignore affected Veryl/Celox variants until fixed |
| Aliased output copy-out order is unspecified | 2 | Accept either complete output value; verify distinct destinations exactly |

The [detailed review](MISMATCH_REVIEW.md#what-may-vary-and-what-may-not) lists
required values and the limits of the evidence. In particular, the statement
that input-argument evaluation order is undefined does not prove a particular
output copy-out rule. Instead, blocking copy-out is required by 4.9.7 and
13.5/13.5.1/13.5.4 do not prescribe its relative order between formals. Both
orders satisfy those requirements; arbitrary result bits do not.

Each `ignored` result includes the checked clauses, the observed tool version,
and upstream links or retained local observations. Verilator's maintainer prioritized
combinational optimization for read-before-write and deliberately chose zero
to avoid the signed-division host exception. Icarus's source explains its width
handling; no direct response to this discrepancy was found. No direct upstream
decision was found for the seven accepted dynamic output connections either;
their exclusions cite the observed acceptances and checked SV rules. The detailed
review keeps these different kinds of evidence explicit. Ignored cases are not executed
or counted as passing; `--include-ignored` reruns the original assertions and
reports actual failures. The reusable corpus has no skips; Celox's backend
matrix separately ignores the 11 variants affected by the two deferred fixes,
alongside existing backend limitations. The review checks below record three
additional SV variants blocked by unsupported indexed part-selects.
All six variants of the four-state zero-divisor case remain enabled.

The IEEE clauses were checked in the locally supplied 2023 edition. They specify
SystemVerilog, not an independent definition of Veryl semantics. Retained Veryl
constant probes demonstrate the two actual IR/emitter inconsistencies without
using Celox or the Rust/VPI adapters. The suite explicitly adopts the checked SV
requirements for these two cases, rather than retaining the current IR values.

## Reviewed blocked cases

The [limitation review](LIMITATIONS.md) separates Veryl restrictions/emission
issues, simulator limitations, a resource timeout, a simulator crash, and a
state-model difference. The exact tool/case manifest adds 73 Verilator and 184
Icarus exclusions to the original ten conformance exclusions. All original
diagnostics are retained in [Verilator](verification/limitations/verilator.json)
and [Icarus](verification/limitations/icarus.json) pre-exclusion reports.
These are not successful assertion checks or blanket allowances for failure.
New, unlisted failures still return a nonzero exit code.

The prior Verilator run's 55 emission errors include 47 cases relying on Celox's extension
for function output/non-local effects inside `always_ff`. Veryl 0.21.0 rejects
those constructs, while the Celox adapter explicitly opts into them. The other
errors are four `UnevaluableValue`, two `InvalidForRange`, and two emitter panics.
Icarus reaches five additional emission errors in four-state cases. The strict
shared emitter preserves these diagnostics.

The prior Icarus run's 122 compilation errors include unsupported function output arguments,
unpacked array assignments/arguments, and additional SV features. They remain
unvalidated. This count includes the 120-second compilation timeout for the
million-element sparse-memory case. Negative-fixture rejections are counted
separately. Full diagnostics for every case are retained in the JSON reports.

The prior Icarus run had two execution issues, now excluded with different reasons:

- `case_switch::test_case_break_inside_comb_function_for` aborts inside `vvp`
  with `vthread_s::cleanup(): Assertion stack_vec4_.empty() failed`.
- `signed_divrem::signed_divrem_i128` produces X on division by zero where the
  existing Celox two-state contract expects zero. IEEE 1800-2023, 11.4.3 specifies
  X for division/remainder by zero. The adapter reports the unknown result
  rather than discarding its mask. This is a state-model difference, not an
  Icarus specification violation; the new four-state case verifies X normally.

The corpus includes Celox regression contracts and extensions. Extraction and
independent execution do not turn every assertion into a proven conformance
requirement. Consumers can select applicable cases using the retained evidence
and `TestCase::expectation`, while keeping unresolved behavior visible.

## Reproduce and maintain

The [crate README](README.md#independent-verification) contains full-run and
single-case commands. Both runners, their adapters, shared process/protocol code,
and VPI array handling are part of the crate. The Nix development shell supplies
both simulators. Regenerate the matrix with `python3 scripts/summarize.py` after
refreshing both current JSON reports; leave `verification/baseline` unchanged.

The adapter checks cover pending-write settlement, falling-edge clocks,
active-high asynchronous resets, NBA sampling, event-level restoration, 130-bit
values, X/Z encoding, and unpacked-array ordering. The runner also tests that
infrastructure or emission failures cannot pass a negative case.

## Local validation of the corrections

- Celox integration/unit run with the SV frontend: 108 binaries, 5,722 passed,
  zero failed, 776 excluded at that run. Subsequently, both shift exclusions
  were removed and all 12 variants of those two cases passed (including Veryl).
- Changed backend/frontend/SLT library tests: 861 passed, zero failed.
- Shared-suite unit/integration/doc tests: 15 passed, zero failed. The three live
  adapter-only tests remain opt-in; the complete external-tool corpus runs above
  executed the adapters for both tools.
- Clippy passed for all targets of Celox, the shared suite, and all six changed
  backend/frontend/SLT crates. Formatting and diff-whitespace checks passed.
- After adding the initial three exclusions, both 647-case external runs were refreshed.
  Exactly the two Verilator and one Icarus statuses changed from `mismatch` to
  `ignored`; all other statuses were unchanged. Each filtered default run exits
  zero without compiling HDL, while `--include-ignored` reproduces the original
  assertion mismatch and exits one for all three. Runner tests cover tool
  scoping, explicit reasons, and infrastructure failures during forced runs.
- After adding the static output-alias warning, a fresh Celox run with the SV
  frontend completed 109 binaries: 5,729 passed, zero failed, 774 excluded.
  This run preceded the two specification-oracle changes. It includes all five
  new warning tests and both alias-order cases across the enabled backends.
- Both complete external runs were refreshed after the seven new Verilator
  exclusions and two alias-order changes. Only those nine Verilator statuses
  changed; the Icarus outcome counts stayed the same. A forced hierarchy run
  reproduced all seven `unexpected_accept` results and the eighth rejection.
- After changing the two specification expectations, both affected Celox test
  binaries were rerun: 895 passed, 11 failed, 200 ignored. All 11 failures belong
  to those two cases. Their pre-exclusion observations are retained: nine numerical
  mismatches, one Veryl simulator internal error, and one Celox SV frontend
  unsupported construct. The SV frontend passes the function-width case.
  [Per-backend observations](verification/repros/veryl_celox_failures.json) and
  reproduction commands are retained in the detailed review.
- The corrected two cases were then rerun in both external simulators and
  passed all four checks. Their current report rows were refreshed from those
  focused runs; all other rows retain the preceding complete 647-case runs.
  The generated matrix and counts were recomputed from those retained rows.
- With the 11 affected Veryl/Celox variants ignored pending fixes, both affected
  test binaries pass normally: 895 passed, zero failed, 211 ignored. The passing
  SV function-width variant remains enabled. Explicitly including the ignored
  variants reproduces all 11 original failures with unchanged expectations.
- After classifying the remaining limitations, both full 648-case external
  runs exit zero with the counts above. Unit tests exercise all 267 exact
  tool/case exclusions, their retained failure evidence, affected-tool scoping,
  explicit rechecks, and new failures on enabled cases. Five live forced
  rechecks reproduce emission, compilation, runtime-crash, and state-model
  failures and exit one.
- Before the zero-divisor fix, the original four-state case passed Icarus,
  Veryl's reference simulator, and Celox's SV frontend. Four Celox backend
  variants failed and were temporarily ignored. Their original observations
  remain in [this pre-fix report](verification/repros/four_state_zero_divisor.json).
- Celox's four-state division and remainder now return all X for known-zero
  divisors, while two-state execution retains Verilator-compatible zero.
  The four ignores have been removed. The shared case now covers mixed
  8/64/65/128-bit signed and unsigned operations and transitions back to known
  results. An additional Celox backend matrix covers clocked assignments,
  constant divisors, and both state modes. The mixed-width case also guards
  the fix for x86 SIMD lifetimes crossing late register-allocation block splits.
  The focused runs pass: six shared-case variants and four backend matrix tests,
  with no exclusions. [Post-fix observations](verification/repros/four_state_zero_divisor_fixed.json)
  retain both coverage and the resolved native diagnostics.
- After both Celox fixes, the full Celox run passes 5,731 tests with zero
  failures and 783 existing exclusions. The four backend library suites pass
  another 659 tests; the reusable suite passes 15 tests with three opt-in
  external checks excluded. These runs use an x86-64 host; ARM64 code generation
  and unit tests pass, but ARM64 machine code has not been executed here.
- Focused external rechecks pass the expanded four-state zero-divisor fixture
  on Icarus 13.0 and the two-state i8/i128/always-FF fixtures on Verilator 5.052.
  The known i64 Verilator exclusion and both unsupported four-state rows stay
  unchanged. Their current rows were refreshed; the other rows retain the
  preceding full-catalogue runs. Formatting and Clippy checks also pass.

Both default external verification commands now complete successfully for the
retained tool versions. Their ignored cases remain unvalidated, and forced
runs expose their real failures. Celox's normal test runs ignore the deferred
implementation failures; explicitly including those variants reproduces the
failures against the specification-based expectations.

## Review regression checks

- `CompilationRejected` is now a public marker available with default features.
  `TestCase::run` accepts only that marker for a negative fixture. Independent
  consumer tests exercise all eight negative cases with missing-tool, timeout,
  I/O, generic adapter, and panic failures; none can pass as language rejection.
  Celox maps source diagnostics explicitly and keeps codegen/adapter errors as
  failures. The external runners use the same public marker.
- The stricter Celox adapter exposed three earlier false-positive SV rejections:
  `hierarchy::test_dynamic_output_port_rmw_preserves_unselected_bits`,
  `hierarchy::test_dynamic_minus_colon_output_port_rmw`, and
  `hierarchy::test_dynamic_step_output_port_rmw` stop with
  `Unsupported SystemVerilog construct: indexed part-select` before reaching
  the output-destination check. These three SV variants are now ignored for
  that limitation; explicitly running them still fails. The four Celox backends
  using the Veryl frontend continue to check their intended rejection.
- Hierarchical paths distinguish ordinary instances (`None`) from array
  elements (`Some(index)`). Live Icarus and Verilator checks both read an ordinary
  instance and generated elements `[0]` and `[1]`, including their nested child.
  All five opt-in live adapter checks pass; the corpus and external-runner
  exclusions are unchanged.
- ARM64 shifts now retain shifted Z payload bits. An X/Z shift count sets both
  output planes to all X, including unknown bits above bit 63 in a wide count.
  The backend regression covers left/right shifts of 8/64/65/128/256-bit values,
  four-bit destinations, constant/runtime counts, and word/width boundaries.
  Unlike the earlier host-only run, all 85 ARM64 backend library tests execute
  successfully under `qemu-aarch64` (QEMU 11.1.0).

The ARM64 run uses the installed Rust 1.98.1 AArch64 standard library, an
`aarch64-unknown-linux-gnu-gcc` linker, and `qemu-aarch64` as Cargo's target runner:
`cargo test --locked -p celox-backend-arm64 --target aarch64-unknown-linux-gnu --lib`.
This validates generated machine-code execution under emulation; physical ARM64
hardware has not been used for this review.

The shared crate passes 10 tests with default features and 17 with all features;
the five opt-in live tests also pass when run explicitly. Celox's focused
`hierarchy`, `four_state`, and `suite_adapter` binaries pass 612 tests with 75
documented backend exclusions. Clippy with `-D warnings` passes for all targets
of Celox, the ARM64 backend, and the shared suite with both external adapters
and the SystemVerilog frontend enabled.

## Icarus rejection diagnostic checks

The Icarus adapter now requires recognized, source-located diagnostics in
addition to a nonzero compiler exit. It checks the complete log and leaves
unrecognized or mixed compiler/tool failures as `compile_error`. Empty logs,
error-count summaries alone, unreadable input, internal compiler failures,
and reserved timeout/tool-launch statuses cannot satisfy a negative fixture.
An unsupported function-port diagnostic alone also does not qualify.

The retained diagnostics for all eight negative fixtures pass this classifier.
A process-level CLI regression exercises 21 scenarios through fake compiler
executables, including ordinary exit codes 1, 2, and 123, source diagnostics
followed by an internal error, and timeout statuses even with source diagnostics.
Only the source-rejection control reports `rejected`; every failure scenario
reports `compile_error` and makes the CLI exit nonzero.

Live Icarus 13.0 execution revalidates all eight expected rejections using the
public `TestCase::run` API. All six opt-in adapter tests pass, as do the shared
crate's 20 non-ignored tests with all features enabled. The existing 648-case
reports and exclusions are unchanged. Unknown diagnostic formats deliberately
remain failures until reviewed, rather than being counted as rejection.

## Wide-count, padding, and Verilator diagnostic checks

Three additional review regressions reproduced on the previous implementation:

- A 128-bit count with Z at bit 64 made native x86 `1 << count` return known
  `8'hff` instead of all X. Both payload and mask lowering now reduce every
  count-mask chunk through the same helper. The Celox regression checks
  left, logical-right, and arithmetic-right shifts of 8/65/129/257-bit values,
  X and Z at count bits 0/63/64/127, and recovery to a known count on all four
  Celox execution backends.
- Cranelift's unknown shifts filled physical padding above a partial-width
  result with ones. A wider bitwise consumer exposed definite ones in the
  register path and excess X bits in the memory path. Both planes now clear
  padding before the result is reused. Direct SIR regressions cover 65/129-bit
  register results, 193/257-bit memory results, and 257-bit shifts narrowed to
  65/129-bit register results. They execute generated code and consume the
  result directly in a wider OR, without an intervening cast/store that could
  conceal the padding bug.
- Verilator's generic `%Error` prefix allowed an I/O failure to satisfy a
  negative case. Recognition now requires the reviewed assignment-type
  diagnostic with an emitted source path and position, and checks the entire
  log. A CLI process regression covers 22 scenarios: the source-rejection
  control, empty/summary-only logs, internal/unsupported/I/O/C++/make/command
  failures, mixed source and tool failures, and abnormal exit statuses. Only
  the source-rejection control succeeds. The retained real diagnostic also
  passes, while a log naming a different source or a signalled process fails.

Focused Celox and x86/Cranelift backend checks pass **1,336 tests**, with 21
existing exclusions, including the new regressions and the existing four-state,
wide-shift, shift-signedness, and native shift-boundary suites. The shared crate
passes **22 tests** with all features. All **seven opt-in live adapter checks**
pass on Verilator 5.052 and Icarus 13.0, including Verilator's actual assignment
rejection and Icarus's eight negative fixtures.
All-target Clippy with `-D warnings` passes for Celox, x86, Cranelift, and the
shared suite with the SV frontend and both external adapters enabled.
The 648-case catalogue, expected values, retained external reports, and all
existing conformance/limitation exclusions are unchanged.
