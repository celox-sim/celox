# Veryl → finite FV conformance gate

This is a reproducible **bounded adapter**, integrated into hwverify CI. It runs
original Celox Rust stimulus/expected-value code, captures symbolic observations,
elaborates the captured Veryl using the actual Veryl parser/analyzer, and checks
all supported finite traces with hwverify's own finite solver. No simulator or
external Z3 supplies the expected outputs.

## Current coverage

Pinned Celox revision: `124a1315096d21b85d9d0d84fd7139363a181cad`
(`celox-test-suite-veryl` 0.8.2). Veryl parser/analyzer: **0.21.0**.

- **665** original cases enumerated, with one disposition each
- **556** cases automatically extracted, **6,142** original assertion predicates
- **46** real cases verified, **118** operation frames, **104** original assertions
- **510** extracted cases rejected by the bounded frontend/analyzer path
- **98** cases have unsupported Rust observation/dataflow forms
- **3** stimulus-only cases have no output assertion
- **8** compilation-rejection fixtures are classified only; they are neither
  verification passes nor claims that the language correctly rejects them

Thus `46 + 510 + 98 + 3 + 8 = 665`. Extraction coverage is not FV coverage.
The earlier prototype verified 9 cases / 17 frames / 19 assertions. Inventorying
all extracted cases first found 18 lowerable by its old frontend; this integration
executes every supported case rather than retaining a selected showcase list.

The checked subset includes unary arithmetic and bitwise operations, reductions,
logic operations, equality and signed/unsigned comparisons, add/subtract/multiply,
bitwise XNOR, saturated dynamic shift counts, ternary expressions, ordinary
conditional statements, casts and constant scalar read-selects. Some constructs
are exercised by additional synthetic source-level tests rather than by an
additional supported upstream case. Explicit casts preserve their typed width
boundary, even when a surrounding comparison's context is one bit. Scalar Rust
reads follow `Scalar::from_bits`: truncate low bits or zero-fill bytes, including
signed Rust types; `bool` reads only bit zero. Equality and inequality are kept as
separate predicates. Repeated assertions are conjoined, never overwritten.

## Run exactly what CI runs

Prerequisites: Rust **1.98.1** with rustfmt, Python **3.12**, Git, a C compiler,
and network access to the pinned public Git repository and locked Cargo crates.
No Python packages, simulator, or Z3 installation is required.

```sh
rustup toolchain install 1.98.1 --profile minimal --component rustfmt
rustup override set 1.98.1
./conformance/veryl/run_ci.sh /absolute/path/to/new-evidence-directory
```

The script fetches the exact Celox commit into a temporary checkout, builds the
current hwverify release binary, builds the real Rust extractor and analyzer,
regenerates the entire trace catalog, validates the committed golden manifest,
and executes every expected-pass case plus negative controls. It does not use a
stored trace/expected-value table or prebuilt executable. Cargo lockfiles pin all
crate dependencies. The analyzer's metadata patch is copied from the same pinned
upstream checkout. The temporary checkout is not uploaded as audit evidence.

For an already available, clean pinned checkout, optionally set
`CELOX_SUITE_ROOT=/path/to/celox/crates/celox-test-suite-veryl` first. The script
checks its exact Git revision and tracked-file cleanliness, then checks source
hashes and captured action hashes against the golden. Generated directories are
ignored; no prototype workspace paths are required. Use a new output directory
for every run so stale reports cannot be mistaken for new results. Python
optimization is rejected because source-mutation controls must execute.

The workflow is [.github/workflows/veryl-fv.yml](../../.github/workflows/veryl-fv.yml):
`push` and `pull_request`, pinned official checkout/upload actions, read-only
contents permission, no persisted credentials, no repository secrets passed to
checks, no cache, and a 30-minute job limit. Audit upload runs even when a check
fails. Hosted status belongs to the specific GitHub run, not to this document.

## Why an assertion can pass

For the complete original fixed input/event schedule, let `R` be the transition
relation and `P_i` be all original assertions at observation frame `i`:

1. Remove **every** expected-output predicate and establish `SAT(R)`
2. For each observation frame, retain the **whole** input/event schedule, remove
   all other expectations, and establish `UNSAT(R AND NOT(P_i))`
3. Pass only if the trace is feasible and every violation query is unsatisfiable

Expected outputs never become assumptions, initial values, transition equations,
or solver-produced fixtures. The catalog additionally changes expected predicates
and confirms the entire design relation is unchanged. Infeasible traces fail;
UNKNOWN never passes. Finite mode is explicit, the only accepted reported
backends are `finite_bv` and `structural_kernel`, and an executable Z3 tripwire is
smoke-tested then armed both as the explicit solver path and `z3` on PATH.

