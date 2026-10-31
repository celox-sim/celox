# lydite

A hardware refinement checker written in Rust. It checks the reset, transitions and
bindings of an implementation against a specification, matching each implementation
commit to one specification step and everything else to a stutter. Input is `.lyd`
or declarative JSON.

## Getting started

Use the repository Rust toolchain (`rust-toolchain.toml`) and Python 3.12. The default
solver route needs Z3; `LYDITE_SOLVER=finite` selects the finite Bool/BV route, which
never falls back to an external solver.

```sh
cargo build --release --locked
# Check syntax, names and types only (not a proof)
../target/release/lydite audit/lemma_candidates/counter.lyd --check --out /tmp/lydite-check
# Verify a small example; --out must be a new directory
LYDITE_SOLVER=finite ../target/release/lydite audit/lemma_candidates/counter.lyd --out /tmp/lydite-counter
```

## Documentation

| Topic | See |
|---|---|
| Running, exit codes, development and CI checks | [Usage](docs/usage.md), [Editor / LSP](docs/usage.md#editor-and-language-server) |
| `.lyd` syntax and its JSON mapping | [Language](docs/language.md), [JSON v2](docs/schema.md) |
| Relational and scoped specifications | [v3](docs/specifications.md), [v4 scoped](docs/scoped-specifications.md), [Expectations and traces](docs/expectations.md) |
| Program invariants and termination | [Program contracts](docs/program-contracts.md) |
| Lemmas and automatic proof search | [Lemmas](docs/lemmas.md), [Proof engine](docs/automatic-proofs.md) |
| What is proved and what is trusted | [Trust boundary](docs/trust.md) |
| CPU models | [Small CPU models](docs/models.md), [RV32I](docs/rv32i.md), [Memory composition](docs/memory-composition.md) |
| Reproduction tests and archived evidence | [Audit index](audit/README.md) |

## Current scope

- The selected 4-stage RV32I implementation passes all 436 preservation conditions
  and 7 global obligations with hand-written lemmas, including composition with
  4/16/64-word memories. Without lemmas, automatic search reaches 431/436.
- The reusable Celox Veryl test suite (745 cases) runs proof-backed in CI: 721 pass and
  24 are recorded exceptions (see `conformance/veryl-proof/case-exceptions.json`).

The Rust translation, obligation generation, kernel and selected solver are trusted;
see the [trust boundary](docs/trust.md). Current results come from
[CI](../.github/workflows/lydite-checks.yml), not archived reports.

The in-process finite backend is [Aplite](../crates/aplite/README.md), a separately
versioned workspace crate. Lydite retains proof construction and orchestration;
its existing `lydite_solver::finite` API re-exports the Aplite implementation.
