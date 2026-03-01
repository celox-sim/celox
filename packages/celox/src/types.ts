/**
 * @celox-sim/celox — Core type definitions
 *
 * These types define the contract between:
 *   - Stream A (type generator): produces ModuleDefinition
 *   - Stream B (NAPI bindings): produces CreateResult / NativeHandles
 *   - Stream C (this package): consumes both
 */

// ---------------------------------------------------------------------------
// Module definition (produced by Stream A's `celox-gen-ts`)
// ---------------------------------------------------------------------------

/**
 * A compiled module descriptor emitted by `celox-gen-ts`.
 * The type parameter `Ports` carries the generated port interface
 * (e.g. `AdderPorts`) so that `Simulator.create(Adder)` returns
 * a correctly-typed DUT accessor.
 */
export interface ModuleDefinition<Ports = Record<string, unknown>> {
  readonly __celox_module: true;
  readonly name: string;
  readonly source: string;
  readonly ports: Record<string, PortInfo>;
  readonly events: string[];
  /** Absolute path to the Veryl project directory (set by Vite plugin). */
  readonly projectPath?: string;
  /** Phantom field — never set at runtime. Carries the `Ports` type. */
  readonly __ports?: Ports;
}

/** Metadata for a single port / interface member. */
export interface PortInfo {
  readonly direction: "input" | "output" | "inout";
  readonly type: "clock" | "reset" | "logic" | "bit";
  readonly width: number;
  readonly arrayDims?: readonly number[];
  readonly is4state?: boolean;
  /** Nested interface members (recursive). */
  readonly interface?: Record<string, PortInfo>;
}

// ---------------------------------------------------------------------------
// Signal layout (produced by Stream B's NAPI `create()`)
// ---------------------------------------------------------------------------

/**
 * Byte-level location of a signal inside the SharedArrayBuffer.
 * @internal
 */
export interface SignalLayout {
  /** Byte offset within the STABLE region. */
  readonly offset: number;
  /** Bit width of the signal. */
  readonly width: number;
  /** Number of bytes occupied (ceil(width/8)). */
  readonly byteSize: number;
  /** If true, an equal-sized mask follows immediately after the value. */
  readonly is4state: boolean;
  readonly direction: "input" | "output" | "inout";
}

// ---------------------------------------------------------------------------
// NAPI handles (produced by Stream B)
// ---------------------------------------------------------------------------

/**
 * Opaque handle returned by NAPI for event-based simulation.
 * @internal
 */
export interface NativeSimulatorHandle {
  tick(eventId: number): void;
  tickN(eventId: number, count: number): void;
  evalComb(): void;
  dump(timestamp: number): void;
  dispose(): void;
}

/**
 * Opaque handle returned by NAPI for time-based simulation.
 * @internal
 */
export interface NativeSimulationHandle {
  addClock(eventId: number, period: number, initialDelay: number): void;
  schedule(eventId: number, time: number, value: number): void;
  runUntil(endTime: number): void;
  step(): number | null;
  time(): number;
  nextEventTime(): number | null;
  evalComb(): void;
  dump(timestamp: number): void;
  dispose(): void;
}

/**
 * Union of both handle types for code that is handle-agnostic.
 * @internal
 */
export type NativeHandle = NativeSimulatorHandle | NativeSimulationHandle;

/**
 * Result returned by NAPI `create()`.
 * @internal
 */
export interface CreateResult<H extends NativeHandle = NativeHandle> {
  /** The simulator's internal memory buffer shared zero-copy. */
  readonly buffer: ArrayBuffer | SharedArrayBuffer;
  /** Per-signal byte layout within `buffer`. */
  readonly layout: Record<string, SignalLayout>;
  /** Event name → internal event ID mapping. */
  readonly events: Record<string, number>;
  /** Native control handle. */
  readonly handle: H;
}

// ---------------------------------------------------------------------------
// User-facing options
// ---------------------------------------------------------------------------

export interface SimulatorOptions {
  /** Enable 4-state (X/Z) simulation. Default: false. */
  fourState?: boolean;
  /** Path to write VCD waveform output. */
  vcd?: string;
  /** Enable Cranelift optimization passes. */
  optimize?: boolean;
  /** False-loop declarations to ignore during compilation. */
  falseLoops?: LoopBreak[];
  /** True-loop declarations with convergence limits. */
  trueLoops?: TrueLoopSpec[];
}

// ---------------------------------------------------------------------------
// Event handle (returned by Simulator.event())
// ---------------------------------------------------------------------------

/** A resolved event reference for use with `tick()`. */
export interface EventHandle {
  readonly name: string;
  readonly id: number;
}

// ---------------------------------------------------------------------------
// 4-state helpers
// ---------------------------------------------------------------------------

/** Sentinel representing all-X. */
export const X = Symbol.for("veryl:X");

/** A 4-state value with explicit bit-level mask. */
export interface FourStateValue {
  readonly __fourState: true;
  readonly value: number | bigint;
  readonly mask: number | bigint;
}

/** Construct a 4-state value. Mask bits set to 1 indicate X. */
export function FourState(
  value: number | bigint,
  mask: number | bigint,
): FourStateValue {
  return { __fourState: true, value, mask };
}

export function isFourStateValue(v: unknown): v is FourStateValue {
  return (
    typeof v === "object" &&
    v !== null &&
    (v as FourStateValue).__fourState === true
  );
}

// ---------------------------------------------------------------------------
// Simulation timeout error
// ---------------------------------------------------------------------------

/**
 * Thrown when a simulation helper exceeds its step budget.
 */
export class SimulationTimeoutError extends Error {
  readonly time: number;
  readonly steps: number;

  constructor(message: string, time: number, steps: number) {
    super(message);
    this.name = "SimulationTimeoutError";
    this.time = time;
    this.steps = steps;
  }
}

// ---------------------------------------------------------------------------
// Loop-break types (for Phase 3c)
// ---------------------------------------------------------------------------

/** Specifies a false-loop to ignore during compilation. */
export interface LoopBreak {
  from: string;
  to: string;
}

/** Specifies a true-loop with a convergence iteration limit. */
export interface TrueLoopSpec extends LoopBreak {
  maxIter: number;
}
