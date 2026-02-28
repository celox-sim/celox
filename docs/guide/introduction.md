# Introduction

Celox is a JIT (Just-In-Time) simulator for [Veryl HDL](https://veryl-lang.org/). It compiles Veryl designs using [Cranelift](https://cranelift.dev/) to produce native machine code, enabling high-speed RTL simulation.

## Key Features

- **JIT Compilation** -- Veryl designs are compiled through a multi-stage pipeline (Veryl &rarr; SLT &rarr; SIR &rarr; native code) for near-native execution speed.
- **Event-Driven Scheduling** -- An event-driven scheduler with multi-clock domain support handles complex timing interactions.
- **4-State Simulation** -- IEEE 1800-compliant 4-state (0, 1, X, Z) value representation with proper X propagation.
- **TypeScript Testbenches** -- Write testbenches in TypeScript with type-safe signal access and modern developer tooling.
- **VCD Waveform Output** -- Generate VCD files for waveform inspection with standard viewers.

## Project Structure

Celox is organized as a Rust + TypeScript workspace:

| Crate / Package | Description |
|---|---|
| `crates/celox` | Core simulator (IR, JIT compilation, runtime) |
| `crates/celox-macros` | Procedural macros |
| `crates/celox-napi` | N-API bindings for Node.js |
| `crates/celox-ts-gen` | CLI tool for TypeScript type generation |
| `packages/celox` | TypeScript runtime package |
| `packages/vite-plugin` | Vite plugin for development integration |

## How It Works

1. **Frontend** -- The Veryl source is parsed using the Veryl analyzer. Module hierarchies, signals, and combinational/sequential blocks are extracted.
2. **Middle-end** -- Combinational blocks are symbolically evaluated into a Symbolic Logic Tree (SLT), then lowered to the Simulator Intermediate Representation (SIR).
3. **Backend** -- SIR instructions are compiled to native machine code by Cranelift and executed directly.

For a deeper look at the architecture, see the [Architecture](../internals/architecture.md) internals document.
