import { expect, test, vi } from "vitest";
import {
	createWasmSimulatorBridge,
	type RawWasmSimulatorHandle,
} from "./wasm-bridge.js";
import { type EdgePlans, WasmEdgeRunner } from "./wasm-edges.js";

// Memory map of a small design driven by hand-written WASM modules:
// `gated = clk & en`, `inverted = ~clk`, and one counter per domain.
const CLK = 0;
const EN = 1;
const GATED = 2;
const INVERTED = 3;
const RISING = 8;
const GATED_COUNT = 9;
const FALLING = 10;

const I32_CONST = 0x41;
const LOAD8 = [0x2d, 0x00, 0x00];
const STORE8 = [0x3a, 0x00, 0x00];

/** A module exporting `run`, which executes `body` over the shared memory. */
function module(body: number[]): Uint8Array {
	const code = [0x00, ...body, 0x0b];
	return new Uint8Array([
		...[0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00],
		...[0x01, 0x04, 0x01, 0x60, 0x00, 0x00],
		...[0x02, 0x0f, 0x01, 0x03, 0x65, 0x6e, 0x76],
		...[0x06, 0x6d, 0x65, 0x6d, 0x6f, 0x72, 0x79, 0x02, 0x00, 0x01],
		...[0x03, 0x02, 0x01, 0x00],
		...[0x07, 0x07, 0x01, 0x03, 0x72, 0x75, 0x6e, 0x00, 0x00],
		...[0x0a, code.length + 2, 0x01, code.length, ...code],
	]);
}

/** `mem[target] = mem[target] + 1`. */
const increment = (target: number) => [
	I32_CONST,
	target,
	I32_CONST,
	target,
	...LOAD8,
	I32_CONST,
	1,
	0x6a,
	...STORE8,
];

// gated = clk & en; inverted = (clk ^ 1) & 1
const comb = module([
	I32_CONST,
	GATED,
	I32_CONST,
	CLK,
	...LOAD8,
	I32_CONST,
	EN,
	...LOAD8,
	0x71,
	...STORE8,
	I32_CONST,
	INVERTED,
	I32_CONST,
	CLK,
	...LOAD8,
	I32_CONST,
	1,
	0x73,
	I32_CONST,
	1,
	0x71,
	...STORE8,
]);

const PLANS: EdgePlans = {
	fourState: false,
	events: {
		clk: {
			signal: { offset: CLK, width: 1 },
			edge: "posedge",
			combRead: true,
			domains: [],
			gated: [
				{
					event: "gated",
					signal: { offset: GATED, width: 1 },
					edge: "posedge",
				},
				{
					event: "inverted",
					signal: { offset: INVERTED, width: 1 },
					edge: "posedge",
				},
			],
		},
		gated: {
			signal: { offset: GATED, width: 1 },
			edge: "posedge",
			combRead: false,
			domains: [],
			gated: [],
		},
		inverted: {
			signal: { offset: INVERTED, width: 1 },
			edge: "posedge",
			combRead: false,
			domains: [],
			gated: [],
		},
	},
};

function raw(plans: EdgePlans | undefined): RawWasmSimulatorHandle {
	const kernels: Record<string, Uint8Array> = {
		clk: module(increment(RISING)),
		gated: module(increment(GATED_COUNT)),
		inverted: module(increment(FALLING)),
	};
	const initial = new Uint8Array(16);
	initial[INVERTED] = 1;
	return {
		layoutJson: "{}",
		eventsJson: JSON.stringify({ clk: 0, gated: 1, inverted: 2 }),
		hierarchyJson: "{}",
		warningsJson: "[]",
		stableSize: 16,
		totalSize: 16,
		dispose: vi.fn(),
		initialMemoryBytes: () => initial,
		combWasmBytes: () => comb,
		eventWasmBytes: (name: string) => kernels[name],
		...(plans ? { edgePlansJson: () => JSON.stringify(plans) } : {}),
	};
}

test("the WASM bridge ticks and moves clocks as the native simulator does", () => {
	const { handle, sharedMemory } = createWasmSimulatorBridge(raw(PLANS));
	const counters = () => [
		sharedMemory[RISING],
		sharedMemory[GATED_COUNT],
		sharedMemory[FALLING],
	];
	sharedMemory[EN] = 1;
	handle.evalComb();

	// A tick is the active edge only: the gate passes it, `~clk` does not
	// rise, and `clk` stays low.
	handle.tick(0);
	expect(counters()).toEqual([1, 1, 0]);
	expect(sharedMemory[CLK]).toBe(0);

	expect(handle.activeEdgeIsRising?.(0)).toBe(true);
	expect(handle.edges?.([0], [true])).toBe(true);
	expect(counters()).toEqual([2, 2, 0]);
	expect(sharedMemory[CLK]).toBe(1);
	// Already high: no edge.
	handle.edges?.([0], [true]);
	expect(counters()).toEqual([2, 2, 0]);
	handle.edges?.([0], [false]);
	expect(counters()).toEqual([2, 2, 1]);

	// A closed gate passes no edge.
	sharedMemory[EN] = 0;
	handle.evalComb();
	handle.edges?.([0], [true]);
	handle.edges?.([0], [false]);
	expect(counters()).toEqual([3, 2, 2]);
});

