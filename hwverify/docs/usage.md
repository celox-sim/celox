# Usage and development

Run commands from the repository root. Use Rust 1.98.1 and Python 3.12, with
locked Cargo dependencies. Z3 5.1.0 is the recorded compatibility-test version;
set `Z3_BIN` for tests using the external-solver route. See the
[trust boundary](trust.md) before interpreting a successful result.

## Inputs and outputs

```sh
cargo build --release --locked
# External solver route:
target/release/hwverify-rs examples/cpu.json --z3 /path/to/z3 --out /tmp/new-cpu
# Validate only; --emit-json also implies --check:
target/release/hwverify-rs examples/memory_increment.hwv --check --out /tmp/new-check
target/release/hwverify-rs examples/array_sum.hwv --emit-json /tmp/array-sum.json --out /tmp/new-lowering
# Finite-only route, with source-authored proposals:
HWVERIFY_SOLVER=finite target/release/hwverify-rs audit/lemma_candidates/counter.hwv --out /tmp/new-counter
# Attach a native lemma module to an existing design:
HWVERIFY_SOLVER=finite target/release/hwverify-rs design.json --lemmas lemmas.hwv --out /tmp/new-proof
```

Replace illustrative `design.json`/`lemmas.hwv` with your files. Use fresh output
directories: the CLI defaults to `results` and writes reports/query evidence.
`.hwv` is detected by extension; `--format hwv|json` overrides detection. The
[native lemma guide](lemmas.md) includes complete examples and failure meanings.

| Exit | Meaning |
|---|---|
| 0 | Verification succeeded, or validation-only succeeded when requested |
| 1 | Counterexample |
| 2 | Invalid input or tool error |
| 3 | Unknown or inadequate contract |

`--check` success does not establish a theorem. Read `report.json` and the
individual obligations; do not convert Unknown, a failed nonvacuity check, or
an unfinished child into success. `.smt2`, `.out` and diagnostic JSON record the
actual queries and attempts.

`HWVERIFY_SOLVER=finite` selects the bounded Bool/BV solver with no Z3 fallback.
`--finite-search-hint query|sat|unsat` (or `HWVERIFY_FINITE_SEARCH_HINT`) affects
search order only. The default `query` follows the obligation's expected result.
A hint does not assert that result. Quantified examples have a separate solver
route and are not general finite-backend quantifier support.

## Required checks

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
Z3_BIN=/path/to/z3 cargo test --release --workspace --locked
cargo build --release --locked
HWVERIFY_BIN="$PWD/target/release/hwverify-rs" python3 -m unittest discover -s scripts -p 'test_json_to_hwv.py'
python3 scripts/check_docs.py
```

Use the release profile for the complete solver suite: some debug-profile tests
need a larger test-thread stack. Keep proof limits unchanged. The Markdown check
covers local links and fragments in all tracked Markdown, including the sealed
historical records retained unchanged.

The mandatory hosted gates are defined in
[the workflow](../.github/workflows/veryl-fv.yml). They include the original Veryl
corpus, symbolic RTL refinement, native lemma diagnostics, selected RV32I, fixed
automatic-proof controls and original-formula mutants. For their exact commands,
dependency preparation and evidence locations, use the [audit index](../audit/README.md).
Do not replace these gates with an old report or a reduced smoke run.

## Source layout

`crates/ir` validates typed models; `crates/syntax` uses the parol grammar and
source spans; `crates/solver` owns finite/Z3 solving and checked proof rules;
`crates/verify` builds refinement/specification obligations; `crates/cli` provides
the command line. [SIR lifting](../crates/sir/README.md) connects imported RTL.
`examples/` and `audit/` contain generators, independent interpreters and tests.

`python3 scripts/json_to_hwv.py INPUT.json OUTPUT.hwv` is a migration printer.
Its output still passes the same semantic validation as handwritten source.
Keep generated runs outside tracked evidence directories. Publish new release
archives as release assets only when needed; ordinary source and test changes
belong in Git. Existing sealed evidence is indexed separately and is never proof
authority for a new run.
