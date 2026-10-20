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
  Clock domains derived from that clock run in the same tick when their clock
  rises: a gated `clk & en` together with `clk`'s own domain, a
  register-driven divider after it. A tick is the clock's active edge only:
  domains clocked by its opposite edge, such as `~clk`, do not run.
- `sim.dispose()` frees the native resources.

::: tip Combinational logic
For purely combinational modules (`always_comb` only), `tick()` is not required. Reading an output automatically evaluates the combinational logic with the current inputs.
:::

### Clock edges

`tick()` stands for a clock's active edge and leaves the clock signal alone.
When a design also reacts to the other edge, for example a register on `~clk`
or on a falling `clk & en`, drive the edges themselves:

```typescript
const clk = sim.event("clk");

sim.posedge(clk);      // clk = 1: domains sensitive to its rising edge run
sim.negedge(clk);      // clk = 0: domains sensitive to its falling edge run
sim.activeEdge(clk);   // the clock's own edge: rising for a posedge clock
sim.inactiveEdge(clk); // the opposite edge
sim.cycle(clk);        // activeEdge(), then inactiveEdge()

// Several clocks at one instant: every domain samples the state before them.
sim.edges([
  [clk, "posedge"],
  [sim.event("clk_b"), "posedge"],
]);
```

An edge moves the clock signal and leaves it at its new level, so it can be
read through `sim.dut`. An edge to the level the signal already has does
nothing. Each edge runs the domains a timed simulation would run at that
transition: the clock's own domain on its active edge, clocks computed from
it (`clk & en`, `~clk`) that see their active edge, sampling the state from
before the edge, and the domains that the resulting updates clock in turn. For
Veryl's abstract `clock` type, `activeEdge()` follows the build's clock type
(`clockType: "negedge"` makes it the falling edge). Reset ports work the same
way: `negedge(rst)` asserts an active-low asynchronous reset.

`cycle()` gives the same results as two timed steps of the clock, a rising
and a falling one.

## Time-Based Simulation

Simulation timestamps and clock periods count global-precision ticks. Veryl
uses 1ps per tick. SystemVerilog uses the design's global time precision
(default 1ps). VCD headers and VPI time properties queried with a NULL handle
use that global precision. VPI queries on a module or its signals return the
module's own time unit and precision.
Existing Veryl VCD exports now declare 1ps instead of 1ns; their numeric
timestamps are unchanged. Native images compiled before this change must be
recompiled for the updated container format.

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

Pass `{ maxSteps }` to `runUntil()` to bound the number of steps. Both forms run in the Rust scheduler:

```typescript
// Unbounded
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

### HDL output bytes

Rust runtime events (`RuntimeEvent::{Display, Write, AssertContinue, AssertFatal}`)
and detailed testbench assertion messages return `Vec<u8>`. JavaScript
`runTest`, `runTestFromProject`, and `runTestWithFrontendArtifact` return
assertion messages as `Uint8Array`. `%s` and `%c` may contain non-UTF-8 bytes or
NUL, so byte payloads are never decoded implicitly. Display/write forwarding
also writes the bytes directly.

Decode explicitly when the design emits UTF-8 text:

```ts
const text = new TextDecoder("utf-8", { fatal: true }).decode(
  result.assertions[0]?.message,
);
```

In Rust, use `String::from_utf8(message)` to validate UTF-8, or compare the
bytes directly. Simulator errors and test failure summaries are human-readable
diagnostics: invalid UTF-8 payloads appear as escaped bytes in those summaries,
while the original bytes remain in the event or assertion result.

This replaces the earlier string message API; update string comparisons to
byte comparisons or explicit decoding.

### Veryl HEAD queues

The `develop` lane supports Veryl HEAD's `$tb::queue::<T, N>` in native
`#[test]` modules. `N = 0` (the default) is unbounded. Bounded queues discard
pushes when full. The supported methods are `push_back`, `push_front`,
`pop_back`, `pop_front`, `drop_back`, `drop_front`, `back`, `front`, `clear`,
`size`, `is_empty`, and `is_full`. Handles are shared by the test's initial
blocks and reset for each execution.

Elements can be signed, wide, or packed structs. Direct four-state values and
selections retain X/Z; empty four-state reads return X. The native expression
VM does not support compound runtime four-state arguments to queue pushes;
these produce an explicit diagnostic. Two-state expressions are supported.
These native components cannot currently be emitted for Verilator or Icarus.
