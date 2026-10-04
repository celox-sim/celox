import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { chmod, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { promisify } from "node:util";
import test from "node:test";

const exec = promisify(execFile);
const source = new URL("./publish-crates.sh", import.meta.url);

async function run(t, mode, env = {}) {
  const root = await mkdtemp(join(tmpdir(), "celox-publish-test-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  await writeFile(join(root, "VERSION"), "0.8.0\n");
  await writeFile(join(root, "publish.sh"), await readFile(source));
  const mocks = {
    curl: `#!/usr/bin/env bash
url="\${@: -1}"
case "$url" in
  */0.8.0) printf '%s' "\${VERSION_STATUS:-200}" ;;
  */celox-test-suite) printf '%s' "\${CRATE_STATUS:-200}" ;;
  *) printf '200' ;;
esac
exit "\${CURL_EXIT:-0}"
`,
    cargo: `#!/usr/bin/env bash
echo "$*" >> cargo.log
if [[ "$1" == package ]]; then
  mkdir -p target/package/celox-test-suite-0.8.0
  touch target/package/celox-test-suite-0.8.0/Cargo.toml
elif [[ "$1" == publish ]]; then
  echo 'the remote server responded with an error (status 403 Forbidden): Trusted Publishing tokens do not support creating new crates. Publish the crate manually, first'
  exit 1
fi
`,
    sleep: "#!/usr/bin/env bash\nexit 99\n",
  };
  for (const [name, contents] of Object.entries(mocks)) {
    await writeFile(join(root, name), contents);
    await chmod(join(root, name), 0o755);
  }
  try {
    const result = await exec("bash", ["publish.sh", mode], {
      cwd: root,
      env: { ...process.env, PATH: `${root}:${process.env.PATH}`, CARGO_REGISTRY_TOKEN: "test", ...env },
    });
    return { ...result, root, code: 0 };
  } catch (error) {
    return { ...error, root };
  }
}

test("preflight accepts registered crates without invoking Cargo", async (t) => {
  const result = await run(t, "preflight");
  assert.equal(result.code, 0);
  await assert.rejects(readFile(join(result.root, "cargo.log")), { code: "ENOENT" });
});

test("missing crate stops publication before any Cargo command", async (t) => {
  const result = await run(t, "publish", { CRATE_STATUS: "404" });
  assert.equal(result.code, 1);
  assert.match(result.stderr, /celox-test-suite has not been bootstrapped/);
  assert.match(result.stderr, /local bootstrap/);
  await assert.rejects(readFile(join(result.root, "cargo.log")), { code: "ENOENT" });
});

for (const status of ["403", "429", "500"]) {
  test(`HTTP ${status} is a lookup failure, not a missing crate`, async (t) => {
    const result = await run(t, "preflight", { CRATE_STATUS: status });
    assert.equal(result.code, 2);
    assert.match(result.stderr, new RegExp(`HTTP ${status}`));
    assert.doesNotMatch(result.stderr, /not been bootstrapped/);
  });
}

test("network failure stops preflight", async (t) => {
  const result = await run(t, "preflight", { CURL_EXIT: "7" });
  assert.equal(result.code, 2);
});

test("existing release versions are skipped", async (t) => {
  const result = await run(t, "publish");
  assert.equal(result.code, 0);
  assert.match(result.stdout, /celox@0.8.0 is already published; skipping/);
  await assert.rejects(readFile(join(result.root, "cargo.log")), { code: "ENOENT" });
});

test("version lookup errors stop publication", async (t) => {
  const result = await run(t, "publish", { VERSION_STATUS: "503" });
  assert.equal(result.code, 2);
  assert.match(result.stderr, /HTTP 503/);
  await assert.rejects(readFile(join(result.root, "cargo.log")), { code: "ENOENT" });
});

test("new-crate rejection is not retried after successful preflight", async (t) => {
  const result = await run(t, "publish", { VERSION_STATUS: "404" });
  assert.equal(result.code, 1);
  assert.match(result.stderr, /Complete the local bootstrap/);
  const calls = await readFile(join(result.root, "cargo.log"), "utf8");
  assert.equal(calls.split("\n").filter((line) => line.startsWith("publish ")).length, 1);
  assert.match(calls, /check --locked --all-targets/);
});
