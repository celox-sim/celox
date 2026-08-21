# Release policy

Celox is released as one lockstep distribution. `VERSION`,
`.release-please-manifest.json`, the npm packages, and all Rust workspace crates
must always carry the same version. The public Rust entry points are `celox`,
`celox-frontend-sdk`, and the Rust adapter API in `celox-napi`; the remaining
published `celox-*` crates satisfy their Cargo dependency graph and remain
implementation details.

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

## crates.io publication

The same tagged stable release is published to crates.io. Nightly channels stay
npm-only because crates.io has no mutable distribution tags. The release
management workflow dispatches **Publish Rust Crates** at that tag after it
verifies that the tag points at the current release commit and that the GitHub
Release is public.
The Rust workflow repeats those checks, verifies every package archive, runs the
external-frontend tests (including loading a separately built N-API addon), and
then publishes in dependency order. A retry skips crate versions already present
on crates.io.

Release Please updates the workspace version, exact internal dependency
requirements, and all matching entries in `Cargo.lock`. `scripts/check-release-version.mjs`
rejects a release if any of those values differs from `VERSION`.

Publication uses crates.io Trusted Publishing. The publish job has
`id-token: write`, runs in the `crates-io` GitHub environment, and exchanges its
GitHub OIDC identity for a temporary crates.io token. There is no persistent
`CARGO_REGISTRY_TOKEN` Actions secret. A manual retry must select the existing
stable tag as the workflow ref. It also requires a published GitHub Release, so
it is safe to use for a failed or partially completed publication retry.

If the publishing implementation itself must be fixed after a partial release,
create a numbered `vX.Y.Z-recovery.N` tag from the reviewed fix commit and run
**Publish Rust Crates** from that tag with `release_tag` set to `vX.Y.Z`. The
workflow requires the source tag to use that exact recovery form, requires
the recovery tag to descend from the stable release tag, requires `VERSION` to
match the stable release, and skips crate versions already visible on crates.io.

Create a GitHub environment named `crates-io` before the first release. Limit
deployments to protected tags matching `v*` and add required reviewers if
publication should require a human approval.

### First crates.io release

crates.io cannot configure a trusted publisher for a crate name that has never
been published. Bootstrap the names locally before the first stable release;
the bootstrap is deliberately not a GitHub Actions job:

1. After this release setup is merged, validate the generated empty `0.0.0`
   placeholder packages locally:

   ```bash
   ./scripts/bootstrap-crates-io.sh package
   ```

2. Create a short-expiry crates.io API token with both the `publish-new` and
   **Manage trusted publishing configurations** endpoint scopes, and a crate
   scope covering `celox` and `celox-*`. Do not save it in GitHub Actions.
3. Pass the token without putting it in shell history, then publish the
   placeholders and register their Trusted Publisher configurations. The
   confirmation is required because crates.io releases are permanent:

   ```bash
   read -rsp 'crates.io bootstrap token: ' CARGO_REGISTRY_TOKEN
   echo
   export CARGO_REGISTRY_TOKEN
   ./scripts/bootstrap-crates-io.sh publish BOOTSTRAP-CELOX-CRATES
   unset CARGO_REGISTRY_TOKEN
   ```

   The script uses the crates.io API to register this exact identity after each
   placeholder becomes visible:

   | Field | Value |
   | --- | --- |
   | GitHub owner | `celox-sim` |
   | GitHub repository | `celox` |
   | Workflow filename | `publish-crates.yml` |
   | Environment | `crates-io` |

   Re-running the command skips published placeholders and correctly configured
   publishers. It stops instead of deleting or replacing any unexpected
   Trusted Publisher configuration. If crates.io rate-limits new crate creation,
   unset the token, wait until the reported retry time, and run the same command
   again to resume.
4. Revoke the bootstrap API token immediately. Add the intended crates.io team
   or backup owners, because the account that performed the bootstrap is the
   initial owner of every new crate.
5. Run the first stable release normally. It publishes the real lockstep version
   through OIDC; `0.0.0` remains only as the name bootstrap. After this succeeds,
   optionally enable **Trusted Publishing Only** for each crate so ordinary API
   tokens cannot publish future versions.

Adding another public crate later requires the same bounded bootstrap only for
that new name. The local script skips existing `0.0.0` placeholders and Trusted
Publisher configurations; existing crates continue to publish through OIDC.

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

Release automation uses the `celox-automation` GitHub App, installed for this
repository with Actions, repository contents, pull request, and Workflows write
access. Store its numeric App ID as the `RELEASE_APP_ID` Actions variable and
its complete PEM private key as the `RELEASE_APP_PRIVATE_KEY` Actions secret.
Each job mints a short-lived, repository-scoped installation token; never store
an installation token itself because it expires. Actions write access lets the
release workflow dispatch the crate publisher at the verified release tag;
Workflows write access lets automation update workflow files. The weekly queue
job uses this token so adding a pull request to the queue starts the required
`merge_group` checks. The Veryl HEAD and vendored-metadata workflows use the
same mechanism so generated branch pushes start normal CI. Using the default
`GITHUB_TOKEN` for these events would prevent their follow-up workflows from
running.

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

Do not create a `CARGO_REGISTRY_TOKEN` repository or environment secret. The
only manually created crates.io credential in this process is the short-expiry
bootstrap token, which must be revoked after the initial crate names are
created.
