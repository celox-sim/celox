# Getting Started

## Prerequisites

- [Node.js](https://nodejs.org/) (v18+)
- [pnpm](https://pnpm.io/)

::: tip Starter Template
A ready-to-use project template is available at [`celox-template`](https://github.com/celox-sim/celox-template). Click **Use this template** on GitHub or clone the repository, then run `pnpm install && pnpm test` to get started immediately.
:::

## Project Setup

Create a new project directory and initialize it:

```bash
mkdir my-celox-project && cd my-celox-project
pnpm init
```

Install Celox and Vitest:

```bash
pnpm add -D @celox-sim/celox @celox-sim/vite-plugin vitest
```

After following the steps below, your project will look like this:

```
my-celox-project/
├── Veryl.toml            # Veryl project config
├── vitest.config.ts      # Vitest + Celox plugin
├── tsconfig.json
├── package.json
├── src/
│   └── Adder.veryl       # Veryl design files
└── test/
    └── adder.test.ts     # TypeScript testbenches
```

### Veryl.toml

Create a `Veryl.toml` at the project root:

```toml
[project]
name    = "my_project"
version = "0.1.0"

[build]
clock_type = "posedge"
reset_type = "async_low"
sources    = ["src"]
```

### vitest.config.ts

```typescript
import { defineConfig } from "vitest/config";
import celox from "@celox-sim/vite-plugin";

export default defineConfig({
  plugins: [celox()],
});
```

### tsconfig.json

```json
{
  "compilerOptions": {
    "target": "ES2022",
    "module": "ES2022",
    "moduleResolution": "bundler",
    "strict": true,
    "esModuleInterop": true,
    "skipLibCheck": true,
    "allowArbitraryExtensions": true,
    "rootDirs": ["src", ".celox/src"]
  },
  "include": ["test", "src", ".celox/src"]
}
```

## Write a Veryl Module

Create `src/Adder.veryl`:

```veryl
module Adder (
    clk: input clock,
    rst: input reset,
    a: input logic<16>,
    b: input logic<16>,
    sum: output logic<17>,
) {
    always_comb {
        sum = a + b;
    }
}
```

## Write a Test

Create `test/adder.test.ts`:

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

The Vite plugin automatically analyzes your `.veryl` files and generates TypeScript type definitions, so imports like `import { Adder } from "../src/Adder.veryl"` are fully typed.

## Run Tests

Add a test script to `package.json`:

```json
{
  "scripts": {
    "test": "vitest run"
  }
}
```

Then run:

```bash
pnpm test
```

## Next Steps

- [Writing Tests](./writing-tests.md) -- Event-based and time-based simulation patterns.
- [Introduction](./introduction.md) -- Architecture overview.
