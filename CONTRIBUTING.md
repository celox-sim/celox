# Development validation

Use the locked environment described in [docs/development.md](docs/development.md).
Cargo builds use the workspace's default `target/`, managed by mbx. Do not set an
ad-hoc `CARGO_TARGET_DIR` or `--target-dir`.

## Select checks from the change

Before running checks, identify the behavior that changed and the code that
consumes it. Run the affected regression first, then the applicable checks below.
For mixed changes, take the union of the required checks. A time limit alone is
not evidence that validation is complete.

| Change | Required local validation |
| --- | --- |
| Documentation | Check changed links and examples; build the affected documentation when its rendering, configuration, or generated API content changes. Compile documentation included in Rust sources and run affected doctests. |
| Repository scripts or CI | Run the relevant script tests; inspect job conditions, artifact dependencies, and required-check failure handling. Validate changed workflow syntax. |
| Rust implementation | Check formatting and lint the affected crates. Run affected unit and regression tests, plus integration tests in consumers of the changed behavior. |
| One backend | Run its unit tests and the common language suite through that backend; include relevant width, four-state, and parallel variants. Run other backends when shared code or contracts also change. |
| Shared frontend, IR, optimizer, layout, or runtime | Run affected unit tests and common behavioral regressions through every affected backend. Broaden to the full common suite when the impact spans language groups or cannot be bounded. Include NAPI/JS integration when the public simulator behavior changes. |
| Shared language cases | Run added or changed executable cases through Celox's backend harness and the available external simulator adapters. Script comments, formatting and source-location shifts alone do not require simulation. Retain existing backend exclusions and unchanged expected behavior. A case-only change does not require rebuilding NAPI. |
| JS or NAPI | Run affected package tests, lint, and type checks; build NAPI and run boundary integration tests when the change uses it. Include WASI/browser or other host targets when the changed behavior affects them. |
| lydite | Run affected Rust tests and the applicable proof/conformance gates from `lydite/`; include Celox replay when the bridge or shared design contract changes. Solver tests require `Z3_BIN`. |
| Dependencies, manifests, toolchain, or an uncertain impact | Use the broader workspace and integration checks. Verify relevant feature and target configurations rather than assuming host tests cover them. |

Tests of portable language behavior belong in `crates/celox-test-suite`; tests of
Celox APIs, diagnostics, optimization settings, or code generation belong in
their owning crate. Preserve a regression's purpose when consolidating tests.

For example, to check a changed shared counter case locally:

```sh
cargo fmt --all -- --check
cargo test --locked -p celox --features systemverilog --test counter
cargo run --locked -p celox-test-suite --features verilator --bin verify-verilator -- --filter counter::
cargo run --locked -p celox-test-suite --features icarus --bin verify-icarus -- --filter counter::
```

Use the actual affected case or group as the filter. For SystemVerilog cases,
use the `verify-sv-verilator` and `verify-sv-icarus` binaries. Missing tools,
empty selections, compilation failures, and simulator errors are not passes.

