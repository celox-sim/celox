# celox.toml

`celox.toml` is an optional Celox-specific configuration file placed alongside `Veryl.toml` at the project root. It extends the Veryl project with settings that are only relevant to simulation and testing.

## Why It Exists

`Veryl.toml`'s `[build] sources` list controls which directories are compiled for production. Test-only modules -- such as helper fixtures, mock peripherals, or reference models -- should not be included in those builds.

`celox.toml` lets you declare extra source directories that Celox loads **only** during simulation and type generation, keeping them out of the standard Veryl build.

## File Structure

Place `celox.toml` next to `Veryl.toml`:

```
my-project/
├── Veryl.toml
├── celox.toml          ← Celox configuration
├── src/
│   └── Adder.veryl     # production sources (listed in Veryl.toml)
└── test_veryl/
    └── Reg.veryl        # test-only sources (listed in celox.toml)
```

## Configuration Reference

```toml
exclude = ["test_veryl/broken.veryl", "src/legacy/**"]

[test]
sources = ["test_veryl"]

[simulation]
max_steps = 100000
```

### `exclude`

| Key | Type | Description |
|---|---|---|
| `exclude` | `string[]` | Glob patterns (relative to project root) for `.veryl` files to exclude from compilation and type generation. Applies to both production sources and test sources. |

Patterns use standard glob syntax (`*`, `**`, `?`, `[...]`). Path separators are always `/` regardless of platform.

### `[test]`

| Key | Type | Description |
|---|---|---|
| `test.sources` | `string[]` | Directories (relative to `celox.toml`) whose `.veryl` files are included in simulation and type generation. |

### `[simulation]`

| Key | Type | Default | Description |
|---|---|---|---|
| `simulation.max_steps` | `integer` | 100,000 | Default step budget for `waitUntil` and `waitForCycles`. A `SimulationTimeoutError` is thrown if the condition is not met within this many steps. Overridden per-call via `{ maxSteps }`. |

## Example

**`Veryl.toml`** — production build, only includes `src/`:

```toml
[project]
name    = "my_project"
version = "0.1.0"

[build]
clock_type = "posedge"
reset_type = "async_low"
sources    = ["src"]
```

**`celox.toml`** — additionally loads `test_veryl/` for simulation, excludes WIP files, and sets a project-wide step budget:

```toml
exclude = ["test_veryl/wip_*.veryl"]

[test]
sources = ["test_veryl"]

[simulation]
max_steps = 50000
```

**`test_veryl/Reg.veryl`** — a test-only module:

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

**`test/reg.test.ts`** — the test imports `Reg` just like any other module:

```typescript
import { describe, test, expect } from "vitest";
import { Simulator } from "@celox-sim/celox";
import { Reg } from "../test_veryl/Reg.veryl";

describe("Reg", () => {
  test("captures input on rising edge", () => {
    const sim = Simulator.create(Reg);

    sim.dut.d = 0xABn;
    sim.tick();
    expect(sim.dut.q).toBe(0xABn);

    sim.dispose();
  });
});
```

The Vite plugin picks up `test_veryl/` automatically and generates type definitions for all modules declared there.

## Behavior

- If `celox.toml` does not exist, Celox uses only the sources listed in `Veryl.toml` and falls back to built-in defaults for all simulation settings.
- All test source directories are merged with the project sources at simulation time. Modules from both are available in the same namespace.
- The Vite plugin regenerates types for test sources on hot reload, just like for production sources.
- `[simulation]` settings apply to every `Simulation.fromProject` / `Simulation.create` call within the project. A per-call `{ maxSteps }` option always takes precedence over the `celox.toml` value.
