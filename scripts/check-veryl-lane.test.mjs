import assert from "node:assert/strict";
import test from "node:test";

import { checkVerylLane } from "./check-veryl-lane.mjs";
import { useVerylHead } from "./use-veryl-head.mjs";

const revision = "0123456789abcdef0123456789abcdef01234567";
const stableManifest = `[workspace.dependencies]
veryl-analyzer = "0.20.3"
veryl-emitter = "0.20.3"
veryl-metadata = "0.20.3"
veryl-parser = { version = "0.20.3", default-features = false }
veryl-path = "0.20.3"
veryl-simulator = "0.20.3"
veryl-std = "0.20.3"
`;

test("accepts released crates only in the stable lane", () => {
  assert.equal(checkVerylLane(stableManifest, "stable"), undefined);
  assert.throws(() => checkVerylLane(useVerylHead(stableManifest, revision), "stable"), /git/);
});

test("requires one exact upstream revision in the HEAD lane", () => {
  const headManifest = useVerylHead(stableManifest, revision);
  assert.equal(checkVerylLane(headManifest, "head"), revision);
  assert.throws(() => checkVerylLane(stableManifest, "head"), /upstream repository/);
  assert.throws(
    () =>
      checkVerylLane(
        headManifest.replace(revision, "fedcba9876543210fedcba9876543210fedcba98"),
        "head",
      ),
    /different revisions/,
  );
});

test("rejects an unknown compatibility lane", () => {
  assert.throws(() => checkVerylLane(stableManifest, "edge"), /stable or head/);
});
