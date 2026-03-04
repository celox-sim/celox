# Child Instance Access

When a Veryl module instantiates submodules, their ports are accessible through the DUT accessor using the instance name.

## Basic Usage

Given a design with an instantiated submodule:

```veryl
module Sub (
    clk:    input  clock,
    i_data: input  logic<8>,
    o_data: output logic<8>,
) {
    always_comb {
        o_data = i_data;
    }
}

module Top (
    clk:     input  clock,
    rst:     input  reset,
    top_in:  input  logic<8>,
    top_out: output logic<8>,
) {
    inst u_sub: Sub (
        clk,
        i_data: top_in,
        o_data: top_out,
    );
}
```

You can observe `u_sub`'s ports through `sim.dut.u_sub`:

```typescript
const sim = Simulator.fromSource(SOURCE, "Top");

sim.dut.top_in = 0xABn;
sim.tick();

// Top-level output
expect(sim.dut.top_out).toBe(0xABn);

// Same value visible through child instance accessor
expect((sim.dut as any).u_sub.o_data).toBe(0xABn);
expect((sim.dut as any).u_sub.i_data).toBe(0xABn);

sim.dispose();
```

Child instance accessors are useful for observing internal signals during debugging, without needing to add extra top-level output ports.

## Type-Safe Access with the Vite Plugin

When using the Vite plugin, the generated type definition includes child instances as nested objects:

```typescript
export interface TopPorts {
  top_in: bigint;
  readonly top_out: bigint;
  readonly u_sub: {
    i_data: bigint;
    readonly o_data: bigint;
  };
}
```

So you can write:

```typescript
import { Top } from "../src/Top.veryl";
const sim = Simulator.create(Top);

expect(sim.dut.u_sub.o_data).toBe(0xABn); // fully typed, no cast needed
```

## Type-Safe Access with `fromSource`

When using `fromSource`, define the interface yourself:

```typescript
interface TopPorts {
  top_in: bigint;
  readonly top_out: bigint;
  readonly u_sub: {
    i_data: bigint;
    readonly o_data: bigint;
  };
}

const sim = Simulator.fromSource<TopPorts>(SOURCE, "Top");
expect(sim.dut.u_sub.o_data).toBe(0xABn);
```

## Nested Hierarchies

Child accessors are recursive — deeply nested instances work the same way:

```typescript
sim.dut.u_mid.u_inner.some_port
```

## DSE and Child Instances

When [Dead Store Elimination](./dead-store-elimination.md) is enabled, child instance accessors may not be available:

- `deadStorePolicy: "preserveTopPorts"` — child instances are stripped from the DUT; `sim.dut.u_sub` will be `undefined`.
- `deadStorePolicy: "preserveAllPorts"` — all instance ports are preserved; child accessors work normally.

If you need to observe sub-instance signals with DSE enabled, use `"preserveAllPorts"`.

## Further Reading

- [Dead Store Elimination](./dead-store-elimination.md) -- DSE policies and their impact on child access.
- [Writing Tests](./writing-tests.md) -- Simulator and Simulation patterns.
- [Type Conversion](./type-conversion.md) -- How Veryl types map to TypeScript.
