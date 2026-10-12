import { existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, expect, test } from "vitest";
import { generateSidecars } from "../../vite-plugin/src/sidecar.js";
import type { GenTsJsonOutput } from "../../vite-plugin/src/types.js";

const roots: string[] = [];
afterEach(() => { for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true }); });
function project(): string {
  const root = mkdtempSync(join(tmpdir(), "celox-sidecars-"));
  roots.push(root);
  mkdirSync(join(root, "src"));
  mkdirSync(join(root, ".celox/src"), { recursive: true });
  writeFileSync(join(root, "src/Main.veryl"), "module Main {}\n");
  return root;
}
function data(root: string): GenTsJsonOutput {
  return { projectPath: root, fileModules: { "src/Main.veryl": ["Main"] }, modules: [{
    moduleName: "Main", sourceFile: "src/Main.veryl", dtsContent: "export declare const Main: {};\n",
    mdContent: "", ports: {}, events: [], isTest: false,
  }] };
}
test("startup removes old declaration sidecars and keeps other generated files", () => {
  const root = project();
  const ghost = join(root, ".celox/src/Removed.d.veryl.ts");
  const components = join(root, ".celox/components.d.ts");
  writeFileSync(ghost, "old declaration");
  writeFileSync(components, "component declaration");
  generateSidecars(data(root), root);
  expect(existsSync(ghost)).toBe(false);
  expect(existsSync(components)).toBe(true);
  expect(existsSync(join(root, ".celox/src/Main.d.veryl.ts"))).toBe(true);
});
test("a file that no longer exports a module loses its previous declaration", () => {
  const root = project();
  generateSidecars(data(root), root);
  generateSidecars({ projectPath: root, modules: [], fileModules: { "src/Main.veryl": [] } }, root);
  expect(existsSync(join(root, ".celox/src/Main.d.veryl.ts"))).toBe(false);
});
test("failed replacement leaves old declaration sidecars available", () => {
  const root = project();
  const ghost = join(root, ".celox/Removed.d.veryl.ts");
  writeFileSync(ghost, "old declaration");
  rmSync(join(root, ".celox/src"), { recursive: true });
  writeFileSync(join(root, ".celox/src"), "blocks directory creation");
  expect(() => generateSidecars(data(root), root)).toThrow();
  expect(existsSync(ghost)).toBe(true);
});
