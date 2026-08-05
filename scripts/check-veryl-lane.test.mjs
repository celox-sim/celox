import assert from "node:assert/strict";
import test from "node:test";

import {
  checkVerylLane,
  detectVerylLane,
} from "./check-veryl-lane.mjs";
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

test("detects a complete released lane", () => {
  assert.deepEqual(detectVerylLane(stableManifest), {
    lane: "stable",
    version: "0.20.3",
  });
  assert.equal(checkVerylLane(stableManifest, "stable"), undefined);
  assert.throws(() => checkVerylLane(stableManifest, "head"), /detected stable/);
});

test("detects a complete HEAD lane at one exact upstream revision", () => {
  const headManifest = useVerylHead(stableManifest, revision);
  assert.deepEqual(detectVerylLane(headManifest), { lane: "head", revision });
  assert.equal(checkVerylLane(headManifest, "head"), revision);
  assert.throws(() => checkVerylLane(headManifest, "stable"), /detected head/);
});

test("allows a stable develop sync before an atomic HEAD roll", () => {
  assert.equal(detectVerylLane(stableManifest).lane, "stable");
  assert.equal(
    detectVerylLane(useVerylHead(stableManifest, revision)).lane,
    "head",
  );
});

test("rejects mixed released and HEAD declarations", () => {
  const headManifest = useVerylHead(stableManifest, revision);
  const mixedManifest = headManifest.replace(
    /^veryl-std = .*$/m,
    'veryl-std = "0.20.3"',
  );
  assert.throws(() => detectVerylLane(mixedManifest), /mix released and HEAD/);
});

test("rejects mismatched released versions and HEAD revisions", () => {
  assert.throws(
    () =>
      detectVerylLane(
        stableManifest.replace(
          'veryl-std = "0.20.3"',
          'veryl-std = "0.20.4"',
        ),
      ),
    /different versions/,
  );

  const headManifest = useVerylHead(stableManifest, revision);
  assert.throws(
    () =>
      detectVerylLane(
        headManifest.replace(revision, "fedcba9876543210fedcba9876543210fedcba98"),
      ),
    /different revisions/,
  );
});

test("rejects missing and malformed dependency declarations", () => {
  assert.throws(
    () => detectVerylLane(stableManifest.replace(/^veryl-std = .*\n/m, "")),
    /exactly one workspace dependency veryl-std/,
  );
  assert.throws(
    () =>
      detectVerylLane(
        stableManifest.replace(
          'veryl-std = "0.20.3"',
          'veryl-std = { path = "vendor/veryl-std" }',
        ),
      ),
    /non-release source/,
  );
  assert.throws(
    () =>
      detectVerylLane(
        useVerylHead(stableManifest, revision).replace(
          "https://github.com/veryl-lang/veryl.git",
          "https://github.com/example/veryl.git",
        ),
      ),
    /upstream repository/,
  );
  assert.throws(
    () =>
      detectVerylLane(
        useVerylHead(stableManifest, revision).replace(revision, "master"),
      ),
    /full revision/,
  );
});

test("rejects an unknown compatibility lane", () => {
  assert.throws(() => checkVerylLane(stableManifest, "edge"), /stable or head/);
});
