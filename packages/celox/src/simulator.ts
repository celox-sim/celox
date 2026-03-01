/**
 * Event-based Simulator.
 *
 * Wraps a NativeSimulatorHandle and provides a high-level TypeScript API
 * for manually controlling clock edges via `tick()`.
 */

import type {
  CreateResult,
  EventHandle,
  FourStateValue,
  ModuleDefinition,
  NativeSimulatorHandle,
  SignalLayout,
  SimulatorOptions,
} from "./types.js";
import { createDut, readFourState, type DirtyState } from "./dut.js";
import {
  loadNativeAddon,
  parseNapiLayout,
  parseHierarchyLayout,
  buildPortsFromLayout,
  wrapDirectSimulatorHandle,
  buildNapiOpts,
} from "./napi-helpers.js";

/**
 * Placeholder for the NAPI binding's `createSimulator()`.
 * Stream B will provide the real implementation; until then tests can
 * inject a mock via `Simulator.create()` options or by replacing this
 * module.
 * @internal
 */
export type NativeCreateFn = (
  source: string,
  moduleName: string,
  options: SimulatorOptions,
) => CreateResult<NativeSimulatorHandle>;

let _nativeCreate: NativeCreateFn | undefined;

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
  private readonly _defaultEventId: number;
  private readonly _state: DirtyState;
  private readonly _buffer: ArrayBuffer | SharedArrayBuffer;
  private readonly _layout: Record<string, SignalLayout>;
  private _disposed = false;

  private constructor(
    handle: NativeSimulatorHandle,
    dut: P,
    events: Record<string, number>,
    state: DirtyState,
    buffer: ArrayBuffer | SharedArrayBuffer,
    layout: Record<string, SignalLayout>,
  ) {
    this._handle = handle;
    this._dut = dut;
    this._events = events;
    this._state = state;
    this._buffer = buffer;
    this._layout = layout;
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
    // When the module was produced by the Vite plugin, delegate to fromProject()
    if (module.projectPath && !options?.__nativeCreate) {
      return Simulator.fromProject<P>(module.projectPath, module.name, options);
    }

    const createFn = options?.__nativeCreate ?? _nativeCreate;
    if (!createFn) {
      throw new Error(
        "Native simulator binding not loaded. " +
          "Ensure @celox-sim/celox-napi is installed.",
      );
    }

    const { fourState, vcd, optimize, falseLoops, trueLoops, clockType, resetType } = options ?? {};
    const result = createFn(module.source, module.name, { fourState, vcd, optimize, falseLoops, trueLoops, clockType, resetType });
    const state: DirtyState = { dirty: false };

    const dut = createDut<P>(
      result.buffer,
      result.layout,
      module.ports,
      result.handle,
      state,
      result.hierarchy,
    );

    return new Simulator<P>(result.handle, dut, result.events, state, result.buffer, result.layout);
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
    const napiOpts = buildNapiOpts(options);
    const raw = new addon.NativeSimulatorHandle(source, top, napiOpts);

    const layout = parseNapiLayout(raw.layoutJson);
    const events: Record<string, number> = JSON.parse(raw.eventsJson);
    const hierarchy = parseHierarchyLayout(raw.hierarchyJson, events);

    const ports = buildPortsFromLayout(layout.signals, events);

    const buf = raw.sharedMemory().buffer;

    const state: DirtyState = { dirty: false };
    const handle = wrapDirectSimulatorHandle(raw);
    const dut = createDut<P>(buf, layout.forDut, ports, handle, state, hierarchy);

    return new Simulator<P>(handle, dut, events, state, buf, layout.forDut);
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
    const napiOpts = buildNapiOpts(options);
    const raw = addon.NativeSimulatorHandle.fromProject(projectPath, top, napiOpts);

    const layout = parseNapiLayout(raw.layoutJson);
    const events: Record<string, number> = JSON.parse(raw.eventsJson);
    const hierarchy = parseHierarchyLayout(raw.hierarchyJson, events);

    const ports = buildPortsFromLayout(layout.signals, events);

    const buf = raw.sharedMemory().buffer;

    const state: DirtyState = { dirty: false };
    const handle = wrapDirectSimulatorHandle(raw);
    const dut = createDut<P>(buf, layout.forDut, ports, handle, state, hierarchy);

    return new Simulator<P>(handle, dut, events, state, buf, layout.forDut);
  }

  /** The DUT accessor object — read/write ports as plain properties. */
  get dut(): P {
    return this._dut;
  }

  /**
   * Trigger a clock edge.
   *
   * @param event  Optional event handle from `this.event()`.
   *               If omitted, ticks the first (default) event.
   * @param count  Number of ticks. Default: 1.
   */
  tick(event?: EventHandle | number, count?: number): void;
  tick(count?: number): void;
  tick(
    eventOrCount?: EventHandle | number,
    count?: number,
  ): void {
    this.ensureAlive();

    let eventId: number;
    let ticks: number;

    if (typeof eventOrCount === "object" && eventOrCount !== null) {
      // tick(eventHandle, count?)
      eventId = (eventOrCount as EventHandle).id;
      ticks = count ?? 1;
    } else if (typeof eventOrCount === "number") {
      // tick(count) — default event
      eventId = this._defaultEventId;
      ticks = eventOrCount;
    } else {
      // tick() — default event, 1 tick
      eventId = this._defaultEventId;
      ticks = 1;
    }

    if (ticks === 1) {
      this._handle.tick(eventId);
    } else if (ticks > 1) {
      this._handle.tickN(eventId, ticks);
    }
    this._state.dirty = false;
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
}
