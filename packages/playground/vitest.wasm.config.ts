/**
 * Vitest config for WASM-only tests.
 *
 * Does NOT use the Celox Vite plugin (which requires genTs() / filesystem
 * access not available under WASI). Instead, tests use Simulator.fromSource()
 * with inline Veryl source.
 */
import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    include: ["test/*-wasm.test.ts"],
  },
});
