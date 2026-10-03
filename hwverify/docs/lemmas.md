# Shared typed lemma candidates

`LemmaCandidate` and the existing version-1 `proof_programs` data now share a
candidate interface for human authors, LLMs and automatic generators. Candidates
are proposals, never proofs. They cannot change the model, binding or original
environment premises. Every proof and use is checked by the existing live
`ProofBundle` with unchanged budgets and opaque, session-bound handles.

The native `.hwv` frontend reuses the existing parol parser, expressions, typed
`forall` names, prime notation and `use` syntax. It lowers to the same candidate
API; source text cannot create proof handles. It supports `design` proof blocks
and separate `lemmas` modules attached with `--lemmas`.

## Native authoring

```hwv
lemmas "Counter step" {
  forall word: bv<8>;
  target counter_step {
    rhs spec.x';
    lemma step {
      context true;
      guard pre;
      claim impl.x' == spec.x';
    }
    use done: step(context: pre);
    result done;
  }
}
```

A `proof { ... }` block inside a design accepts the same body. See
[`counter.hwv`](../audit/lemma_candidates/counter.hwv) for a complete runnable design and
[`rv-delivery.hwv`](../audit/lemma_candidates/rv-delivery.hwv) for the existing manual RV instruction-transfer
and operand-delivery lemmas and the guard use. The instruction-transfer claim
is written directly as `impl.m_ir' == impl.x_ir` under the original address-query
premise and normal-memory-operation guard. The module replaces these three
named proposals in the existing proof programs; it retains the checked projection and rewrite steps.
The selected-RV CI gate loads this module during its fresh proof run and rejects
missing native source coverage. Native live tests also reject a false claim, a
use lacking the guard, and a cyclic dependency with source-linked diagnostics.

```sh
HWVERIFY_SOLVER=finite target/release/hwverify-rs audit/lemma_candidates/counter.hwv --out /fresh/counter
HWVERIFY_SOLVER=finite target/release/hwverify-rs migrated-rv.json --lemmas audit/lemma_candidates/rv-delivery.hwv --out /fresh/rv
# Syntax, names and widths only; does not establish any claim:
target/release/hwverify-rs audit/lemma_candidates/counter.hwv --check --emit-json /fresh/counter.json
```

`forall` variables are referred to directly, without `formal.`. `impl.x'` and
`spec.x'` denote checker-derived next state; inputs and formals cannot be primed.
`pre`, `goal`, `lhs`, and `rhs` refer to the original query. Named `let` expressions
can be module-wide or local to a target. `use` accepts exactly one `context`
connection naming one of those query aliases or a local/module let. A lemma
requires all of `context`, `guard`, and `claim`. Optional `depends name;` or
`depends compose(a, b);` declares prior checked dependencies; `a.claim` and
`a.pre` describe their sequents without assuming them. Bit widths use existing
`.hwv` literal/type syntax. Optional `mode shared_query;` selects the existing
budget mode; the default is `independent_lemmas`.

A sidecar may replace named proposals and add targets, but cannot change model
fields, environment premises, or an existing program's budget mode. Existing
checked projection, instantiation and rewrite instructions remain available
through canonical proof-program data; this minimal surface does not add syntax
for every kernel operation or scoped specifications. New targets require `rhs`
and `result`. Candidate/use locations are retained in diagnostics, and cycle
and candidate type errors include those locations. Small targets may be proved
by the ordinary solver before a proposed lemma is needed.

## Example

Inside a proof program whose `match_rhs` is `["bv",8,0]`:

```json
{
  "steps": [
    {
      "op": "candidate", "id": "zero", "frame": "current_query",
      "context": true,
      "guard": ["eq", "impl.x", ["bv",8,0]],
      "claim": ["eq", "impl.x", ["bv",8,0]],
      "depends_on": [], "source": "manual/zero"
    },
    {
      "op": "use_candidate", "id": "at_target",
      "candidate": "zero", "context": "$pre"
    }
  ],
  "result": "at_target"
}
```

The claim is valid under its guard. Application succeeds only if the actual
query premise implies that guard, and final closure additionally requires the
exact original conclusion. For an unconstrained `impl.x`, this example diagnoses
a guard failure instead of silently assuming `impl.x == 0`.

