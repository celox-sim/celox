import { expect, test } from "vitest";
import {
	buildNapiOpts,
	createSimulatorBridge,
	loadNativeAddon,
} from "./napi-helpers.js";
import { Simulator } from "./simulator.js";

test("plusargs preserve ordering and leading plus signs across the NAPI boundary", () => {
	const plusargs = ["+flag", "+number=1", "+number=2"];
	expect(buildNapiOpts({ plusargs })).toEqual({ plusargs });
	expect(buildNapiOpts({ plusargs: [] })).toEqual({ plusargs: [] });
});

test("the native addon accepts ordered plusargs options", () => {
	const simulator = Simulator.create<{ a: bigint; readonly value: bigint }>(
		{
			__celox_module: true,
			name: "Top",
			sources: [
				{
					path: "Top.veryl",
					content:
						"module Top (a: input logic<8>, value: output logic<8>) { assign value = a; }",
				},
			],
			ports: {
				a: { direction: "input", type: "logic", width: 8 },
				value: { direction: "output", type: "logic", width: 8 },
			},
			events: [],
		},
		{
			plusargs: ["+flag", "+number=1", "+number=2"],
			__nativeCreate: createSimulatorBridge(loadNativeAddon()),
		},
	);
	try {
		simulator.dut.a = 42n;
		expect(simulator.dut.value).toBe(42n);
	} finally {
		simulator.dispose();
	}
});
