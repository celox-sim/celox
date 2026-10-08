# sv-tests conformance

CI runs the elaboration tests of [sv-tests](https://github.com/chipsalliance/sv-tests)
at the commit in [`revision`](revision) against the SystemVerilog frontend, and
fails when any result differs from [`expected.tsv`](expected.tsv).

A test passes when Celox builds its top module, or rejects it if the test is
marked `:should_fail_because:`. Tests whose `:type:` does not include
`elaboration` (preprocessing and parsing only) are not run. Simulation tests
are run as elaboration tests: their `$display` assertions are not checked.

| Result | Meaning |
| --- | --- |
| `pass` | built, or rejected as the test expects |
| `fail` | rejected a valid test, or accepted an invalid one |
| `no_top` | the sources declare no module (class- and package-only tests) |
| `crash` | the build exited abnormally |
| `timeout` | the build took longer than 60 seconds |

A rejection that the test expects may come from an unsupported construct
rather than from the error the test describes.

## Running locally

```bash
git clone https://github.com/chipsalliance/sv-tests.git ../sv-tests
git -C ../sv-tests checkout "$(cat conformance/sv-tests/revision)"
cargo run --release -p celox --features systemverilog --example sv_tests -- \
    ../sv-tests/tests --expected conformance/sv-tests/expected.tsv --report report.tsv
```

`report.tsv` lists each test's tags, top module and the first diagnostic.
After reviewing a change in the results, add `--update` to rewrite
`expected.tsv`. Update `revision` and `expected.tsv` together.
