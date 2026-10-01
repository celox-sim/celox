# Proof-backed Veryl corpus conformance

This job executes **all665 original Rust test bodies** from pinned Celox revision
`124a1315096d21b85d9d0d84fd7139363a181cad`. Its acceptance contract is
**663 actual passes /665**, with two explicitly blocked positive fixtures.
The raw test executable still exits1 and reports those two cases as failed.
The coverage gate accepts only their exact source/design identities and
`InvalidForRange::NegativeBound` diagnostics. It never changes their expectations
into expected compiler rejections.

The663 passes comprise652 observation-checked cases, three smoke-only cases,
and eight genuine expected compilation rejections. These are test-suite results,
not a claim of unbounded equivalence, complete Veryl support, or verification of
all possible stimuli. The independent narrow `../veryl` job is retained.

## Reproduce

Requirements: Rust1.98.1 (with rustfmt), Python3.12, Git, access to the pinned public
Celox source and ordinary crates.io dependencies. No Python packages or external
SMT solver are required.

```sh
rustup toolchain install 1.98.1 --profile minimal --component rustfmt
rustup override set 1.98.1
./conformance/veryl-proof/run_ci.sh /tmp/veryl-proof-evidence
```

Choose a fresh evidence directory. The same command runs in CI. It prepares
checksum-verified sources, applies named patches, builds three independent locked
Cargo workspaces, runs semantic and failure-control tests, executes the full
corpus, compares the reviewed coverage manifest, runs independent compiler/word
matrices, and replays saved finite queries. The solver service is built in release
mode for predictable runtime. Every build has an explicit target directory.
`CELOX_SOURCE_REPOSITORY` may point to a local mirror, but the commit and every
original suite file must still match. The job never downloads prebuilt adapters,
reads historical results as proof, or automatically blesses a new golden.

Generated dependencies and compiler caches live under `work/`, binaries under
`target/`; neither is uploaded or checked in. A checkout does not require the
repository's historical `results/` evidence. CI has read-only contents access,
no repository secrets, no credential persistence, no cache restore, and an
always-upload evidence step. A source or build error still produces a failed
run-status artifact.

## Architecture and semantics

1. The Rust harness links the untouched upstream reusable test suite. Its original
   reads, writes, clocks, host computations, assertions, panics and compile-error
   expectations execute normally. No expected-output values are sent over the
   Backend protocol or generated from the hardware implementation.
2. A compile-only Rust frontend uses the actual Veryl parser/analyzer and Celox
   typed elaboration, hierarchy flattening and ScheduledRtl/SIR generation. It
   never instantiates a simulator, JIT or code generator. Widths, signedness,
   state regions, event aliases, NBA snapshots and signal paths come from this
   typed pipeline. The reusable suite's existing two FF-function semantic
   extensions are explicit and recorded in `allowed_diagnostics`; this
   is a patched frontend contract, not stock Veryl acceptance.
3. Python builds Boolean/bit-vector equations from generic SIR operations.
   Writes and scheduled state updates create SSA definitions. Sparse immutable
   interval storage retains array/state snapshots without dense allocation.
   Wide values are encoded as32-bit chunks (tested through2048 bits); solver
   scalar words remain at most64 bits. Four-state payload/mask equations preserve
   X=(1,1), Z=(0,1), and typed two-state coercions.
4. Before any read, branch or dynamic address is concretized, the finite service
   proves SAT(prefix relation), validates that SAT assignment against the original
   formula, then proves UNSAT(prefix AND value differs from the candidate).
   Only a uniquely proved bit pattern is returned to the original Rust code.
   The relation never adopts a merely selected model state. All earlier external
   observations remain separate from the hardware equations.
5. Named `tick(event)` and lazy combinational evaluation implement the upstream
   Backend contract. Two-state storage starts at zero plus explicit initial
   writes; four-state logic starts unknown plus typed explicit initialization.
   General waveform timing outside the Backend's named-event API is not claimed.
   Display/Write/AssertContinue arguments are proved and logged; console delivery
   is outside this Backend API. Activated fatal events fail closed.
6. SSA compaction is allowed only for pure definition relations, after feasibility
   and joint uniqueness of every active definition. Extra relational assumptions
   prevent compaction; nonunique free variables remain symbolic. Saved snapshots
   substitute only proved values. Wide division reuse likewise requires uniquely
   proved operands and a previously proved quotient/remainder circuit.

The in-process Rust service calls only `hwverify_solver::finite::solve_with_hint`.
No Z3 library, executable, SMT-LIB fallback or simulator is used on this path.
An executable tripwire is tested before running, then armed on PATH; any call
fails the job. Unknown solver results, budgets, transport failures, malformed
replies, unsupported operations and compiler panics fail closed. Other hwverify
jobs/tests may separately use external solvers; they are not this path.

## Coverage and audit contract

`upstream-sources.json` pins all original source bytes, including Rust assertions.
`coverage-manifest.json` pins every case, original expectation/category, source
location/hash, actual design/protocol identity, read/operation counts, and exact
compiler-rejection disposition. It contains hashes and counts, not expected
hardware output tables. Protocol hashes may indirectly reflect read-dependent
stimuli; they are execution-coverage drift guards requiring review, not independent
specification oracles. They are never fed into Backend constraints and never
replace original Rust assertions. Every invocation checks the exact665-case set, duplicate
or missing cases, backend close verdicts, validated SAT/UNSAT query audit,
observation uniqueness and poisoned-observation counterexamples. A wrong
expected sample must produce a validated SAT counterexample for every observed
case. HDL mutation controls also show unchanged original Rust assertions failing.

The two blocked cases are:

- `flip_flop::test_ff_constant_signed_bounds_in_unrolled_loops`
- `synth_dynamic_loop::test_constant_signed_bounds_in_unrolled_synth_loops`

Both require negative constant loop bounds that the pinned language elaborates
as unsigned. A different diagnostic, a legal-source mutation, or an unexpected
successful compile fails the stale blocked contract. There is no catch-all skip
or allow-failure step. Eight independent negative fixtures pass only after real
source rejection; a compiler crash or empty diagnostic never qualifies.

Evidence includes raw per-case results, original design inputs, scheduled SIR,
backend protocol identity, finite queries and epoch relations (gzip), dependency
provenance, executable hashes, negative controls and independent matrix results.
`replay_audit.py PROOF_DIRECTORY` reruns those exact queries. These are replayable
query audits, **not independently checked UNSAT certificates**. The compiler,
relation encoder, finite SAT implementation and UNSAT result remain in the
trusted computing base. SAT models are additionally evaluated against the
original formula.

## Pinned dependency patches

`dependencies.json` records the exact upstream revision, Veryl0.21.0 crate SHA256,
and separate ordered patch hashes. `prepare.py` rejects mismatches before use and
rechecks that original suite sources were not patched. The stack fixes scalar
inout bindings, function snapshot substitution, formal-width constant context,
first-dimension `$size`, direct runtime `$onehot`/`$clog2` acceptance, and numeric
cast signedness. The runtime-function patch is an explicit frontend extension;
constant-only contexts remain rejected. See
[`../../audit/veryl_proof_independent`](../../audit/veryl_proof_independent/README.md)
for separate investigations and compiler/formula boundary checks.

Celox code uses its MIT/Apache-2.0 licenses; the Veryl analyzer and imported suite
carry the included Veryl MIT license. Original sources are fetched at the pinned
revision rather than vendoring a clone. Only adapter code, dependency locks,
small patches, licenses and deterministic manifests are committed.
