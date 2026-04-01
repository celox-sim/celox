/**
 * NAPI helper utilities for the @celox-sim/celox TypeScript runtime.
 *
 * Provides reusable functions for:
 *   - Loading the native addon
 *   - Parsing NAPI layout JSON into SignalLayout
 *   - Building PortInfo from NAPI layout (auto-detect ports)
 *   - Wrapping NAPI handles with zero-copy direct operations
 *   - Creating bridge functions for Simulator.create() / Simulation.create()
 */

import type { NativeCreateSimulationFn } from "./simulation.js";
import type { NativeCreateFn } from "./simulator.js";
import type {
	CreateResult,
	LoopBreak,
	NativeSimulationHandle,
	NativeSimulatorHandle,
	PortInfo,
	SignalLayout,
	SimulatorOptions,
	SourceFile,
	TrueLoopSpec,
} from "./types.js";
import { createWasmSimulatorBridge, isWasmHandle } from "./wasm-bridge.js";

// ---------------------------------------------------------------------------
// Raw NAPI handle shapes (what the .node addon actually exports)
// ---------------------------------------------------------------------------

export interface RawNapiSimulatorHandle {
	readonly layoutJson: string;
	readonly eventsJson: string;
	readonly hierarchyJson: string;
	readonly warningsJson: string;
	readonly stableSize: number;
	readonly totalSize: number;
	// Native (JIT) methods — present when built for native target
	tick?(eventId: number): void;
	tickN?(eventId: number, count: number): void;
	evalComb?(): void;
	dump?(timestamp: number): void;
	sharedMemory?(): Uint8Array;
	// WASM methods — present when built for wasm32 target
	combWasmBytes?(): Uint8Array | number[];
	eventWasmBytes?(name: string): Uint8Array | number[];
	dispose(): void;
}

export interface RawNapiSimulationHandle {
	readonly layoutJson: string;
	readonly eventsJson: string;
	readonly hierarchyJson: string;
	readonly warningsJson: string;
	readonly stableSize: number;
	readonly totalSize: number;
	readonly defaultMaxSteps: number | null;
	addClock(eventId: number, period: number, initialDelay: number): void;
	schedule(eventId: number, time: number, value: number): void;
	runUntil(endTime: number): void;
	step(): number | null;
	time(): number;
	nextEventTime(): number | null;
	evalComb(): void;
	dump(timestamp: number): void;
	sharedMemory(): Uint8Array;
	dispose(): void;
}

export interface NapiFalseLoop {
	from: NapiSignalPath;
	to: NapiSignalPath;
}

export interface NapiTrueLoop {
	from: NapiSignalPath;
	to: NapiSignalPath;
	maxIter: number;
}

export interface NapiSignalPath {
	instancePath: NapiInstanceSegment[];
	varPath: string[];
}

export interface NapiInstanceSegment {
	name: string;
	index: number;
}

export interface NapiParamOverride {
	name: string;
	value: number;
}

export interface NapiOptimizeOptions {
	storeLoadForwarding?: boolean;
	hoistCommonBranchLoads?: boolean;
	bitExtractPeephole?: boolean;
	optimizeBlocks?: boolean;
	splitWideCommits?: boolean;
	commitSinking?: boolean;
	inlineCommitForwarding?: boolean;
	eliminateDeadWorkingStores?: boolean;
	reschedule?: boolean;
	coalesceStores?: boolean;
}

export interface NapiOptions {
	fourState?: boolean;
	vcd?: string;
	optimize?: boolean;
	optimizeOptions?: NapiOptimizeOptions;
	craneliftOptLevel?: string;
	regallocAlgorithm?: string;
	enableAliasAnalysis?: boolean;
	enableVerifier?: boolean;
	falseLoops?: NapiFalseLoop[];
	trueLoops?: NapiTrueLoop[];
	clockType?: string;
	resetType?: string;
	extraSource?: string;
	parameters?: NapiParamOverride[];
	deadStorePolicy?: string;
}

export interface NapiSourceFile {
	content: string;
	path: string;
}

