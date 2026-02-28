# Celox

JIT simulator for [Veryl HDL](https://veryl-lang.org/). Compiles Veryl designs with [Cranelift](https://cranelift.dev/) for high-speed RTL simulation.

## Features

- **JIT Compilation** -- Multi-stage pipeline (Veryl &rarr; SLT &rarr; SIR &rarr; native code) for near-native execution speed
- **Event-Driven Scheduling** -- Multi-clock domain support with proper timing interactions
- **4-State Simulation** -- IEEE 1800-compliant X propagation
- **TypeScript Testbenches** -- Type-safe signal access with modern developer tooling
- **VCD Waveform Output** -- Generate VCD files for waveform inspection

## Quick Start

A ready-to-use project template is available at [`celox-template`](https://github.com/celox-sim/celox-template).

```bash
npm add -D @celox-sim/celox @celox-sim/vite-plugin vitest
```

Write a Veryl module:

```veryl
module Adder (
    clk: input clock,
    rst: input reset,
    a: input logic<16>,
    b: input logic<16>,
    sum: output logic<17>,
) {
    always_comb {
        sum = a + b;
    }
}
```

Write a TypeScript test:

```typescript
import { describe, test, expect } from "vitest";
import { Simulator } from "@celox-sim/celox";
import { Adder } from "../src/Adder.veryl";

describe("Adder", () => {
  test("adds two numbers", () => {
    const sim = Simulator.create(Adder);

    sim.dut.a = 100;
    sim.dut.b = 200;
    sim.tick();
    expect(sim.dut.sum).toBe(300);

    sim.dispose();
  });
});
```

```bash
npm test
```

See the [Getting Started](https://celox-sim.github.io/celox/guide/getting-started) guide for full setup instructions.

## Workspace Structure

| Crate / Package | Description |
|---|---|
| `crates/celox` | Core simulator (IR, JIT compilation, runtime) |
| `crates/celox-macros` | Procedural macros |
| `crates/celox-napi` | N-API bindings for Node.js |
| `crates/celox-ts-gen` | TypeScript type generation library |
| `packages/celox` | TypeScript runtime package |
| `packages/vite-plugin` | Vite plugin for development integration |

## Documentation

- [Guide](https://celox-sim.github.io/celox/guide/introduction) -- Introduction and tutorials
- [API Reference](https://celox-sim.github.io/celox/api/) -- TypeScript API docs
- [Internals](https://celox-sim.github.io/celox/internals/architecture) -- Architecture and design

## License

MIT OR Apache-2.0
