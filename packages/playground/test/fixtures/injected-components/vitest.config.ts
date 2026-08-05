import celox from "@celox-sim/vite-plugin";
import { resolve } from "node:path";
import type { Plugin } from "vite";
import { defineConfig } from "vitest/config";

const virtualTbStep: Plugin = {
	name: "fixture-tb-step",
	resolveId(source) {
		if (source === "virtual:tb-step") return "\0virtual:tb-step";
	},
	load(id) {
		if (id === "\0virtual:tb-step") return "export const step = 2n;";
	},
};

export default defineConfig({
	root: import.meta.dirname,
	resolve: {
		alias: {
			"@fixture/tb-helper": resolve(
				import.meta.dirname,
				"tb-component-helper.ts",
			),
		},
	},
	plugins: [
		virtualTbStep,
		celox({
			testbenchComponents: "./tb-components.ts",
		}),
	],
	test: { include: ["entry.test.ts"] },
});
