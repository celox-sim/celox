/**
 * @celox-sim/celox
 *
 * TypeScript runtime for Celox HDL simulation.
 * Provides zero-FFI signal I/O via SharedArrayBuffer + DataView,
 * with NAPI calls only for control operations (tick, runUntil, etc.).
 *
 * @packageDocumentation
 */

/** @internal */
export type { DirtyState } from "./dut.js";
/** @internal */
export { createChildDut, createDut, readFourState } from "./dut.js";
/** @internal */
export type {
	HierarchyNode,
	RawNapiAddon,
	RawNapiSimulationHandle,
	RawNapiSimulatorHandle,
} from "./napi-helpers.js";
/** @internal */
export {
	buildNapiOpts,
	buildPortsFromLayout,
	clearJitCache,
	createSimulationBridge,
	createSimulatorBridge,
	loadNativeAddon,
	parseHierarchyLayout,
	parseNapiLayout,
	parseSignalPath,
	wrapDirectSimulationHandle,
	wrapDirectSimulatorHandle,
} from "./napi-helpers.js";
// Simulation (time-based)
export { Simulation } from "./simulation.js";
// Simulator (event-based)
export { Simulator } from "./simulator.js";
// Core types
/** @internal */
export type {
	CreateResult,
	EventHandle,
	FourStateValue,
	LoopBreak,
	ModuleDefinition,
	NativeHandle,
	NativeSimulationHandle,
	NativeSimulatorHandle,
	OptimizeOptions,
	ParamOverride,
	PortInfo,
	SignalLayout,
	SimulatorOptions,
	SourceFile,
	TrueLoopSpec,
} from "./types.js";
// 4-state helpers
// Error types
export {
	FourState,
	isFourStateValue,
	SimulationTimeoutError,
	X,
	Z,
} from "./types.js";

// WASM bridge for browser simulation
/** @internal */
export type {
	RawWasmSimulatorHandle,
	WasmBridgeResult,
} from "./wasm-bridge.js";
export {
	createWasmSimulatorBridge,
	createWasmSimulatorBridgeAsync,
	isWasmHandle,
} from "./wasm-bridge.js";

// NAPI bridge (backward compat — re-exports from napi-helpers)
// Consumers that import from "./napi-bridge.js" still work.
