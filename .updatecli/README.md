# Updatecli

Updatecli owns the Python runtime and cocotb versions used by the Rust CI job,
and the stable xezim release used by the Nix reference simulator.
Renovate remains responsible for the repository's other dependencies and is
configured not to update the `actions/setup-python` runtime input.

The xezim pipeline selects the newest stable GitHub release, updates the tag in
`flake.nix`, regenerates `flake.lock` and `nix/xezim/Cargo.lock`, and builds and
checks the resulting Nix executable before opening a single PR containing all
three files. An unchanged release is a no-op. Drafts and prereleases are excluded.
Failed regeneration or validation prevents the PR and restores the input files.
Renovate does not update xezim. No manual lockfile update is required.

The Python/cocotb pipeline selects the newest stable cocotb release from PyPI,
then chooses the newest stable CPython minor that has both:

- a cocotb CPython wheel for manylinux x86-64; and
- a Linux x64 build in the `actions/python-versions` manifest.

This keeps the two versions compatible without permanently capping Python. The
regular CI workflow remains the final compatibility check.

Run the policy locally with:

```sh
export UPDATECLI_GITHUB_TOKEN=...
export UPDATECLI_GITHUB_ACTOR=...
updatecli pipeline diff --config .updatecli/updatecli.d
```

Pull requests run the selector unit tests in the required `Lint & Format`
check. The weekly and manual `updatecli.yml` runs repeat those tests and preview
the policy before applying it. Scheduled and manual updates mint a short-lived token for the `celox-automation` GitHub App from the
existing `RELEASE_APP_ID` variable and `RELEASE_APP_PRIVATE_KEY` secret. Pull
requests created with that installation token trigger the regular CI workflows.