test("a bridge without edge plans keeps the plain tick", () => {
	const { handle, sharedMemory } = createWasmSimulatorBridge(raw(undefined));
	sharedMemory[EN] = 1;
	handle.tick(0);
	expect([sharedMemory[RISING], sharedMemory[GATED_COUNT]]).toEqual([1, 0]);
	expect(handle.edges).toBeUndefined();
});

test("edges of a clock combinational logic does not read keep it settled", () => {
	const { handle, sharedMemory } = createWasmSimulatorBridge(raw(PLANS));
	handle.evalComb();
	// `gated` only clocks its counter: its falling edge runs nothing and
	// leaves combinational logic as it was.
	expect(handle.edges?.([1], [true], false)).toBe(true);
	expect(sharedMemory[GATED_COUNT]).toBe(1);
	expect(handle.edges?.([1], [false], false)).toBe(true);
	expect(handle.edges?.([1], [false], true)).toBe(false);
	expect(handle.edges?.([1], [true], true)).toBe(true);
	expect(sharedMemory[GATED_COUNT]).toBe(2);
});

test("four-state edges judge a gated clock from x and z (IEEE 1800-2023 Table 9-2)", () => {
	// Value bytes with their unknown masks in the following byte.
	const clk = { offset: 0, width: 1 };
	const en = { offset: 2, width: 1 };
	const gated = { offset: 4, width: 1 };
	const COUNT = 8;
	const plans: EdgePlans = {
		fourState: true,
		events: {
			clk: {
				signal: clk,
				edge: "posedge",
				combRead: true,
				domains: [],
				gated: [{ event: "gated", signal: gated, edge: "posedge" }],
			},
			gated: {
				signal: gated,
				edge: "posedge",
				combRead: false,
				domains: [],
				gated: [],
			},
		},
	};
	const memory = { buffer: new ArrayBuffer(16) };
	const bytes = new Uint8Array(memory.buffer);
	const set = (offset: number, level: "0" | "1" | "x" | "z") => {
		bytes[offset] = level === "1" || level === "x" ? 1 : 0;
		bytes[offset + 1] = level === "x" || level === "z" ? 1 : 0;
	};
	// gated = clk & en, with four-state AND: a known 0 wins, then x.
	const evalComb = () => {
		const known0 = (offset: number) =>
			(bytes[offset + 1] ?? 0) === 0 && (bytes[offset] ?? 0) === 0;
		const known1 = (offset: number) =>
			(bytes[offset + 1] ?? 0) === 0 && (bytes[offset] ?? 0) === 1;
		if (known0(clk.offset) || known0(en.offset)) {
			set(gated.offset, "0");
		} else if (known1(clk.offset) && known1(en.offset)) {
			set(gated.offset, "1");
		} else {
			set(gated.offset, "x");
		}
	};
	const kernels = (name: string) =>
		name === "gated"
			? {
					evalApply: () => {
						bytes[COUNT] = (bytes[COUNT] ?? 0) + 1;
					},
				}
			: undefined;
	const runner = new WasmEdgeRunner(
		memory,
		plans,
		{ clk: 0, gated: 1 },
		kernels,
		evalComb,
	);
	const rise = () => runner.edges([0], [true]);
	const fall = () => runner.edges([0], [false]);

	// x & x stays x when the clock rises from x: no edge.
	set(clk.offset, "x");
	set(en.offset, "x");
	evalComb();
	rise();
	expect(bytes[COUNT]).toBe(0);
	// x & 1 -> 1 & 1: a posedge.
	set(clk.offset, "x");
	set(en.offset, "1");
	evalComb();
	rise();
	expect(bytes[COUNT]).toBe(1);
	// z & 1 is x: z -> 1 of the clock is x -> 1 of the gated clock.
	set(clk.offset, "z");
	evalComb();
	rise();
	expect(bytes[COUNT]).toBe(2);
	// A falling edge from x is no posedge, and the clock ends at a known 0.
	set(clk.offset, "x");
	evalComb();
	fall();
	expect(bytes[COUNT]).toBe(2);
	expect([bytes[clk.offset], bytes[clk.offset + 1]]).toEqual([0, 0]);
	rise();
	expect(bytes[COUNT]).toBe(3);
});
