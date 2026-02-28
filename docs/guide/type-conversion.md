# Type Conversion

This page describes how Veryl types are mapped to TypeScript types when `celox-ts-gen` generates module definitions.

## Conversion Table

| Veryl Type | Width | TS Type | 4-State | Notes |
|---|---|---|---|---|
| `clock` | 1 | *(excluded from ports)* | yes | Becomes an event via `addClock()` / `tick()` |
| `reset` | 1 | `number` | yes | |
| `logic<N>` (N &le; 53) | N | `number` | yes | |
| `logic<N>` (N &gt; 53) | N | `bigint` | yes | |
| `bit<N>` (N &le; 53) | N | `number` | no | 2-state only |
| `bit<N>` (N &gt; 53) | N | `bigint` | no | 2-state only |

## 53-bit Threshold

JavaScript `number` is an IEEE 754 double-precision float that can represent integers exactly up to 2<sup>53</sup> &minus; 1 (`Number.MAX_SAFE_INTEGER`). Signals wider than 53 bits use `bigint` to avoid silent precision loss.

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
    at(i: number): number;
    readonly length: number;
  };
}
```

For input array ports, a `set(i, value)` method is also generated:

```ts
interface MyPorts {
  data: {
    at(i: number): number;
    set(i: number, value: number): void;
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
