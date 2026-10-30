# Native Verilator setup

This composite action builds an immutable upstream Verilator commit on Ubuntu,
including Ubuntu 26.04, instead of depending on distribution-specific release
archives. Call it after checkout:

```yaml
- uses: ./.github/actions/setup-verilator
  with:
    version: "5.052"
    revision: ea338be98e1e838d3518809ce8899f85a009963c
```

The action installs build/runtime dependencies, builds and installs upstream's
normal optimized and debug tools with two jobs, then adds `bin` to `PATH` and
sets `VERILATOR_ROOT`. It follows upstream's
[build instructions](https://github.com/verilator/verilator/blob/v5.052/docs/guide/install.rst).
The version is verified against the installed executable; the requested source
commit is checked against the checkout and recorded in the installation.

The exact installation cache key includes the source commit, Ubuntu release,
architecture, compiler and libc versions, and the build recipe. Dependencies
are installed and the executable, source identity and VPI headers are checked
on cache hits too. A cache from another Ubuntu release or build recipe cannot
be restored through a fallback key. Builds stay outside the measured Taxi
compilation and execution intervals.

`verilator-setup.yml` checks Ubuntu 26.04 installation and a second job's cache
restore using a real cocotb 2.1/VPI design. The smoke test compiles a model and
checks all 256 input values, including wraparound.
