import assert from "node:assert/strict";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, utimesSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { startVitest } from "vitest/node";
import { build } from "vite";
import celox from "../../../../vite-plugin/dist/index.js";

const root = mkdtempSync(join(import.meta.dirname, ".project-"));
let ctx;
const runs = [];
const pending = [];
let finishedRun;
const reporter = {
  onTestRunEnd(modules, errors) {
    const result = { ok: modules.length === 1 && modules.every((m) => m.ok()) && errors.length === 0 };
    finishedRun = result;
  },
  // onTestRunEnd precedes Vitest's watch-ready phase. Only make the next
  // filesystem edit after the previous run has returned to watch mode.
  onWatcherStart() {
    if (!finishedRun) return;
    runs.push(finishedRun);
    finishedRun = undefined;
    pending.splice(0).forEach((resolve) => resolve());
  },
};
async function waitForRun(previous) {
  const deadline = Date.now() + 15_000;
  while (runs.length <= previous) {
    await new Promise((resolve, reject) => {
      const timeout = setTimeout(() => reject(new Error("watch did not rerun the test")), deadline - Date.now());
      pending.push(() => { clearTimeout(timeout); resolve(); });
    });
  }
  assert.ok(runs.at(-1).ok, "watch reran with stale or invalid exports");
}
function writeExpected(output, ports) {
  writeFileSync(join(root, "expected.json"), JSON.stringify({ output, ports }));
}
function sourceA(extra = "") {
  return `module A(a: input logic<8>, o: output logic<8>${extra ? `, ${extra}: input logic` : ""}) {
    inst child: B(a: a, o: o);
  }\n`;
}
function sourceB(amount) {
  return `module B(a: input logic<8>, o: output logic<8>) { assign o = a + 8'd${amount}; }\n`;
}
try {
  mkdirSync(join(root, "src"));
  writeFileSync(join(root, "Veryl.toml"), '[project]\nname="watch_e2e"\nversion="0.1.0"\n[build]\nsources=["src"]\n');
  writeFileSync(join(root, "src/A.veryl"), sourceA());
  writeFileSync(join(root, "src/B.veryl"), sourceB(1));
  utimesSync(join(root, "src/B.veryl"), 1_700_000_000, 1_700_000_000);
  writeExpected(4, ["a", "o"]);
  writeFileSync(join(root, "entry.test.ts"), `
    import { test, expect } from "vitest";
    import { readFileSync } from "node:fs";
    import { Simulator } from "@celox-sim/celox";
    import { A } from "./src/A.veryl?dse=preserveAllPorts";
    import { A as Plain } from "./src/A.veryl";
    test("source and catalogue are current", () => {
      const expected = JSON.parse(readFileSync(${JSON.stringify(join(root, "expected.json"))}, "utf8"));
      expect(Object.keys(A.ports).sort()).toEqual(expected.ports);
      expect(Object.keys(Plain.ports).sort()).toEqual(expected.ports);
      expect(A.defaultOptions.deadStorePolicy).toBe("preserveAllPorts");
      const sim = Simulator.create(A);
      try { sim.dut.a = 3n; sim.tick(0); expect(sim.dut.o).toBe(BigInt(expected.output)); }
      finally { sim.dispose(); }
    });
  `);
  ctx = await startVitest("test", [], {
    root, config: false, watch: true, include: ["entry.test.ts"],
    reporters: ["default", reporter], disableConsoleIntercept: true,
  }, { plugins: [celox({ projectRoot: root })] });
  await waitForRun(0);
  for (const [name, edit] of [
    ["imported source ports", () => {
      writeExpected(4, ["a", "flag", "o"]);
      writeFileSync(join(root, "src/A.veryl"), sourceA("flag"));
      utimesSync(join(root, "src/A.veryl"), 1_700_000_000, 1_700_000_000);
    }],
    ["imported source ports with unchanged mtime and size", () => {
      writeExpected(4, ["a", "mark", "o"]);
      writeFileSync(join(root, "src/A.veryl"), sourceA("mark"));
      utimesSync(join(root, "src/A.veryl"), 1_700_000_000, 1_700_000_000);
    }],
    ["non-imported source body with unchanged mtime and size", () => {
      writeExpected(5, ["a", "mark", "o"]);
      writeFileSync(join(root, "src/B.veryl"), sourceB(2));
      utimesSync(join(root, "src/B.veryl"), 1_700_000_000, 1_700_000_000);
    }],
    ["configuration creation", () => writeFileSync(join(root, "celox.toml"), "[test]\nsources=[]\n")],
    ["configuration edit", () => writeFileSync(join(root, "Veryl.toml"), '[project]\nname="watch_e2e"\nversion="0.1.1"\n[build]\nsources=["src"]\n')],
    ["source creation", () => writeFileSync(join(root, "src/Extra.veryl"), "module Extra {}\n")],
    ["source deletion", () => rmSync(join(root, "src/Extra.veryl"))],
    ["configuration deletion", () => rmSync(join(root, "celox.toml"))],
  ]) {
    const previous = runs.length;
    edit();
    await waitForRun(previous);
    // A second HMR transaction used to be emitted for add/unlink events.
    await new Promise((resolve) => setTimeout(resolve, 500));
    assert.equal(runs.length, previous + 1, `${name} reran more than once`);
    if (name === "source creation") assert.ok(existsSync(join(root, ".celox/src/Extra.d.veryl.ts")));
    if (name === "source deletion") assert.ok(!existsSync(join(root, ".celox/src/Extra.d.veryl.ts")));
    console.log(`passed: ${name}`);
  }
  assert.ok(readFileSync(join(root, ".celox/src/A.d.veryl.ts"), "utf8").includes("mark"));
  await ctx.close();
  ctx = undefined;
  writeFileSync(join(root, "entry.ts"), 'export { A } from "./src/A.veryl?dse=preserveAllPorts";');
  const bundle = await build({
    root, configFile: false, publicDir: false,
    plugins: [celox({ projectRoot: root })],
    build: { write: false, lib: { entry: join(root, "entry.ts"), formats: ["es"], fileName: "watch-fixture" } },
  });
  const outputs = (Array.isArray(bundle) ? bundle : [bundle]).flatMap((result) => result.output);
  const code = outputs.find((output) => output.type === "chunk").code;
  assert.ok(code.includes("mark") && code.includes("preserveAllPorts"));
  console.log("passed: client build preserves Veryl exports and query options");
} finally {
  await ctx?.close();
  rmSync(root, { recursive: true, force: true });
}
