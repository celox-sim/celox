/**
 * Checkpoint / restore through the native addon and the WASM bridge.
 */

import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { afterEach, describe, expect, test, vi } from "vitest";
import { Simulation } from "./simulation.js";
import { Simulator } from "./simulator.js";
import {
	createWasmSimulatorBridge,
	type RawWasmSimulatorHandle,
} from "./wasm-bridge.js";

const COUNTER_SOURCE = `
module Counter (
    clk: input clock,
    rst: input reset,
    en: input logic,
    count: output logic<8>,
    next: output logic<8>,
) {
    var count_r: logic<8>;

    always_ff (clk, rst) {
        if_reset {
            count_r = 0;
        } else if en {
            count_r = count_r + 1;
        }
    }

    assign count = count_r;
    assign next = count_r + 1;
}
`;

const OTHER_SOURCE = `
module Other (
    clk: input clock,
    d: input logic<8>,
    q: output logic<8>,
) {
    always_ff (clk) {
        q = d;
    }
}
`;

interface CounterPorts {
	rst: bigint;
	en: bigint;
	readonly count: bigint;
	readonly next: bigint;
}

const tempDirs: string[] = [];
afterEach(() => {
	for (const dir of tempDirs.splice(0)) {
		rmSync(dir, { recursive: true, force: true });
	}
});

function vcdPath(): string {
	const dir = mkdtempSync(path.join(tmpdir(), "celox-checkpoint-"));
	tempDirs.push(dir);
	return path.join(dir, "wave.vcd");
}

function startCounter(sim: Simulator<CounterPorts>): void {
	// Default reset is async low.
	sim.dut.rst = 0n;
	sim.tick();
	sim.dut.rst = 1n;
	sim.dut.en = 1n;
}

function countTrace(sim: Simulator<CounterPorts>, ticks: number): bigint[] {
	const trace: bigint[] = [];
	for (let i = 0; i < ticks; i++) {
		sim.tick();
		trace.push(sim.dut.count);
	}
	return trace;
}

describe("Simulator checkpoints", () => {
	test("restore replays the same cycles", () => {
		const sim = Simulator.fromSource<CounterPorts>(COUNTER_SOURCE, "Counter");
		startCounter(sim);
		sim.tick(5);
		const checkpoint = sim.checkpoint();
		expect(checkpoint.stateSize).toBeGreaterThan(0);
		const saved = [sim.dut.count, sim.dut.next];
		const expected = countTrace(sim, 7);

		for (let i = 0; i < 2; i++) {
			sim.restore(checkpoint);
			// Combinational outputs are settled by the restore itself.
			expect([sim.dut.count, sim.dut.next]).toEqual(saved);
			expect(countTrace(sim, 7)).toEqual(expected);
		}
		sim.dispose();
	});

	test("restore overrides inputs written after the checkpoint", () => {
		const sim = Simulator.fromSource<CounterPorts>(COUNTER_SOURCE, "Counter");
		startCounter(sim);
		sim.tick(3);
		const checkpoint = sim.checkpoint();
		sim.dut.en = 0n;
		sim.restore(checkpoint);
		sim.tick();
		expect(sim.dut.count).toBe(4n);
		sim.dispose();
	});

	test("a checkpoint forks into another simulator of the same design", () => {
		const source = Simulator.fromSource<CounterPorts>(
			COUNTER_SOURCE,
			"Counter",
		);
		const fork = Simulator.fromSource<CounterPorts>(COUNTER_SOURCE, "Counter");
		startCounter(source);
		source.tick(9);
		fork.restore(source.checkpoint());
		expect(countTrace(fork, 5)).toEqual(countTrace(source, 5));
		source.dispose();
		fork.dispose();
	});

	test("restore rejects another design", () => {
		const source = Simulator.fromSource(COUNTER_SOURCE, "Counter");
		const other = Simulator.fromSource(OTHER_SOURCE, "Other");
		expect(() => other.restore(source.checkpoint())).toThrow(
			/different design/,
		);
		source.dispose();
		other.dispose();
	});

	test("restore keeps VCD output and rejects rewound timestamps", () => {
		const first = vcdPath();
		const sim = Simulator.fromSource<CounterPorts>(COUNTER_SOURCE, "Counter", {
			vcd: first,
		});
		startCounter(sim);
		const checkpoint = sim.checkpoint();
		for (let time = 0; time < 4; time++) {
			sim.tick();
			sim.dump(time);
		}
		sim.restore(checkpoint);
		expect(() => sim.dump(1)).toThrow(/earlier than the last dumped/);
		sim.dump(10);

		const second = path.join(path.dirname(first), "second.vcd");
		sim.switchVcd(second);
		sim.dump(0);
		sim.dispose();
		expect(readFileSync(second, "utf8")).toContain("$enddefinitions");
	});

	test("tiered simulators restore across promotion", async () => {
		const sim = Simulator.fromSource<CounterPorts>(COUNTER_SOURCE, "Counter", {
			tier: true,
		});
		startCounter(sim);
		sim.tick(4);
		const checkpoint = sim.checkpoint();
		const expected = countTrace(sim, 6);
		await vi.waitFor(
			() => {
				sim.tick();
				const handle = (
					sim as unknown as { _handle: { tierCompiled?(): boolean | null } }
				)._handle;
				expect(handle.tierCompiled?.()).toBe(true);
			},
			{ timeout: 60_000 },
		);
		sim.restore(checkpoint);
		expect(countTrace(sim, 6)).toEqual(expected);
		sim.dispose();
	});
});

