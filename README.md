# Celox

High-speed JIT simulator for [Veryl HDL](https://veryl-lang.org/). Compiles Veryl designs to native x86-64 machine code for near-hardware simulation performance.

## Performance

Celox's native backend generates optimized x86-64 code directly, outperforming both Cranelift JIT and Verilator on key benchmarks:

| Benchmark | Celox (native) | Cranelift JIT | Verilator |
|---|---|---|---|
| counter_n1000 (sequential) | **245 ns/tick** | 395 ns/tick | 392 ns/tick |
| linear_sec_p6 (combinational) | **9.8 ns/eval** | 140 ns/eval | 19 ns/eval |

## Features

- **Native x86-64 Backend** — Custom compiler pipeline (SIR → MIR → regalloc → x86-64) with SIR-level optimization (store coalescing, vectorize-concat, identity alias bypass), MIR optimization (constant folding, GVN, PEXT fusion), and 32-bit narrow emit
- **Cranelift JIT Fallback** — Automatically used on non-x86-64 platforms (ARM, RISC-V)
- **Event-Driven Scheduling** — Multi-clock domain support with combinational clock cascade
- **4-State Simulation** — IEEE 1800-compliant X/Z propagation
- **TypeScript Testbenches** — Type-safe signal access with Vite integration
- **VCD Waveform Output** — Generate VCD files for waveform inspection

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

## Architecture

```
Veryl → SLT → SIR → [SIRT optimizer] → native x86-64 (or Cranelift IR → JIT)
```

The native backend compiles SIR execution units into optimized machine code through:

1. **SIR-level EU merge** — Combines multiple execution units into one function, enabling cross-EU commit forwarding and eliminating per-EU prologue overhead
2. **SIRT passes** — Store-load forwarding, commit sinking, working memory elimination, coalesced store splitting
3. **ISel** — SIR → MIR lowering with known-bits tracking, constant folding, Cmp+Branch fusion
4. **MIR optimization** — Global value numbering (dominator-tree scoped), algebraic simplification, redundant mask elimination, if-conversion, CFG simplification
5. **Register allocation** — Unified single-pass Braun & Hack MIN algorithm
6. **x86-64 emit** — 32-bit register mode, branch fall-through optimization

## Workspace Structure

| Crate / Package | Description |
|---|---|
| `crates/celox` | Core simulator (IR, native backend, JIT compilation, runtime) |
| `crates/celox-macros` | Procedural macros |
| `crates/celox-napi` | N-API bindings for Node.js |
| `crates/celox-ts-gen` | TypeScript type generation library |
| `packages/celox` | TypeScript runtime package |
| `packages/vite-plugin` | Vite plugin for development integration |

## Documentation

- [Guide](https://celox-sim.github.io/celox/guide/introduction) — Introduction and tutorials
- [API Reference](https://celox-sim.github.io/celox/api/) — TypeScript API docs
- [Internals](https://celox-sim.github.io/celox/internals/architecture) — Architecture and design

## License

MIT OR Apache-2.0
