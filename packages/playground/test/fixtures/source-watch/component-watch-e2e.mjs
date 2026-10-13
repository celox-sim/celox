import assert from "node:assert/strict";
import {
	mkdirSync,
	mkdtempSync,
	readFileSync,
	rmSync,
	writeFileSync,
} from "node:fs";
import { join } from "node:path";
import { startVitest } from "vitest/node";
import celox from "../../../../vite-plugin/dist/index.js";

const root = mkdtempSync(join(import.meta.dirname, ".component-project-"));
const helper = join(root, "helper.ts");
const sidecar = join(root, ".celox/testbench-components/veryl.manifest.json");
const expected = join(root, "expected.json");
let ctx;
let finished;
const runs = [];
const reporter = {
	onTestRunEnd(modules, errors) {
		finished =
			modules.length === 1 &&
			modules.every((module) => module.ok()) &&
			errors.length === 0;
	},
	onWatcherStart() {
		if (finished === undefined) return;
		runs.push(finished);
		finished = undefined;
	},
};
async function waitForFreshRun(previous) {
	const deadline = Date.now() + 15_000;
	while ((runs.length <= previous || !runs.at(-1)) && Date.now() < deadline)
		await new Promise((resolve) => setTimeout(resolve, 25));
	assert.ok(
		runs.length > previous && runs.at(-1),
		"component regeneration did not produce a fresh passing run",
	);
}
try {
	mkdirSync(join(root, "src"));
	writeFileSync(
		join(root, "Veryl.toml"),
		'[project]\nname="component_watch"\nversion="0.1.0"\n[build]\nsources=["src"]\n',
	);
	writeFileSync(join(root, "src/Main.veryl"), "module Main {}\n");
	writeFileSync(helper, "export const width = 8;\n");
	writeFileSync(expected, "8");
	writeFileSync(
		join(root, "components.ts"),
		`
		import { defineTbComponent } from "@celox-sim/celox";
		import { width } from "./helper";
		export default { store: defineTbComponent({ kind: "method_only", create: () => ({}),
			methods: { get: { returns: { width }, call: () => ({ returnValue: 0n }) } } }) };
	`,
	);
	writeFileSync(
		join(root, "entry.test.ts"),
		`
		import { expect, test } from "vitest";
		import { readFileSync } from "node:fs";
		import { Main } from "./src/Main.veryl";
		test("the asynchronous component manifest is current", () => {
			expect(Main).toBeDefined();
			const manifest = JSON.parse(readFileSync(${JSON.stringify(sidecar)}, "utf8"));
			expect(manifest.types.store.methods[0].ret_width)
				.toBe(JSON.parse(readFileSync(${JSON.stringify(expected)}, "utf8")));
		});
	`,
	);
	ctx = await startVitest(
		"test",
		[],
		{
			root,
			config: false,
			watch: true,
			include: ["entry.test.ts"],
			reporters: [reporter],
			disableConsoleIntercept: true,
		},
		{
			plugins: [
				{
					name: "delayed-component-helper",
					async transform(_code, id) {
						if (id === helper)
							await new Promise((resolve) => setTimeout(resolve, 500));
					},
				},
				celox({ projectRoot: root, testbenchComponents: "components.ts" }),
			],
		},
	);
	await waitForFreshRun(0);
	const previous = runs.length;
	writeFileSync(expected, "16");
	writeFileSync(helper, "export const width = 16;\n");
	await waitForFreshRun(previous);
	assert.equal(
		JSON.parse(readFileSync(sidecar, "utf8")).types.store.methods[0].ret_width,
		16,
	);
	await new Promise((resolve) => setTimeout(resolve, 750));
	assert.ok(runs.at(-1), "the final component run used stale manifests");
	// Vitest keeps the exit status of an earlier stale attempt in watch mode.
	// The fixture succeeds only after regeneration yields a fresh passing run.
	process.exitCode = 0;
	console.log(
		"passed: HMR-disabled Vitest reruns after delayed component regeneration",
	);
} finally {
	await ctx?.close();
	rmSync(root, { recursive: true, force: true });
}