describe("Simulation checkpoints", () => {
	function startTimedCounter(sim: Simulation<CounterPorts>): void {
		sim.addClock("clk", { period: 10 });
		sim.dut.rst = 0n;
		sim.runUntil(20);
		sim.dut.rst = 1n;
		sim.dut.en = 1n;
	}

	function timedTrace(
		sim: Simulation<CounterPorts>,
		until: number,
	): [number, bigint][] {
		const trace: [number, bigint][] = [];
		while (sim.nextEventTime() !== null && sim.nextEventTime()! <= until) {
			sim.step();
			trace.push([sim.time(), sim.dut.count]);
		}
		return trace;
	}

	test("restore rewinds time, clocks and pending events", () => {
		const sim = Simulation.fromSource<CounterPorts>(COUNTER_SOURCE, "Counter");
		startTimedCounter(sim);
		sim.runUntil(100);
		const checkpoint = sim.checkpoint();
		expect(checkpoint.time).toBe(100);
		const saved = sim.dut.count;
		const expected = timedTrace(sim, 300);

		sim.restore(checkpoint);
		expect(sim.time()).toBe(100);
		expect(sim.dut.count).toBe(saved);
		expect(timedTrace(sim, 300)).toEqual(expected);
		sim.dispose();
	});

	test("restore removes clocks added after the checkpoint", () => {
		const sim = Simulation.fromSource<CounterPorts>(COUNTER_SOURCE, "Counter");
		const checkpoint = sim.checkpoint();
		sim.addClock("clk", { period: 10 });
		expect(sim.nextEventTime()).not.toBeNull();
		sim.restore(checkpoint);
		expect(sim.nextEventTime()).toBeNull();
		expect(() => sim.waitForCycles("clk", 1)).toThrow();
		sim.dispose();
	});

	test("a checkpoint forks into another simulation of the same design", () => {
		const source = Simulation.fromSource<CounterPorts>(
			COUNTER_SOURCE,
			"Counter",
		);
		const fork = Simulation.fromSource<CounterPorts>(COUNTER_SOURCE, "Counter");
		startTimedCounter(source);
		source.runUntil(100);
		fork.restore(source.checkpoint());
		expect(fork.time()).toBe(100);
		expect(timedTrace(fork, 300)).toEqual(timedTrace(source, 300));
		source.dispose();
		fork.dispose();
	});

	test("a simulation switches VCD files before rewinding", () => {
		const first = vcdPath();
		const sim = Simulation.fromSource<CounterPorts>(COUNTER_SOURCE, "Counter", {
			vcd: first,
		});
		startTimedCounter(sim);
		sim.runUntil(50);
		const checkpoint = sim.checkpoint();
		sim.runUntil(100);
		expect(() => sim.restore(checkpoint)).toThrow(/rewind the VCD output/);
		expect(sim.time()).toBe(100);

		const second = path.join(path.dirname(first), "second.vcd");
		sim.switchVcd(second);
		sim.restore(checkpoint);
		sim.runUntil(80);
		sim.dispose();
		const vcd = readFileSync(second, "utf8");
		expect(vcd).toContain("#55");
		expect(vcd).toContain("#80");
	});

	test("saving settles inputs written through the DUT", () => {
		const source = `
module Pass (clk: input clock, a: input logic<8>, y: output logic<8>) {
    assign y = a + 8'd1;
}
`;
		const sim = Simulation.fromSource<{ a: bigint; readonly y: bigint }>(
			source,
			"Pass",
		);
		sim.dut.a = 5n;
		expect(sim.dut.y).toBe(6n); // settles combinational logic
		sim.dut.a = 7n;
		sim.saveState();
		expect(sim.dut.y).toBe(8n);
		sim.dispose();
	});
});

