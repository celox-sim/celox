# Introduction

Celox is a simulator for [Veryl HDL](https://veryl-lang.org/) with type-safe
TypeScript testbenches. Import a `.veryl` module, drive its inputs, advance its
clocks, and assert its outputs with Vitest.

::: tip Try it in your browser
The [Celox Playground](https://celox-sim.github.io/celox/playground/) runs Veryl
designs without installing a local toolchain.
:::

## What you can do

- Import Veryl modules directly from TypeScript with generated port types.
- Test combinational and sequential designs with manual or scheduled clocks.
- Access child-instance ports when a test needs hierarchy visibility.
- Simulate `X` and `Z` values with optional four-state mode.
- Write VCD waveforms for GTKWave, Surfer, and other viewers.
- Override value parameters and include test-only Veryl sources.

Celox compiles the design when the simulator is created. x86-64 hosts use the
native backend by default; other native hosts use the Cranelift JIT, and the
Playground uses WebAssembly. Backend selection is normally automatic and does
not change the TypeScript testbench API.

## Choose a simulation style

Use `Simulator` when a test should control events explicitly. It is a good fit
for cycle-oriented unit tests:

```typescript
const sim = Simulator.create(Counter);
sim.dut.enable = 1n;
sim.tick();
expect(sim.dut.count).toBe(1n);
sim.dispose();
```

Use `Simulation` when a test needs scheduled clocks and simulation time. It is a
better fit for multi-clock or time-oriented scenarios.

## Next steps

Start with [Getting Started](./getting-started.md) to set up a project, then use
[Writing Tests](./writing-tests.md) for the complete testbench workflow. The
[API Reference](/api/) lists the available TypeScript classes and options.

Compiler and runtime design details are kept separately in
[Simulator Architecture](/internals/architecture).
