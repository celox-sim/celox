# Introduction

Celox is a JIT (Just-In-Time) simulator for [Veryl HDL](https://veryl-lang.org/). It lowers Veryl designs through Celox's simulation IR and compiles them to executable code. On x86-64, Celox uses its own native backend by default; on other targets, it falls back to [Cranelift](https://cranelift.dev/).

::: tip Try it in your browser
The [Celox Playground](https://celox-sim.github.io/celox/playground/) lets you write Veryl modules and run simulations directly in the browser -- no installation required.
:::

## Key Features

- **JIT Compilation** -- Veryl designs are compiled through a multi-stage pipeline (Veryl &rarr; SLT &rarr; SIR &rarr; native code) for near-native execution speed.
- **Native x86-64 Backend** -- On x86-64, Celox uses its own code generator for the fastest simulation throughput.
- **Cranelift Fallback** -- On ARM, RISC-V, and other non-x86-64 targets, Celox can still JIT-compile and run designs.
- **Event-Driven Scheduling** -- An event-driven scheduler with multi-clock domain support handles complex timing interactions.
- **4-State Simulation** -- IEEE 1800-compliant 4-state value representation with proper X propagation.
- **TypeScript Testbenches** -- Write testbenches in TypeScript with type-safe signal access and modern developer tooling.
- **VCD Waveform Output** -- Generate VCD files for waveform inspection with standard viewers.

## Project Structure

Celox is organized as a Rust + TypeScript workspace:

| Crate / Package | Description |
|---|---|
| `crates/celox` | Core simulator (IR, native x86-64 backend, Cranelift JIT, runtime) |
| `crates/celox-macros` | Procedural macros |
| `crates/celox-napi` | N-API bindings for Node.js |
| `crates/celox-ts-gen` | TypeScript type generation library |
| `packages/celox` | TypeScript runtime package |
| `packages/vite-plugin` | Vite / Vitest plugin for typed `.veryl` imports |

## How It Works

1. **Frontend** -- The Veryl source is analyzed and lowered into module hierarchy, signals, events, and combinational / sequential blocks.
2. **Middle-end** -- The logic is transformed through SLT and SIR, then optimized for simulation.
3. **Backend** -- On x86-64, Celox emits native machine code directly. On other targets, it uses Cranelift JIT as the fallback execution backend.

For a deeper look at the architecture, see the [Architecture](../internals/architecture.md) internals document.