describe("State files", () => {
	test("a simulator state loads into a simulator of another optimization level", () => {
		const source = Simulator.fromSource<CounterPorts>(
			COUNTER_SOURCE,
			"Counter",
			{
				optLevel: "O2",
			},
		);
		startCounter(source);
		source.tick(6);
		const bytes = source.saveState();

		const target = Simulator.fromSource<CounterPorts>(
			COUNTER_SOURCE,
			"Counter",
			{
				optLevel: "O0",
			},
		);
		target.loadState(bytes);
		expect([target.dut.count, target.dut.next]).toEqual([
			source.dut.count,
			source.dut.next,
		]);
		expect(countTrace(target, 5)).toEqual(countTrace(source, 5));
		source.dispose();
		target.dispose();
	});

	test("state files survive a round trip through the file system", () => {
		const source = Simulator.fromSource<CounterPorts>(
			COUNTER_SOURCE,
			"Counter",
		);
		startCounter(source);
		source.tick(3);
		const file = path.join(path.dirname(vcdPath()), "counter.state");
		writeFileSync(file, source.saveState());

		const target = Simulator.fromSource<CounterPorts>(
			COUNTER_SOURCE,
			"Counter",
		);
		target.loadState(readFileSync(file));
		expect(target.dut.count).toBe(3n);
		source.dispose();
		target.dispose();
	});

	test("a state of another design is rejected without changes", () => {
		const source = Simulator.fromSource(OTHER_SOURCE, "Other");
		const target = Simulator.fromSource<CounterPorts>(
			COUNTER_SOURCE,
			"Counter",
		);
		startCounter(target);
		target.tick(2);
		expect(() => target.loadState(source.saveState())).toThrow(
			/does not match the design/,
		);
		expect(target.dut.count).toBe(2n);
		source.dispose();
		target.dispose();
	});

	test("a simulation state carries time, clocks and pending events", () => {
		const source = Simulation.fromSource<CounterPorts>(
			COUNTER_SOURCE,
			"Counter",
		);
		source.addClock("clk", { period: 10 });
		source.dut.rst = 0n;
		source.runUntil(20);
		source.dut.rst = 1n;
		source.dut.en = 1n;
		source.runUntil(100);
		const bytes = source.saveState();

		// The target registers no clock: the state file supplies it.
		const target = Simulation.fromSource<CounterPorts>(
			COUNTER_SOURCE,
			"Counter",
		);
		target.loadState(bytes);
		expect(target.time()).toBe(100);
		expect(target.dut.count).toBe(source.dut.count);
		target.waitForCycles("clk", 3);
		source.waitForCycles("clk", 3);
		expect(target.time()).toBe(source.time());
		expect(target.dut.count).toBe(source.dut.count);
		source.dispose();
		target.dispose();
	});

	test("a simulation rejects a state saved from a simulator", () => {
		const simulator = Simulator.fromSource(COUNTER_SOURCE, "Counter");
		const simulation = Simulation.fromSource(COUNTER_SOURCE, "Counter");
		expect(() => simulation.loadState(simulator.saveState())).toThrow(
			/no simulation schedule/,
		);
		simulator.dispose();
		simulation.dispose();
	});
});

