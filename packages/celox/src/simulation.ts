/**
 * Time-based Simulation.
 *
 * Wraps a NativeSimulationHandle and provides a high-level TypeScript API
 * for clock-driven simulation with automatic scheduling.
 */

import type {
  CreateResult,
  EventHandle,
  FourStateValue,
  ModuleDefinition,
  NativeSimulationHandle,
  SignalLayout,
  SimulatorOptions,
} from "./types.js";
import { SimulationTimeoutError } from "./types.js";
import { createDut, readFourState, type DirtyState } from "./dut.js";
import {
  loadNativeAddon,
  parseNapiLayout,
  buildPortsFromLayout,
  wrapDirectSimulationHandle,
  buildNapiOpts,
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
  private readonly _buffer: ArrayBuffer | SharedArrayBuffer;
  private readonly _layout: Record<string, SignalLayout & { typeKind?: string }>;
  private _disposed = false;

  private constructor(
    handle: NativeSimulationHandle,
    dut: P,
    events: Record<string, number>,
    state: DirtyState,
    buffer: ArrayBuffer | SharedArrayBuffer,
    layout: Record<string, SignalLayout & { typeKind?: string }>,
  ) {
    this._handle = handle;
    this._dut = dut;
    this._events = events;
    this._state = state;
    this._buffer = buffer;
    this._layout = layout;
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

    const { fourState, vcd, optimize, falseLoops, trueLoops, clockType, resetType } = options ?? {};
    const result = createFn(module.source, module.name, { fourState, vcd, optimize, falseLoops, trueLoops, clockType, resetType });
    const state: DirtyState = { dirty: false };

    const dut = createDut<P>(
      result.buffer,
      result.layout,
      module.ports,
      result.handle,
      state,
    );

    return new Simulation<P>(result.handle, dut, result.events, state, result.buffer, result.layout);
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
    const napiOpts = buildNapiOpts(options);
    const raw = new addon.NativeSimulationHandle(source, top, napiOpts);

    const layout = parseNapiLayout(raw.layoutJson);
    const events: Record<string, number> = JSON.parse(raw.eventsJson);

    const ports = buildPortsFromLayout(layout.signals, events);

    const buf = raw.sharedMemory().buffer;

    const state: DirtyState = { dirty: false };
    const handle = wrapDirectSimulationHandle(raw);
    const dut = createDut<P>(buf, layout.forDut, ports, handle, state);

    return new Simulation<P>(handle, dut, events, state, buf, layout.signals);
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
    const napiOpts = buildNapiOpts(options);
    const raw = addon.NativeSimulationHandle.fromProject(projectPath, top, napiOpts);

    const layout = parseNapiLayout(raw.layoutJson);
    const events: Record<string, number> = JSON.parse(raw.eventsJson);

    const ports = buildPortsFromLayout(layout.signals, events);

    const buf = raw.sharedMemory().buffer;

    const state: DirtyState = { dirty: false };
    const handle = wrapDirectSimulationHandle(raw);
    const dut = createDut<P>(buf, layout.forDut, ports, handle, state);

    return new Simulation<P>(handle, dut, events, state, buf, layout.signals);
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
   *
   * When `maxSteps` is provided, steps are counted in TS and a
   * `SimulationTimeoutError` is thrown if the budget is exhausted before
   * reaching `endTime`. Without `maxSteps` the fast Rust path is used.
   */
  runUntil(endTime: number, opts?: { maxSteps?: number }): void {
    this.ensureAlive();
    if (opts?.maxSteps == null) {
      this._handle.runUntil(endTime);
      this._state.dirty = false;
      return;
    }
    const max = opts.maxSteps;
    let steps = 0;
    while (this._handle.time() < endTime) {
      const t = this._handle.step();
      if (t == null) break;
      steps++;
      if (steps >= max) {
        this._state.dirty = false;
        throw new SimulationTimeoutError(
          `runUntil: exceeded ${max} steps at time ${this._handle.time()} (target ${endTime})`,
          this._handle.time(),
          steps,
        );
      }
    }
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

  /**
   * Step until `condition()` returns true.
   *
   * @returns The simulation time when the condition became true.
   * @throws SimulationTimeoutError if `maxSteps` is exceeded.
   */
  waitUntil(
    condition: () => boolean,
    opts?: { maxSteps?: number },
  ): number {
    this.ensureAlive();
    const max = opts?.maxSteps ?? 100_000;
    let steps = 0;
    while (!condition()) {
      const t = this._handle.step();
      this._state.dirty = false;
      if (t == null) break;
      steps++;
      if (steps >= max) {
        throw new SimulationTimeoutError(
          `waitUntil: condition not met after ${max} steps at time ${this._handle.time()}`,
          this._handle.time(),
          steps,
        );
      }
    }
    return this._handle.time();
  }

  /**
   * Wait for `count` cycles of the given clock event.
   *
   * Celox schedules 2 steps per clock cycle (rising + falling edge),
   * so this steps `count * 2` times.
   *
   * @returns The simulation time after the cycles complete.
   * @throws SimulationTimeoutError if `maxSteps` is exceeded.
   */
  waitForCycles(
    _event: string | EventHandle,
    count: number,
    opts?: { maxSteps?: number },
  ): number {
    this.ensureAlive();
    const totalSteps = count * 2;
    const max = opts?.maxSteps ?? 100_000;
    let stepped = 0;
    for (let i = 0; i < totalSteps; i++) {
      const t = this._handle.step();
      this._state.dirty = false;
      if (t == null) break;
      stepped++;
      if (stepped >= max) {
        throw new SimulationTimeoutError(
          `waitForCycles: exceeded ${max} steps at time ${this._handle.time()}`,
          this._handle.time(),
          stepped,
        );
      }
    }
    return this._handle.time();
  }

  /**
   * Assert and release a reset signal.
   *
   * The active level is determined automatically from the Veryl type:
   * - `reset` / `reset_async_high` / `reset_sync_high` → active-high (1)
   * - `reset_async_low` / `reset_sync_low` → active-low (0)
   *
   * Writes the active value, advances `activeCycles` clock cycles
   * (default 2), then writes the inactive value.
   */
  reset(
    signal: string,
    opts?: { activeCycles?: number },
  ): void {
    this.ensureAlive();
    const sig = this._layout[signal];
    if (!sig) {
      throw new Error(
        `Unknown port '${signal}'. Available: ${Object.keys(this._layout).join(", ")}`,
      );
    }
    const typeKind = sig.typeKind ?? "";
    if (!typeKind.startsWith("reset")) {
      throw new Error(
        `Port '${signal}' is not a reset signal (type_kind: '${typeKind}').`,
      );
    }
    const isActiveLow = typeKind === "reset_async_low" || typeKind === "reset_sync_low";
    const activeValue = isActiveLow ? 0 : 1;
    const inactiveValue = isActiveLow ? 1 : 0;
    const cycles = opts?.activeCycles ?? 2;

    const dut = this._dut as Record<string, unknown>;
    dut[signal] = activeValue;
    for (let i = 0; i < cycles * 2; i++) {
      this._handle.step();
    }
    dut[signal] = inactiveValue;
    this._state.dirty = false;
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
