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
chunks. The solver does not sample models randomly or promise uniform sampling.

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
