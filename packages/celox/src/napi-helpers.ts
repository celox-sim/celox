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

import type {
  CreateResult,
  NativeSimulatorHandle,
  NativeSimulationHandle,
  PortInfo,
  SignalLayout,
  SimulatorOptions,
} from "./types.js";
import type { NativeCreateFn } from "./simulator.js";
import type { NativeCreateSimulationFn } from "./simulation.js";

// ---------------------------------------------------------------------------
// Raw NAPI handle shapes (what the .node addon actually exports)
// ---------------------------------------------------------------------------

export interface RawNapiSimulatorHandle {
  readonly layoutJson: string;
  readonly eventsJson: string;
  readonly stableSize: number;
  readonly totalSize: number;
  tick(eventId: number): void;
  tickN(eventId: number, count: number): void;
  evalComb(): void;
  dump(timestamp: number): void;
  sharedMemory(): Uint8Array;
  dispose(): void;
}

export interface RawNapiSimulationHandle {
  readonly layoutJson: string;
  readonly eventsJson: string;
  readonly stableSize: number;
  readonly totalSize: number;
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

export interface NapiOptions {
  fourState?: boolean;
  vcd?: string;
}

export interface RawNapiAddon {
  NativeSimulatorHandle: {
    new (code: string, top: string, options?: NapiOptions): RawNapiSimulatorHandle;
    fromProject(projectPath: string, top: string, options?: NapiOptions): RawNapiSimulatorHandle;
  };
  NativeSimulationHandle: {
    new (code: string, top: string, options?: NapiOptions): RawNapiSimulationHandle;
    fromProject(projectPath: string, top: string, options?: NapiOptions): RawNapiSimulationHandle;
  };
  genTs(projectPath: string): string;
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
}

/**
 * Parse the NAPI layout JSON into SignalLayout records.
 * Returns both the full layout (with type_kind for port detection) and
 * the DUT-compatible layout (without type_kind).
 */
export function parseNapiLayout(json: string): {
  signals: Record<string, SignalLayout & { typeKind: string; arrayDims?: number[] }>;
  forDut: Record<string, SignalLayout>;
} {
  const raw: Record<string, RawSignalLayout> = JSON.parse(json);
  const signals: Record<string, SignalLayout & { typeKind: string; arrayDims?: number[] }> = {};
  const forDut: Record<string, SignalLayout> = {};

  for (const [name, r] of Object.entries(raw)) {
    const sl: SignalLayout = {
      offset: r.offset,
      width: r.width,
      byteSize: r.byte_size > 0 ? r.byte_size : Math.ceil(r.width / 8),
      is4state: r.is_4state,
      direction: r.direction as "input" | "output" | "inout",
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

  return { signals, forDut };
}

/**
 * Build PortInfo records from the NAPI layout signals.
 * This auto-detects port metadata so users don't need to hand-write ModuleDefinition.
 */
export function buildPortsFromLayout(
  signals: Record<string, SignalLayout & { typeKind: string; arrayDims?: number[] }>,
  _events: Record<string, number>,
): Record<string, PortInfo> {
  const ports: Record<string, PortInfo> = {};

  for (const [name, sig] of Object.entries(signals)) {
    const typeKind = sig.typeKind;
    let portType: "clock" | "reset" | "logic" | "bit";
    switch (typeKind) {
      case "clock":
        portType = "clock";
        break;
      case "reset":
        portType = "reset";
        break;
      case "bit":
        portType = "bit";
        break;
      default:
        portType = "logic";
        break;
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
    ports[name] = port;
  }

  return ports;
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
      raw.tick(eventId);
    },
    tickN(eventId: number, count: number): void {
      raw.tickN(eventId, count);
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
      direction: r.direction as "input" | "output" | "inout",
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
    source: string,
    moduleName: string,
    options: SimulatorOptions,
  ): CreateResult<NativeSimulatorHandle> => {
    const napiOpts: NapiOptions = {};
    if (options?.fourState) napiOpts.fourState = options.fourState;
    if (options?.vcd) napiOpts.vcd = options.vcd;
    const raw = new addon.NativeSimulatorHandle(source, moduleName, Object.keys(napiOpts).length > 0 ? napiOpts : undefined);

    const layout = parseLegacyLayout(raw.layoutJson);
    const events: Record<string, number> = JSON.parse(raw.eventsJson);

    const buf = raw.sharedMemory().buffer;
    const handle = wrapDirectSimulatorHandle(raw);

    return { buffer: buf, layout, events, handle };
  };
}

/**
 * Create a `NativeCreateSimulationFn` from a raw NAPI addon, suitable for
 * `Simulation.create(module, { __nativeCreate: ... })`.
 */
export function createSimulationBridge(addon: RawNapiAddon): NativeCreateSimulationFn {
  return (
    source: string,
    moduleName: string,
    options: SimulatorOptions,
  ): CreateResult<NativeSimulationHandle> => {
    const napiOpts: NapiOptions = {};
    if (options?.fourState) napiOpts.fourState = options.fourState;
    if (options?.vcd) napiOpts.vcd = options.vcd;
    const raw = new addon.NativeSimulationHandle(source, moduleName, Object.keys(napiOpts).length > 0 ? napiOpts : undefined);

    const layout = parseLegacyLayout(raw.layoutJson);
    const events: Record<string, number> = JSON.parse(raw.eventsJson);

    const buf = raw.sharedMemory().buffer;
    const handle = wrapDirectSimulationHandle(raw);

    return { buffer: buf, layout, events, handle };
  };
}
