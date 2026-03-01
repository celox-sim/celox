# Type Conversion

This page describes how Veryl types are mapped to TypeScript types when `celox-ts-gen` generates module definitions.

## Conversion Table

| Veryl Type | TS Type | 4-State | Notes |
|---|---|---|---|
| `clock` | *(excluded from ports)* | yes | Becomes an event via `addClock()` / `tick()` |
| `reset` | `bigint` | yes | |
| `logic<N>` | `bigint` | yes | |
| `bit<N>` | `bigint` | no | 2-state only |

All signal port values use `bigint` regardless of width. This ensures a consistent type across all signals and avoids type changes when signal widths are modified.

## Direction and Mutability

| Direction | Read | Write | TS Modifier |
|---|---|---|---|
| `input` | yes | yes | *(mutable)* |
| `output` | yes | no | `readonly` |
| `inout` | yes | yes | *(mutable)* |

Output ports are declared `readonly` in the generated `Ports` interface. Attempting to assign to an output port results in a TypeScript compile error.

## Clock Ports

Ports typed as `clock` (including `clock_posedge` and `clock_negedge`) are **not** included in the generated `Ports` interface. Instead, they appear in the module's `events` array and are used with:

- `Simulator.tick()` / `Simulator.event()` for event-based simulation
- `Simulation.addClock()` for time-based simulation

## Array Ports

Array ports (e.g., `output logic<32>[4]`) are represented as an object with indexed access:

```ts
interface CounterPorts {
  readonly cnt: {
    at(i: number): bigint;
    readonly length: number;
  };
}
```

For input array ports, a `set(i, value)` method is also generated:

```ts
interface MyPorts {
  data: {
    at(i: number): bigint;
    set(i: number, value: bigint): void;
    readonly length: number;
  };
}
```

## 4-State vs 2-State

| Type | 4-State |
|---|---|
| `logic` | yes |
| `clock` | yes |
| `reset` | yes |
| `bit` | no |

When `fourState: true` is enabled in `SimulatorOptions`, 4-state signals carry an additional mask alongside the value. Mask bits set to 1 indicate unknown (X) bits. Use the `FourState()` helper to construct 4-state values and the vitest matchers (`toBeX`, `toBeAllX`, `toBeNotX`) to assert on them.

See [4-State Simulation](./four-state.md) for details.
