# Proof-backed Veryl corpus conformance

This job executes **every case of the in-tree reusable suite**
(the Veryl suite of `crates/celox-test-suite`) with reads backed by finite proofs, compiled by
the in-tree Celox frontend and the workspace Veryl. Its acceptance contract is the
reviewed `coverage-manifest.json` plus `case-exceptions.json`: currently
**720 actual passes / 744** and 24 recorded exceptions. The raw test executable
still exits 1 and reports the exceptions as failed. The coverage gate accepts each
exception only with its exact recorded failure and design identities; it never
changes an expectation into an expected compiler rejection.

The 720 passes comprise 709 observation-checked cases, three smoke-only cases and
eight genuine expected compilation rejections. These are test-suite results, not a
claim of unbounded equivalence, complete Veryl support, or verification of all
possible stimuli. The independent narrow `../veryl` job is retained.

## Reproduce

Requirements: the repository Rust toolchain (with rustfmt), Python3.12, Git and
ordinary crates.io dependencies. No Python packages or external SMT solver are
required.

```sh
./conformance/veryl-proof/run_ci.sh /tmp/veryl-proof-evidence
```

Choose a fresh evidence directory. The same command runs in CI. It records the
in-tree Celox revision, uncommitted compiler/suite paths and suite file hashes in
`work/provenance.json`, builds `lydite-celox` (the SIR exporter and suite runner)
and the release solver service in the workspace, runs semantic and
failure-control tests, executes the whole suite, compares the reviewed coverage
manifest, runs independent compiler/word matrices, and replays saved finite
queries. The job never downloads prebuilt adapters, reads historical results as
proof, or automatically blesses a new golden.

Run state and compiler caches live under `work/`, binaries under the repository
`target/`; neither is uploaded or checked in. A checkout does not require the
repository's historical `results/` evidence. CI has read-only contents access,
no repository secrets, no credential persistence, no cache restore, and an
always-upload evidence step. A source or build error still produces a failed
run-status artifact.

## Architecture and semantics

1. The Rust harness (`lydite-celox-suite`) links the unchanged reusable test suite.
   Its reads, writes, clocks, host computations, assertions, panics and
   compile-error expectations execute normally. No expected-output values are sent over the
   Backend protocol or generated from the hardware implementation.
2. A compile-only Rust frontend uses the actual Veryl parser/analyzer and Celox
   typed elaboration, hierarchy flattening and ScheduledRtl/SIR generation. It
   never instantiates a simulator, JIT or code generator. Widths, signedness,
   state regions, event aliases, NBA snapshots and signal paths come from this
   typed pipeline. The reusable suite's existing two FF-function semantic
   extensions are explicit and recorded in `allowed_diagnostics`; this
   is a frontend contract, not stock Veryl acceptance.
3. Python builds Boolean/bit-vector equations from generic SIR operations.
   Writes and scheduled state updates create SSA definitions. Sparse immutable
   interval storage retains array/state snapshots without dense allocation.
   Wide values are encoded as32-bit chunks (tested through2048 bits); solver
   scalar words remain at most64 bits. Four-state payload/mask equations preserve
   X=(1,1), Z=(0,1), and typed two-state coercions.
4. Before any read, branch or dynamic address is concretized, the finite service
   proves SAT(prefix relation), validates that SAT assignment against the original
   formula, then proves UNSAT(prefix AND value differs from the candidate).
   Only a uniquely proved bit pattern is returned to the suite case.
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

The in-process Rust service calls only `lydite_solver::finite::solve_with_hint`.
No Z3 library, executable, SMT-LIB fallback or simulator is used on this path.
An executable tripwire is tested before running, then armed on PATH; any call
fails the job. Unknown solver results, budgets, transport failures, malformed
replies, unsupported operations and compiler panics fail closed. Other lydite
jobs/tests may separately use external solvers; they are not this path.

## Coverage and audit contract

`coverage-manifest.json` pins every case, expectation/category, source
file and a hash of its parsed script, actual design/protocol identity, read/operation counts, and exact
compiler-rejection disposition. It contains hashes and counts, not expected
hardware output tables. Script comments, whitespace and diagnostic positions
are omitted from the identity; HDL source strings remain verbatim. Source line
numbers stay in the per-run catalogue for diagnostics. Thus moving an unrelated
case or editing a script comment does not rewrite the reviewed manifest.
Protocol hashes may indirectly reflect read-dependent
stimuli; they are execution-coverage drift guards requiring review, not independent
specification oracles. They are never fed into Backend constraints and never
replace suite assertions. Every invocation checks the exact suite case set, duplicate
or missing cases, backend close verdicts, validated SAT/UNSAT query audit,
observation uniqueness and poisoned-observation counterexamples. A wrong
expected sample must produce a validated SAT counterexample for every observed
case. HDL mutation controls also show unchanged suite assertions failing.

`case-exceptions.json` lists each case allowed to fail, with its kind, reason
and exact failure message:

- `veryl_language_restriction`: Veryl rejects the design. Two cases need
  negative constant loop bounds (`InvalidForRange::NegativeBound`), and four call
  runtime `$clog2`/`$onehot` outside a function, which Veryl rejects as
  unsynthesizable (veryl-lang/veryl#2604).
- `unsupported_by_proof_backend`: twelve cases drive their design from a native
  Veryl testbench (`Backend::run_testbench`), which this backend does not run.
- `known_celox_failure`: five cases that the Celox frontend gets wrong today and
  that Celox's own tests also ignore on every backend. Four of them depend on
  logic declared inside an interface, which the Veryl analyzer IR drops.
- `celox_unsupported`: one case whose design the Celox frontend refuses with a
  typed `Unsupported` error (generic interface ports, #1088). The bridge reports
  it as `frontend_unsupported`, which is neither a source rejection nor a
  backend fault: a `CompilationError` case cannot pass through it, and the gate
  accepts it only under this exception kind.

A different failure, a legal-source mutation, or an unexpected success fails the
stale exception, which must then be reviewed. There is no catch-all skip or
allow-failure step. Eight independent negative fixtures pass only after real
source rejection; a compiler crash or empty diagnostic never qualifies.

### Updating the contract

Adding, editing executable content or removing a suite case, or changing what the frontend accepts,
changes the contract. Rerun the gate, then review the candidate before adopting
it:

```sh
./conformance/veryl-proof/run_ci.sh /tmp/new-proof   # fails on the manifest mismatch
diff <(jq -S . conformance/veryl-proof/coverage-manifest.json) <(jq -S . /tmp/new-proof/coverage/coverage-candidate.json)
```

Copy `coverage-candidate.json` over `coverage-manifest.json` only after checking
that every disposition change is intended. A new failure must be fixed, or
recorded in `case-exceptions.json` with its kind, reason and exact failure.

Evidence includes raw per-case results, original design inputs, scheduled SIR,
backend protocol identity, finite queries and epoch relations (gzip), dependency
provenance, executable hashes, negative controls and independent matrix results.
`replay_audit.py PROOF_DIRECTORY` reruns those exact queries. These are replayable
query audits, **not independently checked UNSAT certificates**. The compiler,
relation encoder, finite SAT implementation and UNSAT result remain in the
trusted computing base. SAT models are additionally evaluated against the
original formula.

## Compiler and suite source

The exporter and suite runner build from the in-tree Celox crates with the
workspace Veryl, without patches. See
[`../../audit/veryl_proof_independent`](../../audit/README.md)
for separate investigations and compiler/formula boundary checks.

Celox code uses its MIT/Apache-2.0 licenses; the Veryl analyzer and suite designs
carry the included Veryl MIT license.
