# CLAUDE.md

## Project Overview

Celox is a JIT simulator for Veryl HDL. It compiles Veryl designs with Cranelift for high-speed simulation. Future plans include SystemVerilog/Verilog support.

## Build Commands

```bash
cargo build              # Build all crates
cargo test               # Run all tests
cargo test -p celox      # Run tests for the core crate only
cargo run -p celox-ts-gen -- --help  # TypeScript type generator CLI
```

### Snapshot Tests

```bash
cargo insta test         # Run snapshot tests
cargo insta accept       # Accept snapshot changes
```

## Workspace Structure

| Crate / Package | Description |
|---|---|
| `crates/celox` | Core simulator (IR, JIT compilation, runtime) |
| `crates/celox-macros` | Procedural macros |
| `crates/celox-napi` | N-API bindings for Node.js |
| `crates/celox-ts-gen` | CLI tool for TypeScript type generation |
| `packages/celox` | TypeScript runtime package |
| `packages/vite-plugin` | Vite plugin |

## Veryl Submodule

The `deps/veryl/` directory contains a fork of Veryl (`tignear/veryl`). The workspace depends on `veryl-analyzer`, `veryl-parser`, `veryl-metadata`, and `veryl-path` from this submodule.

- `default-features = false` is set on `veryl-parser` to suppress parser regeneration during builds.
- After updating the submodule, run `cargo test` to verify compatibility.

## Rust Edition

This project uses Rust **edition 2024**.
