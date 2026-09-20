/**
 * Event-based Simulator.
 *
 * Wraps a NativeSimulatorHandle and provides a high-level TypeScript API
 * for manually controlling clock edges via `tick()`.
 */

import { createDut, type DirtyState, readFourState } from "./dut.js";
import {
	buildNapiOpts,
	buildPortsFromLayout,
	filterHierarchyForDse,
	loadNativeAddon,
	parseHierarchyLayout,
	parseNapiLayout,
	recoverWasmFourStateLayout,
	wrapDirectSimulatorHandle,
} from "./napi-helpers.js";
import type {
	CreateResult,
	EventHandle,
	FourStateValue,
	FrontendSimulatorHandle,
	ModuleDefinition,
	NativeSimulatorHandle,
	SignalLayout,
	SimulatorOptions,
	SourceFile,
} from "./types.js";
import { createWasmSimulatorBridge, isWasmHandle } from "./wasm-bridge.js";

/**
 * Placeholder for the NAPI binding's `createSimulator()`.
 * Stream B will provide the real implementation; until then tests can
 * inject a mock via `Simulator.create()` options or by replacing this
 * module.
 * @internal
 */
export type NativeCreateFn = (
	sources: ReadonlyArray<SourceFile>,
	moduleName: string,
	options: SimulatorOptions,
) => CreateResult<NativeSimulatorHandle>;

let _nativeCreate: NativeCreateFn | undefined;

const U32_MAX = 0xffff_ffff;

function assertU32(value: number, label: string): void {
	if (!Number.isInteger(value) || value < 0 || value > U32_MAX) {
		throw new RangeError(
			`${label} ${value} must be an integer between 0 and ${U32_MAX}`,
		);
	}
}

/**
 * Register the NAPI binding at module load time.
 * Called once by the package entry point after loading the native addon.
 * @internal
 */
export function setNativeSimulatorCreate(fn: NativeCreateFn): void {
	_nativeCreate = fn;
}

// ---------------------------------------------------------------------------
// Simulator
// ---------------------------------------------------------------------------

export class Simulator<P = Record<string, unknown>> {
	private readonly _handle: NativeSimulatorHandle;
	private readonly _dut: P;
	private readonly _events: Record<string, number>;
	private readonly _eventIds: ReadonlySet<number>;
	private readonly _defaultEventId: number;
	private readonly _state: DirtyState;
	private readonly _buffer: ArrayBuffer | SharedArrayBuffer;
	private readonly _layout: Record<string, SignalLayout>;
	private readonly _warnings: readonly string[];
	private _disposed = false;

	private constructor(
		handle: NativeSimulatorHandle,
		dut: P,
		events: Record<string, number>,
		state: DirtyState,
		buffer: ArrayBuffer | SharedArrayBuffer,
		layout: Record<string, SignalLayout>,
		warnings: string[],
	) {
		this._handle = handle;
		this._dut = dut;
		this._events = events;
		this._eventIds = new Set(Object.values(events));
		this._state = state;
		this._buffer = buffer;
		this._layout = layout;
		this._warnings = warnings;
		const keys = Object.keys(events);
		this._defaultEventId = keys.length > 0 ? events[keys[0]!]! : -1;
	}

	/**
	 * Create a Simulator for the given module.
	 *
	 * ```ts
	 * import { Adder } from "./generated/Adder.js";
	 * const sim = Simulator.create(Adder);
	 * ```
	 */
	static create<P>(
		module: ModuleDefinition<P>,
		options?: SimulatorOptions & {
			/** Override for testing — inject a mock NAPI create function. */
			__nativeCreate?: NativeCreateFn;
		},
	): Simulator<P> {
		const merged = { ...module.defaultOptions, ...options };

		// When the module was produced by the Vite plugin, delegate to fromProject()
		if (module.projectPath && !merged?.__nativeCreate) {
			return Simulator.fromProject<P>(module.projectPath, module.name, merged);
		}

		const createFn = merged?.__nativeCreate ?? _nativeCreate;
		if (!createFn) {
			throw new Error(
				"Native simulator binding not loaded. " +
					"Ensure @celox-sim/celox-napi is installed.",
			);
		}

		// Forward every public SimulatorOptions field so newly added options do not
		// require this factory to maintain a second allowlist. The create override is
		// test-only plumbing and must not cross the native factory boundary.
		const { __nativeCreate: _createOverride, ...nativeOptions } = merged;
		const result = createFn(module.sources, module.name, nativeOptions);
		const state: DirtyState = { dirty: false };

		// Always prefer NAPI-derived ports (from hierarchy) over module.ports.
		// module.ports has widths/arrayDims baked at generation time, which become
		// stale when parameters are overridden. hierarchy.ports reflects the actual
		// compiled layout, consistent with fromSource()/fromProject().
		const hierarchy = result.hierarchy
			? filterHierarchyForDse(result.hierarchy, nativeOptions.deadStorePolicy)
			: undefined;
		const portDefs = hierarchy?.ports ?? module.ports;
		const dut = createDut<P>(
			result.buffer,
			result.layout,
			portDefs,
			result.handle,
			state,
			hierarchy,
		);

		return new Simulator<P>(
			result.handle,
			dut,
			result.events,
			state,
			result.buffer,
			result.layout,
			result.warnings ?? [],
		);
	}

