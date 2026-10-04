import { spawnSync } from "node:child_process";
import { createRequire } from "node:module";
import { dirname, join, resolve } from "node:path";
import { type ModuleDefinition, Simulator } from "@celox-sim/celox";
import { describe, expect, test } from "vitest";
import { runGenTs } from "../../vite-plugin/src/generator.js";
import { generateSidecars } from "../../vite-plugin/src/sidecar.js";

const fixtureRoot = resolve(import.meta.dirname, "fixtures/generated-dts");

function typescriptCompiler(): string {
	const require = createRequire(import.meta.url);
	return join(dirname(require.resolve("typescript/package.json")), "bin/tsc");
}

describe("generated declarations", () => {
	const output = runGenTs(fixtureRoot);
	generateSidecars(output, fixtureRoot);

	test("type-check against the published runtime types", () => {
		const result = spawnSync(
			process.execPath,
			[typescriptCompiler(), "-p", fixtureRoot, "--pretty", "false"],
			{ encoding: "utf-8" },
		);
		expect(`${result.stdout}${result.stderr}`).toBe("");
		expect(result.status).toBe(0);
	});

	test("nested generate-block members match the runtime DUT keys", () => {
		const mod = output.modules.find((m) => m.moduleName === "Nested")!;
		const definition: ModuleDefinition<{
			d: bigint;
			readonly q: bigint;
			blk: { readonly "blk2.deep": bigint };
		}> = {
			__celox_module: true,
			name: mod.moduleName,
			sources: [],
			projectPath: fixtureRoot,
			ports: mod.ports,
			events: mod.events,
		};
		const sim = Simulator.create(definition);
		try {
			sim.dut.d = 0x5an;
			expect(sim.dut.q).toBe(0x5an);
			expect(sim.dut.blk["blk2.deep"]).toBe(0x5an);
		} finally {
			sim.dispose();
		}
	});
});
