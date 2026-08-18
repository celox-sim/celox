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

The scheduled workflow uses `GITHUB_TOKEN`. GitHub may require a maintainer to
approve the CI runs on pull requests created with that token.
