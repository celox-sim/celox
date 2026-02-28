/**
 * NAPI helper utilities for the @celox-sim/celox TypeScript runtime.
 *
 * Provides reusable functions for:
 *   - Loading the native addon
 *   - Parsing NAPI layout JSON into SignalLayout
 *   - Building PortInfo from NAPI layout (auto-detect ports)
 *   - Wrapping NAPI handles with synced memory operations
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
  tickSynced(eventId: number, input: Buffer): Buffer;
  evalComb(): void;
  evalCombSynced(input: Buffer): Buffer;
  dump(timestamp: number): void;
  readMemory(): Buffer;
  writeMemory(data: Buffer, offset: number): void;
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
  runUntilSynced(endTime: number, input: Buffer): Buffer;
  step(): number | null;
  stepSynced(input: Buffer): { time: number | null; buffer: Buffer };
  time(): number;
  evalComb(): void;
  evalCombSynced(input: Buffer): Buffer;
  dump(timestamp: number): void;
  readMemory(): Buffer;
  writeMemory(data: Buffer, offset: number): void;
  dispose(): void;
}

export interface RawNapiAddon {
  NativeSimulatorHandle: {
    new (code: string, top: string): RawNapiSimulatorHandle;
    fromProject(projectPath: string, top: string): RawNapiSimulatorHandle;
  };
  NativeSimulationHandle: {
    new (code: string, top: string): RawNapiSimulationHandle;
    fromProject(projectPath: string, top: string): RawNapiSimulationHandle;
  };
}

// ---------------------------------------------------------------------------
// Native addon loading
// ---------------------------------------------------------------------------

import { createRequire } from "node:module";
import path from "node:path";

/**
 * Load the native NAPI addon.
 *
 * @param addonPath  Explicit path to the `.node` file.
 *                   If omitted, tries common locations relative to this package.
 */
export function loadNativeAddon(addonPath?: string): RawNapiAddon {
  const require = createRequire(import.meta.url);

  if (addonPath) {
    return require(addonPath) as RawNapiAddon;
  }

  // Try common locations relative to the package
  const candidates = [
    // Workspace development: next to the celox-napi crate
    path.resolve(
      import.meta.dirname ?? __dirname,
      "../../../crates/celox-napi/celox.linux-x64-gnu.node",
    ),
  ];

  for (const candidate of candidates) {
    try {
      return require(candidate) as RawNapiAddon;
    } catch {
      // Try next candidate
    }
  }

  throw new Error(
    `Failed to load NAPI addon. Tried: ${candidates.join(", ")}. ` +
      `Build it first with: cargo build -p celox-napi`,
  );
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
}

/**
 * Parse the NAPI layout JSON into SignalLayout records.
 * Returns both the full layout (with type_kind for port detection) and
 * the DUT-compatible layout (without type_kind).
 */
export function parseNapiLayout(json: string): {
  signals: Record<string, SignalLayout & { typeKind: string }>;
  forDut: Record<string, SignalLayout>;
} {
  const raw: Record<string, RawSignalLayout> = JSON.parse(json);
  const signals: Record<string, SignalLayout & { typeKind: string }> = {};
  const forDut: Record<string, SignalLayout> = {};

  for (const [name, r] of Object.entries(raw)) {
    const sl: SignalLayout = {
      offset: r.offset,
      width: r.width,
      byteSize: r.byte_size > 0 ? r.byte_size : Math.ceil(r.width / 8),
      is4state: r.is_4state,
      direction: r.direction as "input" | "output" | "inout",
    };
    signals[name] = { ...sl, typeKind: r.type_kind };
    forDut[name] = sl;
  }

  return { signals, forDut };
}

/**
 * Build PortInfo records from the NAPI layout signals.
 * This auto-detects port metadata so users don't need to hand-write ModuleDefinition.
 */
export function buildPortsFromLayout(
  signals: Record<string, SignalLayout & { typeKind: string }>,
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

    ports[name] = {
      direction: sig.direction,
      type: portType,
      width: sig.width,
      is4state: sig.is4state,
    };
  }

  return ports;
}

// ---------------------------------------------------------------------------
// Memory synchronisation helpers
// ---------------------------------------------------------------------------

/**
 * One-time initial copy from native memory into a buffer.
 */
export function syncFromNative(
  raw: { readMemory(): Buffer },
  buf: ArrayBuffer | SharedArrayBuffer,
): void {
  const nativeBuf = raw.readMemory();
  const target = new Uint8Array(buf);
  target.set(nativeBuf);
}

function copyResultToBuffer(
  result: Buffer,
  buf: ArrayBuffer | SharedArrayBuffer,
): void {
  const target = new Uint8Array(buf);
  target.set(result);
}

// ---------------------------------------------------------------------------
// Handle wrapping — synced operations (1 NAPI call + 2 copies)
// ---------------------------------------------------------------------------

/**
 * Wrap a raw NAPI simulator handle to use synced operations.
 * Sends the buffer contents as a Buffer argument, receives updated Buffer back.
 */
