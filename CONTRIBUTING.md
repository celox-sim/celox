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

### Code scanning

`.github/workflows/codeql.yml` does not gate merges, so pull requests and
Mergify queue drafts do not run it. Every Sunday at 04:17 UTC, and on manual
dispatch, it scans Actions, C/C++, JavaScript/TypeScript, Python, and Rust, with
at most two analyses running concurrently. Compiled languages use CodeQL's
`none` build mode. There is no push-triggered scan.

The workflow is inactive until the repository variable
`CODEQL_ADVANCED_ENABLED` is set to `true`. To replace the existing automatic
scans after this workflow merges:

1. In repository Settings, switch CodeQL analysis from default to advanced
   setup, disabling default setup. Keep the existing alerts and their history.
2. Disable the separate automatic GitHub Code Quality scans in Settings;
   these are independent of CodeQL default setup and would otherwise continue
   using runners on PRs and pushes.
3. Set the repository Actions variable `CODEQL_ADVANCED_ENABLED` to `true`,
   then manually dispatch `codeql.yml` on `master`.
4. Confirm that all five analyses upload results and that PRs no longer start
   CodeQL or Code Quality scans.

For rollback, unset `CODEQL_ADVANCED_ENABLED` and re-enable default setup.
This variable allows the replacement to be reviewed without running both
setups at once. Until cutover, GitHub's existing automatic scans remain active.
See GitHub's [advanced setup migration instructions](https://docs.github.com/en/code-security/how-tos/find-and-fix-code-vulnerabilities/configure-code-scanning/configuring-advanced-setup-for-code-scanning).

### Product checks

Rust caches in `ci.yml` are restored on every branch but saved only on the
default branch. A PR's merge-ref cache cannot be restored by another PR, so
saving one copy per PR consumes the shared cache quota without warming other
queues. `rust-cache.yml` builds the Rust Tests targets on master after toolchain,
manifest, or CI-cache setup changes, or on manual dispatch. It uses the same job
ID, toolchain, environment and CI features as Rust Tests, skips the build on an
exact cache hit, and does not execute tests. The normal daily master CI warms
the other job caches. Caches remain dependency-only; workspace binaries are not
added to the quota. Toolchain changes and cold caches still require compilation.
Mergify-only configuration edits run script validation, not product compilation;
mixed Rust changes and unknown paths retain their existing coverage.


Pull requests use `scripts/ci-changes.mjs` to select jobs and scope Rust tests
by affected crates. Windows and ARM64 checks run in Mergify queue draft PRs,
legacy merge groups, and daily/manual full runs. Queue drafts run the full Celox Rust
workspace when Rust is affected. Mergify checks up to five batches concurrently
across three queue rules. Parallel mode separates independent file scopes while
retaining combined-tree validation for overlapping scopes:

| Queue | PR heads | Batch size | Maximum collection wait |
| --- | --- | --- | --- |
| `dependencies` | `renovate/*`, except Veryl updates | 5 | 1 minute |
| `changes` | Other heads: features, fixes, CI, and manual dependency updates | 3 | 1 minute |
| `integration` | `integration/*`, `renovate/veryl-crates`, `upgrade/veryl-*`, the Release Please branch, and legacy `master` sync PRs | 1 | None |