export interface RawNapiAddon {
	NativeSimulatorHandle: {
		new (
			sources: NapiSourceFile[],
			top: string,
			options?: NapiOptions,
		): RawNapiSimulatorHandle;
		fromProject(
			projectPath: string,
			top: string,
			options?: NapiOptions,
		): RawNapiSimulatorHandle;
	};
	NativeSimulationHandle: {
		new (
			sources: NapiSourceFile[],
			top: string,
			options?: NapiOptions,
		): RawNapiSimulationHandle;
		fromProject(
			projectPath: string,
			top: string,
			options?: NapiOptions,
		): RawNapiSimulationHandle;
	};
	genTs(projectPath: string): string;
	clearJitCache(): void;
}

// ---------------------------------------------------------------------------
// Native addon loading
// ---------------------------------------------------------------------------

import { createRequire } from "node:module";

/**
 * Load the native NAPI addon.
 *
 * Resolution: `@celox-sim/celox-napi` package (works both in workspace dev
 * and when installed from npm — napi-rs generated index.js handles platform
 * detection). An explicit path can override this.
 *
 * @param addonPath  Explicit path to the `.node` file (overrides auto-detection).
 */
export function loadNativeAddon(addonPath?: string): RawNapiAddon {
	const require = createRequire(import.meta.url);

	if (addonPath) {
		return require(addonPath) as RawNapiAddon;
	}

	try {
		return require("@celox-sim/celox-napi") as RawNapiAddon;
	} catch (e) {
		throw new Error(
			`Failed to load NAPI addon from @celox-sim/celox-napi. ` +
				`Build it with: pnpm run build:napi`,
			{ cause: e },
		);
	}
}

// ---------------------------------------------------------------------------
// Signal path parsing
// ---------------------------------------------------------------------------

/**
 * Parse a signal path string into instance-path + var-path components.
 *
 * Format: `instanceSeg1.instanceSeg2:varSeg1.varSeg2`
 *   - `:` separates instance path from variable path
 *   - Without `:`, the whole string is the variable path
 *   - Instance segments may include `[N]` array indices
 *
 * Examples:
 *   - `"v"` → { instancePath: [], varPath: ["v"] }
 *   - `"p2:i"` → { instancePath: [{name:"p2",index:0}], varPath: ["i"] }
 *   - `"a.b[3]:x.y"` → { instancePath: [{name:"a",index:0},{name:"b",index:3}], varPath: ["x","y"] }
 */
export function parseSignalPath(path: string): NapiSignalPath {
	const colonIdx = path.indexOf(":");
	if (colonIdx < 0) {
		return { instancePath: [], varPath: path.split(".") };
	}

	const instPart = path.slice(0, colonIdx);
	const varPart = path.slice(colonIdx + 1);

	const instancePath: NapiInstanceSegment[] = [];
	if (instPart.length > 0) {
		for (const seg of instPart.split(".")) {
			const bracketIdx = seg.indexOf("[");
			if (bracketIdx >= 0) {
				const name = seg.slice(0, bracketIdx);
				const index = Number.parseInt(seg.slice(bracketIdx + 1, -1), 10);
				instancePath.push({ name, index });
			} else {
				instancePath.push({ name: seg, index: 0 });
			}
		}
	}

	return { instancePath, varPath: varPart.split(".") };
}

/**
 * Build NapiOptions from SimulatorOptions.
 * Returns `undefined` when no options are set (to skip the NAPI options arg).
 */
