import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { chmod, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import http from "node:http";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import test from "node:test";

const execFileAsync = promisify(execFile);
const repoRoot = dirname(dirname(fileURLToPath(import.meta.url)));
const bootstrapSource = join(repoRoot, "scripts/bootstrap-crates-io.sh");
const expectedConfig = (crate) => ({
  id: 1,
  crate,
  repository_owner: "celox-sim",
  repository_owner_id: 1,
  repository_name: "celox",
  workflow_filename: "publish-crates.yml",
  environment: "crates-io",
  created_at: "2026-08-20T00:00:00Z",
});

async function startFixture({ crates, published = [], configs = new Map() }) {
  const root = await mkdtemp(join(tmpdir(), "celox-bootstrap-test-"));
  const scriptsDir = join(root, "scripts");
  const binDir = join(root, "bin");
  const publishedDir = join(root, "published");
  const tempDir = join(root, "tmp");
  const cargoLog = join(root, "cargo.log");
  const posts = [];

  await Promise.all([
    mkdir(scriptsDir),
    mkdir(binDir),
    mkdir(publishedDir),
    mkdir(tempDir),
  ]);
  await Promise.all(
    published.map((crate) => writeFile(join(publishedDir, crate), "")),
  );

  const server = http.createServer(async (request, response) => {
    const url = new URL(request.url, "http://localhost");
    const cratePath = url.pathname.match(/^\/api\/v1\/crates\/([^/]+)(?:\/0\.0\.0)?$/);

    if (request.method === "GET" && cratePath) {
      const crate = decodeURIComponent(cratePath[1]);
      try {
        await readFile(join(publishedDir, crate));
        response.writeHead(200).end("{}");
      } catch {
        response.writeHead(404).end('{"errors":[{"detail":"not found"}]}');
      }
      return;
    }

    if (url.pathname !== "/api/v1/trusted_publishing/github_configs") {
      response.writeHead(404).end();
      return;
    }
    if (request.headers.authorization !== "test-token") {
      response
        .writeHead(403, { "content-type": "application/json" })
        .end('{"errors":[{"detail":"authentication required"}]}');
      return;
    }

    if (request.method === "GET") {
      const crate = url.searchParams.get("crate");
      const githubConfigs = configs.get(crate) ?? [];
      response
        .writeHead(200, { "content-type": "application/json" })
        .end(JSON.stringify({ github_configs: githubConfigs, meta: { total: githubConfigs.length } }));
      return;
    }

    if (request.method === "POST") {
      let body = "";
      for await (const chunk of request) body += chunk;
      const config = { ...expectedConfig(JSON.parse(body).github_config.crate), ...JSON.parse(body).github_config };
      configs.set(config.crate, [config]);
      posts.push(config);
      response
        .writeHead(200, { "content-type": "application/json" })
        .end(JSON.stringify({ github_config: config }));
      return;
    }

    response.writeHead(405).end();
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));

  const address = server.address();
  assert(address && typeof address !== "string");
  const api = `http://127.0.0.1:${address.port}/api/v1`;
  const source = (await readFile(bootstrapSource, "utf8")).replace(
    'crates_io_api="https://crates.io/api/v1"',
    `crates_io_api="${api}"`,
  );
  assert(source.includes(`crates_io_api="${api}"`));

  const bootstrap = join(scriptsDir, "bootstrap-crates-io.sh");
  const listScript = join(scriptsDir, "publish-crates.sh");
  const cargo = join(binDir, "cargo");
  await writeFile(bootstrap, source);
  await writeFile(
    listScript,
    `#!/usr/bin/env bash\nset -euo pipefail\n[[ "\${1:-}" == list ]]\nprintf '%s\\n' ${crates.map((crate) => `'${crate}'`).join(" ")}\n`,
  );
  await writeFile(
    cargo,
    `#!/usr/bin/env bash
set -euo pipefail
mode="\${1:-}"
shift || true
manifest=""
while [[ $# -gt 0 ]]; do
  if [[ "$1" == --manifest-path ]]; then
    manifest="$2"
    shift 2
  else
    shift
  fi
done
crate="$(basename "$(dirname "$manifest")")"
if [[ "$mode" == publish ]]; then
  printf '%s\\n' "$crate" >>"$FAKE_CARGO_LOG"
  : >"$FAKE_PUBLISHED_DIR/$crate"
fi
`,
  );
  await Promise.all([chmod(bootstrap, 0o755), chmod(listScript, 0o755), chmod(cargo, 0o755)]);

  return {
    async run() {
      return execFileAsync(
        bootstrap,
        ["publish", "BOOTSTRAP-CELOX-CRATES"],
        {
          env: {
            ...process.env,
            CARGO_REGISTRY_TOKEN: "test-token",
            FAKE_CARGO_LOG: cargoLog,
            FAKE_PUBLISHED_DIR: publishedDir,
            PATH: `${binDir}:${process.env.PATH}`,
            TMPDIR: tempDir,
          },
          maxBuffer: 1024 * 1024,
        },
      );
    },
    cargoLog,
    configs,
    posts,
    async close() {
      await new Promise((resolve, reject) =>
        server.close((error) => (error ? reject(error) : resolve())),
      );
      await rm(root, { recursive: true, force: true });
    },
  };
}

test("publishes missing placeholders and configures only missing publishers", async () => {
  const configs = new Map([
    ["celox-analysis", [expectedConfig("celox-analysis")]],
  ]);
  const fixture = await startFixture({
    crates: ["celox-analysis", "celox-design", "celox-frontend-sdk"],
    published: ["celox-analysis", "celox-design"],
    configs,
  });

  try {
    const { stdout, stderr } = await fixture.run();
    assert.match(stdout, /celox-analysis Trusted Publisher is already configured; skipping/);
    assert.match(stdout, /configured celox-design Trusted Publisher/);
    assert.match(stdout, /publishing celox-frontend-sdk@0\.0\.0 placeholder/);
    assert.match(stdout, /configured celox-frontend-sdk Trusted Publisher/);
    assert.doesNotMatch(`${stdout}${stderr}`, /test-token/);
    assert.deepEqual(
      fixture.posts.map((config) => config.crate),
      ["celox-design", "celox-frontend-sdk"],
    );
    assert.equal((await readFile(fixture.cargoLog, "utf8")).trim(), "celox-frontend-sdk");
  } finally {
    await fixture.close();
  }
});

test("stops instead of replacing an unexpected publisher", async () => {
  const wrongConfig = {
    ...expectedConfig("celox-analysis"),
    repository_owner: "unexpected-owner",
  };
  const fixture = await startFixture({
    crates: ["celox-analysis"],
    published: ["celox-analysis"],
    configs: new Map([["celox-analysis", [wrongConfig]]]),
  });

  try {
    await assert.rejects(fixture.run(), (error) => {
      assert.match(error.stderr, /unexpected Trusted Publisher configuration; stopping/);
      assert.match(error.stderr, /unexpected-owner\/celox/);
      return true;
    });
    assert.deepEqual(fixture.posts, []);
  } finally {
    await fixture.close();
  }
});
