/**
 * @celox-sim/celox
 *
 * TypeScript runtime for Celox HDL simulation.
 * Provides zero-FFI signal I/O via SharedArrayBuffer + DataView,
 * with NAPI calls only for control operations (tick, runUntil, etc.).
 */

// Core types
export type {
  ModuleDefinition,
  PortInfo,
  SignalLayout,
  SimulatorOptions,
  EventHandle,
  CreateResult,
  NativeHandle,
  NativeSimulatorHandle,
  NativeSimulationHandle,
  FourStateValue,
} from "./types.js";

// 4-state helpers
export { X, FourState, isFourStateValue } from "./types.js";

// Simulator (event-based)
export { Simulator } from "./simulator.js";

// Simulation (time-based)
export { Simulation } from "./simulation.js";

// DUT accessor (advanced / internal use)
export { createDut, readFourState } from "./dut.js";
export type { DirtyState } from "./dut.js";

// NAPI helpers (new — preferred)
export {
  loadNativeAddon,
  parseNapiLayout,
  buildPortsFromLayout,
  wrapDirectSimulatorHandle,
  wrapDirectSimulationHandle,
  createSimulatorBridge,
  createSimulationBridge,
} from "./napi-helpers.js";
export type { RawNapiAddon, RawNapiSimulatorHandle, RawNapiSimulationHandle } from "./napi-helpers.js";

// NAPI bridge (backward compat — re-exports from napi-helpers)
// Consumers that import from "./napi-bridge.js" still work.
