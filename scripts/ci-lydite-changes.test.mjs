import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import test from "node:test";
import { fileURLToPath } from "node:url";
import {
  lockChanges,
  lyditeScope,
  isLydite,
  pullRequestScope,
} from "./ci-lydite-changes.mjs";
import { celoxRustTestScope } from "./ci-changes.mjs";
import { readWorkspace } from "./ci-rust-scope.mjs";
const root = fileURLToPath(new URL("../", import.meta.url));
const packages = readWorkspace(root);
const scope = (...files) => lyditeScope(files, packages);
const both = { core: true, bridge: true };
const neither = { core: false, bridge: false };

test("Lydite core changes and shared configuration run both lanes", () => {
  for (const path of [
    "crates/lydite/src/main.rs",
    "crates/lydite-ir/src/lib.rs",
    "lydite/editor/server.py",
    "Cargo.lock",
    "Cargo.toml",
    "rust-toolchain.toml",
    ".github/workflows/lydite-checks.yml",
    "scripts/ci-lydite-changes.mjs",
    "vendor/new/src/lib.rs",
    "crates/new/src/lib.rs",
  ])
    assert.deepEqual(scope(path), both, path);
  assert.deepEqual(lyditeScope(null, packages), both);
});

test("Celox dependencies select the bridge, without rerunning the editor/core", () => {
  for (const name of [
    "celox",
    "celox-design",
    "celox-frontend-veryl",
    "celox-sir",
    "celox-runtime",
    "celox-test-suite",
    "lydite-celox",
  ])
    assert.deepEqual(
      scope(`crates/${name}/src/lib.rs`),
      { core: false, bridge: true },
      name,
    );
});

test("unrelated consumers and upstream private tests do not select Lydite", () => {
  for (const path of [
    "crates/celox-napi/src/lib.rs",
    "crates/celox/tests/counter.rs",
    "crates/celox/README.md",
    "packages/celox/src/index.ts",
    "docs/guide.md",
    "scripts/report-full-ci.mjs",
    // Lydite's jobs use neither the Nix shell nor Celox's CI workflow.
    "flake.lock",
    "flake.nix",
    ".github/workflows/ci.yml",
  ])
    assert.deepEqual(scope(path), neither, path);
  assert.deepEqual(scope(), neither);
  assert.deepEqual(
    scope("packages/celox/src/index.ts", "crates/lydite/src/main.rs"),
    both,
  );
});

test("PRs run the bridge only for Lydite-owned changes", () => {
  const pr = (...files) => pullRequestScope(files, packages);
  for (const path of [
    "crates/celox/src/lib.rs",
    "crates/celox-test-suite/src/veryl/cases/flip_flop.vtest",
  ])
    assert.deepEqual(pr(path), neither, path);
  for (const path of [
    "Cargo.lock",
    "rust-toolchain.toml",
    ".github/workflows/lydite-checks.yml",
    "scripts/ci-lydite-changes.mjs",
  ])
    assert.deepEqual(pr(path), { core: true, bridge: false }, path);
  for (const path of [
    "crates/lydite-celox/src/lib.rs",
    "crates/lydite/src/main.rs",
    "lydite/conformance/veryl-proof/case-exceptions.json",
  ])
    assert.equal(pr(path).bridge, true, path);
  assert.deepEqual(pullRequestScope(null, packages), both);
});

test("Cargo.lock edits select only the lanes that reach the changed entries", () => {
  const lockfile = readFileSync(`${root}Cargo.lock`, "utf8");
  const edit = (name, from, to) => {
    const start = lockfile.indexOf(`[[package]]\nname = "${name}"\n`);
    assert.notEqual(start, -1, name);
    const end = lockfile.indexOf("\n\n", start);
    const entry = lockfile.slice(start, end);
    assert.ok(entry.includes(from), name);
    return lockChanges(
      lockfile,
      lockfile.slice(0, start) + entry.replace(from, to) + lockfile.slice(end),
    );
  };
  const pr = (lock) => pullRequestScope(["Cargo.lock"], packages, lock);
  // A Celox crate gaining a workspace dependency reaches no Lydite lane.
  assert.deepEqual(
    pr(edit("celox-vpi", ' "celox",\n', ' "celox",\n "celox-design",\n')),
    neither,
  );
  assert.deepEqual(pr(edit("serde_json", "dependencies = [", "dependencies = [\n \"new\",")), {
    core: true,
    bridge: false,
  });
  assert.deepEqual(
    lyditeScope(["Cargo.lock"], packages, edit("celox", "dependencies = [", "dependencies = [\n \"new\",")),
    { core: false, bridge: true },
  );
  assert.deepEqual(pr(lockChanges(lockfile, lockfile)), neither);
  assert.deepEqual(pr(null), { core: true, bridge: false });
});

