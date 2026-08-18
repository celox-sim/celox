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

Changelog entries come from GitHub's merged pull requests, not from the raw
commits reachable through a merge. A pull request therefore appears once under
**What's Changed**, even when its branch contains multiple Conventional Commits.
The pull request title is the release-note text and links back to the pull
request.

## Automated release train

Release Please maintains one release pull request against `master`. Every Monday
at 03:00 UTC, the release workflow first tries to add that pull request directly
to the merge queue. If requirements are still pending, it enables auto-merge and
retries until GitHub accepts the pull request. It succeeds only after observing a
merge queue entry; required-check failures or a queue timeout therefore remain
visible as a failed workflow. If there are no releasable changes, the run is a
no-op.

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

`master` is both the normal Celox development trunk and the stable release lane.
It always uses released Veryl crates and is the only source of stable Celox
releases. Every pull request targeting `master` is checked for accidental Veryl
git dependencies.

`develop` is a non-release compatibility overlay: the latest `master` plus the
changes needed by the last Veryl HEAD revision that passed CI. **Sync Develop**
opens `master` to `develop` pull requests and enables auto-merge. Ordinary Celox
features and fixes belong on `master`, never only on `develop`, so the overlay is
disposable and can be rebuilt after a Veryl release.

**Veryl HEAD Integration** creates `integration/veryl-head` from `develop`, pins
all Veryl crates to one exact upstream `master` commit, and opens a pull request
back to `develop`. Passing rolls auto-merge. A failing roll remains open and is
not replaced by the next scheduled run, making the compatibility regression an
actionable pull request that a maintainer can take over. The integration branch
is temporary and must never target `master`.

Renovate groups released Veryl crates into one `fix(veryl):` pull request against
`master`. The trusted vendoring workflow merges the tested `develop` overlay into
that release candidate, retains Renovate's released dependency declarations,
downloads the matching `veryl-metadata` crate, reapplies the `git-command`-only
default, and refreshes `Cargo.lock` in an isolated container. Required CI then
tests the complete promotion. A source conflict or a HEAD-only change that is
not compatible with the release leaves the existing Renovate pull request and
its failed check for investigation. Every `develop` update redispatches the
trusted synchronization for an open Veryl release candidate, so a compatibility
fix cannot be missed merely because Renovate opened its pull request first. Once
merged, the normal master-to-develop sync and next HEAD roll rebuild the overlay
on the new stable base.

## Nightly channels

The NAPI publication workflow publishes two daily prerelease channels from
immutable commits:

| npm dist-tag | Source | Veryl compatibility |
| --- | --- | --- |
| `nightly-stable` | `master` | Latest released Veryl crates |
| `nightly-head` | `develop` | Last Veryl HEAD roll accepted by CI |

Nightly versions use the next patch plus the channel, source commit timestamp,
and abbreviated source revision, for example
`0.1.36-nightly.head.20260805123456.g0123456789ab`. Publishing the same commit is
idempotent. **Queue Nightly Packages** dispatches the existing trusted publisher
workflow from each source branch so npm provenance names the actual source
commit. Manual dispatch can publish either nightly channel when run from its
corresponding branch, or retry a stable tag. The packages remain one lockstep
distribution in every channel.

## Repository configuration

Release automation uses the `celox-release-please` GitHub App, installed for this
repository with repository contents and pull request write access. Store its
numeric App ID as the `RELEASE_APP_ID` Actions variable and its complete PEM
private key as the `RELEASE_APP_PRIVATE_KEY` Actions secret. Each job mints a
short-lived, repository-scoped installation token; never store an installation
token itself because it expires. The weekly queue job uses this token so adding a
pull request to the queue starts the required `merge_group` checks. The Veryl
HEAD and vendored-metadata workflows use the same mechanism so generated branch
pushes start normal CI. Using the default `GITHUB_TOKEN` for these events would
prevent their follow-up workflows from running.

The App has Issues write permission for Release Please's status labels. The
weekly workflow still identifies the release pull request by the exact
`release-please--branches--master--components--celox` branch from this repository.
It rechecks the `release:hold` label while waiting and disables auto-merge or
removes an existing queue entry before returning when the release is held.

Configure merge commits to use the pull request title as the merge commit title.
This preserves the Conventional Commit title consumed by Release Please. Protect
`master` and `develop` and require both **Conventional Commit title** and the
normal CI checks; do not allow those checks to be bypassed by automation. Enable
repository auto-merge so dependency rolls, lane synchronization, and weekly
releases can queue checked pull requests. Allow the automation token to create
`develop` on first use and force-update the disposable `integration/veryl-head`
branch; never grant a force-push exception on `master` or `develop`.