export function wrapSimulatorHandle(
  raw: RawNapiSimulatorHandle,
  buf: ArrayBuffer | SharedArrayBuffer,
  _stableSize: number,
): NativeSimulatorHandle {
  return {
    tick(eventId: number): void {
      const input = Buffer.from(buf);
      const result = raw.tickSynced(eventId, input);
      copyResultToBuffer(result, buf);
    },
    evalComb(): void {
      const input = Buffer.from(buf);
      const result = raw.evalCombSynced(input);
      copyResultToBuffer(result, buf);
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
 * Wrap a raw NAPI simulation handle to use synced operations.
 */
export function wrapSimulationHandle(
  raw: RawNapiSimulationHandle,
  buf: ArrayBuffer | SharedArrayBuffer,
  _stableSize: number,
): NativeSimulationHandle {
  return {
    addClock(eventId: number, period: number, initialDelay: number): void {
      raw.addClock(eventId, period, initialDelay);
    },
    schedule(eventId: number, time: number, value: number): void {
      raw.schedule(eventId, time, value);
    },
    runUntil(endTime: number): void {
      const input = Buffer.from(buf);
      const result = raw.runUntilSynced(endTime, input);
      copyResultToBuffer(result, buf);
    },
    step(): number | null {
      const input = Buffer.from(buf);
      const r = raw.stepSynced(input);
      copyResultToBuffer(r.buffer, buf);
      return r.time;
    },
    time(): number {
      return raw.time();
    },
    evalComb(): void {
      const input = Buffer.from(buf);
      const result = raw.evalCombSynced(input);
      copyResultToBuffer(result, buf);
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
// Legacy bridge helpers — used by napi-bridge.ts for backward compat
// ---------------------------------------------------------------------------

function copyNativeToShared(
  nativeHandle: { readMemory(): Buffer },
  sab: SharedArrayBuffer,
): void {
  const nativeBuf = nativeHandle.readMemory();
  const target = new Uint8Array(sab);
  target.set(nativeBuf);
}

function copySharedToNative(
  sab: SharedArrayBuffer,
  nativeHandle: { writeMemory(data: Buffer, offset: number): void },
): void {
  const src = Buffer.from(sab);
  nativeHandle.writeMemory(src, 0);
}

function bridgeSimulatorHandle(
  raw: RawNapiSimulatorHandle,
  sab: SharedArrayBuffer,
): NativeSimulatorHandle {
  return {
    tick(eventId: number): void {
      copySharedToNative(sab, raw);
      raw.tick(eventId);
      copyNativeToShared(raw, sab);
    },
    evalComb(): void {
      copySharedToNative(sab, raw);
      raw.evalComb();
      copyNativeToShared(raw, sab);
    },
    dump(timestamp: number): void {
      raw.dump(timestamp);
    },
    dispose(): void {
      raw.dispose();
    },
  };
}

function bridgeSimulationHandle(
  raw: RawNapiSimulationHandle,
  sab: SharedArrayBuffer,
): NativeSimulationHandle {
  return {
    addClock(eventId: number, period: number, initialDelay: number): void {
      raw.addClock(eventId, period, initialDelay);
    },
    schedule(eventId: number, time: number, value: number): void {
      raw.schedule(eventId, time, value);
    },
    runUntil(endTime: number): void {
      copySharedToNative(sab, raw);
      raw.runUntil(endTime);
      copyNativeToShared(raw, sab);
    },
    step(): number | null {
      copySharedToNative(sab, raw);
      const t = raw.step();
      copyNativeToShared(raw, sab);
      return t;
    },
    time(): number {
      return raw.time();
    },
    evalComb(): void {
      copySharedToNative(sab, raw);
      raw.evalComb();
      copyNativeToShared(raw, sab);
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
// Simulator bridge (backward compat — used by Simulator.create())
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

/**
 * Create a `NativeCreateFn` from a raw NAPI addon, suitable for
 * `Simulator.create(module, { __nativeCreate: ... })`.
 */
export function createSimulatorBridge(addon: RawNapiAddon): NativeCreateFn {
  return (
    source: string,
    moduleName: string,
    _options: SimulatorOptions,
  ): CreateResult<NativeSimulatorHandle> => {
    const raw = new addon.NativeSimulatorHandle(source, moduleName);

    const layout = parseLegacyLayout(raw.layoutJson);
    const events: Record<string, number> = JSON.parse(raw.eventsJson);
    const stableSize = raw.stableSize;

    const sab = new SharedArrayBuffer(stableSize);
    copyNativeToShared(raw, sab);

    const handle = bridgeSimulatorHandle(raw, sab);

    return { buffer: sab, layout, events, handle };
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
    _options: SimulatorOptions,
  ): CreateResult<NativeSimulationHandle> => {
    const raw = new addon.NativeSimulationHandle(source, moduleName);

    const layout = parseLegacyLayout(raw.layoutJson);
    const events: Record<string, number> = JSON.parse(raw.eventsJson);
    const stableSize = raw.stableSize;

    const sab = new SharedArrayBuffer(stableSize);
    copyNativeToShared(raw, sab);

    const handle = bridgeSimulationHandle(raw, sab);

    return { buffer: sab, layout, events, handle };
  };
}
