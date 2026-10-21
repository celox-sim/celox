# Upstream SystemVerilog testbench smoke test

Daily full CI runs the unmodified `cc_graycode_tb` from
[PULP common_cells](https://github.com/pulp-platform/common_cells), pinned by
[`revision`](revision). The same job runs on manual `ci.yml` dispatches and on
develop's nightly full CI. It exercises the `celox run --project` CLI with the
SystemVerilog frontend and native timed simulation.

The default 9-bit TB applies 1,536 inputs using loops, `#1` delays, an untimed
task, and immediate assertions. It checks binary/Gray round trips and changes
in the Gray code population count. This is a small combinational design smoke
test, not coverage of all common_cells modules or clocked testbenches.

The gate first requires the original RTL/TB to exit successfully. It then forces
the Gray encoder output to zero in a separate RTL copy and requires exit code 1
with an assertion diagnostic. Compilation errors, crashes and timeouts do not
satisfy this negative control. Upstream files are never edited. Each execution
has a two-minute wall timeout and the CLI's default scheduler step limit.

To reproduce from the repository root:

```sh
revision="$(cat conformance/common-cells/revision)"
git init work/common-cells
git -C work/common-cells fetch --depth 1 https://github.com/pulp-platform/common_cells.git "$revision"
git -C work/common-cells checkout FETCH_HEAD
cargo build --locked -p celox-vpi --features systemverilog --bin celox
node scripts/run-common-cells.mjs work/common-cells target/debug/celox work/common-cells-results
```

The output directory contains the generated `celox.toml`, negative-control
sources, both logs, and a `result.json` written only after both checks pass.
Use a fresh output directory for each run. CI retains this directory for 14 days,
including on failure; failures feed the existing Full CI issue reporter.

Update the pin deliberately after reviewing and running the upstream TB. The
negative-control assignment is checked explicitly so an upstream rewrite cannot
silently remove that check.
