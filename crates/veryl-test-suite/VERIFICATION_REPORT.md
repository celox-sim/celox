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
  Both Celox frontends reject these destinations, and the old extension was
  removed. Icarus rejects all eight; Verilator rejects one and accepts seven.
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
  and Celox's SV frontend pass; Celox's four Veryl-path backends currently
  return known zero and are ignored for this new case pending a fix. The
  existing unknown-operand case remains enabled on those backends.

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
matrix separately ignores the 11 variants affected by the two deferred fixes
and four variants of the new four-state zero-divisor case.

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
