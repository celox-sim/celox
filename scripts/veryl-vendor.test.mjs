import assert from "node:assert/strict";
import test from "node:test";

import {
  defaultFeatures,
  disableGitoxideDefault,
  promoteReleasedVerylDependencies,
  requestedVerylVersion,
  verifyLockfile,
  verifyVendor,
} from "./veryl-vendor.mjs";

const rootManifest = (version = "0.20.2") => `[workspace.dependencies]
veryl-analyzer = "${version}"
veryl-emitter = "${version}"
veryl-metadata = "${version}"
veryl-parser = { version = "${version}", default-features = false }
veryl-path = "${version}"
veryl-simulator = "${version}"
veryl-std = "${version}"

[patch.crates-io]
veryl-metadata = { path = "vendor/veryl-metadata" }
`;

const vendorManifest = (version = "0.20.2", defaults = '"git-command", "git-gitoxide"') => `[package]
name = "veryl-metadata"
version = "${version}"

[features]
default = [${defaults}]
git-command = []
git-gitoxide = ["dep:gix"]

[lib]
path = "src/lib.rs"
`;

const lockfile = (version = "0.20.2", source = "") => `version = 4

[[package]]
name = "veryl-metadata"
version = "${version}"
${source}
dependencies = []

[[package]]
name = "veryl-parser"
version = "${version}"
source = "registry+https://github.com/rust-lang/crates.io-index"
`;

test("reads one exact Veryl release from workspace dependencies", () => {
  assert.equal(requestedVerylVersion(rootManifest()), "0.20.2");
});

test("rejects Veryl crates that are not in lockstep", () => {
  assert.throws(
    () => requestedVerylVersion(rootManifest().replace('veryl-std = "0.20.2"', 'veryl-std = "0.20.3"')),
    /not in lockstep/,
  );
});

test("promotes only released Veryl declarations onto the develop manifest", () => {
  const developManifest = rootManifest("0.20.2")
    .replace('[workspace.dependencies]\n', '[workspace.dependencies]\nlocal-overlay = "develop"\n')
    .replace(
      'veryl-parser = { version = "0.20.2", default-features = false }',
      'veryl-parser = { git = "https://github.com/veryl-lang/veryl.git", rev = "0123456789abcdef0123456789abcdef01234567", default-features = false }',
    );
  const releaseManifest = rootManifest("0.20.3").replace(
    '[patch.crates-io]',
    'release-only = "unchanged-by-promotion"\n\n[patch.crates-io]',
  );

  const promoted = promoteReleasedVerylDependencies(developManifest, releaseManifest);

  assert.equal(requestedVerylVersion(promoted), "0.20.3");
  assert.match(promoted, /^local-overlay = "develop"$/m);
  assert.doesNotMatch(promoted, /release-only/);
});

test("removes gitoxide from the vendored default features", () => {
  const patched = disableGitoxideDefault(vendorManifest());
  assert.deepEqual(defaultFeatures(patched), ["git-command"]);
  assert.match(patched, /git-gitoxide = \["dep:gix"\]/);
});

test("accepts a synchronized vendor manifest", () => {
  assert.doesNotThrow(() => verifyVendor(rootManifest(), vendorManifest("0.20.2", '"git-command"')));
});

test("rejects a stale vendor manifest", () => {
  assert.throws(
    () => verifyVendor(rootManifest("0.20.3"), vendorManifest("0.20.2", '"git-command"')),
    /does not match requested Veryl/,
  );
});

test("accepts a vendored lockfile entry without a registry source", () => {
  assert.doesNotThrow(() => verifyLockfile(rootManifest(), lockfile()));
});

test("rejects a registry lockfile entry", () => {
  assert.throws(
    () =>
      verifyLockfile(
        rootManifest(),
        lockfile("0.20.2", 'source = "registry+https://github.com/rust-lang/crates.io-index"'),
      ),
    /registry veryl-metadata/,
  );
});