External results are reused locally by default, including between worktrees,
when the case, verifier and tool fingerprints match. New/changed cases and
prior failures run again; `--fresh` forces execution. A Celox implementation
change against an unchanged shared case does not require external revalidation
unless it also changes the external adapter/emitter or its dependencies.
Normal validation writes only under `target/`: omit `--report` and do not
regenerate checked-in JSON or summary tables just because a case was added or
passed. The daily gate checks the live catalogue and accepts new passing cases
without a baseline refresh. Retained failure evidence changes only after review
of a changed failure; proof contracts change when executable coverage changes.
See the [runner documentation](crates/celox-test-suite/README.md#independent-verification).
Daily CI uses `--fresh` and runs the complete corpus afresh.

## Finish validation

Validation is complete when the selected required checks pass and relevant
failures are resolved. Record the commands, their scope, and any required checks
that could not run in the handoff or PR. If a check fails, investigate it and fix
failures caused by the change. Report unrelated failures explicitly.

Reuse successful results for unchanged source, dependencies, configuration, and
tools. Further edits invalidate checks whose inputs or behavior they affect.
Repeat or broaden checks when those edits, failures, or concrete unresolved
concerns justify it; otherwise finish the task. The pre-push hook remains a
minimum safety net, not the definition of validation completeness.

## CI coverage and cadence

Pull requests and merge groups use `scripts/ci-changes.mjs` to select jobs.
Each change is validated where it matters rather than repeatedly:

- Pull requests run the Linux jobs (lint, Rust tests, Linux and WASI NAPI, JS
  on Ubuntu, Playground). Windows NAPI/JS and the ARM64 NAPI and backend jobs
  are skipped there, which their required checks accept, and run in the merge
  group instead. They rarely fail when the Linux jobs pass. Dispatch `ci.yml` on
  a branch to run them before queueing.
- Pull requests run only the Rust tests their changes can affect
  (`scripts/ci-rust-scope.mjs`):
    - A library change runs its package and every package that depends on it.
    - A change to one integration test file runs only that test binary.
    - A backend or frontend crate that only one variant of celox's
      `all_backends!` tests uses runs only that variant, plus the tests that
      are not generated per variant. For example, `celox-backend-x86` runs
      `native` and `native_parallel`, and `celox-frontend-sv` runs `sv` and the
      `systemverilog` binary.
    - Manifests, the lockfile, the toolchain, shared test data and CI changes
      run the whole workspace.

  Merge groups and the daily run always run the whole workspace.
- The merge group validates the exact tree that lands, so pushes to `master` and
  `develop` do not run CI again. Daily full runs keep those branches' build
  caches current for pull requests.
- `sync-develop.yml` opens or updates the master-to-develop synchronization pull
  request once a day (22:07 UTC) rather than on every master push. Dispatch it
  to sync sooner.

Job selection:

- Rust tests, benches, the common test suite, benchmark crates, VPI, the separate
  wasm binding crate, and lydite retain Rust checks without triggering NAPI and
  JavaScript builds. Crate exclusions are checked against NAPI's transitive
  runtime/build dependencies, including optional and target-specific edges.
- Runtime source changes keep binding and JS coverage. Changes under `src/`,
  including unit tests mixed with implementation, remain conservative.
- Cargo manifests and lockfiles retain broad coverage. Mixed changes combine
  their requirements; unknown paths and failed change detection run all jobs.
- Rust-only changes still pass through the existing `Rust Test & NAPI Build`
  required check. Only explicitly unneeded producers may be skipped; failures
  and cancellations fail the check.

The classifier selects CI jobs, not individual Rust tests. Selected Rust CI
still runs the full workspace suite with CI features, doctests, default-feature
checks, ARM64 backend checks, and the explicit cocotb test. Local validation
uses the finer behavior-based matrix above.

| Cadence | Coverage |
| --- | --- |
| Daily at 01:17 UTC (10:17 JST) | Full `ci.yml` on the default branch, with every change-classifier output enabled. Includes native/WASI NAPI, JS/browser tests, ARM64, script tests, and all ordinary workspace tests. |
| Daily from `nightly.yml` at 02:43 UTC (11:43 JST) | Dispatch the same full CI on `develop`, with detection of its Veryl dependency lane. Queue nightly packages and the small pinned/HEAD Heliodor Linux boot compatibility checks. |
| Daily at 01:47 UTC (10:47 JST) | `bench.yml` measures Rust, Verilator, and TypeScript on one host and publishes master history. Manual dispatch supports branch measurements. |
| Daily at 02:17 UTC (11:17 JST) | `codspeed.yml` measures compile time on the default branch using CPU simulation. Execution failures and CodSpeed performance-check failures open or update a branch issue; the next healthy run closes it. Manual dispatch supports branch measurements. PRs, merge groups, and pushes do not run CodSpeed. |
| Daily at 02:37 UTC (11:37 JST) | `heliodor-bench.yml` runs the complete master Linux suite on x86-64 and AArch64. Explicit manual selections retain focused suite runs and ARM64 profiling. PRs run only the benchmark tooling tests when those files change. |
| Every full CI run | Run every shared Veryl and SystemVerilog case against both Verilator and Icarus, with locked Nix tools. Run the live adapter tests normally marked ignored because they need external tools. Preserve reports and per-case diagnostics as artifacts for 14 days, including on failure; matrix failures do not cancel the other comparisons. |
| Weekly, and on pull requests with relevant changes | Existing `lydite.yml` proof, conformance, editor, and mutation gates. |
| On demand | Dispatch `ci.yml` on the desired branch to run full CI and external comparisons, regardless of its diff. |
| Merge groups that cut a release | A `master` merge group whose diff changes `.release-please-manifest.json` (only the release pull request does there; syncing it into `develop` is not a release) runs full CI and the external comparisons. `Rust Test & NAPI Build` then requires the comparisons to pass, so a release cannot merge after only change-based checks. Daily and manual full runs do not gate on the comparisons; their failures reach the full CI issue below. A `master` merge group whose diff cannot be determined is treated the same way. |

Full runs have a separate concurrency group so ordinary pushes cannot cancel
them. Scheduled and dispatched full runs on `master` and `develop` end with
`Report full validation`: a failed or cancelled job opens the issue
"Full CI is failing on <branch>", or comments on it while it stays open, and
the next passing full run closes it. They report failures normally; they are not advisory jobs with
`continue-on-error`. The external comparison gate accepts an existing retained
failure only when its case ID, expectation, status, phase, retained diagnostic,
and tool version match the checked-in baseline. These cases still run every
day and remain failures in the report. New or changed failures, abnormal runner
exits, cached results, inconsistent counts, and missing catalogue cases fail
the gate. Diagnostic comparison ignores only outer line whitespace and line
endings, including checkout-dependent Verilator padding; wording and source
locations must match. Improvements may pass without updating the baseline.
Baseline changes require review rather than automatic acceptance of a new run.

Scheduled workflows begin using this configuration after
it reaches the default branch; the develop dispatch also requires the updated
`ci.yml` on `develop`.

Known unsupported backend cases and reviewed external-tool discrepancies remain
excluded, with their original assertions and reasons. The external reports
identify excluded cases rather than counting them as passes; see the suite's
[verification evidence](crates/celox-test-suite/verification/README.md) and
[tool limitations](crates/celox-test-suite/LIMITATIONS.md). Expected-to-fail
regressions and manual scaling measurements are not made passing by blindly
running every `#[ignore]` test. New exclusions need a concrete reason and
evidence; an unexpected failure must not become a skip.
