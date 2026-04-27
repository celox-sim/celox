# Triage

This page defines the lightweight maintainer workflow for prioritizing GitHub work in `celox`.

## Goal

Keep the queue small, visible, and biased toward:

1. correctness of simulator behavior
2. user-facing usability regressions
3. mergeable work already in flight

GitHub Projects can still be used as a view layer, but the source of truth should remain:

- Issues for work items
- Pull requests for implementation
- Tracking issues for multi-step themes

## Queue Buckets

Every open issue and PR should fit one of these buckets.

### Now

Work that should be reviewed, merged, or fixed before taking on new feature work.

Typical criteria:

- open PR that is already testable and close to merge
- bug that breaks CI, docs, playground, or common workflows
- semantic bug that can produce incorrect simulation results

### Next

Important follow-up work that should start after `Now` is cleared.

Typical criteria:

- core feature gaps with direct user impact
- support gaps already split into concrete issues
- diagnostics improvements around common failure cases

### Later

Useful work that is real but not on the immediate path.

Typical criteria:

- dependency maintenance
- future backend expansion
- cleanup that depends on upstream release timing

### Tracking

Umbrella issues that organize a theme but should not compete directly with execution issues.

Typical criteria:

- parent issue for a family of sub-issues
- inventory / classification issue
- roadmap marker spanning several PRs

## Priority Rules

Use these precedence rules when two tasks compete.

1. Merge or unblock high-signal open PRs before opening fresh work.
2. Prefer correctness over optimization unless a proven perf regression is blocking a release.
3. Prefer user-visible breakage over internal cleanup.
4. Prefer concrete child issues over broad tracking issues.
5. Keep upstream-waiting tasks out of the active queue unless the external blocker moved.

## Recommended Fields

If a GitHub Project is used, keep the schema minimal:

- `Bucket`: `Now`, `Next`, `Later`, `Tracking`
- `Area`: `sim-core`, `backend-native`, `ts-runtime`, `playground`, `ci/docs`, `upstream-veryl`
- `Kind`: `bug`, `feature`, `tracking`, `debt`, `dependency`

Avoid duplicating the same meaning in both labels and project fields.

## 2026-04-27 Snapshot

This snapshot reflects the open GitHub queue inspected on April 27, 2026.

### Now

- No open PRs at the time of this snapshot. PR [#79](https://github.com/celox-sim/celox/pull/79) and PR [#78](https://github.com/celox-sim/celox/pull/78) were both merged on April 27, 2026.

### Next

- Issue [#44](https://github.com/celox-sim/celox/issues/44): array literal lowering gaps
- Issue [#45](https://github.com/celox-sim/celox/issues/45): expression lowering gaps
- Issue [#33](https://github.com/celox-sim/celox/issues/33): report true loop location
- Issue [#65](https://github.com/celox-sim/celox/issues/65): remaining unsupported `for`-loop shapes in simulator lowering

### Later

- Issue [#42](https://github.com/celox-sim/celox/issues/42): const function evaluation in module params
  The current `std_mux` / `std_demux` smoke tests pass on Celox backends, so this issue likely
  needs closure or scope correction rather than new implementation work.
- Issue [#72](https://github.com/celox-sim/celox/issues/72): remove `deps/veryl` submodule once `veryl` `0.20.0` is available
- Issue [#36](https://github.com/celox-sim/celox/issues/36): dependency dashboard
- Issue [#16](https://github.com/celox-sim/celox/issues/16): instance-contiguous `MemoryLayout`
- Issue [#30](https://github.com/celox-sim/celox/issues/30): native testbench DSE
- Issue [#25](https://github.com/celox-sim/celox/issues/25): support Veryl native testbench
  Native testbench support now exists, so this issue likely needs closure or a narrower remaining-scope update.
- Issue [#26](https://github.com/celox-sim/celox/issues/26): native ARM backend
- Issue [#27](https://github.com/celox-sim/celox/issues/27): native RISC-V backend
- Issue [#24](https://github.com/celox-sim/celox/issues/24): improve generic-argument error message
- Issue [#64](https://github.com/celox-sim/celox/issues/64): SystemVerilog components/module instantiations in simulator parser
- Issue [#66](https://github.com/celox-sim/celox/issues/66): system function calls in FF lowering
- Issue [#67](https://github.com/celox-sim/celox/issues/67): remaining comb expression/function lowering shapes
- Issue [#68](https://github.com/celox-sim/celox/issues/68): remaining FF expression-lowering shapes

### Tracking

- Issue [#41](https://github.com/celox-sim/celox/issues/41): simulator support gaps by category
- Issue [#43](https://github.com/celox-sim/celox/issues/43): function-call lowering support
- Issues [#57](https://github.com/celox-sim/celox/issues/57), [#58](https://github.com/celox-sim/celox/issues/58), [#59](https://github.com/celox-sim/celox/issues/59), [#60](https://github.com/celox-sim/celox/issues/60), [#61](https://github.com/celox-sim/celox/issues/61), [#62](https://github.com/celox-sim/celox/issues/62), [#63](https://github.com/celox-sim/celox/issues/63): concrete child issues under the broader function-call support theme

## Operating Cadence

Run this review whenever a PR merges or a new user-visible bug appears.

1. Re-evaluate all open PRs and move mergeable ones into `Now`.
2. Remove closed issues from the bucket list.
3. Promote only one or two `Next` items at a time.
4. Leave broad roadmap or upstream-waiting tasks in `Later` or `Tracking`.

## Should This Be A Skill?

Usually not at first.

For this repository, the stable asset should be this document because:

- the queue is repo-specific
- issue numbers and priorities change over time
- maintainers need a visible policy in the repo itself

A skill becomes useful when the workflow is reused across repositories, for example:

- a personal weekly GitHub triage routine
- a standard way to classify PRs and issues across several projects
- a repeatable sequence of GitHub connector actions with consistent output

Good split:

- repo-specific policy: docs page in this repository
- reusable personal workflow: skill
