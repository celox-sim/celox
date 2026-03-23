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

import type { NativeSimulatorHandle } from "./types.js";

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
export interface RawWasmSimulatorHandle {
	readonly layoutJson: string;
	readonly eventsJson: string;
	readonly hierarchyJson: string;
	readonly warningsJson: string;
	readonly stableSize: number;
	readonly totalSize: number;
	combWasmBytes(): Uint8Array | number[];
	eventWasmBytes(name: string): Uint8Array | number[];
	dispose(): void;
}

// ---------------------------------------------------------------------------
// Detection
// ---------------------------------------------------------------------------

/**
 * Detect if a handle was produced by a wasm32-compiled addon.
 *
 * WASM handles expose `combWasmBytes()` instead of `tick()`.
 */
export function isWasmHandle(
	handle: unknown,
): handle is RawWasmSimulatorHandle {
	return (
		typeof handle === "object" &&
		handle !== null &&
		typeof (handle as Record<string, unknown>).combWasmBytes === "function" &&
		typeof (handle as Record<string, unknown>).tick !== "function"
	);
}

// ---------------------------------------------------------------------------
// Bridge
// ---------------------------------------------------------------------------

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

	const handle: NativeSimulatorHandle = {
		tick(eventId: number): void {
			// eval_comb → eval_apply_ff → eval_comb
			(combInstance.exports.run as CallableFunction)();
			const evInst = eventInstances.get(eventId);
			if (evInst) (evInst.exports.run as CallableFunction)();
			(combInstance.exports.run as CallableFunction)();
		},
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

	const handle: NativeSimulatorHandle = {
		tick(eventId: number): void {
			(combInstance.exports.run as CallableFunction)();
			const evInst = eventInstances.get(eventId);
			if (evInst) (evInst.exports.run as CallableFunction)();
			(combInstance.exports.run as CallableFunction)();
		},
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
	};

	return { handle, sharedMemory };
}
