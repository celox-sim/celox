import { createRequire } from "node:module";
import { afterEach, describe, expect, test } from "vitest";
import {
	createSimulationBridge,
	createSimulatorBridge,
	loadNativeAddon,
} from "./napi-helpers.js";
import { Simulation } from "./simulation.js";
import { Simulator } from "./simulator.js";
import type { ModuleDefinition } from "./types.js";

const require = createRequire(import.meta.url);
const path = require.resolve("../fixtures/faulty-addon.cjs");
const fixture = require(path) as {
	__stats: { created: number; disposed: number };
	__settings: { stage: string; disposeThrows: boolean };
	__error: Error;
};
const addon = loadNativeAddon(path);
const module: ModuleDefinition<Record<string, unknown>> = {
	__celox_module: true,
	name: "Top",
	sources: [],
	ports: {},
	events: [],
};
const factories = [
	[
		"Simulator.fromSource",
		() => Simulator.fromSource("", "Top", { nativeAddonPath: path }),
	],
	[
		"Simulator.fromSource tiered",
		() =>
			Simulator.fromSource("", "Top", { nativeAddonPath: path, tier: true }),
	],
	[
		"Simulator.fromProject",
		() => Simulator.fromProject(".", "Top", { nativeAddonPath: path }),
	],
	[
		"Simulator.fromProject tiered",
		() =>
			Simulator.fromProject(".", "Top", { nativeAddonPath: path, tier: true }),
	],
	[
		"Simulator.fromFrontendArtifact",
		() => Simulator.fromFrontendArtifact("{}", { nativeAddonPath: path }),
	],
	[
		"Simulator.create",
		() =>
			Simulator.create(module, {
				__nativeCreate: createSimulatorBridge(addon),
			}),
	],
	[
		"Simulation.fromSource",
		() => Simulation.fromSource("", "Top", { nativeAddonPath: path }),
	],
	[
		"Simulation.fromProject",
		() => Simulation.fromProject(".", "Top", { nativeAddonPath: path }),
	],
	[
		"Simulation.fromFrontendArtifact",
		() => Simulation.fromFrontendArtifact("{}", { nativeAddonPath: path }),
	],
	[
		"Simulation.create",
		() =>
			Simulation.create(module, {
				__nativeCreate: createSimulationBridge(addon),
			}),
	],
] as const;
afterEach(() => {
	fixture.__settings.stage = "ok";
	fixture.__settings.disposeThrows = false;
});
describe.each(factories)("%s construction ownership", (_name, create) => {
	test.each(["layout", "events", "hierarchy", "warnings", "memory", "dut"])(
		"releases the handle after %s failure",
		(stage) => {
			fixture.__settings.stage = stage;
			const before = { ...fixture.__stats };
			expect(create).toThrow();
			expect(fixture.__stats.created - before.created).toBe(1);
			expect(fixture.__stats.disposed - before.disposed).toBe(1);
		},
	);
	test("preserves the original error if disposal also fails", () => {
		fixture.__settings.stage = "memory";
		fixture.__settings.disposeThrows = true;
		const before = { ...fixture.__stats };
		expect(create).toThrow(fixture.__error);
		expect(fixture.__stats.disposed - before.disposed).toBe(1);
	});
	test("transfers ownership on success", () => {
		const before = { ...fixture.__stats };
		const sim = create();
		expect(fixture.__stats.disposed - before.disposed).toBe(0);
		sim.dispose();
		expect(fixture.__stats.disposed - before.disposed).toBe(1);
	});
});
test("releases a handle after browser WASM instantiation fails", () => {
	fixture.__settings.stage = "wasm";
	const before = { ...fixture.__stats };
	expect(() =>
		Simulator.fromSource("", "Top", { nativeAddonPath: path }),
	).toThrow(/WebAssembly|expected|magic|wasm/i);
	expect(fixture.__stats.disposed - before.disposed).toBe(1);
});
