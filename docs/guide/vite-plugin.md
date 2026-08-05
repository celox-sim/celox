# Vite Plugin

The `@celox-sim/vite-plugin` package connects Veryl source files to Vite-based
tools. In Vitest, it also acts as a test-runner adapter: importing a Veryl test
file turns its `#[test]` modules into ordinary Vitest cases. The plugin is not
limited to that role; regular Veryl modules are exposed as typed module
definitions for handwritten TypeScript tests and other Vite applications.

## What It Does

The plugin handles four things automatically:

1. **Module resolution** -- Allows `import { Counter } from "../src/Counter.veryl"` to work in test files.
2. **Vitest integration** -- Converts each imported `#[test]` module into a Vitest `test()` case, runs it with Celox, and reports Veryl assertions as Vitest failures.
3. **Type generation** -- Produces `.d.veryl.ts` sidecar files so TypeScript understands the shape of each module (ports, events, types).
4. **Hot reload** -- When a `.veryl` file changes, the plugin invalidates its cache and regenerates types.

Under the hood, the plugin calls the `celox-ts-gen` type generator and native
simulator through the NAPI addon. You do not need to generate types or create
Vitest wrappers manually.

## Installation

```bash
pnpm add -D @celox-sim/vite-plugin
```

## Configuration

### Basic

```ts
// vitest.config.ts
import { defineConfig } from "vitest/config";
import celox from "@celox-sim/vite-plugin";

export default defineConfig({
  plugins: [celox()],
});
```

The plugin automatically finds the nearest `Veryl.toml` by walking up from the Vite project root.

### Using Vitest as the Veryl Test Runner

Create a normal Vitest entry file that imports a Veryl file containing one or
more `#[test]` modules:

```ts
// test/counter.test.ts
import "./CounterTest.veryl";
```

When Vitest evaluates the import, the plugin registers one Vitest case for each
Veryl test module. Celox executes the testbench with its native simulator;
failed Veryl assertions, including their source locations, appear in the
Vitest report. This lets Vitest provide discovery, filtering, watch mode,
reporters, and CI integration without rewriting the testbench in TypeScript.

Importing an ordinary, non-test Veryl module instead returns a typed
`ModuleDefinition`. It can be driven from a handwritten TypeScript test through
`Simulator` or `Simulation`, and the same import mechanism can be used by other
Vite-based tools.

### Custom Project Root

If `Veryl.toml` is not in the Vite root or a parent directory, specify the path explicitly:

```ts
export default defineConfig({
  plugins: [
    celox({
      projectRoot: "./path/to/veryl-project",
    }),
  ],
});
```

### TypeScript Testbench Components

Native Vitest testbenches can use synchronous components created with
`defineTbComponent` without building a Rust or Wasm component library:

```ts
// test/tb-components.ts
import { defineTbComponent } from "@celox-sim/celox";

const store = defineTbComponent<{ value: bigint }>({
  kind: "method_only",
  create: () => ({ value: 0n }),
  methods: {
    set: {
      args: [{ name: "value", type: "value" }],
      call: ({ state }, [value]) => { state.value = value as bigint; },
    },
    get: {
      returns: { width: 8 },
      call: ({ state }) => ({ returnValue: state.value }),
    },
  },
});

export default { store };
```

Point the Vite plugin at the component module:

```ts
export default defineConfig({
  plugins: [celox({
    testbenchComponents: "./test/tb-components.ts",
  })],
});
```

Register the generated manifest directory as a normal Veryl component source:

```toml
# Veryl.toml
[[components]]
path = ".celox/testbench-components"
```

The plugin loads the module through Vite and writes the extracted interfaces to
`.celox/testbench-components/veryl.manifest.json`. Veryl's existing component
discovery then makes the definitions visible to the compiler and language
server, including method arguments and return types. No Rust or Wasm artifact
is generated. The plugin also supplies the interfaces during TypeScript type
generation and imports the original module in generated Vitest cases. Veryl can
then declare `var model: $comp::store;`.

Generate the manifest at least once before opening or reloading the Veryl
project in an editor. If the language server does not notice a subsequent
manifest update, reload its workspace. Runtime callbacks are available to
native Node/Vitest execution; browser Wasm simulation does not currently
support synchronous JavaScript component callbacks.

### tsconfig.json

To enable TypeScript support for `.veryl` imports, add the following to `tsconfig.json`:

```json
{
  "compilerOptions": {
    "allowArbitraryExtensions": true,
    "rootDirs": ["src", ".celox/src"]
  },
  "include": ["test", "src", ".celox/src"]
}
```

- `allowArbitraryExtensions` allows TypeScript to resolve `.d.veryl.ts` files.
- `rootDirs` tells TypeScript to treat the `.celox/` sidecar directory as a virtual overlay on the source tree.

## Generated Files

The plugin generates sidecar files in the `.celox/` directory, mirroring the source tree:

```
my-project/
├── src/
│   └── Counter.veryl          # Veryl source
├── .celox/
│   ├── src/
│   │   └── Counter.d.veryl.ts # Generated type definition
│   └── testbench-components/
│       └── veryl.manifest.json    # Generated component interfaces
└── vitest.config.ts
```

Add `.celox/` to your `.gitignore`:

```
.celox/
```

## Query Parameters

### `?dse=` — Dead Store Elimination

Append `?dse=` to the import path to enable [Dead Store Elimination](./dead-store-elimination.md) for the imported module:

```typescript
import { Top } from "../src/Top.veryl?dse=preserveAllPorts";
```

| Value | Behavior |
|---|---|
| `?dse=preserveTopPorts` | Only top-module ports survive DSE |
| `?dse=preserveAllPorts` | Ports of all instances survive DSE |
| `?dse` (no value) | Defaults to `preserveAllPorts` |

The policy is embedded in the `ModuleDefinition` as `defaultOptions.deadStorePolicy` and automatically applied when `Simulator.create()` or `Simulation.create()` is called. Caller-supplied options override the default.

## Plugin Options

| Option | Type | Default | Description |
|---|---|---|---|
| `projectRoot` | `string` | *(auto-detected)* | Path to the directory containing `Veryl.toml` |
| `testbenchComponents` | `string` | *(none)* | Component module injected into generated native Vitest testbenches |
