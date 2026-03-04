# Dead Store Elimination (DSE)

Dead Store Elimination removes stores to signals that are never read, producing faster JIT code. It is especially effective for large designs where most internal signals are unobserved during testing.

## Policies

| Policy | Behavior |
|---|---|
| `"off"` (default) | No elimination — all stores preserved |
| `"preserveTopPorts"` | Eliminate except stores to top-module ports |
| `"preserveAllPorts"` | Eliminate except stores to ports of **all** instances |

## Usage

### With `fromSource` / `fromProject`

```typescript
const sim = Simulator.fromSource(SOURCE, "Top", {
  deadStorePolicy: "preserveTopPorts",
});
```

### With Vite Plugin

Use the `?dse=` query parameter on the import path:

```typescript
import { Top } from "../src/Top.veryl?dse=preserveAllPorts";

const sim = Simulator.create(Top);
```

The policy is baked into the `ModuleDefinition` as `defaultOptions.deadStorePolicy`. You can override it per-call:

```typescript
// Module has ?dse=preserveAllPorts, but this call uses preserveTopPorts
const sim = Simulator.create(Top, {
  deadStorePolicy: "preserveTopPorts",
});
```

`?dse` without a value defaults to `"preserveAllPorts"` (the safe choice).

## Choosing a Policy

### `preserveTopPorts`

Best for **performance-focused benchmarks** where you only need the top-module I/O. Sub-instance signals are eliminated, so `sim.dut.u_sub` will be `undefined`:

```typescript
const sim = Simulator.fromSource(SOURCE, "Top", {
  deadStorePolicy: "preserveTopPorts",
});

sim.dut.top_in = 0xABn;
sim.tick();
expect(sim.dut.top_out).toBe(0xABn);

// Child instance is NOT available
expect((sim.dut as any).u_sub).toBeUndefined();
```

### `preserveAllPorts`

Best for **general testing** — gives you DSE performance benefits while keeping all instance ports accessible for debugging:

```typescript
const sim = Simulator.fromSource(SOURCE, "Top", {
  deadStorePolicy: "preserveAllPorts",
});

sim.dut.top_in = 0x42n;
sim.tick();
expect(sim.dut.top_out).toBe(0x42n);

// Child instance ports remain accessible
expect(sim.dut.u_sub.o_data).toBe(0x42n);
```

::: tip Internal variables
With both DSE policies, internal variables (`var` declarations that are not ports) may be eliminated if no execution unit reads them. Only port signals are guaranteed to survive.
:::

## How It Works

1. The Rust optimizer analyzes which stores are loaded by execution units.
2. Stores that are never loaded are candidates for elimination.
3. The policy adds **externally live** signals to the protected set:
   - `PreserveTopPorts` — top-module port addresses only.
   - `PreserveAllPorts` — port addresses from every instance in the design.
4. On the TypeScript side, `filterHierarchyForDse()` adjusts the DUT hierarchy so that eliminated sub-instances are not exposed as broken accessors.

## Further Reading

- [Child Instance Access](./hierarchy.md) — How the DUT hierarchy works.
- [Vite Plugin](./vite-plugin.md) — Plugin configuration and query parameters.
- [Writing Tests](./writing-tests.md) — Simulator and Simulation patterns.
