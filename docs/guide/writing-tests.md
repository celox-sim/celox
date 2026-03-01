# Writing Tests

Celox provides two simulation modes: **event-based** (`Simulator`) for manual clock control, and **time-based** (`Simulation`) for automatic clock generation.

## Event-Based Simulation

`Simulator` gives you direct control over clock ticks. This is useful for combinational logic or when you need fine-grained control over timing.

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

- `Simulator.create(Module)` creates a simulator instance from a Veryl module definition.
- Signal values are read and written via `sim.dut.<port>`.
- `sim.tick()` advances the simulation by one clock cycle.
- `sim.dispose()` frees the native resources.

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
    sim.dut.rst = 1;
    sim.runUntil(20);

    // Release reset and enable counting
    sim.dut.rst = 0;
    sim.dut.en = 1;
    sim.runUntil(100);

    expect(sim.dut.count).toBeGreaterThan(0);
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

```typescript
const sim = Simulation.create(Counter);
sim.addClock("clk", { period: 10 });

// Assert rst=1 for 2 cycles, then release to 0
sim.reset("rst");

// Custom: 3 cycles, active-low reset
sim.reset("rst_n", { activeCycles: 3, activeValue: 0 });
```

### Waiting for Conditions

```typescript
// Wait until a condition is met (polls via step())
const t = sim.waitUntil(() => sim.dut.done === 1);

// Wait for a specific number of clock cycles
const t = sim.waitForCycles("clk", 10);
```

Both methods accept an optional `{ maxSteps }` parameter (default: 100,000). A `SimulationTimeoutError` is thrown if the step budget is exceeded:

```typescript
import { SimulationTimeoutError } from "@celox-sim/celox";

try {
  sim.waitUntil(() => sim.dut.done === 1, { maxSteps: 1000 });
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
  fourState: true,      // Enable 4-state (X/Z) simulation
  vcd: "./dump.vcd",    // Write VCD waveform output
  optimize: true,       // Enable Cranelift optimization passes
});
```

## Type-Safe Imports

The Vite plugin automatically generates TypeScript type definitions for your `.veryl` files. When you write:

```typescript
import { Counter } from "../src/Counter.veryl";
```

All ports are fully typed -- you get autocompletion and compile-time checks for port names, and the correct numeric type (`number` or `bigint`) based on signal width.

## Running Tests

```bash
pnpm test
```

## Further Reading

- [4-State Simulation](./four-state.md) -- Using X and Z values in testbenches.
- [Architecture](/internals/architecture) -- The simulation pipeline in detail.
- [API Reference](/api/) -- Full TypeScript API documentation.