// ---------------------------------------------------------------------------
// WASM bridge
// ---------------------------------------------------------------------------

/**
 * A module importing `env.memory` and exporting `run`, whose body is
 * `mem[target] = mem[source] + addend` (all as single bytes).
 */
function byteAddModule(target: number, source: number, addend: number) {
	const body = [
		0x00, // no locals
		0x41,
		target, // i32.const target
		0x41,
		source, // i32.const source
		0x2d,
		0x00,
		0x00, // i32.load8_u
		0x41,
		addend, // i32.const addend (< 64)
		0x6a, // i32.add
		0x3a,
		0x00,
		0x00, // i32.store8
		0x0b, // end
	];
	return new Uint8Array([
		...[0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00],
		...[0x01, 0x04, 0x01, 0x60, 0x00, 0x00], // type: () -> ()
		...[0x02, 0x0f, 0x01, 0x03, 0x65, 0x6e, 0x76], // import "env"
		...[0x06, 0x6d, 0x65, 0x6d, 0x6f, 0x72, 0x79, 0x02, 0x00, 0x01], // "memory"
		...[0x03, 0x02, 0x01, 0x00], // function 0: type 0
		...[0x07, 0x07, 0x01, 0x03, 0x72, 0x75, 0x6e, 0x00, 0x00], // export "run"
		...[0x0a, body.length + 2, 0x01, body.length, ...body],
	]);
}

const STATE_OFFSET = 32;
const COUNTER = 32;
const OUTPUT = 33;

function wasmHandle(
	overrides: Partial<RawWasmSimulatorHandle> = {},
): RawWasmSimulatorHandle {
	return {
		layoutJson: "{}",
		eventsJson: JSON.stringify({ clk: 0 }),
		hierarchyJson: "{}",
		warningsJson: "[]",
		stableSize: 40,
		totalSize: 64,
		stateOffset: STATE_OFFSET,
		stateFingerprint: "counter",
		dispose: vi.fn(),
		initialMemoryBytes: () => new Uint8Array(64),
		// comb: output = counter + 10; event: counter += 1
		combWasmBytes: () => byteAddModule(OUTPUT, COUNTER, 10),
		eventWasmBytes: () => byteAddModule(COUNTER, COUNTER, 1),
		...overrides,
	};
}

describe("WASM bridge checkpoints", () => {
	test("restore returns to the saved state and settles comb logic", () => {
		const { handle, sharedMemory } = createWasmSimulatorBridge(wasmHandle());
		handle.tickN(0, 3);
		expect([sharedMemory[COUNTER], sharedMemory[OUTPUT]]).toEqual([3, 13]);
		const checkpoint = handle.checkpoint!();
		expect(checkpoint.stateSize).toBe(40 - STATE_OFFSET);

		handle.tickN(0, 2);
		sharedMemory[OUTPUT] = 0;
		handle.restore!(checkpoint);
		expect([sharedMemory[COUNTER], sharedMemory[OUTPUT]]).toEqual([3, 13]);
	});

	test("restore rejects a checkpoint of another design", () => {
		const source = createWasmSimulatorBridge(wasmHandle());
		const other = createWasmSimulatorBridge(
			wasmHandle({ stateFingerprint: "other" }),
		);
		expect(() => other.handle.restore!(source.handle.checkpoint!())).toThrow(
			/different design/,
		);
	});

	test("addons without checkpoint metadata do not offer checkpoints", () => {
		const { handle } = createWasmSimulatorBridge(
			wasmHandle({ stateOffset: undefined, stateFingerprint: undefined }),
		);
		expect(handle.checkpoint).toBeUndefined();
		expect(handle.restore).toBeUndefined();
	});
});
