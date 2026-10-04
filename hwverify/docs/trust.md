# Proof meaning and trust boundaries

A successful run establishes the obligations generated for the supplied model
and environment. It does not establish that the specification captures the
intended hardware, or that the checker implementation is formally correct.

## Validation and model semantics

The parol AST is source structure, not proof. Syntax lowers to canonical v2
`Design`, v3 relational `Specification`, or v4 `ScopedSpecification` data.
Shared IR validators check names, widths, expressions, every assignment and wire
DAG (including unused declarations), contract fields and state-only restrictions
before solving. JSON duplicate keys are rejected. Validated model fields are
private and immutable through their public APIs; no generated Rust is trusted.

Reset is active-high synchronous priority. Next-state assignments are simultaneous.
The v2 binding relates spec/implementation state. At an implementation commit the
spec takes one step; otherwise it stutters. Reset establishment, preservation,
commit eligibility, optional hold, nonvacuity and conditional progress are separate
obligations. Binding and rank must depend only on state. A finite unsigned rank
must decrease on enabled noncommit transitions. This does not guarantee progress
through infinite external stalls or repeated reset, or termination of an arbitrary
program. [Program contracts](program-contracts.md) add explicit invariants,
postconditions and termination obligations under their environment conditions.

V3/v4 finite examples and safety bridges have their own
[relational semantics](specifications.md), [scoped composition](scoped-specifications.md)
and [quantifier ordering](expectations.md). They do not inherit the v2 progress
claim. Nonempty relations and one-step induction countermodels do not establish
reset reachability. A countermodel may expose a weak invariant rather than a
reachable hardware bug.

## Trusted implementation

The trusted path includes Rust typing/lowering and obligation generation,
expression normalization, the structural UNSAT kernel, the selected finite solver
or Z3, and relevant compiler/importer and composition rules. SIR/Veryl support has
an explicit synchronous two-state model; see [the lifter](../../crates/hwverify-sir/README.md).
Low-level Rust term constructors are implementation APIs, not a soundness proof.

The finite backend validates SAT assignments against the original formula and
context. Unsupported operators, malformed results, exhausted budgets and failed
replay remain errors/Unknown. Its UNSAT results and the structural kernel are
trusted. The [Lean memory-rule proofs](../proof/README-ja.md) and independent
concrete interpreters test particular boundaries; they do not certify all Rust
code, lowering, the SAT solver or imported RTL semantics. Future Verus/Lean-style
verification is not an implemented guarantee.

## Checked proof composition

Automatic search and [source-authored lemmas](lemmas.md) propose typed claims,
guards and dependencies. They cannot modify the original model or add environment
premises. Only a fresh checked sequent creates an opaque, session-bound handle.
Use discharges the entire guarded antecedent. Rewrites and branch joins must
close the exact original premise/conclusion; both sides of a guard are required.
Dependencies cannot create same-query induction by cycling.

Proof programs retain the exact original environment, separately from formals,
and reject stale or substituted aliases before matching a query. Diagnostic query
indices identify live attempts; human labels and source locations confer no
proof authority. A valid unused lemma or a vacuous false guard is not evidence
that the target closed or that a state is reachable.

CPU/memory composition additionally binds the actual imported source, interfaces,
wiring, helpers and tool identities and requires fresh independent component
proofs in the same session. See [memory composition](memory-composition.md) and
[selected RV32I](rv32i.md). Loading a successful report cannot mint a handle.

## Budgets and evidence

Finite primitive queries default to 100M work, 1M allocated clauses and 10 seconds.
Whole-query finite branch splitting shares its limits; independent lemma bundles
have explicitly larger aggregate cost. Failed earlier attempts remain recorded.
See [proof-engine modes](automatic-proofs.md) for the distinction. Work counters,
kernel costs and wall time are not interchangeable; Unknown time is not a proof
speedup. A timeout is not a process-wide hard real-time guarantee.

SHA-256 binds recorded bytes to an expected manifest, not their truth or author.
A trusted checkout supplies the manifest trust anchor. Saved SMT, reports, source
snapshots and archives are replay/integrity evidence, not independently checked
UNSAT certificates. Historical results keep their original source/tool identity
and paths. Fresh CI must pass for the commit being reviewed; archived passes
cannot substitute for it. FPGA timing, concrete simulation and token latency
remain separate from architectural refinement.
