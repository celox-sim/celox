import assert from "node:assert/strict";
import test from "node:test";

import {
  lockedVerylVersions,
  useVerylHead,
  verylDependencyNames,
} from "./use-veryl-head.mjs";

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

test("is idempotent when the manifest already pins the requested revision", () => {
  const pinned = useVerylHead(manifest, revision);

  assert.equal(useVerylHead(pinned, revision), pinned);
});

test("rejects an ambiguous revision", () => {
  assert.throws(() => useVerylHead(manifest, "master"), /full lowercase git revision/);
});

function lockfile(lockedRevision) {
  const source = `git+https://github.com/veryl-lang/veryl.git?rev=${lockedRevision}#${lockedRevision}`;
  return [
    "version = 4",
    ...verylDependencyNames.map(
      (name) => `[[package]]\nname = "${name}"\nversion = "0.21.0"\nsource = "${source}"\n`,
    ),
    '[[package]]\nname = "veryl-metadata"\nversion = "0.20.2"\n',
  ].join("\n");
}

test("reads the versions locked at the requested revision", () => {
  assert.deepEqual(
    lockedVerylVersions(lockfile(revision), revision),
    Object.fromEntries(verylDependencyNames.map((name) => [name, "0.21.0"])),
  );
  assert.equal(
    lockedVerylVersions(
      lockfile("fedcba9876543210fedcba9876543210fedcba98"),
      revision,
    ),
    undefined,
  );
});

test("declares the locked versions so the git dependencies are not wildcards", () => {
  const result = useVerylHead(
    manifest,
    revision,
    lockedVerylVersions(lockfile(revision), revision),
  );

  for (const name of verylDependencyNames) {
    assert.match(
      result,
      new RegExp(`^${name} = \\{ version = "0\\.21\\.0", git = .* rev = "${revision}"`, "m"),
    );
  }
  assert.equal(
    useVerylHead(result, revision, lockedVerylVersions(lockfile(revision), revision)),
    result,
  );
});