	/**
	 * Create a Simulator directly from Veryl source code.
	 *
	 * Automatically discovers ports from the NAPI layout — no
	 * `ModuleDefinition` needed.
	 *
	 * ```ts
	 * const sim = Simulator.fromSource<AdderPorts>(ADDER_SOURCE, "Adder");
	 * sim.dut.a = 100;
	 * sim.dut.b = 200;
	 * sim.tick();
	 * expect(sim.dut.sum).toBe(300);
	 * ```
	 */
	static fromSource<P = Record<string, unknown>>(
		source: string,
		top: string,
		options?: SimulatorOptions & { nativeAddonPath?: string },
	): Simulator<P> {
		const addon = loadNativeAddon(options?.nativeAddonPath);
		if (options?.tier && !addon.NativeSimulatorHandle.newTiered) {
			throw new Error(
				"The loaded Celox native addon does not support tiered execution.",
			);
		}
		const napiOpts = buildNapiOpts(options);
		const raw = options?.tier
			? addon.NativeSimulatorHandle.newTiered!(
					[{ content: source, path: "" }],
					top,
					napiOpts,
				)
			: new addon.NativeSimulatorHandle(
					[{ content: source, path: "" }],
					top,
					napiOpts,
				);
		const isWasm = isWasmHandle(raw);

		const layout =
			isWasm && options?.fourState
				? recoverWasmFourStateLayout(parseNapiLayout(raw.layoutJson))
				: parseNapiLayout(raw.layoutJson);
		const events: Record<string, number> = JSON.parse(raw.eventsJson);
		const rawHierarchy = parseHierarchyLayout(raw.hierarchyJson, events);
		const hierarchy = filterHierarchyForDse(
			rawHierarchy,
			options?.deadStorePolicy,
		);

		// When hierarchy is populated, use its signals for port detection.
		// When hierarchy is empty (e.g. WASM-compiled addon), fall back to
		// the flat layout signals which always have full signal info.
		const hasHierarchySignals = Object.keys(hierarchy.signals).length > 0;
		const ports = hasHierarchySignals
			? buildPortsFromLayout(hierarchy.signals, events)
			: buildPortsFromLayout(layout.signals, events);

		// Detect WASM-compiled addon and use the bridge
		let buf: ArrayBuffer | SharedArrayBuffer;
		let handle: NativeSimulatorHandle;
		if (isWasm) {
			const bridge = createWasmSimulatorBridge(raw);
			buf = bridge.sharedMemory.buffer;
			handle = bridge.handle;
		} else {
			buf = raw.sharedMemory().buffer;
			handle = wrapDirectSimulatorHandle(raw);
		}

		const state: DirtyState = { dirty: false };
		const dut = createDut<P>(
			buf,
			layout.forDut,
			ports,
			handle,
			state,
			hasHierarchySignals ? hierarchy : undefined,
		);

		const warnings: string[] = JSON.parse(raw.warningsJson ?? "[]");

		return new Simulator<P>(
			handle,
			dut,
			events,
			state,
			buf,
			layout.forDut,
			warnings,
		);
	}

	/**
	 * Create a Simulator from a versioned artifact emitted by an external
	 * frontend built with `celox-frontend-sdk`.
	 */
	static fromFrontendArtifact<P = Record<string, unknown>>(
		artifactJson: string,
		options?: SimulatorOptions & { nativeAddonPath?: string },
	): Simulator<P> {
		if (options?.tier) {
			throw new Error(
				"tiered execution is not supported for frontend artifact handles.",
			);
		}
		const addon = loadNativeAddon(options?.nativeAddonPath);
		const factory = addon.NativeSimulatorHandle.fromFrontendArtifact;
		if (!factory) {
			throw new Error(
				"The loaded Celox native addon does not support frontend artifacts.",
			);
		}
		const raw = factory.call(
			addon.NativeSimulatorHandle,
			artifactJson,
			buildNapiOpts(options),
		);
		return Simulator.fromFrontendHandle<P>(raw, options);
	}

