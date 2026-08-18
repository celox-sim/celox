# Updatecli

Updatecli owns the Python runtime and cocotb versions used by the Rust CI job.
Renovate remains responsible for the repository's other dependencies and is
configured not to update the `actions/setup-python` runtime input.

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

Pull request validation uses the read-only `GITHUB_TOKEN`. Scheduled and manual
updates mint a short-lived token for the `celox-automation` GitHub App from the
existing `RELEASE_APP_ID` variable and `RELEASE_APP_PRIVATE_KEY` secret. Pull
requests created with that installation token trigger the regular CI workflows.
