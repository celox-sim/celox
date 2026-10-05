/**
 * Saved simulation state.
 *
 * A checkpoint wraps the opaque state saved by the native or WASM handle.
 * It can be restored any number of times, into the simulator that created it
 * or into another one created from the same design.
 */

import type { NativeCheckpoint, NativeSimulationCheckpoint } from "./types.js";

/** Saved state of a `Simulator`, created by `Simulator.checkpoint()`. */
export class SimulatorCheckpoint {
	/** @internal */
	readonly _native: NativeCheckpoint;

	/** @internal */
	constructor(native: NativeCheckpoint) {
		this._native = native;
	}

	/** Size of the saved design state in bytes. */
	get stateSize(): number {
		return this._native.stateSize;
	}
}

/**
 * Saved state of a `Simulation`, created by `Simulation.checkpoint()`:
 * the design state together with simulation time, clocks and pending events.
 */
export class SimulationCheckpoint {
	/** @internal */
	readonly _native: NativeSimulationCheckpoint;

	/** @internal */
	constructor(native: NativeSimulationCheckpoint) {
		this._native = native;
	}

	/** Simulation time at which the checkpoint was taken. */
	get time(): number {
		return this._native.time;
	}

	/** Size of the saved design state in bytes. */
	get stateSize(): number {
		return this._native.stateSize;
	}
}
