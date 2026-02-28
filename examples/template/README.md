# My Celox Project

A hardware simulation project using [Celox](https://github.com/celox-sim/celox) and [Veryl](https://veryl-lang.org/).

## Prerequisites

- [Node.js](https://nodejs.org/) >= 18
- [Rust](https://www.rust-lang.org/tools/install) (for the Celox native addon)
- [Veryl](https://veryl-lang.org/) toolchain

## Quick Start

```bash
# Install dependencies
pnpm install

# Run tests
pnpm test
```

## Project Structure

```
├── Veryl.toml          # Veryl project configuration
├── vitest.config.ts    # Vitest config with Celox plugin
├── src/
│   ├── Adder.veryl     # 16-bit combinational adder
│   └── Counter.veryl   # 8-bit sequential counter
└── test/
    ├── adder.test.ts   # Event-based simulation tests
    └── counter.test.ts # Time-based simulation tests
```

## Writing Tests

### Event-based simulation (Simulator)

Use `Simulator` for combinational logic or when you want manual control over clock edges:

```ts
import { Simulator } from "@celox-sim/celox";

const sim = Simulator.fromProject<MyPorts>(".", "MyModule");
sim.dut.input = 42;
sim.tick();
expect(sim.dut.output).toBe(expectedValue);
sim.dispose();
```

### Time-based simulation (Simulation)

Use `Simulation` for sequential logic with automatic clock generation:

```ts
import { Simulation } from "@celox-sim/celox";

const sim = Simulation.fromProject<MyPorts>(".", "MyModule");
sim.addClock("clk", { period: 10 });

sim.dut.rst = 1;
sim.runUntil(20);
sim.dut.rst = 0;

sim.runUntil(100);
expect(sim.dut.output).toBe(expectedValue);
sim.dispose();
```

## Next Steps

- Add your own Veryl modules in `src/`
- Write tests in `test/`
- See the [Celox documentation](https://github.com/celox-sim/celox) for the full API
- See the [Veryl documentation](https://doc.veryl-lang.org/) for the Veryl language reference
