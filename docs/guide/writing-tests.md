# Writing Tests

Celox provides two simulation modes: **event-based** (`Simulator`) for manual clock control, and **time-based** (`Simulation`) for automatic clock generation.

## Event-Based Simulation

`Simulator` gives you direct control over clock ticks. Use it when you want to drive clock edges explicitly, step by step.

```typescript
import { describe, test, expect } from "vitest";
import { Simulator } from "@celox-sim/celox";
import { Reg } from "../src/Reg.veryl";

describe("Reg", () => {
  test("captures input on clock edge", () => {
    const sim = Simulator.create(Reg);

    // Load a value and clock it in
    sim.dut.d = 0xABn;
    sim.tick();
    expect(sim.dut.q).toBe(0xABn);

    // Change input — output should not change until next tick
    sim.dut.d = 0xCDn;
    expect(sim.dut.q).toBe(0xABn);
    sim.tick();
    expect(sim.dut.q).toBe(0xCDn);

    sim.dispose();
  });
});
```

Where `src/Reg.veryl` is:

```veryl
module Reg (
    clk: input  clock,
    rst: input  reset,
    d:   input  logic<8>,
    q:   output logic<8>,
) {
    always_ff (clk, rst) {
        if_reset {
            q = 0;
        } else {
            q = d;
        }
    }
}
```

- `Simulator.create(Module)` creates a simulator instance from a Veryl module definition.
- Signal values are read and written via `sim.dut.<port>`.
- `sim.tick()` advances the simulation by one clock cycle.
- `sim.dispose()` frees the native resources.

::: tip Combinational logic
For purely combinational modules (`always_comb` only), `tick()` is not required. Reading an output automatically evaluates the combinational logic with the current inputs.
:::

## Time-Based Simulation

`Simulation` manages clock generation for you. This is the natural choice for sequential logic with clocked flip-flops.

```typescript
import { describe, test, expect } from "vitest";
import { Simulation } from "@celox-sim/celox";
import { Counter } from "../src/Counter.veryl";

describe("Counter", () => {
  test("counts up when enabled", () => {
    const sim = Simulation.create(Counter);

    sim.addClock("clk", { period: 10 });

    // Assert reset
    sim.dut.rst = 1n;
    sim.runUntil(20);

    // Release reset and enable counting
    sim.dut.rst = 0n;
    sim.dut.en = 1n;
    sim.runUntil(100);

    expect(sim.dut.count).toBeGreaterThan(0n);
    expect(sim.time()).toBe(100);

    sim.dispose();
  });
});
```

- `sim.addClock("clk", { period: 10 })` adds a clock with period 10 (toggles every 5 time units).
- `sim.runUntil(t)` advances simulation time to `t`.
- `sim.time()` returns the current simulation time.

## Testbench Helpers

The `Simulation` class provides convenience methods for common testbench patterns.

### Reset Helper

The active level is determined automatically from the Veryl port type (`reset`, `reset_async_high`, `reset_async_low`, etc.), so you never need to specify the polarity manually.

```typescript
const sim = Simulation.create(Counter);
sim.addClock("clk", { period: 10 });

// Assert rst for 2 cycles (default), then release
sim.reset("rst");

// Custom: hold reset for 3 clock cycles
sim.reset("rst_n", { activeCycles: 3 });
```

### Waiting for Conditions

```typescript
// Wait until a condition is met (polls via step())
const t = sim.waitUntil(() => sim.dut.done === 1n);

// Wait for a specific number of clock cycles
const t = sim.waitForCycles("clk", 10);
```

