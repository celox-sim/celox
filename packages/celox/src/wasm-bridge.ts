/**
 * WASM bridge for running simulation via browser WebAssembly.
 *
 * When the NAPI addon is compiled for wasm32 (no JIT backend), the
 * NativeSimulatorHandle exposes `combWasmBytes()` and `eventWasmBytes(name)`
 * instead of `tick()` / `evalComb()`.  This bridge instantiates those WASM
 * modules and presents a handle that is compatible with the existing
 * Simulator / DUT code.
 *
 * @module
 */

import type {
	FrontendSimulatorHandle,
	NativeSimulationHandle,
	NativeSimulatorHandle,
	WasmFrontendSimulatorHandle,
} from "./types.js";
import {
	type EdgeKernels,
	type EdgePlans,
	WasmEdgeRunner,
} from "./wasm-edges.js";

// ---------------------------------------------------------------------------
// Minimal WebAssembly type declarations for environments without DOM lib.
// These mirror the subset of the WebAssembly API that we use.
// ---------------------------------------------------------------------------
/* eslint-disable @typescript-eslint/no-namespace */
declare namespace WebAssembly {
	interface MemoryDescriptor {
		initial: number;
		maximum?: number;
		shared?: boolean;
	}
	class Memory {
		constructor(descriptor: MemoryDescriptor);
		readonly buffer: ArrayBuffer;
	}
	class Module {
		constructor(bytes: ArrayBuffer | ArrayBufferView);
	}
	class Instance {
		constructor(module: Module, importObject?: Record<string, unknown>);
		readonly exports: Record<string, unknown>;
	}
	function compile(bytes: ArrayBuffer | ArrayBufferView): Promise<Module>;
	function instantiate(
		module: Module,
		importObject?: Record<string, unknown>,
	): Promise<Instance>;
}

// ---------------------------------------------------------------------------
// Raw WASM handle shape (what the wasm32-compiled addon exposes)
// ---------------------------------------------------------------------------

/**
 * Handle shape returned by a wasm32-compiled NativeSimulatorHandle.
 * Extends the standard metadata getters with WASM bytecode accessors.
 */
export type RawWasmSimulatorHandle = WasmFrontendSimulatorHandle;

// ---------------------------------------------------------------------------
// Detection
// ---------------------------------------------------------------------------

/**
 * Detect if a handle was produced by a wasm32-compiled addon.
 *
 * WASM handles expose `combWasmBytes()` instead of `tick()`.
 */
export function isWasmHandle(
	handle: FrontendSimulatorHandle,
): handle is RawWasmSimulatorHandle;
export function isWasmHandle(handle: NativeSimulationHandle): boolean;
export function isWasmHandle(handle: unknown): handle is RawWasmSimulatorHandle;
export function isWasmHandle(handle: unknown): boolean {
	return (
		typeof handle === "object" &&
		handle !== null &&
		typeof (handle as Record<string, unknown>).initialMemoryBytes ===
			"function" &&
		typeof (handle as Record<string, unknown>).combWasmBytes === "function" &&
		typeof (handle as Record<string, unknown>).eventWasmBytes === "function" &&
		typeof (handle as Record<string, unknown>).tick !== "function"
	);
}

// ---------------------------------------------------------------------------
// Bridge
// ---------------------------------------------------------------------------

/** Checkpoint of a bridge-driven simulator: a copy of its design state. */
interface WasmCheckpoint {
	readonly stateSize: number;
	readonly fingerprint: string;
	readonly state: Uint8Array;
}

/**
 * Checkpoint methods for a bridge, which owns the simulation memory.
 * The raw handle says which bytes hold design state and which layout they
 * belong to; addons that predate checkpoints provide neither.
 */
function wasmCheckpointMethods(
	raw: RawWasmSimulatorHandle,
	memory: WebAssembly.Memory,
	evalComb: () => void,
): Pick<NativeSimulatorHandle, "checkpoint" | "restore"> {
	const start = raw.stateOffset;
	const fingerprint = raw.stateFingerprint;
	if (start === undefined || fingerprint === undefined) return {};
	const end = raw.stableSize;
	return {
		checkpoint(): WasmCheckpoint {
			const state = new Uint8Array(memory.buffer, start, end - start).slice();
			return { stateSize: state.byteLength, fingerprint, state };
		},
		restore(checkpoint): void {
			const saved = checkpoint as Partial<WasmCheckpoint>;
			if (saved.fingerprint !== fingerprint || !saved.state) {
				throw new Error("the checkpoint was taken from a different design");
			}
			new Uint8Array(memory.buffer, start, end - start).set(saved.state);
			evalComb();
		},
	};
}