	/**
	 * Wrap a raw simulator handle returned by a frontend-owned native or WASM
	 * addon.
	 *
	 * This is the adapter boundary for frontend packages. A frontend's public
	 * API can accept its own artifact type, call its native `fromMyArtifact`
	 * function, then pass the returned handle here. No Celox artifact JSON is
	 * involved.
	 */
	static fromFrontendHandle<P = Record<string, unknown>>(
		raw: FrontendSimulatorHandle,
		options?: Pick<SimulatorOptions, "deadStorePolicy">,
	): Simulator<P> {
		const isWasm = isWasmHandle(raw);
		// Frontend artifacts carry an exact state kind for every signal, including
		// clock/reset ports. Do not apply the legacy WASM type-kind inference here.
		const layout = parseNapiLayout(raw.layoutJson);
		const events: Record<string, number> = JSON.parse(raw.eventsJson);
		const hierarchy = filterHierarchyForDse(
			parseHierarchyLayout(raw.hierarchyJson, events),
			options?.deadStorePolicy,
		);
		const hasHierarchySignals = Object.keys(hierarchy.signals).length > 0;
		const ports = hasHierarchySignals
			? buildPortsFromLayout(hierarchy.signals, events)
			: buildPortsFromLayout(layout.signals, events);

		let buffer: ArrayBuffer | SharedArrayBuffer;
		let handle: NativeSimulatorHandle;
		if (isWasm) {
			const bridge = createWasmSimulatorBridge(raw);
			buffer = bridge.sharedMemory.buffer;
			handle = bridge.handle;
		} else {
			buffer = raw.sharedMemory().buffer;
			handle = wrapDirectSimulatorHandle(raw);
		}
		const state: DirtyState = { dirty: isWasm };
		const dut = createDut<P>(
			buffer,
			layout.forDut,
			ports,
			handle,
			state,
			hasHierarchySignals ? hierarchy : undefined,
		);
		const warnings: string[] = JSON.parse(raw.warningsJson ?? "[]");
		return new Simulator<P>(
			handle,
			dut,
			events,
			state,
			buffer,
			layout.forDut,
			warnings,
		);
	}

	/**
	 * Create a Simulator from a Veryl project directory.
	 *
	 * Searches upward from `projectPath` for `Veryl.toml`, gathers all
	 * `.veryl` source files, and builds the simulator using the project's
	 * clock/reset settings.
	 *
	 * ```ts
	 * const sim = Simulator.fromProject<MyPorts>("./my-project", "Top");
	 * ```
	 */
	static fromProject<P = Record<string, unknown>>(
		projectPath: string,
		top: string,
		options?: SimulatorOptions & { nativeAddonPath?: string },
	): Simulator<P> {
		const addon = loadNativeAddon(options?.nativeAddonPath);
		if (options?.tier && !addon.NativeSimulatorHandle.newTieredFromProject) {
			throw new Error(
				"The loaded Celox native addon does not support tiered execution.",
			);
		}
		const napiOpts = buildNapiOpts(options);
		const raw = options?.tier
			? addon.NativeSimulatorHandle.newTieredFromProject!(
					projectPath,
					top,
					napiOpts,
				)
			: addon.NativeSimulatorHandle.fromProject(projectPath, top, napiOpts);
		const isWasm = isWasmHandle(raw);

		const layout =
			isWasm && options?.fourState
				? recoverWasmFourStateLayout(parseNapiLayout(raw.layoutJson))
				: parseNapiLayout(raw.layoutJson);
		const events: Record<string, number> = JSON.parse(raw.eventsJson);
		const rawHierarchy = parseHierarchyLayout(raw.hierarchyJson, events);
		const hierarchy = filterHierarchyForDse(
			rawHierarchy,
			options?.deadStorePolicy,
		);

		const hasHierarchySignals = Object.keys(hierarchy.signals).length > 0;
		const ports = hasHierarchySignals
			? buildPortsFromLayout(hierarchy.signals, events)
			: buildPortsFromLayout(layout.signals, events);

		// Detect WASM-compiled addon and use the bridge
		let buf: ArrayBuffer | SharedArrayBuffer;
		let handle: NativeSimulatorHandle;
		if (isWasm) {
			const bridge = createWasmSimulatorBridge(raw);
			buf = bridge.sharedMemory.buffer;
			handle = bridge.handle;
		} else {
			buf = raw.sharedMemory().buffer;
			handle = wrapDirectSimulatorHandle(raw);
		}

		const state: DirtyState = { dirty: false };
		const dut = createDut<P>(
			buf,
			layout.forDut,
			ports,
			handle,
			state,
			hasHierarchySignals ? hierarchy : undefined,
		);

		const warnings: string[] = JSON.parse(raw.warningsJson ?? "[]");

		return new Simulator<P>(
			handle,
			dut,
			events,
			state,
			buf,
			layout.forDut,
			warnings,
		);
	}

