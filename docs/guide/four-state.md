# 4-State Simulation

Celox supports IEEE 1800-compliant 4-state simulation with X (unknown) and Z (high-impedance) values. This page explains how to use 4-state features from TypeScript testbenches.

## Enabling 4-State Mode

Pass `{ fourState: true }` when creating a simulator:

```typescript
import { Simulator } from "@celox-sim/celox";
import { MyModule } from "../src/MyModule.veryl";

const sim = Simulator.create(MyModule, { fourState: true });
```

The same option works with `Simulation`:

```typescript
import { Simulation } from "@celox-sim/celox";
import { MyModule } from "../src/MyModule.veryl";

const sim = Simulation.create(MyModule, { fourState: true });
```

::: warning
Without `fourState: true`, all signals behave as 2-state. X and Z values cannot be written or read.
:::

## Veryl Types and 4-State

Whether a signal supports 4-state depends on its Veryl type:

| Type | State | Notes |
|------|-------|-------|
| `logic` | 4-state | Primary 4-state type |
| `clock`, `reset` | 4-state | Control signals |
| `bit` | 2-state | Mask is always 0 |

When a 4-state value is assigned to a `bit`-type variable inside the design, the mask (X bits) is automatically cleared to 0. This prevents unintended X propagation through 2-state boundaries.

## Writing X Values

### Assigning All-X

Use the `X` sentinel to set all bits of a port to X:

```typescript
import { Simulator, X } from "@celox-sim/celox";
import { MyModule } from "../src/MyModule.veryl";

const sim = Simulator.create(MyModule, { fourState: true });

sim.dut.data_in = X;
sim.tick();
```

### Assigning Specific Bits as X

Use `FourState(value, mask)` to control individual bits. Mask bits set to 1 indicate X:

```typescript
import { Simulator, FourState } from "@celox-sim/celox";
import { MyModule } from "../src/MyModule.veryl";

const sim = Simulator.create(MyModule, { fourState: true });

// Bits [3:0] = 0101, bits [7:4] = X
sim.dut.data_in = FourState(0b0000_0101, 0b1111_0000);
sim.tick();
```

For wide signals (> 53 bits), use `bigint`:

```typescript
sim.dut.wide_data = FourState(0x1234n, 0xFF00n);
```

### Z Sentinel

The `Z` sentinel is also available for high-impedance:

```typescript
import { Z } from "@celox-sim/celox";

sim.dut.bus = Z;
```

## Reading 4-State Values

### Standard Read

Reading a port via `sim.dut.<port>` returns only the **value** portion (mask is not included):

```typescript
const val = sim.dut.result; // number or bigint — X bits read as 0
```

### Reading the Full Value/Mask Pair

To inspect X bits, use `readFourState()`:

```typescript
import { readFourState } from "@celox-sim/celox";

const [value, mask] = readFourState(sim.buffer, sim.layout["result"]);

if (mask !== 0) {
  console.log("Result contains X bits:", mask.toString(2));
}
```

The return value is a tuple `[value, mask]` where:

| mask bit | value bit | Meaning |
|----------|-----------|---------|
| 0 | 0 | `0` |
| 0 | 1 | `1` |
| 1 | 0 | `X` |

## Example: Testing X Propagation

```typescript
import { describe, test, expect } from "vitest";
import { Simulator, X, FourState } from "@celox-sim/celox";
import { ALU } from "../src/ALU.veryl";

describe("ALU", () => {
  test("X input propagates to output", () => {
    const sim = Simulator.create(ALU, { fourState: true });

    sim.dut.a = X;
    sim.dut.b = 42;
    sim.tick();

    // Arithmetic with X produces all-X result
    // Use readFourState to verify X propagation
    // ...

    sim.dispose();
  });

  test("known-0 AND cancels X", () => {
    const sim = Simulator.create(ALU, { fourState: true });

    // a = X, but b = 0 — AND should produce known 0
    sim.dut.a = X;
    sim.dut.b = 0;
    sim.dut.op = 0; // AND
    sim.tick();

    sim.dispose();
  });

  test("partial X with FourState", () => {
    const sim = Simulator.create(ALU, { fourState: true });

    // Lower 4 bits known, upper 4 bits X
    sim.dut.a = FourState(0x05, 0xF0);
    sim.dut.b = 0xFF;
    sim.tick();

    sim.dispose();
  });
});
```

## X Propagation Rules

Celox follows IEEE 1800 X propagation semantics:

| Operation | Behavior |
|-----------|----------|
| `a & b` | Known `0` cancels X |
| `a \| b` | Known `1` cancels X |
| `a ^ b` | X if either operand is X |
| `+`, `-`, `*`, `/`, `%` | Any X in operands makes the entire result X |
| `==`, `!=`, `<`, `>` | Any X in operands makes the result X |
| `if (x_cond)` | X selector merges both branches conservatively |
| Shift by X amount | Entire result becomes X |

## Further Reading

- [4-State Internals](/internals/four-state) -- Representation model, normalization, and JIT compilation details.