This is nonvacuous universal checking over these **complete finite traces**. It
does not prove arbitrary inputs, every prefix, liveness, unbounded behavior, or
whole-language conformance. hwverify's other legacy/default-Z3 tests are separate
from this workflow and are not claimed to have lost their Z3 dependency.

## Coverage must not silently shrink

`coverage-manifest.json` is a deterministic, reviewed manifest for all 665 cases.
It records the original Rust file/line/hash, captured design hashes, ordered
stimulus/predicate hash, assertion count, exact expected disposition, explicit
unsupported reason, and operation count for expected-pass cases. It contains no
expected-value tables consumed by the checker.

CI regenerates `actual-coverage.json` and requires exact equality. Missing or
extra cases, changed sources/predicates, newly skipped cases, changed rejection
reasons, missing/duplicate solver results, failed checks, UNKNOWN, and fallback
all make CI red. There is no catch-all skip, `continue-on-error`, or automatic
baseline blessing. To inspect a proposed frontend/upstream change without
verification:

```sh
python3 conformance/veryl/catalog.py --catalog-only --out /tmp/new-catalog
```

That command emits a **candidate**, marked `catalog_only_not_verified`; it never
updates the committed manifest. Review the actual diff and run all newly
supported cases/controls before intentionally updating the golden.

First-blocker counts are an inventory, not proof coverage. The current important
ones include functions/interfaces (113), signal/expression widths outside the
subset (69), arrays (55), unsupported statements (43), multiple processes (36),
and partial-write selects (12). Exact per-case details are saved in the evidence.
The pinned analyzer itself rejects some cases and panics on two `inout` fixtures;
these are explicit unsupported analyzer dispositions, never accepted language
rejection tests. The gate pins their identities and cannot silently classify a
new expected-pass case that way.

## Semantic limits and trust boundary

- Scalar signals/expressions from 1 through 64 bits; one module; at most one
  combinational and one positive-edge FF process; whole-signal assignments
- Zero-initialized two-state storage is the Celox **Backend contract**, not a
  general RTL initialization guarantee
- Explicit `eval_comb`/named `tick` schedule; no general HDL scheduler. Initial
  direct reads, implicit settle on reads, pending-input tick with a combinational
  process, negedge clocks, multiple clock/process scheduling and asynchronous
  reset assertion edges reject
- Array/struct/union/interface/hierarchy/function/general loop and case handling,
  division/remainder/power, dynamic selects/partial writes, four-state designs,
  mask assertions and X/Z values remain unsupported where encountered
- No manual Veryl parser, concrete circuit output recorder, circuit-derived
  oracle, per-case semantics table, or solver fallback is used
- The actual Veryl analyzer, this projection/lowering, hwverify's finite solver
  and structural kernel remain trusted software, tested rather than formally
  proven correct

## Controls and evidence

Every one of the 46 upstream baselines has three mandatory negative controls:
negate an assertion (feasible failure), make the initial state impossible
(infeasible failure), and add a contradictory duplicate predicate (feasible
failure). Source-level controls recompile independently mutated original Rust
expectations, Rust stimulus, and Veryl logic; invalid Veryl still captures the
same expected predicates, demonstrating that capture does not derive an oracle
from the hardware.

Rust capture tests cover borrowing, operand order, computed loops, aliases,
scalar/four-state export, lazy diagnostics, and sticky rejection of unhandled
actual-dependent reads. Python tests cover finite-trace nonvacuity,
nondeterminism, cross-frame premise contamination, UNKNOWN, stale results,
source-to-analyzer arithmetic/signedness/branch/projection adversaries, and
coverage-gate mutations. Each synthetic semantic positive has a feasible
negative twin. Test failure aborts the job.

The evidence directory contains `run-status.json`, `run.log`, tool/binary
fingerprints, freshly extracted traces/provenance, `verification/report.json`,
`verification/actual-coverage.json`, source/typed IR/lowered relations, per-query
solver reports, tripwire smoke evidence, and source-mutation results. A missing
runtime tripwire marker is required for success. This repository stores reusable
code, dependency locks, golden coverage hashes and licenses, not generated
clones, caches, executables or historical bulk evidence.

## Provenance and licenses

Original suite and metadata patch:
[celox-sim/celox at the pinned revision](https://github.com/celox-sim/celox/tree/124a1315096d21b85d9d0d84fd7139363a181cad).
Celox copyright (c) 2025 tignear, MIT OR Apache-2.0; notices are in `licenses/`.
The generated copy preserves upstream source byte-for-byte except the central
case macro and explicitly appended recorder support. Assertion source locators
link back to the pinned original files. Veryl registry packages are checksum
locked at 0.21.0; their original notices remain in fetched dependencies.
