# sv-tests conformance

CI runs the elaboration and simulation tests of
[sv-tests](https://github.com/chipsalliance/sv-tests) at the commit in
[`revision`](revision) against the SystemVerilog frontend, and fails when any
result differs from [`expected.tsv`](expected.tsv).

Each test runs in the mode sv-tests picks for a simulator: simulation when its
`:type:` includes `simulation`, else elaboration. Preprocessing- and
parsing-only tests are not run.

- **Elaboration:** Celox succeeds when it builds the top module.
- **Simulation:** Celox also runs the design until `$finish` or until no event
  is left, and succeeds when the run ends without a runtime error or `$fatal`.

A test passes when Celox succeeds, or fails if the test is marked
`:should_fail_because:`. A simulation test additionally passes only if every
`:assert: <expression>` line it prints (with `$display`, `$error`, ...)
evaluates true. As in the sv-tests log parser, the expressions are Python and
are evaluated with `python3`.

| Result | Meaning |
| --- | --- |
| `pass` | succeeded with every `:assert:` true, or failed as the test expects |
| `fail` | failed a valid test, accepted an invalid one, or printed a false `:assert:` |
| `no_top` | the sources declare no module (class- and package-only tests) |
| `crash` | the build or the simulation exited abnormally |
| `timeout` | the build and simulation took longer than 60 seconds |

A rejection that the test expects may come from an unsupported construct
rather than from the error the test describes.

## Running locally

```bash
git clone https://github.com/chipsalliance/sv-tests.git ../sv-tests
git -C ../sv-tests checkout "$(cat conformance/sv-tests/revision)"
cargo run --release -p celox --features systemverilog --example sv_tests -- \
    ../sv-tests/tests --expected conformance/sv-tests/expected.tsv --report report.tsv
```

`report.tsv` lists each test's tags, mode, top module and the first diagnostic,
runtime error or false assertion.
After reviewing a change in the results, add `--update` to rewrite
`expected.tsv`. Update `revision` and `expected.tsv` together.
