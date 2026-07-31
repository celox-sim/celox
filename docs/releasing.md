# Release policy

Celox is released as one lockstep npm distribution. `VERSION`,
`.release-please-manifest.json`, `@celox-sim/celox`,
`@celox-sim/celox-napi`, and `@celox-sim/vite-plugin` must always carry the same
version. Rust workspace crates are internal implementation units, use version
`0.0.0`, and are not published to crates.io.

## Versioning before 1.0

Pull request titles are the input to release automation:

| Pull request title | Release effect |
| --- | --- |
| `fix:`, `feat:`, `perf:`, `revert:` | Patch release |
| Any allowed type with `!`, such as `feat(api)!:` | Minor release |
| `build:`, `chore:`, `ci:`, `docs:`, `refactor:`, `test:` | No release |

After 1.0, normal Semantic Versioning applies: fixes are patch releases,
features are minor releases, and breaking changes are major releases.

## Automated release train

Release Please maintains one release pull request against `master`. Every Monday
at 03:00 UTC, the release workflow enables auto-merge for that pull request. The
required checks and merge queue decide when it is safe to merge. If there are no
releasable changes, the run is a no-op.

The commit that introduces this policy is the release-history baseline. Earlier
commits after `v0.1.34` are intentionally not retroactively classified; release
automation starts with pull requests merged after this policy lands.

Add the `release:hold` label to the release pull request to keep it out of the
weekly train. Run the **Release Management** workflow manually to queue an
unheld release immediately.

Merging the release pull request changes `.release-please-manifest.json`; that
push, rather than a tag push, starts the NAPI build and npm publication workflow.
Release Please may still create an immutable version tag and GitHub Release as
release metadata, but tags are not deployment triggers.

## Veryl dependency lanes

`master` is the release lane. It always uses released Veryl crates and is the
only source of Celox releases. Renovate groups those crates into one
`fix(veryl):` pull request. A trusted `pull_request_target` workflow downloads
the matching `veryl-metadata` crate, reapplies the `git-command`-only default,
refreshes `Cargo.lock` in an isolated container, and pushes the result back to
the Renovate branch. The normal lint job rejects a stale or incorrectly featured
vendor copy, so the initial Renovate commit cannot race ahead of synchronization.
After a three-day release-age guard, required CI checks passing causes that pull
request to merge automatically. Its `fix` title then creates a Celox patch in
the next automated release train. The complete HEAD-to-Veryl-release-to-Celox-
release path therefore needs no routine manual step; a compatibility failure
simply stops at the required checks.

`integration/veryl-head` is a generated, non-release branch. **Veryl HEAD
Integration** recreates it from the latest `master` every day and whenever the
stable dependency definition changes, pins all Veryl crates to one exact commit
from upstream `master`, and lets the normal CI workflow test that commit. A
failure on this branch is an early compatibility signal; do not merge or release
this branch. The next successful run replaces it, so work on failures in a
separate pull request or git worktree.

## Repository configuration

The release workflow requires a `RELEASE_PLEASE_TOKEN` Actions secret containing
a fine-grained token or GitHub App token with repository contents and pull request
write access. The Veryl HEAD and vendored-metadata workflows use the same token
to make their generated branch pushes start normal CI. Using the default
`GITHUB_TOKEN` would prevent workflows from running on bot-created pull request
updates.

Configure merge commits to use the pull request title as the merge commit title.
This preserves the Conventional Commit title consumed by Release Please. Protect
`master` and require both **Conventional Commit title** and the normal CI checks;
do not allow those checks to be bypassed by the weekly release automation. Enable
repository auto-merge so the weekly workflow can queue a checked release pull
request. Allow the automation token to force-update the disposable
`integration/veryl-head` branch; never grant that exception on `master`.
