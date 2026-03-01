/**
 * @celox-sim/celox
 *
 * TypeScript runtime for Celox HDL simulation.
 * Provides zero-FFI signal I/O via SharedArrayBuffer + DataView,
 * with NAPI calls only for control operations (tick, runUntil, etc.).
 *
 * @packageDocumentation
 */

// Core types
export type {
  ModuleDefinition,
  PortInfo,
  SimulatorOptions,
  EventHandle,
  FourStateValue,
  LoopBreak,
  TrueLoopSpec,
} from "./types.js";

/** @internal */
export type {
  SignalLayout,
  CreateResult,
  NativeHandle,
  NativeSimulatorHandle,
  NativeSimulationHandle,
} from "./types.js";

// 4-state helpers
export { X, FourState, isFourStateValue } from "./types.js";

// Error types
export { SimulationTimeoutError } from "./types.js";

// Simulator (event-based)
export { Simulator } from "./simulator.js";

// Simulation (time-based)
export { Simulation } from "./simulation.js";

/** @internal */
export { createDut, createChildDut, readFourState } from "./dut.js";
/** @internal */
export type { DirtyState } from "./dut.js";

/** @internal */
export {
  loadNativeAddon,
  parseNapiLayout,
  parseHierarchyLayout,
  buildPortsFromLayout,
  wrapDirectSimulatorHandle,
  wrapDirectSimulationHandle,
  createSimulatorBridge,
  createSimulationBridge,
  parseSignalPath,
  buildNapiOpts,
} from "./napi-helpers.js";
/** @internal */
export type { HierarchyNode, RawNapiAddon, RawNapiSimulatorHandle, RawNapiSimulationHandle } from "./napi-helpers.js";

// NAPI bridge (backward compat — re-exports from napi-helpers)
// Consumers that import from "./napi-bridge.js" still work.