export function buildNapiOpts(
	options?: SimulatorOptions,
): NapiOptions | undefined {
	if (!options) return undefined;

	const napiOpts: NapiOptions = {};
	let hasOpt = false;

	if (options.fourState) {
		napiOpts.fourState = options.fourState;
		hasOpt = true;
	}
	if (options.vcd) {
		napiOpts.vcd = options.vcd;
		hasOpt = true;
	}
	if (options.optimize != null) {
		napiOpts.optimize = options.optimize;
		hasOpt = true;
	}
	if (options.falseLoops && options.falseLoops.length > 0) {
		napiOpts.falseLoops = options.falseLoops.map((lb: LoopBreak) => ({
			from: parseSignalPath(lb.from),
			to: parseSignalPath(lb.to),
		}));
		hasOpt = true;
	}
	if (options.trueLoops && options.trueLoops.length > 0) {
		napiOpts.trueLoops = options.trueLoops.map((tl: TrueLoopSpec) => ({
			from: parseSignalPath(tl.from),
			to: parseSignalPath(tl.to),
			maxIter: tl.maxIter,
		}));
		hasOpt = true;
	}
	if (options.clockType) {
		napiOpts.clockType = options.clockType;
		hasOpt = true;
	}
	if (options.resetType) {
		napiOpts.resetType = options.resetType;
		hasOpt = true;
	}
	if (options.extraSource) {
		napiOpts.extraSource = options.extraSource;
		hasOpt = true;
	}
	if (options.parameters && options.parameters.length > 0) {
		napiOpts.parameters = options.parameters.map((p) => ({
			name: p.name,
			value: typeof p.value === "bigint" ? Number(p.value) : p.value,
		}));
		hasOpt = true;
	}
	if (options.craneliftOptLevel) {
		const clMap: Record<string, string> = {
			none: "none",
			speed: "speed",
			speedAndSize: "speed_and_size",
		};
		napiOpts.craneliftOptLevel =
			clMap[options.craneliftOptLevel] ?? options.craneliftOptLevel;
		hasOpt = true;
	}
	if (options.regallocAlgorithm) {
		const raMap: Record<string, string> = {
			backtracking: "backtracking",
			singlePass: "single_pass",
		};
		napiOpts.regallocAlgorithm =
			raMap[options.regallocAlgorithm] ?? options.regallocAlgorithm;
		hasOpt = true;
	}
	if (options.enableAliasAnalysis != null) {
		napiOpts.enableAliasAnalysis = options.enableAliasAnalysis;
		hasOpt = true;
	}
	if (options.enableVerifier != null) {
		napiOpts.enableVerifier = options.enableVerifier;
		hasOpt = true;
	}
	if (options.optimizeOptions) {
		const oo = options.optimizeOptions;
		const napiOo: NapiOptimizeOptions = {};
		if (oo.storeLoadForwarding != null)
			napiOo.storeLoadForwarding = oo.storeLoadForwarding;
		if (oo.hoistCommonBranchLoads != null)
			napiOo.hoistCommonBranchLoads = oo.hoistCommonBranchLoads;
		if (oo.bitExtractPeephole != null)
			napiOo.bitExtractPeephole = oo.bitExtractPeephole;
		if (oo.optimizeBlocks != null) napiOo.optimizeBlocks = oo.optimizeBlocks;
		if (oo.splitWideCommits != null)
			napiOo.splitWideCommits = oo.splitWideCommits;
		if (oo.commitSinking != null) napiOo.commitSinking = oo.commitSinking;
		if (oo.inlineCommitForwarding != null)
			napiOo.inlineCommitForwarding = oo.inlineCommitForwarding;
		if (oo.eliminateDeadWorkingStores != null)
			napiOo.eliminateDeadWorkingStores = oo.eliminateDeadWorkingStores;
		if (oo.reschedule != null) napiOo.reschedule = oo.reschedule;
		if (oo.coalesceStores != null)
			napiOo.coalesceStores = oo.coalesceStores;
		napiOpts.optimizeOptions = napiOo;
		hasOpt = true;
	}
	if (options.deadStorePolicy && options.deadStorePolicy !== "off") {
		const map: Record<string, string> = {
			preserveTopPorts: "preserve_top_ports",
			preserveAllPorts: "preserve_all_ports",
		};
		napiOpts.deadStorePolicy =
			map[options.deadStorePolicy] ?? options.deadStorePolicy;
		hasOpt = true;
	}

	return hasOpt ? napiOpts : undefined;
}

// ---------------------------------------------------------------------------
// Layout parsing helpers
// ---------------------------------------------------------------------------

interface RawSignalLayout {
	offset: number;
	width: number;
	byte_size: number;
	is_4state: boolean;
	direction: string;
	type_kind: string;
	array_dims?: number[];
	associated_clock?: string;
}

/**
 * Parse the NAPI layout JSON into SignalLayout records.
 * Returns both the full layout (with type_kind for port detection) and
 * the DUT-compatible layout (without type_kind).
 */