/**
 * The edge runner of a bridge, when the raw handle provides edge plans.
 * Phase modules of split kernels are compiled on first use.
 */
function wasmEdgeRunner(
	raw: RawWasmSimulatorHandle,
	memory: WebAssembly.Memory,
	events: Record<string, number>,
	eventInstances: Map<number, WebAssembly.Instance>,
	evalComb: () => void,
): WasmEdgeRunner | undefined {
	if (!raw.edgePlansJson) return undefined;
	const plans = JSON.parse(raw.edgePlansJson()) as EdgePlans;
	const run = (instance: WebAssembly.Instance) => () =>
		(instance.exports.run as CallableFunction)();
	const phase = (name: string, kind: "evalOnly" | "apply") => {
		if (!raw.eventPhaseWasmBytes) return undefined;
		try {
			const bytes = new Uint8Array(raw.eventPhaseWasmBytes(name, kind));
			const module = new WebAssembly.Module(bytes);
			return run(new WebAssembly.Instance(module, { env: { memory } }));
		} catch {
			return undefined;
		}
	};
	const cache = new Map<string, EdgeKernels | undefined>();
	const kernels = (name: string): EdgeKernels | undefined => {
		if (cache.has(name)) return cache.get(name);
		const id = events[name];
		const instance = id === undefined ? undefined : eventInstances.get(id);
		let kernel: EdgeKernels | undefined;
		if (instance) {
			const evalOnly = phase(name, "evalOnly");
			const apply = phase(name, "apply");
			kernel =
				evalOnly && apply
					? { evalApply: run(instance), evalOnly, apply }
					: { evalApply: run(instance) };
		}
		cache.set(name, kernel);
		return kernel;
	};
	return new WasmEdgeRunner(memory, plans, events, kernels, evalComb);
}

function edgeMethods(
	edges: WasmEdgeRunner | undefined,
): Pick<NativeSimulatorHandle, "edges" | "activeEdgeIsRising"> {
	if (!edges) return {};
	return {
		edges: (eventIds, rising, dirty) => edges.edges(eventIds, rising, dirty),
		activeEdgeIsRising: (eventId) => edges.activeEdgeIsRising(eventId),
	};
}

/** Result of creating a WASM simulator bridge. */
export interface WasmBridgeResult {
	/** Handle compatible with the existing Simulator code. */
	handle: NativeSimulatorHandle;
	/** The raw shared memory as a Uint8Array (backed by WebAssembly.Memory). */
	sharedMemory: Uint8Array;
}

/**
 * Create a NativeSimulatorHandle-compatible wrapper from a wasm32-compiled
 * NAPI handle.
 *
 * 1. Reads metadata (layout, events, sizes) from the raw handle.
 * 2. Creates a WebAssembly.Memory large enough for the simulation state.
 * 3. Synchronously compiles and instantiates the combinational and event
 *    WASM modules, importing the shared memory.
 * 4. Returns a handle whose `tick()` / `evalComb()` drive the WASM instances.
 *
 * @returns A bridge result with the wrapped handle and shared memory view.
 */
