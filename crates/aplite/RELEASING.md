# Releasing Aplite

Aplite has its own package version and release-please configuration:
`release-please-aplite-config.json` and `.release-please-aplite-manifest.json`
at the repository root. Its version is not inherited from `workspace.package`.
The initially empty manifest lets the Rust release strategy start at 0.1.0.

The Aplite Release Management workflow opens a separate release PR, updates
this crate's manifest and changelog plus only Aplite's entry in the shared
Cargo.lock, and creates `aplite-vX.Y.Z` tags and GitHub Releases after that PR
merges. Release PRs are reviewed and queued explicitly. Creating an Aplite
release dispatches `publish-aplite.yml` at that exact tag. The workflow checks
the version and published GitHub Release, runs Aplite tests and verifies its
standalone package, then publishes with a short-lived crates.io OIDC token.
The Celox publisher does not publish Aplite. Changes to both consumers and
Aplite can require both releases.

Trusted Publishing requires an existing crate. The empty `aplite 0.0.0`
bootstrap reserves the name; the first implementation release remains 0.1.0.
Configure the crates.io Trusted Publisher for owner `celox-sim`, repository
`celox`, workflow `publish-aplite.yml`, and environment `crates-io`. That GitHub
environment must allow `aplite-v*` tags. No persistent registry token is needed.

To retry a failed publication, dispatch the workflow at the reviewed release tag:

```sh
gh workflow run publish-aplite.yml --repo celox-sim/celox --ref aplite-v0.1.0
```

An already published version is skipped. Branch refs, mismatched versions,
draft releases and prereleases are rejected. Keep Aplite's release cadence
independent of Celox.

The shared workspace remains the development environment. Changes to the solver
also run the lydite consumer tests and proof gates. The full RV32I acceptance
campaign requires the pinned frontend and release checker documented under
`lydite/docs/rv32i.md`; unit tests are not a replacement for that campaign.
