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

/** A source file with its path and content. */
export interface SourceFile {
	readonly path: string;
	readonly content: string;
}

/**
 * A compiled module descriptor emitted by `celox-gen-ts`.
 * The type parameter `Ports` carries the generated port interface
 * (e.g. `AdderPorts`) so that `Simulator.create(Adder)` returns
 * a correctly-typed DUT accessor.
 */
export interface ModuleDefinition<Ports = Record<string, unknown>> {
	readonly __celox_module: true;
	readonly name: string;
	readonly sources: ReadonlyArray<SourceFile>;
	readonly ports: Record<string, PortInfo>;
	readonly events: string[];
	/** Absolute path to the Veryl project directory (set by Vite plugin). */
	readonly projectPath?: string;
	/** Default options baked in by the Vite plugin (e.g. via `?dse=` query). */
	readonly defaultOptions?: SimulatorOptions;
	/** Phantom field — never set at runtime. Carries the `Ports` type. */
	readonly __ports?: Ports;
}

/** Metadata for a single port / interface member. */
export interface PortInfo {
	readonly direction: "input" | "output" | "inout" | "internal";
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
	readonly direction: "input" | "output" | "inout" | "internal";
	/** The Veryl type kind (e.g. "clock", "reset_async_high", "logic"). */
	readonly typeKind?: string;
	/** For reset signals, the name of the associated clock (from FfDeclaration). */
	readonly associatedClock?: string;
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
	/** Full instance hierarchy (optional — present when NAPI provides it). */
	readonly hierarchy?: import("./napi-helpers.js").HierarchyNode;
	/** Analyzer warnings emitted during compilation. */
	readonly warnings?: string[];
}

// ---------------------------------------------------------------------------
// User-facing options
// ---------------------------------------------------------------------------

/** Per-pass optimizer control flags. All default to `true` when omitted. */
export interface OptimizeOptions {
	/** Store-load forwarding: propagates stored values to subsequent loads. */
	storeLoadForwarding?: boolean;
	/** Hoists loads shared across all branches to the branch entry. */
	hoistCommonBranchLoads?: boolean;
	/** Converts `(value >> shift) & mask` patterns into direct ranged loads. */
	bitExtractPeephole?: boolean;
	/** General block-level optimizations (dead block removal, merging). */
	optimizeBlocks?: boolean;
	/** Splits wide commit operations into narrower ones for efficiency. */
	splitWideCommits?: boolean;
	/** Sinks commit operations closer to their use site. */
	commitSinking?: boolean;
	/** Inlines values forwarded through commit operations. */
	inlineCommitForwarding?: boolean;
	/** Removes working-memory stores that are never read. */
	eliminateDeadWorkingStores?: boolean;
	/** Reorders instructions for better Cranelift code generation. */
	reschedule?: boolean;
	/** Merges consecutive narrow stores into wider Concat+Store operations. */
	coalesceStores?: boolean;
}

