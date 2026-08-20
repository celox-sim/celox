# Celox

[![npm version](https://img.shields.io/npm/v/%40celox-sim%2Fcelox.svg)](https://www.npmjs.com/package/@celox-sim/celox)
[![crates.io](https://img.shields.io/crates/v/celox.svg)](https://crates.io/crates/celox)

**An experimental, compiler-based RTL simulator for [Veryl](https://veryl-lang.org/).**

Celox compiles an elaborated Veryl design into executable simulation kernels and
exposes the design through a type-safe TypeScript API. It is both a practical
way to test RTL with Vitest and an open testbed for exploring how RTL simulators
should be structured.

[Try the Playground](https://celox-sim.github.io/celox/playground/) ·
[Read the guide](https://celox-sim.github.io/celox/guide/introduction) ·
[Use the starter template](https://github.com/celox-sim/celox-template) ·
[Browse the API](https://celox-sim.github.io/celox/api/)

## Why Celox exists

Celox explores a simple question: what does a modern RTL simulator architecture
look like when compilation, scheduling, state representation, code generation,
and testbench integration are designed together?

The project makes those boundaries explicit:

- Veryl-specific analysis ends at a source-independent design representation.
- Combinational dependencies and clock domains are scheduled before execution.
- A backend-independent IR and state layout are shared by multiple code
  generators.
- Native and WebAssembly execution use the same runtime contract.
- The testbench sees the same typed design API regardless of the execution
  backend.

This makes Celox useful as an architecture laboratory without reducing it to a
compiler demo: the result can run real RTL tests, in Node.js or in a browser.

## What you can do today

- Import `.veryl` modules directly into TypeScript with generated port and
  hierarchy types.
- Write assertions, fixtures, and parameterized tests with Vitest.
- Choose explicit event-based stepping or scheduled, time-based simulation.
- Exercise multiple clock domains and combinational clock cascades.
- Enable four-state simulation and drive or inspect `X` and `Z` values.
- Override top-level parameters and include test-only Veryl sources.
- Inspect child instances and emit VCD waveforms.
- Compile a Veryl design into a native executable and run cocotb tests through VPI.
- Build an external netlist frontend with the Rust SDK and ship its simulator as
  a native application binary or a frontend-specific N-API/WASI addon.
- Run through the custom x86-64 backend, the Cranelift fallback, or WebAssembly.
  A custom AArch64 backend is also available behind an experimental feature.

## Quick start

The fastest way to start is the
[`celox-template`](https://github.com/celox-sim/celox-template). For an existing
Veryl project, install Celox, its Vite plugin, and Vitest:

```bash
npm add -D @celox-sim/celox @celox-sim/vite-plugin vitest
```

Enable the plugin in `vitest.config.ts`:

```typescript
import { defineConfig } from "vitest/config";
import celox from "@celox-sim/vite-plugin";

export default defineConfig({
  plugins: [celox()],
});
```

Given a Veryl module such as `src/Adder.veryl`:

```veryl
module Adder (
    a: input  logic<16>,
    b: input  logic<16>,
    sum: output logic<17>,
) {
    always_comb {
        sum = a + b;
    }
}
```

you can import the design and test it as a typed object:

```typescript
import { describe, expect, test } from "vitest";
import { Simulator } from "@celox-sim/celox";
import { Adder } from "../src/Adder.veryl";

describe("Adder", () => {
  test("adds two values", () => {
    const sim = Simulator.create(Adder);

    try {
      sim.dut.a = 100n;
      sim.dut.b = 200n;
      expect(sim.dut.sum).toBe(300n);
    } finally {
      sim.dispose();
    }
  });
});
```

The Vite plugin analyzes the project and generates TypeScript sidecars, so port
names, signal values, and visible hierarchy are checked by TypeScript. See the
[Getting Started guide](https://celox-sim.github.io/celox/guide/getting-started)
for the required `Veryl.toml` and `tsconfig.json` setup.

## Two simulation styles

`Simulator` gives a test direct control over events. It is a good fit for
combinational blocks and cycle-oriented unit tests:

```typescript
const sim = Simulator.create(Counter);
sim.dut.enable = 1n;
sim.tick();
expect(sim.dut.count).toBe(1n);
sim.dispose();
```

`Simulation` manages clocks and simulation time. It is intended for multi-clock
and time-oriented scenarios:

```typescript
const sim = Simulation.create(Counter);
sim.addClock("clk", { period: 10 });
sim.reset("rst");
sim.runUntil(100);
expect(sim.time()).toBe(100);
sim.dispose();
```

## Architecture

```text
Veryl source
    │
    ▼
frontend analysis and hierarchy elaboration
    │
    ▼
symbolic logic and dependency scheduling
    │
    ▼
Simulator IR (SIR) and backend-independent optimization
    │
    ▼
shared physical state layout
    │
    ├──► native x86-64
    ├──► Cranelift JIT
    ├──► WebAssembly
    └──► native AArch64 (experimental)
             │
             ▼
      event-driven runtime
             │
             ▼
    Rust / Node.js / browser hosts
```

The shared pipeline is deliberate. Scheduling and RTL semantics do not have to
be reimplemented for every target, while backend experiments can still own
their instruction selection, machine IR, register allocation, and code
emission. The runtime separates next-state evaluation from commit when clock
domains trigger together, then propagates combinational changes until the step
settles.

For details, see the
[architecture overview](https://celox-sim.github.io/celox/internals/architecture),
[compiler components](https://celox-sim.github.io/celox/internals/compiler-crate-architecture),
and [SIR reference](https://celox-sim.github.io/celox/internals/ir-reference).

## Project scope

Celox is under active development. Its current focus is synchronous RTL written
in Veryl and tested at the design level. It is not a general SystemVerilog
simulator, a gate-level timing simulator, or an implementation of detailed
delta-cycle event semantics.

That narrower scope is intentional: it keeps the simulator small enough to make
architectural changes, compare execution strategies, and test new compiler and
runtime boundaries in a complete working system. Expect unsupported constructs
and API changes while the design is still evolving.

## Documentation

- [Getting Started](https://celox-sim.github.io/celox/guide/getting-started)
- [Writing Tests](https://celox-sim.github.io/celox/guide/writing-tests)
- [Celox CLI and cocotb](https://celox-sim.github.io/celox/guide/cocotb)
- [External Frontends and Rust Binaries](https://celox-sim.github.io/celox/guide/external-frontends)
- [Four-State Simulation](https://celox-sim.github.io/celox/guide/four-state)
- [VCD Waveforms](https://celox-sim.github.io/celox/guide/vcd)
- [TypeScript API](https://celox-sim.github.io/celox/api/)
- [Simulator Internals](https://celox-sim.github.io/celox/internals/architecture)
- [Release Policy](.github/RELEASING.md)

## Development

Celox is a Rust and pnpm workspace. The main local checks are:

```bash
cargo test
pnpm install
pnpm run build:napi
pnpm run build
pnpm test
```

Architecture discussions, bug reports, and focused experiments are welcome in
[GitHub Issues](https://github.com/celox-sim/celox/issues).

The repository dev container includes cocotb 2.0.1 and Verilator for native
VPI integration tests and SystemVerilog benchmarks.

## License

Licensed under either [Apache License 2.0](LICENSE-APACHE) or
[MIT](LICENSE-MIT), at your option.