export function createWasmSimulatorBridge(
	raw: RawWasmSimulatorHandle,
): WasmBridgeResult {
	const totalSize = raw.totalSize;
	const stableSize = raw.stableSize;

	// Create shared WebAssembly.Memory
	const pages = Math.max(1, Math.ceil(totalSize / 65536));
	const memory = new WebAssembly.Memory({ initial: pages });
	new Uint8Array(memory.buffer, 0, totalSize).set(
		new Uint8Array(raw.initialMemoryBytes()),
	);

	// Compile and instantiate comb WASM module (synchronous)
	const combBytes = new Uint8Array(raw.combWasmBytes());
	const combModule = new WebAssembly.Module(combBytes);
	const combInstance = new WebAssembly.Instance(combModule, {
		env: { memory },
	});

	// Parse events and instantiate per-event WASM modules
	const events: Record<string, number> = JSON.parse(raw.eventsJson);
	const eventInstances = new Map<number, WebAssembly.Instance>();

	for (const [name, id] of Object.entries(events)) {
		const bytes = new Uint8Array(raw.eventWasmBytes(name));
		const mod = new WebAssembly.Module(bytes);
		const inst = new WebAssembly.Instance(mod, { env: { memory } });
		eventInstances.set(id, inst);
	}

	const sharedMemory = new Uint8Array(memory.buffer, 0, stableSize);

	const edges = wasmEdgeRunner(raw, memory, events, eventInstances, () =>
		(combInstance.exports.run as CallableFunction)(),
	);
	const handle: NativeSimulatorHandle = {
		tick(eventId: number): void {
			if (edges?.hasDerived(eventId)) {
				edges.tick(eventId);
				return;
			}
			// eval_comb → eval_apply_ff → eval_comb
			(combInstance.exports.run as CallableFunction)();
			const evInst = eventInstances.get(eventId);
			if (evInst) (evInst.exports.run as CallableFunction)();
			(combInstance.exports.run as CallableFunction)();
		},
		...edgeMethods(edges),
		tickN(eventId: number, count: number): void {
			for (let i = 0; i < count; i++) {
				this.tick(eventId);
			}
		},
		evalComb(): void {
			(combInstance.exports.run as CallableFunction)();
		},
		dump(_timestamp: number): void {
			// No VCD support in browser WASM mode
		},
		dispose(): void {
			raw.dispose();
		},
		...wasmCheckpointMethods(raw, memory, () =>
			(combInstance.exports.run as CallableFunction)(),
		),
	};

	return { handle, sharedMemory };
}

/**
 * Asynchronous version of `createWasmSimulatorBridge` that uses
 * `WebAssembly.compile()` + `WebAssembly.instantiate()`.
 *
 * Preferred for large modules in browsers where synchronous compilation
 * may be rejected.
 */
export async function createWasmSimulatorBridgeAsync(
	raw: RawWasmSimulatorHandle,
): Promise<WasmBridgeResult> {
	const totalSize = raw.totalSize;
	const stableSize = raw.stableSize;

	const pages = Math.max(1, Math.ceil(totalSize / 65536));
	const memory = new WebAssembly.Memory({ initial: pages });
	new Uint8Array(memory.buffer, 0, totalSize).set(
		new Uint8Array(raw.initialMemoryBytes()),
	);

	// Compile comb module
	const combBytes = new Uint8Array(raw.combWasmBytes());
	const combModule = await WebAssembly.compile(combBytes);
	const combInstance = await WebAssembly.instantiate(combModule, {
		env: { memory },
	});

	// Parse events and compile per-event modules
	const events: Record<string, number> = JSON.parse(raw.eventsJson);
	const eventInstances = new Map<number, WebAssembly.Instance>();

	const eventEntries = Object.entries(events);
	await Promise.all(
		eventEntries.map(async ([name, id]) => {
			const bytes = new Uint8Array(raw.eventWasmBytes(name));
			const mod = await WebAssembly.compile(bytes);
			const inst = await WebAssembly.instantiate(mod, { env: { memory } });
			eventInstances.set(id, inst);
		}),
	);

	const sharedMemory = new Uint8Array(memory.buffer, 0, stableSize);

	const edges = wasmEdgeRunner(raw, memory, events, eventInstances, () =>
		(combInstance.exports.run as CallableFunction)(),
	);
	const handle: NativeSimulatorHandle = {
		tick(eventId: number): void {
			if (edges?.hasDerived(eventId)) {
				edges.tick(eventId);
				return;
			}
			(combInstance.exports.run as CallableFunction)();
			const evInst = eventInstances.get(eventId);
			if (evInst) (evInst.exports.run as CallableFunction)();
			(combInstance.exports.run as CallableFunction)();
		},
		...edgeMethods(edges),
		tickN(eventId: number, count: number): void {
			for (let i = 0; i < count; i++) {
				this.tick(eventId);
			}
		},
		evalComb(): void {
			(combInstance.exports.run as CallableFunction)();
		},
		dump(_timestamp: number): void {
			// No VCD support in browser WASM mode
		},
		dispose(): void {
			raw.dispose();
		},
		...wasmCheckpointMethods(raw, memory, () =>
			(combInstance.exports.run as CallableFunction)(),
		),
	};

	return { handle, sharedMemory };
}
