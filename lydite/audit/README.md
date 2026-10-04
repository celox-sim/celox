# Tests and verification evidence

Start with [usage](../docs/usage.md), [proof trust boundaries](../docs/trust.md)
and the [selected RV32I contract](../docs/rv32i.md). This directory contains
the evaluators and regression controls that the required gates run, with the
machine-readable summaries they compare against. Historical counts and source
hashes describe their recorded runs, not the current checkout's CI status.

One-off investigation campaigns, sealed archives and their raw evidence stay in
the [original hwverify repository](https://github.com/tignear/hwverify/tree/ae8f84cca8859097ac2b0a7a4f6e311355c2b48d/audit), where
the commands below the "Sealed archives" heading must be run.

## Fresh required gates

Run from `lydite/` with fresh output directories outside the checkout.
The [workflow](../../.github/workflows/lydite.yml) is the authoritative ordering.
The original-corpus preparation builds the pinned frontend needed by the later
symbolic/RV gates; a failed dependency fetch is a setup failure, never a pass.

| Gate | Command or instructions |
|---|---|
| Language, Rust and documentation | [Development checks](../docs/usage.md#required-checks) |
| Original finite Veryl adapter | `./conformance/veryl/run_ci.sh /tmp/new-veryl`; [scope/dependencies](../conformance/veryl/README.md) |
| Original 665-case proof-backed corpus | `./conformance/veryl-proof/run_ci.sh /tmp/new-proof-corpus`; [coverage contract](../conformance/veryl-proof/README.md) |
| Symbolic RTL, capacity, register and memory reuse | `./conformance/veryl-symbolic/run_ci.sh /tmp/new-symbolic`; [adapter](../conformance/veryl-symbolic/README.md) |
| Native/API lemma diagnostics | `python3 -m audit.lemma_candidates.ci --out /tmp/new-lemmas` |
| Selected RV32I architecture, memory, latency and mutations | `./conformance/veryl-symbolic/run_rv32i_ci.sh /tmp/new-rv32i` |
| Fixed automatic proofs | `./conformance/veryl-symbolic/run_automatic_ci.sh /tmp/new-automatic` |
| Equality sharing | `python3 -m audit.equality_sharing.ci --checker ../target/release/lydite --out /tmp/new-sharing` |
| Whole-word frontiers | `python3 -m audit.word_frontier.ci --checker ../target/release/lydite --expected-checker-sha256 "$CHECKER_SHA" --out /tmp/new-frontiers` |
| Guarded expansion | `python3 -m audit.guarded_expansion.ci --checker ../target/release/lydite --expected-checker-sha256 "$CHECKER_SHA" --out /tmp/new-expansion` |

For the last two commands, set `CHECKER_SHA=$(sha256sum ../target/release/lydite | cut -d' ' -f1)` after the locked release build. The fixed matrices keep all positive,
held-out, renamed, mutation and explicitly Unknown rows; do not reduce them to a
single successful fixture. Hosted CI uploads each gate's fresh raw artifacts.
The native RV gate requires the source-authored instruction/delivery lemmas to
contribute to closure, along with successful guarded use and source diagnostics.

## Recorded evidence and independent checks

| Area | Machine-readable records / executable checks |
|---|---|
| Automatic discovery | [Evidence summary](automatic_proof/evidence-summary.json), [fixed matrix](automatic_proof/ci-matrix.json), `audit.automatic_proof.test_generate`, `audit.automatic_proof.test_ci` |
| Compound sharing | [Evidence summary](equality_sharing/evidence-summary.json), `audit.equality_sharing.test_generate`, `test_evidence`, `test_ci` |
| Word frontiers | [Evidence summary](https://github.com/tignear/hwverify/blob/ae8f84cca8859097ac2b0a7a4f6e311355c2b48d/audit/word_frontier/evidence-summary.json), `audit.word_frontier.test_generate`, `test_ci`, `rv_address_replay` |
| Guarded expansion | [Current orientation summary](guarded_expansion/orientation-summary.json), [initial summary](guarded_expansion/evidence-summary.json), `audit.guarded_expansion.test_generate`, `test_ci` |
| Native lemmas | [Validation summary](lemma_candidates/validation-summary.json), Rust `lemma_candidates` / `native_lemmas` tests, `audit.lemma_candidates.test_migrate` |
| Selected source proofs and mutations | `audit.veryl_scaling.test_rv32i_*`, `rv32i_selected_ci`; [sealed selected records](rv32i_milestone/evidence/manifest.json) |
| CPU and memory composition | [Contracts and commands](../docs/memory-composition.md), `audit.veryl_scaling.test_cpu_*`, readonly/writable array differential scripts |
| Language and quantified/scoped examples | `scripts/test_json_to_lyd.py`, Rust CLI/syntax/verify tests; historical JSON/logs in `language_independent`, `syntax_0_8_independent`, `prime_0_9_independent`, `bare_quantifiers_independent`, `composition_independent` |
| Concrete CPU oracle and counterexample replay | `cpu_simulation.py`, `replay_counterexamples.py`, `interpreter.py`; `cpu_simulation_results.json`, `counterexample_replay_results.json` |
| Structural/finite solver campaigns | `structural_solver`, `structural_independent`, `finite_solver_independent`, `finite_speed_independent`, `branch_solver_scaling*`, `branch_solver_decomposition*`, `expected_result_search*`; corresponding JSON/log/query trees under `results/` |
| Physical implementation | [Synthesis reproduction](https://github.com/tignear/hwverify/blob/ae8f84cca8859097ac2b0a7a4f6e311355c2b48d/synthesis/README.md), [emitted-SV differential/mutation instructions](veryl_scaling/split_candidate_tests/README.md) |
| Small memory-law formalization | [Lean proof and Rust comparison](../proof/README-ja.md) |

Independent integer interpretation and SAT replay supplement the checker; they do
not certify UNSAT. The old counterexample-first policy under `counterexample_search*`
is a rejected experiment; the active caller-directed search is described in
[the engine guide](../docs/automatic-proofs.md). Frozen independent review records
in `results/*/REPORT.md` retain their exact historical identities and limitations.
Do not run historical helpers over their tracked outputs; use fresh destinations
and inspect any original-machine paths first.

## Sealed archives

These archives live in the original repository. They retain unique original SMT queries, reports, witnesses and source
snapshots. They are preserved with their existing per-member manifests and
verification rules. The old source-distribution ZIP is unnecessary: its 2,098
files are exactly recoverable from Git commit `4e6daa4e`. Do not add source ZIPs
or duplicate progress reports for routine changes. New release archives belong
with release assets when genuinely needed; existing unique verification evidence
must not be discarded merely because a newer run exists.

Verify the recorded bundles without executing archived programs:

```sh
python3 -m unittest audit.rv32i_milestone.test_verify_evidence
python3 -m audit.rv32i_milestone.verify_evidence
python3 -m audit.automatic_proof.verify_phase1_evidence
python3 -m audit.equality_sharing.publication.verify_evidence
python3 -m audit.word_frontier.publication.verify_evidence
python3 -m audit.guarded_expansion.verify_evidence
```

Their `--extract` option requires a new external destination. Guarded expansion
uses the supplied prefix for two distinct bundle destinations. All member hashes,
paths, sizes and embedded artifact maps are checked before extraction; absolute
historical paths inside files stay unchanged. Use each manifest's `relocation_map`
when inspecting those paths. `--check-originals` needs the original machine's
roots and is not required for portable verification. A failed second extraction
pass may leave a partial new directory; never treat it as success.

The two older `structural_solver/*.tar.gz` experiments are separately bound by
[archive_manifest.json](https://github.com/tignear/hwverify/blob/ae8f84cca8859097ac2b0a7a4f6e311355c2b48d/audit/structural_solver/archive_manifest.json). Their initial
and pre-hash timings differ and are not replacements for the final recorded run.
They retain unique bytes and are not duplicates of each other or the checkout.

Hash verification checks integrity, not theorem validity or authorship. Saved
reports never recreate live proof handles. Historical manifests and the five
sealed synthesis READMEs retain their original bytes, even where their prose
mentions old paths or provisional status. Current commands and claims are in the
linked guides above; do not rewrite historical provenance to look current.
