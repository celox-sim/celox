# Proof search, checked reuse and budgets

The solver has several decomposition layers. Their coverage and resource contracts
are different; the [trust boundary](trust.md) applies to every layer.

| Layer | Acceptance rule | Resource scope |
|---|---|---|
| Program-contract partitions | Every partition plus coverage; explicit complement retained | Automatic Z3 partitions use 1s each, stop starting new preservation queries after 30s; other/manual obligations retain their own limits |
| Finite Boolean alternatives | All alternatives UNSAT, or original-formula validated SAT | One whole-query work/deadline/allocation budget, including copies and discarded learned clauses |
| Refinement conjunction children | Every original post-conjunct from the full original premise | Failed monolithic attempt plus separately accounted child attempts |
| Checked proof bundles | Fresh auxiliary sequents, exact rewrites, exhaustive guards, original goal | `independent_lemmas` or `shared_query`, as below |

Program invariants, rank and specifications remain user inputs; decomposition is
not invariant synthesis. Candidate partition values are hints, never assumptions.
The structural UNSAT kernel can close supported local expression/memory identities;
unsupported residuals still need a solver. Symbolic addresses are not assumed
distinct. SAT always requires a model replay against the original expression and
context. Inspect the machine-readable plan, query and budget reports rather than
inferring proof from a chosen plan.

## Automatic discovery

```sh
LYDITE_SOLVER=finite LYDITE_AUTOMATIC_PROOFS=independent_lemmas \
  ../target/release/lydite design.json --out /tmp/new-automatic-proof
```

The setting enables conjunctive decomposition and proposes proofs for Unknown
refinement children. Absent/`0` leaves the default behavior unchanged. Invalid
settings, use outside finite mode, or a conflicting explicit proof-program budget
mode are rejected. There is no external solver fallback.

`crates/lydite-solver/src/automatic.rs` compares typed operator trees, proposes frontier
equalities and exhaustive mux splits. Source names, ISA opcodes, numeric term IDs
and saved results do not authorize a proof. Whole-word scheduling can prefer BV
frontiers with variable endpoints before splitting their internal muxes. It is a
bounded heuristic, not arbitrary lemma invention or full congruence saturation.

Checked compound equality sharing collects positive conjuncts and guarded
implication paths from the current premise. Paths may compose or reverse equality
edges, but every contributing edge needs a fresh proof. Guarded edges require
exact premise discharge before use. Cofactored views and guarded word-definition
expansion likewise require a fresh equivalence proof under the complete branch
premise. Both branches must close the original goal. A sibling conclusion is never
silently added as an assumption. Overlapping/cyclic substitutions cannot authorize
recursive rewriting.

Search caps include 100,000 planner visits, three guard levels, eight leaves and
eight frontier equalities per leaf; deadlines are checked between visits at one
second. Equality sharing caps include 64 facts, 4,096 source-scan visits, 4,224
source-plus-adjacency visits, and paths of at most 16 edges/8,192 visits. Cofactor
views are bounded to depth 256 and 100,000 visits. Structural comparisons are not
constant-time or interruptible, so visits are not machine-operation counts.
Current limits and accounting are implemented in the solver and checked by its
regressions; neither a saved plan nor a diagnostic JSON field can bypass them.

## Budget modes

`independent_lemmas` retains the original unsuccessful attempts and gives each
fresh lemma the default 100M-work, 1M-clause, 10-second primitive limits. Its
aggregate budget is intentionally larger than one query. Reports retain all
attempts, planner work and the live proof graph.

`shared_query` debits planning, validation, derived rewrites and proof leaves
against one matched bundle's limits. The structural kernel is disabled on this
strict path. Abandoned search is charged before fallback. The earlier monolithic
conjunction attempt remains separate. Shared mode may lose coverage, but cannot
turn an unfinished attempt into success.

Bundle work includes its primitive work: do not add those overlapping totals.
Allocated clauses include cumulative allocations, not only simultaneously live
clauses. Failed multi-unit budget debits may report slightly more than a cap;
that attempt remains Unknown and is not a successful over-budget proof. Fixed
acceptance gates preserve their exact tested rules, including explicitly allowed
diagnostic Unknown cases.

`--finite-search-hint query|sat|unsat` changes search order, not logical truth.
SAT-oriented probing and resumed search share the query's resources; a contrary
result is still returned normally after validation.

## Current scope and reproducible controls

The selected hint-free RV32I input closes 431/436 children; indices 362, 364,
366, 430 and 434 remain Unknown. The whole result is Unknown. With current manual
native/API proposals, all 436 children and seven globals close. See
[RV32I](rv32i.md) and [lemma authoring](lemmas.md).

The fixed gates preserve positive controls, held-out widths, alpha renaming and
original-formula SAT mutants. Their commands and machine-readable evidence are
listed in the [audit index](../audit/README.md). In particular, the word-frontier
RV address mutations have separate concrete reset-reachable replays: 0/2 obtained
validated full-original Rust microstep SAT. A SAT child or a separate hold failure
must not be relabeled as a full-original Rust counterexample.