export function parseNapiLayout(json: string): {
	signals: Record<
		string,
		SignalLayout & {
			typeKind: string;
			arrayDims?: number[];
			associatedClock?: string;
		}
	>;
	forDut: Record<string, SignalLayout>;
} {
	const raw: Record<string, RawSignalLayout> = JSON.parse(json);
	const signals: Record<
		string,
		SignalLayout & {
			typeKind: string;
			arrayDims?: number[];
			associatedClock?: string;
		}
	> = {};
	const forDut: Record<string, SignalLayout> = {};

	for (const [name, r] of Object.entries(raw)) {
		const sl: SignalLayout = {
			offset: r.offset,
			width: r.width,
			byteSize: r.byte_size > 0 ? r.byte_size : Math.ceil(r.width / 8),
			is4state: r.is_4state,
			direction: r.direction as SignalLayout["direction"],
		};
		const entry: SignalLayout & {
			typeKind: string;
			arrayDims?: number[];
			associatedClock?: string;
		} = {
			...sl,
			typeKind: r.type_kind,
		};
		if (r.array_dims && r.array_dims.length > 0) {
			entry.arrayDims = r.array_dims;
		}
		if (r.associated_clock) {
			entry.associatedClock = r.associated_clock;
		}
		signals[name] = entry;
		forDut[name] = sl;
	}

	return { signals, forDut };
}

/**
 * Build PortInfo records from the NAPI layout signals.
 * This auto-detects port metadata so users don't need to hand-write ModuleDefinition.
 *
 * Hierarchical signal names (e.g. "bus.data", "bus.valid" from an interface port)
 * are grouped into a nested PortInfo with an `interface` map so that createDut can
 * expose them as `dut.bus.data` and `dut.bus.valid`.
 */
export function buildPortsFromLayout(
	signals: Record<
		string,
		SignalLayout & { typeKind: string; arrayDims?: number[] }
	>,
	_events: Record<string, number>,
): Record<string, PortInfo> {
	// Step 1: Build flat PortInfo for every signal name
	const flat: Record<string, PortInfo> = {};

	for (const [name, sig] of Object.entries(signals)) {
		const typeKind = sig.typeKind;
		let portType: "clock" | "reset" | "logic" | "bit";
		if (typeKind === "clock") {
			portType = "clock";
		} else if (typeKind.startsWith("reset")) {
			portType = "reset";
		} else if (typeKind === "bit") {
			portType = "bit";
		} else {
			portType = "logic";
		}

		const port: PortInfo = {
			direction: sig.direction,
			type: portType,
			width: sig.width,
			is4state: sig.is4state,
		};
		if (sig.arrayDims && sig.arrayDims.length > 0) {
			(port as { arrayDims: readonly number[] }).arrayDims = sig.arrayDims;
		}
		flat[name] = port;
	}

	// Step 2: Group hierarchical signals (e.g. "bus.data") into nested PortInfo.
	// Veryl interface ports are exactly one level deep, so "bus.data" → parent "bus",
	// member "data". The parent entry receives an `interface` map of its members;
	// the flat layout keys are kept as-is so createNestedDut can look them up.
	const ports: Record<string, PortInfo> = {};
	const ifMaps = new Map<string, Record<string, PortInfo>>();

	for (const [name, port] of Object.entries(flat)) {
		const dotIdx = name.indexOf(".");
		if (dotIdx < 0) {
			ports[name] = port;
			continue;
		}
		// Hierarchical signal: first segment is the interface port name
		const parentName = name.slice(0, dotIdx);
		const memberName = name.slice(dotIdx + 1);
		if (!ifMaps.has(parentName)) {
			const ifMap: Record<string, PortInfo> = {};
			ifMaps.set(parentName, ifMap);
			ports[parentName] = {
				direction: "inout",
				type: "logic",
				width: 0,
				interface: ifMap,
			};
		}
		ifMaps.get(parentName)![memberName] = port;
	}

	return ports;
}

// ---------------------------------------------------------------------------
// Hierarchy layout
// ---------------------------------------------------------------------------

export interface HierarchyNode {
	moduleName: string;
	signals: Record<
		string,
		SignalLayout & { typeKind: string; arrayDims?: number[] }
	>;
	forDut: Record<string, SignalLayout>;
	ports: Record<string, PortInfo>;
	children: Record<string, HierarchyNode[]>;
}

interface RawHierarchyNode {
	module_name: string;
	signals: Record<string, RawSignalLayout>;
	children: Record<string, RawHierarchyNode[]>;
}

