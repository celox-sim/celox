# Aplite

A bounded, in-process constraint solver for quantifier-free Boolean, bit-vector
and finite-observation array expressions. Aplite is extracted from lydite's
finite solver, including its existing bit blasting, CDCL search, decomposition,
model replay and memory-expression normalization.

```rust
use aplite::{Env, Limits, SearchHint, Sort, Verdict, bv, eq, solve_with_hint, var};

let x = var("x".into(), Sort::Bv(8));
let formula = eq(x.clone(), bv(8, 7));
let context = Env::from([("x".to_owned(), x)]);
let result = solve_with_hint(&formula, &context, Limits::default(), SearchHint::Sat);
assert_eq!(result.verdict, Verdict::Sat);
assert!(result.original_formula_validated);
assert_eq!(result.assignments["x"], aplite::Scalar::Bv { width: 8, value: 7 });
```

`SearchHint` chooses search order; it is not an assumption. Unsupported terms,
invalid inputs detected by the solver, and exhausted budgets yield `Unknown`,
which must not be treated as `Unsat`. SAT assignments are evaluated against the
original query and requested context. UNSAT trusts the Rust implementation;
there is no independently checked UNSAT certificate. The expression constructors
are a low-level IR, not a SystemVerilog parser or a complete typed input validator.
The scalar backend uses words up to 64 bits; callers may encode wider values in
chunks. `solve_seeded` selects initial SAT phases using a caller-owned seed for scalar
Boolean/bit-vector queries. The same input and seed reproduce the search within
a solver version, provided budgets permit completion. It returns only replayed
models and preserves `Unknown` on unsupported input or exhausted budgets. Arrays
remain available through `solve`/`solve_with_hint`, not `solve_seeded`.

Seeded search is not uniform model sampling: it must not be used to claim the
legal-combination distribution required by SystemVerilog randomization. Weighted
sampling, `solve before`, and cyclic randomization require a separate policy.

The crate has no dependency on Celox, lydite, Z3, or an external executable.
Lydite retains its proof-obligation generation, proof orchestration, structural
kernel, external-Z3 route and frontend integration. Existing RV32I proof campaigns
exercise that complete pipeline; extracting its finite backend does not turn
those proofs into a standalone Aplite frontend feature.

## Development and releases

Aplite is a member of the Celox Cargo workspace, sharing the lockfile, build
cache and lint configuration. Its explicit package version, changelog and
`aplite-vX.Y.Z` tags are independent of Celox's versions.

Run `cargo test --locked -p aplite` and the lydite consumer tests after changes.
The Lydite CI lane also tests Aplite directly. See [RELEASING.md](RELEASING.md)
for its independent release process.

```rust
use aplite::{Env, Limits, Sort, Verdict, boolv, solve_seeded, var};
let context = Env::from([("data".into(), var("data".into(), Sort::Bv(32)))]);
let result = solve_seeded(&boolv(true), &context, Limits::default(), 42);
assert_eq!(result.verdict, Verdict::Sat);
assert!(result.original_formula_validated);
```
