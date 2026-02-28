/**
 * Time-based Simulation.
 *
 * Wraps a NativeSimulationHandle and provides a high-level TypeScript API
 * for clock-driven simulation with automatic scheduling.
 */

import type {
  CreateResult,
  ModuleDefinition,
  NativeSimulationHandle,
  SimulatorOptions,
} from "./types.js";
import { createDut, type DirtyState } from "./dut.js";
import {
  loadNativeAddon,
  parseNapiLayout,
  buildPortsFromLayout,
  wrapDirectSimulationHandle,
} from "./napi-helpers.js";

/**
 * Placeholder for the NAPI binding's `createSimulation()`.
 * Stream B will provide the real implementation.
 * @internal
 */
export type NativeCreateSimulationFn = (
  source: string,
  moduleName: string,
  options: SimulatorOptions,
) => CreateResult<NativeSimulationHandle>;

let _nativeCreate: NativeCreateSimulationFn | undefined;

/**
 * Register the NAPI binding at module load time.
 * @internal
 */
export function setNativeSimulationCreate(fn: NativeCreateSimulationFn): void {
  _nativeCreate = fn;
}

// ---------------------------------------------------------------------------
// Simulation
// ---------------------------------------------------------------------------

export class Simulation<P = Record<string, unknown>> {
  private readonly _handle: NativeSimulationHandle;
  private readonly _dut: P;
  private readonly _events: Record<string, number>;
  private readonly _state: DirtyState;
  private _disposed = false;

  private constructor(
    handle: NativeSimulationHandle,
    dut: P,
    events: Record<string, number>,
    state: DirtyState,
  ) {
    this._handle = handle;
    this._dut = dut;
    this._events = events;
    this._state = state;
  }

  /**
   * Create a Simulation for the given module.
   *
   * ```ts
   * import { Top } from "./generated/Top.js";
   * const sim = Simulation.create(Top);
   * sim.addClock("clk", { period: 10 });
   * ```
   */
  static create<P>(
    module: ModuleDefinition<P>,
    options?: SimulatorOptions & {
      __nativeCreate?: NativeCreateSimulationFn;
    },
  ): Simulation<P> {
    // When the module was produced by the Vite plugin, delegate to fromProject()
    if (module.projectPath && !options?.__nativeCreate) {
      return Simulation.fromProject<P>(module.projectPath, module.name, options);
    }

    const createFn = options?.__nativeCreate ?? _nativeCreate;
    if (!createFn) {
      throw new Error(
        "Native simulator binding not loaded. " +
          "Ensure @celox-sim/celox-napi is installed.",
      );
    }

    const { fourState, vcd } = options ?? {};
    const result = createFn(module.source, module.name, { fourState, vcd });
    const state: DirtyState = { dirty: false };

    const dut = createDut<P>(
      result.buffer,
      result.layout,
      module.ports,
      result.handle,
      state,
    );

    return new Simulation<P>(result.handle, dut, result.events, state);
  }

  /**
   * Create a Simulation directly from Veryl source code.
   *
   * Automatically discovers ports from the NAPI layout — no
   * `ModuleDefinition` needed.
   *
   * ```ts
   * const sim = Simulation.fromSource<CounterPorts>(COUNTER_SOURCE, "Counter");
   * sim.addClock("clk", { period: 10 });
   * sim.runUntil(100);
   * ```
   */
  static fromSource<P = Record<string, unknown>>(
    source: string,
    top: string,
    options?: SimulatorOptions & { nativeAddonPath?: string },
  ): Simulation<P> {
    const addon = loadNativeAddon(options?.nativeAddonPath);
    const napiOpts: Record<string, unknown> = {};
    if (options?.fourState) napiOpts.fourState = options.fourState;
    if (options?.vcd) napiOpts.vcd = options.vcd;
    const raw = new addon.NativeSimulationHandle(source, top, Object.keys(napiOpts).length > 0 ? napiOpts : undefined);

    const layout = parseNapiLayout(raw.layoutJson);
    const events: Record<string, number> = JSON.parse(raw.eventsJson);

    const ports = buildPortsFromLayout(layout.signals, events);

    const buf = raw.sharedMemory().buffer;

    const state: DirtyState = { dirty: false };
    const handle = wrapDirectSimulationHandle(raw);
    const dut = createDut<P>(buf, layout.forDut, ports, handle, state);

    return new Simulation<P>(handle, dut, events, state);
  }