test("version-only release PRs skip Lydite", () => {
  const release = [
    ".release-please-manifest.json",
    "CHANGELOG.md",
    "Cargo.lock",
    "Cargo.toml",
    "VERSION",
    "crates/celox-napi/package.json",
    "packages/celox/package.json",
    "packages/vite-plugin/package.json",
  ];
  assert.deepEqual(pullRequestScope(release, packages), neither);
  // Without the release manifest, Cargo changes are ordinary dependency edits.
  assert.deepEqual(pullRequestScope(release.slice(1), packages), {
    core: true,
    bridge: false,
  });
  assert.equal(
    pullRequestScope([...release, "crates/lydite/src/main.rs"], packages).bridge,
    true,
  );
});

test("dependency selection follows new transitive edges automatically", () => {
  const graph = [
    { name: "lydite", dir: "crates/lydite", dependencies: [] },
    {
      name: "lydite-celox",
      dir: "crates/lydite-celox",
      dependencies: ["bridge-api"],
    },
    {
      name: "bridge-api",
      dir: "crates/bridge-api",
      dependencies: ["new-runtime"],
    },
    { name: "new-runtime", dir: "crates/new-runtime", dependencies: [] },
  ];
  assert.deepEqual(lyditeScope(["crates/new-runtime/src/lib.rs"], graph), {
    core: false,
    bridge: true,
  });
  graph[0].dependencies.push("new-runtime");
  assert.deepEqual(lyditeScope(["crates/new-runtime/src/lib.rs"], graph), both);
});

test("Celox packages do not consume the excluded Lydite packages", () => {
  for (const pkg of packages.filter((p) => !isLydite(p.name))) {
    assert.ok(
      pkg.dependencies.every((dep) => !isLydite(dep)),
      pkg.name,
    );
  }
});

test("daily, manual and unknown-diff invocations run every lane", () => {
  for (const event of ["schedule", "workflow_dispatch", "merge_group"]) {
    const output = execFileSync(
      process.execPath,
      ["scripts/ci-lydite-changes.mjs"],
      {
        cwd: root,
        env: { ...process.env, GITHUB_EVENT_NAME: event, GITHUB_OUTPUT: "" },
        encoding: "utf8",
      },
    );
    assert.match(output, /core=true\nbridge=true/);
  }
});

test("workflows transfer Rust tests and gate each Lydite lane", () => {
  const ci = readFileSync(`${root}.github/workflows/ci.yml`, "utf8");
  assert.match(
    ci,
    /cargo nextest run --locked --workspace --exclude 'lydite\*'/,
  );
  assert.match(
    ci,
    /cargo test --locked --workspace --exclude 'lydite\*' --doc/,
  );
  const workflow = readFileSync(`${root}.github/workflows/lydite-checks.yml`, "utf8");
  assert.match(
    workflow,
    /pull_request:\n  merge_group:\n  schedule:\n    - cron: "41 3 \* \* \*"/,
  );
  for (const [job, lane] of [
    ["documentation", "core"],
    ["editor", "core"],
    ["veryl-fv", "core"],
    ["bridge", "bridge"],
  ]) {
    assert.ok(
      workflow.includes(
        `  ${job}:\n    needs: changes\n    if: always() && (needs.changes.result != 'success' || needs.changes.outputs.${lane} == 'true')`,
      ),
      job,
    );
  }
  assert.match(workflow, /cargo test --locked "\$\{args\[@\]\}"/);
  assert.match(
    workflow,
    /  report:\n    name: Report Lydite validation\n[\s\S]*REPORT_NAME: Lydite validation\n        run: node scripts\/report-full-ci\.mjs/,
  );
  // Execute the actual required-check script for all selected/skipped combinations.
  const script = workflow
    .split("          python3 - <<'PYCODE'\n")[1]
    .split("          PYCODE")[0]
    .replace(/^          /gm, "");
  for (const core of [false, true])
    for (const bridge of [false, true]) {
      const needs = {
        changes: {
          result: "success",
          outputs: { core: String(core), bridge: String(bridge) },
        },
      };
      for (const [job, selected] of [
        ["rust", core || bridge],
        ["documentation", core],
        ["editor", core],
        ["veryl-fv", core],
        ["bridge", bridge],
      ])
        needs[job] = { result: selected ? "success" : "skipped" };
      const run = () =>
        execFileSync("python3", ["-c", script], {
          env: { ...process.env, NEEDS: JSON.stringify(needs) },
          stdio: "pipe",
        });
      run();
      for (const job of Object.keys(needs)) {
        const original = needs[job].result;
        for (const status of [
          "failure",
          "cancelled",
          ...(original === "success" ? ["skipped"] : []),
        ]) {
          needs[job].result = status;
          assert.throws(run, `${job}: ${status}`);
        }
        needs[job].result = original;
      }
    }
});

test("scoped Celox PR tests also exclude the bridge and Lydite fixtures", () => {
  for (const path of [
    "crates/celox/src/lib.rs",
    "crates/lydite/src/main.rs",
    "lydite/examples/counter.lyd",
  ]) {
    const result = celoxRustTestScope([path], packages);
    assert.equal(result.full, false);
    assert.ok(result.packages.every((name) => !isLydite(name)));
    assert.ok(result.libraries.every((name) => !isLydite(name)));
    if (!path.startsWith("crates/celox/"))
      assert.deepEqual(result.packages, []);
  }
});
