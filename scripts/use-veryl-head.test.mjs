import assert from "node:assert/strict";
import test from "node:test";

import { useVerylHead } from "./use-veryl-head.mjs";

const revision = "0123456789abcdef0123456789abcdef01234567";
const manifest = `[workspace.dependencies]
veryl-analyzer = "0.20.2"
veryl-emitter = "0.20.2"
veryl-metadata = "0.20.2"
veryl-parser = { version = "0.20.2", default-features = false }
veryl-path = "0.20.2"
veryl-simulator = "0.20.2"
veryl-std = "0.20.2"

[patch.crates-io]
veryl-metadata = { path = "vendor/veryl-metadata" }
`;

test("pins every Veryl workspace dependency to one HEAD revision", () => {
  const result = useVerylHead(manifest, revision);

  for (const name of [
    "veryl-analyzer",
    "veryl-emitter",
    "veryl-metadata",
    "veryl-parser",
    "veryl-path",
    "veryl-simulator",
    "veryl-std",
  ]) {
    assert.match(
      result,
      new RegExp(`^${name} = \\{ git = .* rev = "${revision}"`, "m"),
    );
  }
  assert.match(result, /^veryl-parser = .*default-features = false/m);
  assert.match(
    result,
    /^veryl-metadata = \{ path = "vendor\/veryl-metadata" \}$/m,
  );
});

test("rejects an ambiguous revision", () => {
  assert.throws(() => useVerylHead(manifest, "master"), /full lowercase git revision/);
});