The head conditions are mutually exclusive, including for explicit queue
commands. The existing `merge-ready` rule selects the matching queue without
changing which PRs may auto-merge; manual-review dependency groups still require
an explicit merge request. All three queues retain the same GitHub protections,
queue validation, title checks, release hold, and release authorization gate.
Queue categories control batch sizes; file scopes control independence. The
`pr-policy` scope contains only the title workflow and its standalone title,
release-impact, and batch validators. Everything else (including unknown paths,
Rust dependencies, Veryl, NAPI, and feature changes) shares `core`. This lets a
PR-title repair advance without inheriting a speculative Veryl update, while
compiler and dependency changes still validate together. Mixed PRs share both
scopes. Core is capped at two concurrent batches and PR policy at one, within
the global five-batch ceiling; these are batch limits, not runner counts.
Changes to `.mergify.yml`, `release-please-config.json`, or `VERSION` are barriers, requiring
ordering across both scopes. Extend the narrow policy allowlist only after
checking its runtime dependencies. Scopes do not change CI job selection.
PRs whose source CI passed lint and skipped both the Rust/NAPI aggregate and
Ubuntu JS tests receive high queue priority. This prioritizes lightweight work
using the actual CI classification, including docs and standalone scripts,
rather than a `ci:` title or bot identity. Other PRs retain medium priority.
Running checks are not interrupted: independent scopes can progress separately,
but overlapping work already being tested and barriers still impose ordering.
This does not prioritize jobs inside GitHub Actions or preempt occupied runners.
See [Mergify priorities](https://docs.mergify.com/merge-queue/priority/) and
[Mergify queue modes](https://docs.mergify.com/merge-queue/queue-modes/).

[Mergify stacks](https://docs.mergify.com/merge-queue/stacks/) (`stack/*`
heads pushed by `mergify stack push --no-github-native`) use the `changes`
queue. A member above the bottom targets the branch below it and carries a
`Depends-On: #N` or same-repository `Depends-On: https://github.com/OWNER/REPO/pull/N`
marker; the queue accepts it on that basis and lands the whole
chain on the stack root, bottom-up. Queueing the top member queues every member
below it. The batch validator resolves each member's root through the same
marker chain, so a stack branch without it is still refused.

Lydite tests use the independent change-selected and daily cadence described
below, including on Mergify queue draft PRs.
The `Queue validation` merge condition requires the selected checks in `ci.yml` to pass;
ordinary PR checks cannot satisfy it. The `ci:full` label opts a PR into the
same coverage on its next push. Unrelated paths may still skip jobs.

- Enable both Merge Queue and Workflow Automation in Mergify; the latter runs
  the label-based queue rule.
- Version-only Release Please PRs defer product tests to the queue, where the
  release manifest change requires full validation including external suites.
- Add `merge-ready` to request automatic queueing, or comment `@mergifyio queue`
  for an ordinary PR. A failed queue entry receives `dequeued`; investigate it,
  remove that label, and request queueing again. There is no automatic retry loop.
- A PR that conflicts with its base branch does not enter the queue. Once a
  rebase resolves the conflict, `merge-ready` queues it without manual
  label changes. A PR that conflicts only with a PR ahead of it in the queue is
  held without running checks; it is dequeued, with `dequeued`, only if that
  PR merges.
- To cancel an automatic merge request, remove `merge-ready`, then comment
  `@mergifyio dequeue`. Removing the label alone does not remove a queued PR.
- The release PR requires `merge-ready` from the weekly release workflow and
  must not carry `release:hold`. Dependency rolls and lane synchronization use
  the same queue request label.
- Queue draft title checks validate the original PRs and their release impact,
  using the release policy from the exact batch tree.
- GitHub's strict up-to-date setting must be off for draft-PR queues: Mergify
  validates the combined tree instead of updating every original PR. Required
  status checks remain enabled. Keep the `Queue validation` merge condition.
- `merge_group` remains supported for rollback to GitHub's native queue.
- The queue validates the tree before it lands, so pushes to `master` and
  `develop` do not run CI again. Daily full runs keep those branches' caches warm.
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
- Workflow, `scripts/` and `.updatecli/` changes run the repository script
  tests in `Lint & Format`, including the Heliodor fixture and Updatecli
  selector tests. A test that runs on pull requests must be a required check
  or feed one; otherwise it belongs to a scheduled workflow.
- Rust-only changes still pass through the existing `Rust Test & NAPI Build`
  required check. Only explicitly unneeded producers may be skipped; failures
  and cancellations fail the check.

Lydite has its own test cadence (`scripts/ci-lydite-changes.mjs`):

- Lydite sources and fixtures run its core and bridge lanes. The core lane owns
  the proof-engine Rust tests, editor and standalone Veryl conformance. The
  bridge lane (`bridge` job) runs replay and proof-backed Veryl/symbolic corpus
  gates against the in-tree Celox exporter in one job sharing a release build.
- The bridge takes about an hour of runner time, so Celox changes do not run it
  on PRs or queue drafts even when Cargo's dependency graph reaches
  `lydite-celox`. The daily and manual runs on `master` (scheduled) and
  `develop` (dispatched by `nightly.yml`) enable every lane and report failures
  in the issue "Lydite validation is failing on <branch>", closed by the next
  passing run. NAPI/JS changes, upstream private tests and Markdown do not
  reach Lydite.
- Version-only Release Please diffs, including their queue drafts, skip Lydite.
- PR workflows come from the PR's merge ref, so branches based on an older base
  still carry the earlier `lydite.yml`, which selected every lane. That
  workflow file stays disabled in repository settings; `lydite-checks.yml`
  replaces it. Re-enable nothing under the old path.
- Shared dependency/toolchain configuration runs the core lane; an unreadable
  diff or dependency graph runs both lanes. The graph includes optional, build
  and dev edges. A `Cargo.lock` diff selects a lane only when a changed lock
  entry is in that lane's lockfile dependency closure, so adding a dependency
  to a Celox crate does not run Lydite. The Nix flake and `ci.yml` do not
  select Lydite; its jobs use neither. Celox's `conformance/sv-tests/` corpus
  also skips Lydite; Lydite's fixtures live under `lydite/conformance/`.
- Ordinary `ci.yml` tests exclude `lydite*`, including daily and release runs;
  formatting/lint/build checks remain there. The dedicated workflow tests the
  selected Lydite packages, including doctests, and installs its solver.
- `Lydite checks` accepts skipped lanes only when change detection explicitly
  excludes them, and fails on selected-job failures or cancellations. Repository
  branch protection must require this check to make it a merge gate.

Ordinary PRs also scope Rust compilation and execution:

- Library changes select the crate and its transitive consumers. Backend and
  SV frontend changes narrow Celox integration tests to the affected variants;
  shared implementation changes retain all affected variants.
- Integration test or recognized Insta snapshot changes select only the
  corresponding test binaries, using Cargo `--test` as well as a nextest
  filter. Packages needing whole-package coverage and packages needing specific
  binaries get separate Cargo invocations, so mixed changes do not discard
  their compilation scope. Shared helpers and unknown fixtures retain the
  package's tests; runtime changes retain their transitive consumers.
- A shared `.vtest` case file selects the Celox test binaries that reference
  its group, along with the suite's catalogue/parser checks and other direct
  catalogue consumers. Unknown groups retain dependency-based coverage.
  Case data does not select library API documentation or doctests. Shared
  harness and emitter changes still reach all consumers.
- Changes confined to a known SV test module select that module within the
  `systemverilog` binary, including for shared SV case groups. Changes to the
  adapter, crate root or an unknown helper retain the whole binary. Celox-only
  tests do not enable external-simulator features on their suite dependency;
  tests of the suite itself retain those features.
- VPI integration test edits select their binaries. The ignored cocotb test
  and Python setup run only when that binary or VPI's whole-package coverage
  is selected. Its Celox features match the preceding nextest build.
- Clippy selects affected crates and test targets, including Lydite. Rust API
  documentation and doctests select libraries reached through runtime/build
  dependencies; dev-only consumers still run tests but do not rebuild their
  API documentation. Test-only changes skip API documentation and doctests.
- sv-tests conformance runs when the Celox library or its runtime/build
  dependencies change, or its runner/expectations change. Test infrastructure
  and downstream consumers do not select this separate release-mode build.
- Python/cocotb setup runs only when VPI tests are selected.

Queue drafts, merge groups, releases, `ci:full`, and daily/manual full runs
retain the existing workspace tests, lint, documentation, sv-tests and platform
coverage. Cargo manifest, lockfile, toolchain, unknown-path and failed scope
analysis changes fall back to broad validation. Required checks permit skips
only when explicitly unneeded and still reject failures and cancellations.
Local validation uses the behavior-based matrix above.

| Cadence | Coverage |
| --- | --- |
| Daily at 01:17 UTC (10:17 JST) | Full `ci.yml` on the default branch, with every change-classifier output enabled. Includes native/WASI NAPI, JS/browser tests, ARM64, script tests, and all ordinary Celox workspace tests. |
| Daily from `nightly.yml` at 02:43 UTC (11:43 JST) | Dispatch the same full CI on `develop`, with detection of its Veryl dependency lane. Queue nightly packages and the small pinned/HEAD Heliodor Linux boot compatibility checks. |
| Daily at 01:47 UTC (10:47 JST) | `bench.yml` measures Rust, Verilator, and TypeScript on one host and publishes master history. Manual dispatch supports branch measurements. |
| Daily at 02:17 UTC (11:17 JST) | `codspeed.yml` measures compile time on the default branch using CPU simulation. Execution failures and CodSpeed performance-check failures open or update a branch issue; the next healthy run closes it. Manual dispatch supports branch measurements. PRs, merge groups, and pushes do not run CodSpeed. |
| Daily at 02:37 UTC (11:37 JST) | `heliodor-bench.yml` runs the complete master Linux suite on x86-64 and AArch64. Explicit manual selections retain focused suite runs and ARM64 profiling. PRs do not run this workflow; its tooling tests run in `Lint & Format` with the other script tests. |
| Every full CI run | Run every shared Veryl and SystemVerilog case against both Verilator and Icarus, with locked Nix tools. Run the live adapter tests normally marked ignored because they need external tools. Preserve reports and per-case diagnostics as artifacts for 14 days, including on failure; matrix failures do not cancel the other comparisons. |
| Affected pull requests and merge groups; daily at 03:41 UTC (12:41 JST); on demand | `lydite-checks.yml` owns Lydite Rust tests, proof, conformance, editor, and mutation gates. Unrelated changes skip these jobs, and Celox changes skip the bridge. Daily and manual runs enable all lanes and report failures in an issue. |
| On demand | Dispatch `ci.yml` on the desired branch to run full CI and external comparisons, regardless of its diff. |
| Pull requests that cut a release | A `master` pull request or legacy merge group whose diff changes `.release-please-manifest.json` (only the release pull request does there; syncing it into `develop` is not a release) runs full CI and the external comparisons. `Rust Test & NAPI Build` then requires the comparisons to pass, so a release cannot merge after only change-based checks. Daily and manual full runs do not gate on the comparisons; their failures reach the full CI issue below. A `master` pull request or legacy merge group whose diff cannot be determined is treated the same way. |

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