/**
 * Parse the hierarchy JSON from NAPI into a HierarchyNode tree.
 * Converts snake_case keys to camelCase and auto-detects ports.
 */
export function parseHierarchyLayout(
	json: string,
	events: Record<string, number>,
): HierarchyNode {
	const raw: RawHierarchyNode = JSON.parse(json);

	// WASM-compiled addon may return an empty object `{}` for hierarchy.
	// Treat it as an empty hierarchy node.
	if (!raw.signals && !raw.module_name) {
		return {
			moduleName: "",
			signals: {},
			forDut: {},
			ports: {},
			children: {},
		};
	}

	return convertHierarchyNode(raw, events);
}

function convertHierarchyNode(
	raw: RawHierarchyNode,
	events: Record<string, number>,
): HierarchyNode {
	const signals: Record<
		string,
		SignalLayout & { typeKind: string; arrayDims?: number[] }
	> = {};
	const forDut: Record<string, SignalLayout> = {};

	for (const [name, r] of Object.entries(raw.signals)) {
		const sl: SignalLayout = {
			offset: r.offset,
			width: r.width,
			byteSize: r.byte_size > 0 ? r.byte_size : Math.ceil(r.width / 8),
			is4state: r.is_4state,
			direction: r.direction as SignalLayout["direction"],
		};
		const entry: SignalLayout & { typeKind: string; arrayDims?: number[] } = {
			...sl,
			typeKind: r.type_kind,
		};
		if (r.array_dims && r.array_dims.length > 0) {
			entry.arrayDims = r.array_dims;
		}
		signals[name] = entry;
		forDut[name] = sl;
	}

	const ports = buildPortsFromLayout(signals, events);

	const children: Record<string, HierarchyNode[]> = {};
	for (const [name, instances] of Object.entries(raw.children)) {
		children[name] = instances.map((inst) =>
			convertHierarchyNode(inst, events),
		);
	}

	return {
		moduleName: raw.module_name,
		signals,
		forDut,
		ports,
		children,
	};
}

/**
 * Filter a hierarchy tree based on the dead store elimination policy.
 *
 * - `"off"` / undefined → full hierarchy (no filtering)
 * - `"preserveTopPorts"` → strip children (only top-module ports survive DSE)
 * - `"preserveAllPorts"` → hierarchy intact (all instance ports survive DSE)
 */
export function filterHierarchyForDse(
	hierarchy: HierarchyNode,
	policy?: "off" | "preserveTopPorts" | "preserveAllPorts",
): HierarchyNode {
	if (!policy || policy === "off") return hierarchy;
	if (policy === "preserveTopPorts") return { ...hierarchy, children: {} };
	// "preserveAllPorts" — hierarchy intact, Rust-side ensures only ports survive
	return hierarchy;
}

// ---------------------------------------------------------------------------
// Handle wrapping — zero-copy direct operations
// ---------------------------------------------------------------------------

/**
 * Wrap a raw NAPI simulator handle with direct (zero-copy) operations.
 * The buffer is shared between JS and Rust — no copies per tick.
 */
export function wrapDirectSimulatorHandle(
	raw: RawNapiSimulatorHandle,
): NativeSimulatorHandle {
	return {
		tick(eventId: number): void {
			raw.tick!(eventId);
		},
		tickN(eventId: number, count: number): void {
			raw.tickN!(eventId, count);
		},
		evalComb(): void {
			raw.evalComb!();
		},
		dump(timestamp: number): void {
			raw.dump!(timestamp);
		},
		dispose(): void {
			raw.dispose();
		},
	};
}

/**
 * Wrap a raw NAPI simulation handle with direct (zero-copy) operations.
 */
export function wrapDirectSimulationHandle(
	raw: RawNapiSimulationHandle,
): NativeSimulationHandle {
	return {
		addClock(eventId: number, period: number, initialDelay: number): void {
			raw.addClock(eventId, period, initialDelay);
		},
		schedule(eventId: number, time: number, value: number): void {
			raw.schedule(eventId, time, value);
		},
		runUntil(endTime: number): void {
			raw.runUntil(endTime);
		},
		step(): number | null {
			return raw.step();
		},
		time(): number {
			return raw.time();
		},
		nextEventTime(): number | null {
			return raw.nextEventTime();
		},
		evalComb(): void {
			raw.evalComb();
		},
		dump(timestamp: number): void {
			raw.dump(timestamp);
		},
		dispose(): void {
			raw.dispose();
		},
	};
}