export interface SimulatorOptions {
	/** Enable 4-state (X) simulation. Default: false. */
	fourState?: boolean;
	/** Path to write VCD waveform output. */
	vcd?: string;
	/**
	 * Optimization level preset. Default: "O1".
	 * - "O0": No SIR optimizations (except TailCallSplit). Fast compilation.
	 * - "O1": All SIR optimizations enabled. Default.
	 * - "O2": O1 + dead store elimination (PreserveTopPorts).
	 */
	optLevel?: "O0" | "O1" | "O2";
	/**
	 * Per-pass overrides applied on top of `optLevel`.
	 * Each entry: "+sir:<pass_name>" to enable, "-sir:<pass_name>" to disable.
	 *
	 * Valid SIR passes: store_load_forwarding, hoist_common_branch_loads,
	 * bit_extract_peephole, optimize_blocks, split_wide_commits, commit_sinking,
	 * inline_commit_forwarding, eliminate_dead_working_stores, reschedule,
	 * coalesce_stores, gvn, concat_folding, xor_chain_folding, vectorize_concat,
	 * split_coalesced_stores, partial_forward, identity_store_bypass, tail_call_split
	 *
	 * @example ["-sir:reschedule", "+sir:gvn"]
	 */
	passOverrides?: string[];
	/**
	 * Shorthand to enable/disable all SIRT optimization passes.
	 * `true` = all on (O1), `false` = all off (O0).
	 * Overridden by `optLevel` or `optimizeOptions` if set.
	 * @deprecated Use `optLevel` instead.
	 */
	optimize?: boolean;
	/**
	 * Per-pass SIRT optimizer flags. Takes precedence over `optimize`.
	 * @deprecated Use `optLevel` + `passOverrides` instead.
	 */
	optimizeOptions?: OptimizeOptions;
	/** Cranelift backend optimization level. Default: "speed". */
	craneliftOptLevel?: "none" | "speed" | "speedAndSize";
	/**
	 * Register allocator algorithm for the Cranelift backend. Default: "backtracking".
	 * - "backtracking": Slower compilation but generates better code with fewer spills.
	 * - "singlePass": Much faster compilation but generates code with more register spills.
	 */
	regallocAlgorithm?: "backtracking" | "singlePass";
	/**
	 * Enable alias analysis in the Cranelift egraph optimization pass. Default: true.
	 * Only effective when `craneliftOptLevel` is not "none".
	 */
	enableAliasAnalysis?: boolean;
	/**
	 * Enable the Cranelift IR verifier. Default: true.
	 * Disabling saves compile time at the cost of less validation.
	 */
	enableVerifier?: boolean;
	/** False-loop declarations to ignore during compilation. */
	falseLoops?: LoopBreak[];
	/** True-loop declarations with convergence limits. */
	trueLoops?: TrueLoopSpec[];
	/** Clock polarity. Default: "posedge". */
	clockType?: "posedge" | "negedge";
	/** Reset type. Default: "async_low". */
	resetType?: "async_high" | "async_low" | "sync_high" | "sync_low";
	/** Additional Veryl source to append to the main source code. */
	extraSource?: string;
	/** Top-level module parameter overrides. */
	parameters?: ParamOverride[];
	/**
	 * Dead store elimination policy. Default: "off".
	 * When `optLevel` is "O2", defaults to "preserveTopPorts" unless explicitly set.
	 */
	deadStorePolicy?: "off" | "preserveTopPorts" | "preserveAllPorts";
}

/** A parameter override for a top-level module parameter. */
export interface ParamOverride {
	name: string;
	value: number | bigint;
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

/** Sentinel representing all-X (unknown). X = (v=1, m=1) per bit. */
export const X = Symbol.for("veryl:X");

/** Sentinel representing all-Z (high-impedance). Z = (v=0, m=1) per bit. */
export const Z = Symbol.for("veryl:Z");

/**
 * A 4-state value with explicit bit-level mask.
 *
 * Encoding per bit:
 * - `(v=0, m=0)` → 0
 * - `(v=1, m=0)` → 1
 * - `(v=0, m=1)` → Z (high-impedance)
 * - `(v=1, m=1)` → X (unknown)
 */
export interface FourStateValue {
	readonly __fourState: true;
	readonly value: bigint;
	readonly mask: bigint;
}

/** Values assignable to a writable 4-state signal. */
export type FourStateSignalValue =
	| bigint
	| typeof X
	| typeof Z
	| FourStateValue;

/**
 * Construct a 4-state value with explicit value and mask.
 *
 * Mask bits set to 1 with value=1 indicate X (unknown).
 * Mask bits set to 1 with value=0 indicate Z (high-impedance).
 */
export function FourState(
	value: number | bigint,
	mask: number | bigint,
): FourStateValue {
	return { __fourState: true, value: BigInt(value), mask: BigInt(mask) };
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
