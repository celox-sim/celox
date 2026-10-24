/**
 * The playground's simulators: the compiled WebAssembly modules of a design,
 * driven as `@celox-sim/celox` drives them in the browser.
 *
 * Ticks, explicit edges and timed steps run through the edge runner of
 * `@celox-sim/celox`, from the build-time edge plans, so derived and gated
 * clocks fire exactly as in the native simulator.
 *
 * @module
 */

import {
	cachedEdgeKernels,
	type KernelRun,
	WasmEdgeRunner,
} from "@celox-sim/celox/wasm-edges";
import type { CompiledSimulator } from "./celox-compiler-protocol.js";
import {
	initializeFourStateMemory,
	type PlaygroundEventHandle,
	type PlaygroundSignalValue,
	runPlaygroundTicks,
	writeSignalValue,
} from "./playground-runtime-helpers.js";
import { evaluateWasmFloat } from "./wasm-float.js";

/** A clock edge, as `Simulator.edges()` of `@celox-sim/celox` names it. */
export type PlaygroundClockEdge = "posedge" | "negedge";

/** An event, by handle or ID. */
export type PlaygroundEvent = PlaygroundEventHandle | number;

/** Receives `dump()` snapshots for the waveform viewer. */
export type PlaygroundDumpSink = (
	timestamp: number,
	signals: { name: string; width: number }[],
	values: bigint[],
) => void;

export type PlaygroundFourStateValue = {
	__fourState: true;
	value: bigint;
	mask: bigint;
};

function runExport(instance: WebAssembly.Instance): KernelRun {
	return () => (instance.exports.run as CallableFunction)();
}

/** One instantiated design: its memory, kernels and edge runner. */
class PlaygroundInstance {
	readonly layout: Record<string, any>;
	readonly events: Record<string, number>;
	readonly eventNames: string[];
	readonly knownEventIds: ReadonlySet<number>;
	readonly view: DataView;
	readonly runner: WasmEdgeRunner;
	readonly fourState: boolean;
	/** Combinational logic has pending input changes. */
	dirty = false;
	private readonly comb: KernelRun;

	constructor(compiled: CompiledSimulator) {
		this.layout = compiled.layout;
		this.events = compiled.events;
		this.eventNames = Object.keys(compiled.events);
		this.knownEventIds = new Set(Object.values(compiled.events));
		this.fourState = compiled.fourState;
		const pages = Math.max(1, Math.ceil(compiled.totalSize / 65536));
		const memory = new WebAssembly.Memory({ initial: pages });
		initializeFourStateMemory(memory, compiled.fourStateInitRegions);
		this.view = new DataView(memory.buffer);
		const instantiate = (module: WebAssembly.Module) =>
			runExport(
				new WebAssembly.Instance(module, {
					env: { memory, celox_extern_4294967295: evaluateWasmFloat },
				}),
			);
		this.comb = instantiate(compiled.combModule);
		const evalApply = new Map<string, KernelRun>();
		for (const [name, module] of Object.entries(compiled.eventModules)) {
			evalApply.set(name, instantiate(module));
		}
		const kernels = cachedEdgeKernels(
			(name) => evalApply.get(name),
			(name, kind) => {
				const phases = compiled.phaseModules[name];
				return phases ? instantiate(phases[kind]) : undefined;
			},
		);
		this.runner = new WasmEdgeRunner(
			memory,
			compiled.edgePlans,
			compiled.events,
			kernels,
			this.comb,
		);
	}

	evalComb(): void {
		this.comb();
		this.dirty = false;
	}

	flushDirty(): void {
		if (this.dirty) this.evalComb();
	}

	/** A clock period of the event's signal, as `tick`. */
	tickOne(eventId: number): void {
		this.runner.tick(eventId);
		this.dirty = false;
	}

	/** Coincident edges; see `Simulator.edges()` of `@celox-sim/celox`. */
	edges(eventIds: number[], rising: boolean[]): void {
		this.dirty = !this.runner.edges(eventIds, rising, this.dirty);
	}

	activeIsRising(eventId: number): boolean {
		return this.runner.activeEdgeIsRising(eventId);
	}

	resolveEvent(name: string): number {
		const id = this.events[name];
		if (id === undefined) {
			throw new Error(
				`Unknown event '${name}'. Available: ${this.eventNames.join(", ")}`,
			);
		}
		return id;
	}

	signal(name: string): any {
		const sig = this.layout[name];
		if (!sig) {
			throw new Error(
				`Unknown port '${name}'. Available: ${Object.keys(this.layout).join(", ")}`,
			);
		}
		return sig;
	}

	read(sig: any, plane = 0): bigint {
		let value = 0n;
		const base = sig.offset + plane * sig.byte_size;
		for (let i = sig.byte_size - 1; i >= 0; i--)
			value = (value << 8n) | BigInt(this.view.getUint8(base + i));
		if (sig.width < 64) value &= (1n << BigInt(sig.width)) - 1n;
		return value;
	}

	write(name: string, value: PlaygroundSignalValue): void {
		const sig = this.layout[name];
		if (!sig) throw new Error(`Signal '${name}' not found`);
		writeSignalValue(this.view, sig, value, this.fourState);
		this.dirty = true;
	}

