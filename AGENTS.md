# Agent Instructions

## Validate before pushing

The pre-push hook is only a fast minimum safety net. Before pushing, inspect the
actual change and autonomously run the formatting, checks, and tests needed to
validate it. Do not treat passing the hook as sufficient validation, and do not
defer tests to CI merely because the hook does not run them.

Choose focused tests that exercise the changed behavior, then broaden validation
in proportion to the change's scope and risk. If a required test cannot be run,
state that explicitly in the handoff instead of silently omitting it.

Use the change-based validation matrix in [CONTRIBUTING.md](CONTRIBUTING.md).
Select the required checks from the affected behavior and its consumers before
running them. Once they pass and no relevant failure remains unresolved,
validation is complete. Repeat or broaden checks only for further changes, a
failure, or a concrete unresolved concern. Do not repeat successful checks on
the same source and configuration solely for reassurance. Daily full CI and
periodic conformance gates provide broader coverage between changes.

External Verilator/Icarus checks are needed when executable shared cases or the
external adapters/emitter change. A Celox backend/frontend fix against an
unchanged shared case does not by itself require rerunning external simulators.
Script comments, formatting and diagnostic line shifts do not require external
simulation. Use focused filters; the local runners reuse unchanged successful
evidence across worktrees by default. Use `--fresh` for an intentional fresh run.
Do not refresh checked-in verification JSON, summary tables or proof manifests
as routine validation. Normal results belong under `target/`; change retained
evidence only when the task changes a reviewed failure or coverage contract.

When a task has a clear requested outcome, pursue it without waiting for
step-by-step instructions. Inspect the repository, make reasonable assumptions,
implement the change, and continue while a safe, in-scope next step remains.

Do not stop after proposing a plan or making a partial change when the remaining
work can be discovered and completed locally. If a check fails, investigate and
fix failures caused by the change before reporting completion. Report assumptions,
validation performed, and any unresolved limitation in the final handoff.

Ask for direction only when a missing choice would materially change the result,
or when the next action needs additional authority, is destructive, or affects
systems outside the requested scope. Autonomy does not permit broadening the task,
bypassing safety checks, or committing, pushing, or publishing unless requested.

## Adding tests

When adding tests, consider adding them to `crates/celox-test-suite` so
other compiler and simulator implementations can reuse the Veryl source,
stimulus, and assertions. Prefer the shared suite for implementation-independent
language behavior and portable regressions. Keep tests of Celox-specific APIs,
IR, diagnostics, tracing, performance, and optimization settings in their
owning crate. Wire shared cases into Celox's backend test harness, preserve
relevant backend exclusions, and validate the new cases with the available
external simulator adapters as well.

## Pull request titles

Pull request titles must use Conventional Commits because release automation uses
the title of each merged pull request to calculate the next version and changelog.
Use this format:

```text
<type>(optional-scope)[!]: <description>
```

Allowed types are `build`, `chore`, `ci`, `docs`, `feat`, `fix`, `perf`,
`refactor`, `revert`, and `test`. Scopes must be lowercase. Add `!` immediately
before the colon for a breaking public API or compatibility change. Examples:

```text
fix(parser): preserve enum member widths
feat(api)!: remove legacy simulator options
```

Before opening or updating a pull request, choose a compliant title. Do not use
prefixes such as `[codex]`, sentence-style titles without a type, or merge-message
titles such as `Merge pull request #123`.
