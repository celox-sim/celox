# Writing Tests

Celox supports writing testbenches in TypeScript, providing type-safe access to your Veryl design signals with modern developer tooling.

## Overview

The TypeScript testbench workflow consists of three parts:

1. **Type Generation** -- `celox-ts-gen` generates TypeScript type definitions from your Veryl design, providing type-safe signal accessors.
2. **NAPI Bindings** -- `celox-napi` exposes the JIT simulator runtime to Node.js via N-API with zero-copy memory sharing.
3. **TypeScript Runtime** -- The `@celox-sim/celox` package provides the high-level API for driving simulations.

## Project Setup

Ensure the NAPI bindings and TypeScript packages are built:

```bash
pnpm build:napi
pnpm build
```

## Writing a Testbench

A typical testbench imports the simulator runtime and the generated types for your design:

```typescript
import { Simulator } from "@celox-sim/celox";

// Create a simulator instance for your design
const sim = await Simulator.create("path/to/your/design.veryl");

// Access signals with type-safe accessors
sim.dut.clk.set(0n);
sim.dut.reset.set(1n);

// Advance simulation
await sim.step();

// Read signal values
const output = sim.dut.result.get();
```

## Clock and Reset

Drive clock and reset signals to initialize your design:

```typescript
// Assert reset
sim.dut.reset.set(1n);
await sim.tick(5); // Hold reset for 5 clock cycles

// Release reset
sim.dut.reset.set(0n);
await sim.tick(1);
```

## Running Tests

Tests can be run with any standard Node.js test runner. The project uses Vitest:

```bash
pnpm test:js
```

## Benchmarks

To run benchmarks with a release build:

```bash
pnpm bench
```

## Further Reading

- [4-State Simulation](../internals/four-state.md) -- How X and Z values are handled.
- [Architecture](../internals/architecture.md) -- The simulation pipeline in detail.