	/** A DUT whose output reads settle combinational logic first. */
	dut(): Record<string, bigint> {
		return new Proxy({} as Record<string, bigint>, {
			get: (_, prop: string) => {
				const sig = this.layout[prop];
				if (!sig) return undefined;
				if (sig.direction !== "input") this.flushDirty();
				return this.read(sig);
			},
			set: (_, prop: string, value: PlaygroundSignalValue) => {
				this.write(prop, value);
				return true;
			},
		});
	}

	fourStateOf(portName: string): PlaygroundFourStateValue {
		const sig = this.signal(portName);
		if (sig.direction !== "input") this.flushDirty();
		const value = this.read(sig);
		const mask = sig.is_4state ? this.read(sig, 1) : 0n;
		return { __fourState: true, value, mask };
	}

	dump(timestamp: number, sink: PlaygroundDumpSink | undefined): void {
		this.flushDirty();
		const entries = Object.entries(this.layout);
		sink?.(
			timestamp,
			entries.map(([name, sig]) => ({ name, width: sig.width })),
			entries.map(([, sig]) => this.read(sig)),
		);
	}
}

/**
 * A simulator with untimed ticks and explicit edges, as `Simulator` of
 * `@celox-sim/celox`.
 */
export function createPlaygroundSimulator(
	compiled: CompiledSimulator,
	sink?: PlaygroundDumpSink,
) {
	const design = new PlaygroundInstance(compiled);
	const defaultEventId =
		design.eventNames.length > 0
			? (design.events[design.eventNames[0] as string] as number)
			: -1;
	const tickRuntime = {
		defaultEventId,
		knownEventIds: design.knownEventIds,
		flushDirty: () => design.flushDirty(),
		tickOne: (eventId: number) => design.tickOne(eventId),
	};

	const resolveEventId = (event: PlaygroundEvent | undefined): number => {
		if (event === undefined) {
			if (design.knownEventIds.size === 0) {
				throw new Error("Simulator has no events");
			}
			return defaultEventId;
		}
		const id = typeof event === "number" ? event : event.id;
		if (!design.knownEventIds.has(id)) {
			throw new RangeError(`Unknown event ID ${id}`);
		}
		return id;
	};

	const sim = {
		dut: design.dut(),
		warnings: [] as readonly string[],
		tick(eventOrCount?: PlaygroundEvent, count?: number) {
			runPlaygroundTicks(tickRuntime, eventOrCount, count);
		},
		/**
		 * Move event signals at one instant, as coincident edges: every domain
		 * sensitive to one of them evaluates against the state before the
		 * edges, then all of them commit.
		 */
		edges(
			moves: ReadonlyArray<readonly [PlaygroundEvent, PlaygroundClockEdge]>,
		) {
			const ids: number[] = [];
			const rising: boolean[] = [];
			for (const [event, edge] of moves) {
				if (edge !== "posedge" && edge !== "negedge") {
					throw new RangeError(`Unknown clock edge '${String(edge)}'`);
				}
				ids.push(resolveEventId(event));
				rising.push(edge === "posedge");
			}
			design.edges(ids, rising);
		},
		/** Move a clock's signal to 1 and run every domain sensitive to it. */
		posedge(event?: PlaygroundEvent) {
			sim.edges([[resolveEventId(event), "posedge"]]);
		},
		/** Move a clock's signal to 0; see `posedge()`. */
		negedge(event?: PlaygroundEvent) {
			sim.edges([[resolveEventId(event), "negedge"]]);
		},
		/** The event's active edge. */
		activeEdge(event?: PlaygroundEvent) {
			const id = resolveEventId(event);
			sim.edges([[id, design.activeIsRising(id) ? "posedge" : "negedge"]]);
		},
		/** The edge opposite to the event's active edge. */
		inactiveEdge(event?: PlaygroundEvent) {
			const id = resolveEventId(event);
			sim.edges([[id, design.activeIsRising(id) ? "negedge" : "posedge"]]);
		},
		event(name: string): PlaygroundEventHandle {
			return { name, id: design.resolveEvent(name) };
		},
		fourState(portName: string): PlaygroundFourStateValue {
			return design.fourStateOf(portName);
		},
		dump(timestamp: number) {
			design.dump(timestamp, sink);
		},
		dispose() {},
	};
	return sim;
}

/**
 * A timed simulation, as `Simulation` of `@celox-sim/celox`: scheduled
 * signal changes at one time are coincident edges.
 */