Both methods accept an optional `{ maxSteps }` parameter. The default step budget is 100,000, but can be changed project-wide via [`[simulation] max_steps` in `celox.toml`](./celox-toml.md#simulation). A `SimulationTimeoutError` is thrown if the step budget is exceeded:

```typescript
import { SimulationTimeoutError } from "@celox-sim/celox";

try {
  sim.waitUntil(() => sim.dut.done === 1n, { maxSteps: 1000 });
} catch (e) {
  if (e instanceof SimulationTimeoutError) {
    console.log(`Timed out at time ${e.time} after ${e.steps} steps`);
  }
}
```

### Timeout Guard for runUntil

Pass `{ maxSteps }` to `runUntil()` to enable step counting. Without it, the fast Rust path is used with no overhead:

```typescript
// Fast Rust path (no overhead)
sim.runUntil(10000);

// Guarded: throws SimulationTimeoutError if budget is exceeded
sim.runUntil(10000, { maxSteps: 500 });
```

## Simulator Options

Both `Simulator` and `Simulation` accept the following options:

```typescript
const sim = Simulator.fromSource(source, "Top", {
  fourState: true,      // Enable 4-state (X) simulation
  vcd: "./dump.vcd",    // Write VCD waveform output
  optLevel: "O1",       // "O0" | "O1" | "O2"
  passOverrides: ["-sir:reschedule"], // per-pass overrides on top of optLevel
  clockType: "posedge", // Clock polarity (default: "posedge")
  resetType: "async_low", // Reset type (default: "async_low")
  deadStorePolicy: "preserveAllPorts", // Dead store elimination policy
  parameters: [         // Top-level parameter overrides
    { name: "WIDTH", value: 16 },
  ],

  // Legacy API remains available for compatibility
  optimize: true,
  optimizeOptions: {
    reschedule: true,
  },
  craneliftOptLevel: "speed", // "none" | "speed" | "speedAndSize"
  regallocAlgorithm: "backtracking", // "backtracking" | "singlePass"
  enableAliasAnalysis: true, // alias analysis in egraph pass
  enableVerifier: true,      // Cranelift IR verifier
});
```

The current recommended entry point is `optLevel` plus `passOverrides`.

- `optLevel: "O0"` prioritizes compile time over simulation speed.
- `optLevel: "O1"` is the standard default.
- `optLevel: "O2"` adds dead store elimination on top of `O1`.

`optimize` and `optimizeOptions` still exist for backward compatibility, but new code is clearer when it uses `optLevel` / `passOverrides`.

### Compile-Time Tuning

If compilation is too slow, start with these knobs:

- **`optLevel: "O0"`** -- reduces Celox-side SIR optimization work.
- **`regallocAlgorithm: "singlePass"`** -- Switches from the backtracking register allocator to a single-pass allocator. Much faster compilation but generates code with more register spills and moves (slower simulation).
- **`craneliftOptLevel: "none"`** -- Disables the egraph optimization pass entirely.
- **`enableVerifier: false`** -- Skips IR verification.
- **`enableAliasAnalysis: false`** -- Disables alias analysis during the egraph pass (only effective when `craneliftOptLevel` is not `"none"`).

## Type-Safe Imports

The Vite plugin automatically generates TypeScript type definitions for your `.veryl` files. When you write:

```typescript
import { Counter } from "../src/Counter.veryl";
```

All ports are fully typed -- you get autocompletion and compile-time checks for port names. All signal port values use `bigint`.

## Running Tests

```bash
pnpm test
```

## Choosing a Factory Method

All three factory methods produce an equivalent simulator — the difference is where the source comes from.

**`Simulator.create(Module)` / `Simulation.create(Module)`** is the standard choice when using the Vite plugin. The imported `Module` carries the project path, so `create` delegates directly to `fromProject` and picks up all source files and `Veryl.toml` settings automatically. Port types are fully generated.

```typescript
import { Adder } from "../src/Adder.veryl"; // generated by Vite plugin
const sim = Simulator.create(Adder);
```

**`fromProject(path, name)`** does the same thing as `create` but takes an explicit path. Use it when you need to point at a project directory without a statically imported module — for example, from a Node.js script that is not part of a Vite build.

```typescript
const sim = Simulator.fromProject("./my-project", "Adder");
```

**`fromSource(source, name)`** compiles a raw Veryl source string with no `Veryl.toml`. Clock and reset settings must be passed explicitly via options. Useful for self-contained tests or when the design lives entirely in the test file.

```typescript
const SOURCE = `
module Adder ( ... ) { ... }
`;
const sim = Simulator.fromSource(SOURCE, "Adder", {
  clockType: "posedge",
  resetType: "async_low",
});
```

## Further Reading

- [4-State Simulation](./four-state.md) -- Using X values in testbenches.
- [Parameter Overrides](./parameter-overrides.md) -- Overriding module parameters at simulation time.
- [Dead Store Elimination](./dead-store-elimination.md) -- Speeding up simulation with DSE.
- [Architecture](/internals/architecture) -- The simulation pipeline in detail.
- [API Reference](/api/) -- Full TypeScript API documentation.
