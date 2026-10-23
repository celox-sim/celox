# Type Conversion

This page describes how Veryl types are mapped to TypeScript types when `celox-ts-gen` generates module definitions.

## Conversion Table

| Veryl Type | TS Type | 4-State | Notes |
|---|---|---|---|
| `clock` | *(excluded from ports)* | yes | Becomes an event via `addClock()` / `tick()` |
| `reset` | `bigint` | yes | |
| `logic<N>` | `bigint` | yes | |
| `bit<N>` | `bigint` | no | 2-state only |
| `f32` / `f64` | `bigint` | no | The IEEE 754 bit pattern. Scalar arithmetic, comparisons, f32/f64 precision conversions and integer conversions up to 64 bits are supported. Numeric display supports `%f`, `%e` and `%g`, including field widths and precision. Aggregate numeric conversions and integer conversions wider than 64 bits remain unsupported ([#1653](https://github.com/celox-sim/celox/issues/1653)) |
| `string` | — | no | Non-constant strings are supported only in testbench code. RTL can use string literals, constants and parameters ([#1400](https://github.com/celox-sim/celox/issues/1400)) |

All signal port values use `bigint` regardless of width. This ensures a consistent type across all signals and avoids type changes when signal widths are modified.

Floating-point to integer casts round to the nearest integer, with halfway
values rounded away from zero. `$rtoi` in SystemVerilog truncates instead.
Celox converts NaN and infinity to integer zero; finite results outside the
target integer range retain their low bits. These out-of-range choices should
not be used as portable behavior across simulators.

Veryl analyzer 0.22 can mis-evaluate some floating-point constant expressions,
including negative `f32` casts and dependent constants or parameters. These
upstream limitations are tracked in [#1657](https://github.com/celox-sim/celox/issues/1657).

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

Array indices must be integers in the range `0 <= i < length`. Both `at()`
and `set()` throw a `RangeError` for an invalid index.

## 4-State vs 2-State

| Type | 4-State |
|---|---|
| `logic` | yes |
| `clock` | yes |
| `reset` | yes |
| `bit` | no |

When `fourState: true` is enabled in `SimulatorOptions`, 4-state signals carry an additional mask alongside the value. Mask bits set to 1 indicate unknown (X) bits. Use the `FourState()` helper to construct 4-state values and the vitest matchers (`toBeX`, `toBeAllX`, `toBeNotX`) to assert on them.

See [4-State Simulation](./four-state.md) for details.
