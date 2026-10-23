import assert from "node:assert/strict";
import { createRequire } from "node:module";
import test from "node:test";
import { createAddonModule } from "@emnapi/core";

const require = createRequire(import.meta.url);

for (const format of ["import", "require"]) {
  test(`emnapi plugins initialize with the installed core (${format})`, async () => {
    const runtime =
      format === "import"
        ? await import("@napi-rs/wasm-runtime")
        : require("@napi-rs/wasm-runtime");
    // Exercise plugin initialization without compiling a WASM addon. A stale
    // bundled plugin fails here when emnapi changes its internal module API.
    const addon = createAddonModule({
      context: runtime.createContext(),
      plugins: [runtime.emnapiAsyncWorkPlugin, runtime.emnapiTSFNPlugin],
    });
    assert.equal(typeof addon.initWorker, "function");
    assert.equal(typeof addon.imports.napi.napi_create_async_work, "function");
    assert.equal(
      typeof addon.imports.napi.napi_create_threadsafe_function,
      "function",
    );
  });
}