  /**
   * Create a Simulation from a Veryl project directory.
   *
   * Searches upward from `projectPath` for `Veryl.toml`, gathers all
   * `.veryl` source files, and builds the simulation using the project's
   * clock/reset settings.
   *
   * ```ts
   * const sim = Simulation.fromProject<MyPorts>("./my-project", "Top");
   * sim.addClock("clk", { period: 10 });
   * ```
   */
  static fromProject<P = Record<string, unknown>>(
    projectPath: string,
    top: string,
    options?: SimulatorOptions & { nativeAddonPath?: string },
  ): Simulation<P> {
    const addon = loadNativeAddon(options?.nativeAddonPath);
    const napiOpts: Record<string, unknown> = {};
    if (options?.fourState) napiOpts.fourState = options.fourState;
    if (options?.vcd) napiOpts.vcd = options.vcd;
    const raw = addon.NativeSimulationHandle.fromProject(projectPath, top, Object.keys(napiOpts).length > 0 ? napiOpts : undefined);

    const layout = parseNapiLayout(raw.layoutJson);
    const events: Record<string, number> = JSON.parse(raw.eventsJson);

    const ports = buildPortsFromLayout(layout.signals, events);

    const buf = raw.sharedMemory().buffer;

    const state: DirtyState = { dirty: false };
    const handle = wrapDirectSimulationHandle(raw);
    const dut = createDut<P>(buf, layout.forDut, ports, handle, state);

    return new Simulation<P>(handle, dut, events, state);
  }

  /** The DUT accessor object — read/write ports as plain properties. */
  get dut(): P {
    return this._dut;
  }

  /**
   * Register a periodic clock.
   *
   * @param name    Clock event name (must match a `clock` port).
   * @param opts    `period` in time units; optional `initialDelay`.
   */
  addClock(
    name: string,
    opts: { period: number; initialDelay?: number },
  ): void {
    this.ensureAlive();
    const eventId = this.resolveEvent(name);
    this._handle.addClock(eventId, opts.period, opts.initialDelay ?? 0);
  }

  /**
   * Schedule a one-shot value change for a signal.
   *
   * @param name  Event/signal name.
   * @param opts  `time` — absolute time to apply; `value` — value to set.
   */
  schedule(name: string, opts: { time: number; value: number }): void {
    this.ensureAlive();
    const eventId = this.resolveEvent(name);
    this._handle.schedule(eventId, opts.time, opts.value);
  }

  /**
   * Run the simulation until the given time.
   * Processes all scheduled events up to and including `endTime`.
   * evalComb is called internally; dirty is cleared on return.
   */
  runUntil(endTime: number): void {
    this.ensureAlive();
    this._handle.runUntil(endTime);
    this._state.dirty = false;
  }

  /**
   * Advance to the next scheduled event.
   *
   * @returns The time of the processed event, or `null` if no events remain.
   */
  step(): number | null {
    this.ensureAlive();
    const t = this._handle.step();
    this._state.dirty = false;
    return t;
  }

  /** Current simulation time. */
  time(): number {
    this.ensureAlive();
    return this._handle.time();
  }

  /**
   * Peek at the time of the next scheduled event without advancing.
   *
   * @returns The time of the next event, or `null` if no events are scheduled.
   */
  nextEventTime(): number | null {
    this.ensureAlive();
    return this._handle.nextEventTime();
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

  private resolveEvent(name: string): number {
    const id = this._events[name];
    if (id === undefined) {
      throw new Error(
        `Unknown event '${name}'. Available: ${Object.keys(this._events).join(", ")}`,
      );
    }
    return id;
  }

  private ensureAlive(): void {
    if (this._disposed) {
      throw new Error("Simulation has been disposed");
    }
  }
}
