#!/bin/bash
# Run playground WASM tests with the WASI addon.
# Uses a separate vitest config that skips the Vite plugin (genTs needs native FS).
export NAPI_RS_FORCE_WASI=1
exec npx vitest run --config vitest.wasm.config.ts "$@"