export function createPlaygroundSimulation(
	compiled: CompiledSimulator,
	sink?: PlaygroundDumpSink,
) {
	const design = new PlaygroundInstance(compiled);
	const { layout } = design;

	type QueueEntry = {
		time: number;
		eventId: number;
		nextVal: number;
		clockHalfPeriod?: number;
	};
	const queue: QueueEntry[] = [];
	const clocks = new Map<string, { period: number; eventId: number }>();
	let currentTime = 0;

	function enqueue(entry: QueueEntry) {
		let lo = 0;
		let hi = queue.length;
		while (lo < hi) {
			const mid = (lo + hi) >>> 1;
			if ((queue[mid] as QueueEntry).time <= entry.time) lo = mid + 1;
			else hi = mid;
		}
		queue.splice(lo, 0, entry);
	}

	/** Pop every change at the next time and run them as coincident edges. */
	function processStep(): number | null {
		const first = queue[0];
		if (!first) return null;
		const t = first.time;
		const batch: QueueEntry[] = [];
		while (queue.length > 0 && (queue[0] as QueueEntry).time === t) {
			batch.push(queue.shift() as QueueEntry);
		}
		currentTime = t;
		design.edges(
			batch.map((entry) => entry.eventId),
			batch.map((entry) => entry.nextVal !== 0),
		);
		design.flushDirty();
		for (const entry of batch) {
			if (entry.clockHalfPeriod != null) {
				enqueue({
					time: currentTime + entry.clockHalfPeriod,
					eventId: entry.eventId,
					nextVal: 1 - entry.nextVal,
					clockHalfPeriod: entry.clockHalfPeriod,
				});
			}
		}
		return currentTime;
	}

	const dut = design.dut();
	const sim = {
		dut,
		warnings: [] as readonly string[],

		addClock(name: string, opts: { period: number; initialDelay?: number }) {
			const eventId = design.resolveEvent(name);
			clocks.set(name, { period: opts.period, eventId });
			// The first edge (0→1) at the delay, then toggles every half period.
			enqueue({
				time: currentTime + (opts.initialDelay ?? 0),
				eventId,
				nextVal: 1,
				clockHalfPeriod: opts.period / 2,
			});
		},

		schedule(name: string, opts: { time: number; value: number }) {
			enqueue({
				time: opts.time,
				eventId: design.resolveEvent(name),
				nextVal: opts.value,
			});
		},

		step: processStep,

		time(): number {
			return currentTime;
		},

		nextEventTime(): number | null {
			return queue[0]?.time ?? null;
		},

		runUntil(endTime: number, opts?: { maxSteps?: number }) {
			const max = opts?.maxSteps;
			let steps = 0;
			while (queue.length > 0 && (queue[0] as QueueEntry).time <= endTime) {
				processStep();
				steps++;
				if (max != null && steps >= max) {
					throw new Error(
						`runUntil: exceeded ${max} steps at time ${currentTime} (target ${endTime})`,
					);
				}
			}
			currentTime = endTime;
		},

		waitUntil(condition: () => boolean, opts?: { maxSteps?: number }): number {
			const max = opts?.maxSteps ?? 100_000;
			let steps = 0;
			while (!condition()) {
				if (queue.length === 0) break;
				processStep();
				steps++;
				if (steps >= max) {
					throw new Error(
						`waitUntil: condition not met after ${max} steps at time ${currentTime}`,
					);
				}
			}
			return currentTime;
		},

		waitForCycles(
			clock: string,
			count: number,
			opts?: { maxSteps?: number },
		): number {
			if (!clocks.has(clock)) {
				throw new Error(
					`No clock registered for '${clock}'. Call addClock() first.`,
				);
			}
			const sig = layout[clock];
			if (!sig) throw new Error(`No layout entry for clock '${clock}'.`);
			const readClk = () => design.view.getUint8(sig.offset) & 1;
			let prev = readClk();
			let remaining = count;
			return sim.waitUntil(() => {
				const curr = readClk();
				if (prev === 0 && curr !== 0) remaining--;
				prev = curr;
				return remaining <= 0;
			}, opts);
		},

		reset(signal: string, opts?: { activeCycles?: number; duration?: number }) {
			const sig = design.signal(signal);
			const typeKind: string = sig.type_kind ?? "";
			if (!typeKind.startsWith("reset")) {
				throw new Error(
					`Port '${signal}' is not a reset signal (type_kind: '${typeKind}').`,
				);
			}
			const isActiveLow =
				typeKind === "reset_async_low" || typeKind === "reset_sync_low";
			const activeValue = isActiveLow ? 0n : 1n;
			const inactiveValue = isActiveLow ? 1n : 0n;

			if (opts?.duration != null) {
				dut[signal] = activeValue;
				sim.runUntil(currentTime + opts.duration);
			} else {
				const firstClock = clocks.keys().next().value;
				if (firstClock) {
					dut[signal] = activeValue;
					sim.waitForCycles(firstClock, opts?.activeCycles ?? 2);
				} else {
					// No clock registered: assert the reset as an edge of its own.
					const eventId = design.events[signal];
					if (eventId !== undefined) {
						dut[signal] = inactiveValue;
						design.edges([eventId], [activeValue === 1n]);
						design.flushDirty();
					} else {
						dut[signal] = activeValue;
					}
				}
			}

			dut[signal] = inactiveValue;
		},

		event(name: string): PlaygroundEventHandle {
			return { name, id: design.resolveEvent(name) };
		},

		fourState(portName: string): PlaygroundFourStateValue {
			return design.fourStateOf(portName);
		},

		dump(timestamp: number) {
			design.dump(timestamp, sink);
		},

		dispose() {},
	};
	return sim;
}
