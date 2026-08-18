// Set this after Vitest starts so only the Celox addon, not the test runner's
// own native dependencies, is forced onto the WASI binding.
process.env.NAPI_RS_WASI_FLAVOR = "wasm32-wasi";
process.env.NAPI_RS_FORCE_WASI = "true";
