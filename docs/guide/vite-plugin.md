# Vite Plugin

The `@celox-sim/vite-plugin` package provides seamless integration between Veryl source files and the TypeScript/Vitest toolchain.

## What It Does

The plugin handles three things automatically:

1. **Module resolution** -- Allows `import { Counter } from "../src/Counter.veryl"` to work in test files.
2. **Type generation** -- Produces `.d.veryl.ts` sidecar files so TypeScript understands the shape of each module (ports, events, types).
3. **Hot reload** -- When a `.veryl` file changes, the plugin invalidates its cache and regenerates types.

Under the hood, the plugin calls the `celox-ts-gen` type generator via the NAPI addon. You never need to run the generator manually.

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
│   └── src/
│       └── Counter.d.veryl.ts # Generated type definition
└── vitest.config.ts
```

Add `.celox/` to your `.gitignore`:

```
.celox/
```

## Options

| Option | Type | Default | Description |
|---|---|---|---|
| `projectRoot` | `string` | *(auto-detected)* | Path to the directory containing `Veryl.toml` |