// ---------------------------------------------------------------------------
// Legacy layout parser (used by bridge helpers)
// ---------------------------------------------------------------------------

function parseLegacyLayout(json: string): Record<string, SignalLayout> {
	const raw: Record<string, RawSignalLayout> = JSON.parse(json);
	const result: Record<string, SignalLayout> = {};
	for (const [name, r] of Object.entries(raw)) {
		result[name] = {
			offset: r.offset,
			width: r.width,
			byteSize: r.byte_size > 0 ? r.byte_size : Math.ceil(r.width / 8),
			is4state: r.is_4state,
			direction: r.direction as SignalLayout["direction"],
		};
	}
	return result;
}

// ---------------------------------------------------------------------------
// Simulator bridge (used by Simulator.create())
// ---------------------------------------------------------------------------

/**
 * Create a `NativeCreateFn` from a raw NAPI addon, suitable for
 * `Simulator.create(module, { __nativeCreate: ... })`.
 */
export function createSimulatorBridge(addon: RawNapiAddon): NativeCreateFn {
	return (
		sources: ReadonlyArray<SourceFile>,
		moduleName: string,
		options: SimulatorOptions,
	): CreateResult<NativeSimulatorHandle> => {
		const napiOpts = buildNapiOpts(options);
		const napiSources = sources.map((s) => ({
			content: s.content,
			path: s.path,
		}));
		const raw = new addon.NativeSimulatorHandle(
			napiSources,
			moduleName,
			napiOpts,
		);

		const layout = parseLegacyLayout(raw.layoutJson);
		const events: Record<string, number> = JSON.parse(raw.eventsJson);
		const hierarchy = parseHierarchyLayout(raw.hierarchyJson, events);

		// Detect WASM-compiled addon and use the bridge
		let buf: ArrayBuffer | SharedArrayBuffer;
		let handle: NativeSimulatorHandle;
		if (isWasmHandle(raw)) {
			const bridge = createWasmSimulatorBridge(raw);
			buf = bridge.sharedMemory.buffer;
			handle = bridge.handle;
		} else {
			buf = raw.sharedMemory!().buffer;
			handle = wrapDirectSimulatorHandle(raw);
		}

		const warnings: string[] = JSON.parse(raw.warningsJson ?? "[]");

		return { buffer: buf, layout, events, handle, hierarchy, warnings };
	};
}

/**
 * Create a `NativeCreateSimulationFn` from a raw NAPI addon, suitable for
 * `Simulation.create(module, { __nativeCreate: ... })`.
 */
export function createSimulationBridge(
	addon: RawNapiAddon,
): NativeCreateSimulationFn {
	return (
		sources: ReadonlyArray<SourceFile>,
		moduleName: string,
		options: SimulatorOptions,
	): CreateResult<NativeSimulationHandle> => {
		// WASM addon does not export NativeSimulationHandle
		if (!addon.NativeSimulationHandle) {
			throw new Error(
				"Simulation is not supported in WASM mode. " +
					"The WASM-compiled addon only supports event-based Simulator, not time-based Simulation.",
			);
		}

		const napiOpts = buildNapiOpts(options);
		const napiSources = sources.map((s) => ({
			content: s.content,
			path: s.path,
		}));
		const raw = new addon.NativeSimulationHandle(
			napiSources,
			moduleName,
			napiOpts,
		);

		const layout = parseLegacyLayout(raw.layoutJson);
		const events: Record<string, number> = JSON.parse(raw.eventsJson);
		const hierarchy = parseHierarchyLayout(raw.hierarchyJson, events);

		const buf = raw.sharedMemory().buffer;
		const handle = wrapDirectSimulationHandle(raw);
		const warnings: string[] = JSON.parse(raw.warningsJson ?? "[]");

		return { buffer: buf, layout, events, handle, hierarchy, warnings };
	};
}

/**
 * Clear the process-global JIT compilation cache.
 *
 * Call this when source files have changed and cached compiled code
 * may be stale (e.g. during HMR or between benchmark iterations).
 */
export function clearJitCache(addon: RawNapiAddon): void {
	addon.clearJitCache();
}
