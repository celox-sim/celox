import assert from "node:assert/strict";
import { resolve } from "node:path";
import { resolveConfig } from "vite";
import { loadTestbenchComponentModule } from "../../../../vite-plugin/dist/testbench-components.js";

const root = import.meta.dirname;
const registry = resolve(root, "tb-components.ts");
const helper = resolve(root, "tb-component-helper.ts");
const virtualTbStep = {
	name: "fixture-tb-step",
	resolveId(source) {
		if (source === "virtual:tb-step") return "\0virtual:tb-step";
	},
	load(id) {
		if (id === "\0virtual:tb-step") return "export const step = 2n;";
	},
};
const config = await resolveConfig(
	{
		root,
		resolve: { alias: { "@fixture/tb-helper": helper } },
		plugins: [virtualTbStep],
	},
	"serve",
	"development",
);

const loaded = await loadTestbenchComponentModule(registry, root, config);
assert.ok(
	loaded.dependencies.has(helper),
	"aliased registry dependency is missing from the HMR dependency graph",
);