The same object can be constructed through
`hwverify_verify::lemma_candidate::LemmaCandidate`, serialized with `to_step`,
read with `from_step`, and resolved with `lower`. `TypedLemmaCandidate` contains
typed terms but carries **no proof authority**. Both entry paths use
`ProofPrograms::from_json` and `try_query`; no generated Rust is loaded or trusted.

## Context, dependencies and timing

A candidate checks `context AND guard => claim`. Factoring a literal `true` keeps
the legacy exact antecedent unchanged. `use_candidate` freshly proves that its
use context implies the **entire** checked antecedent, then calls the existing
kernel application rule. A stronger arbitrary context cannot close an original
query with weaker premises.

`depends_on` identifies prior live checked handles. All handle references in
candidate expressions must be declared. Dependencies are ordering/reuse
constraints, not permission to assume their claims. The existing explicit
`project`, `instantiate`, `apply`, `prepare_rewrite`, `finish_rewrite` and `join`
operations retain their checked semantics. Rewrite-plan getters may describe a
new proposed sequent; they do not prove it.

Dependencies, including expression references and use/composition edges, are
checked for same-query cycles before execution. Errors name the program and
`/steps/N (id)` cycle path. Forward dependencies must be reordered; there is no
implicit induction or simultaneous recursive proof rule. A failed or Unknown
claim stops execution before any dependent handle is available.

`frame: "current_query"` is the only accepted frame. Canonical typed aliases
`impl.x`/`spec.x` denote current state and `impl_next.x`/`spec_next.x` denote the
checker-derived next state, including reset/commit priority. Formal variables
and bit widths use the existing typed expression format. Unsupported frames,
unknown aliases, width mismatches and stale model contexts are rejected. This
interface does not introduce arbitrary history variables or multi-cycle rules.
`source` is an optional nonsemantic location label, not an assumption.

## Diagnostics

Fresh bundle reports add `lemma_candidates`, with separate validity, application
and target-closure fields:

- `proposed`: no successful proof; `claim_true` is null.
- `checked` / `established`: a fresh proof established the guarded claim.
- `applied`: a checked kernel rule consumed the live handle. The target may still
  be open; actual root dependencies determine usefulness, not textual references.
- `lemma_counterexample`: SAT validated against the lemma's own formula. The
  original query replay is reported separately; auxiliary SAT is not a target bug.
- `use_context_does_not_establish_guard`: the lemma is established, but the
  proposed use context does not imply its antecedent.
- `established_but_unused`, `established_but_insufficient` or `target_not_closed`:
  validity did not provide the requested closure.
- `unknown_budget`: a finite limit prevented a decision; no handle is released.
- `target_closed`: the live checker established the original target, independently
  of whether any particular proposed lemma was useful.

Live query indices link to the existing query evidence, counterexample and budget
records. Labels and source strings are descriptive and may collide without
changing attribution. Guard feasibility and reachability are explicitly `not_checked` here.
In particular, a vacuously valid false-guard lemma is not evidence of a reachable
state, and one-step SAT never establishes reachability from reset. Saved reports,
`claim_true` values and numeric proof-node IDs cannot be submitted as authority.

## Manual RV migration and validation

The six existing manual RV programs now pass through `migrate.py`. It preserves
all expressions and replaces only `prove` operations and exact adjacent
premise-discharge/application pairs whose intermediate handle is not reused.
All original model fields, branch complements, substitutions and final goals
remain intact. The current migration executes **27 candidates and 8 explicit
guard uses**, reproducing **436/436** children and **7/7** global obligations.
The hint-free baseline remains **431/436**, with Unknown children
362, 364, 366, 430 and 434. No automatic-coverage gain is claimed.

The required CI gate runs live interface/adversarial tests and migration tests.
The existing selected-RV gate now also requires all six migrated programs and
every expected candidate/use diagnostic; it still requires fresh complete proofs
before issuing composition handles. Both provenance checks include the migration
and acceptance source. See the [validation summary](../audit/lemma_candidates/validation-summary.json) for local measured results.

```sh
python3 -m audit.lemma_candidates.ci --out /fresh/candidate-checks
python3 -m audit.lemma_candidates.migrate old-design.json new-design.json
```

No large evidence archive is added to the repository. Hosted CI retains complete
fresh artifacts through its existing upload steps.