	/** The DUT accessor object — read/write ports as plain properties. */
	get dut(): P {
		return this._dut;
	}

	/** Analyzer warnings emitted during compilation. */
	get warnings(): readonly string[] {
		return this._warnings;
	}

	/**
	 * Trigger a clock edge.
	 *
	 * A single numeric argument is the tick count for the default event. When a
	 * second argument is not `undefined`, a numeric first argument is treated as
	 * a raw event ID. Therefore, use `tick(id, 1)` to tick a raw event ID once.
	 * Prefer an `EventHandle` returned by `this.event()` when selecting an event.
	 * `tick(undefined, count)` ticks the default event.
	 *
	 * @param event  Optional event handle or known raw event ID. If omitted,
	 *               ticks the first (default) event.
	 * @param count  Number of ticks as an unsigned 32-bit integer. Zero evaluates
	 *               pending combinational logic without triggering an event.
	 *               Default: 1.
	 * @throws RangeError if the count or selected event ID is invalid, or if the
	 *         selected event ID is not known to this Simulator.
	 * @throws Error if a positive count uses the default event but the Simulator
	 *         has no events.
	 */
	tick(event?: EventHandle | number, count?: number): void;
	tick(count?: number): void;
	tick(eventOrCount?: EventHandle | number, count?: number): void {
		this.ensureAlive();

		let eventId: number;
		let ticks: number;
		let hasExplicitEvent = false;

		if (typeof eventOrCount === "object" && eventOrCount !== null) {
			// tick(eventHandle, count?)
			eventId = (eventOrCount as EventHandle).id;
			ticks = count ?? 1;
			hasExplicitEvent = true;
		} else if (typeof eventOrCount === "number") {
			if (count !== undefined) {
				// tick(rawEventId, count)
				eventId = eventOrCount;
				ticks = count;
				hasExplicitEvent = true;
			} else {
				// tick(count) — default event
				eventId = this._defaultEventId;
				ticks = eventOrCount;
			}
		} else {
			// tick() / tick(undefined, count) — default event
			eventId = this._defaultEventId;
			ticks = count ?? 1;
		}

		assertU32(ticks, "Tick count");
		if (hasExplicitEvent) {
			this.assertKnownEventId(eventId);
		} else if (ticks > 0) {
			if (this._eventIds.size === 0) {
				throw new Error("Simulator has no events to tick");
			}
			this.assertKnownEventId(eventId);
		}

		if (this._state.dirty) {
			this._handle.evalComb();
			this._state.dirty = false;
		}

		if (ticks === 1) {
			this._handle.tick(eventId);
		} else if (ticks > 1) {
			this._handle.tickN(eventId, ticks);
		}
	}

	/** Resolve an event name to a handle for use with `tick()`. */
	event(name: string): EventHandle {
		const id = this._events[name];
		if (id === undefined) {
			throw new Error(
				`Unknown event '${name}'. Available: ${Object.keys(this._events).join(", ")}`,
			);
		}
		return { name, id };
	}

	/**
	 * Read the raw 4-state (value + mask) pair for the named port.
	 */
	fourState(portName: string): FourStateValue {
		this.ensureAlive();
		const sig = this._layout[portName];
		if (!sig) {
			throw new Error(
				`Unknown port '${portName}'. Available: ${Object.keys(this._layout).join(", ")}`,
			);
		}
		const [value, mask] = readFourState(this._buffer, sig);
		return { __fourState: true, value, mask };
	}

	/** Write current signal values to VCD at the given timestamp. */
	dump(timestamp: number): void {
		this.ensureAlive();
		this._handle.dump(timestamp);
	}

	/** Release native resources. */
	dispose(): void {
		if (!this._disposed) {
			this._disposed = true;
			// Mark DUT state as disposed BEFORE freeing Rust memory to prevent
			// use-after-free through lingering DUT references.
			this._state.disposed = true;
			this._handle.dispose();
		}
	}

	// -----------------------------------------------------------------------
	// Internal
	// -----------------------------------------------------------------------

	private ensureAlive(): void {
		if (this._disposed) {
			throw new Error("Simulator has been disposed");
		}
	}

	private assertKnownEventId(eventId: number): void {
		assertU32(eventId, "Event ID");
		if (!this._eventIds.has(eventId)) {
			const available = [...this._eventIds].sort((a, b) => a - b).join(", ");
			throw new RangeError(
				`Unknown event ID ${eventId}. Available IDs: ${available || "(none)"}`,
			);
		}
	}
}
