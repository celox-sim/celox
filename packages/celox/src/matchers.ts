/**
 * vitest custom matchers for Veryl simulation.
 *
 * Usage:
 *   import { setupMatchers } from "@celox-sim/celox/matchers";
 *   setupMatchers();
 *
 *   expect(dut.y).toBeX();
 *   expect(dut.y).not.toBeZ();
 */

import { readFourState } from "./dut.js";
import type { SignalLayout } from "./types.js";

// ---------------------------------------------------------------------------
// Matcher declarations (augment vitest's Assertion interface)
// ---------------------------------------------------------------------------

declare module "vitest" {
	interface Assertion {
		/** Assert that the value has any X/Z bits (mask !== 0). */
		toBeX(): void;
		/** Assert that the value is all-X (v=1,m=1 for all bits). */
		toBeAllX(): void;
		/** Assert that the value has no X/Z bits (mask === 0). */
		toBeNotX(): void;
		/** Assert that the value has any Z bits (mask=1,value=0 somewhere). */
		toBeZ(): void;
		/** Assert that the value is all-Z (v=0,m=1 for all bits). */
		toBeAllZ(): void;
	}

	interface AsymmetricMatchersContaining {
		toBeX(): void;
		toBeAllX(): void;
		toBeNotX(): void;
		toBeZ(): void;
		toBeAllZ(): void;
	}
}

// ---------------------------------------------------------------------------
// 4-state inspection context
// ---------------------------------------------------------------------------

/**
 * A wrapper that carries the SharedArrayBuffer + SignalLayout alongside
 * the value, so matchers can inspect the raw 4-state representation.
 *
 * Usage:
 *   const ref = sim.fourStateRef("y");
 *   expect(ref).toBeX();
 */
export interface FourStateRef {
	readonly __fourStateRef: true;
	readonly buffer: SharedArrayBuffer;
	readonly layout: SignalLayout;
}

export function fourStateRef(
	buffer: SharedArrayBuffer,
	layout: SignalLayout,
): FourStateRef {
	return { __fourStateRef: true, buffer, layout };
}

function isFourStateRef(v: unknown): v is FourStateRef {
	return (
		typeof v === "object" &&
		v !== null &&
		(v as FourStateRef).__fourStateRef === true
	);
}

// ---------------------------------------------------------------------------
// Matcher implementations
// ---------------------------------------------------------------------------

function getValueAndMask(received: unknown): [bigint, bigint] {
	if (!isFourStateRef(received)) {
		throw new TypeError(
			"4-state matchers require a FourStateRef. " +
				"Use sim.fourStateRef(name) to get one.",
		);
	}
	return readFourState(received.buffer, received.layout);
}

const customMatchers = {
	toBeX(received: unknown) {
		const [, mask] = getValueAndMask(received);
		const pass = mask !== 0n;
		return {
			pass,
			message: () =>
				pass
					? `expected signal NOT to have X/Z bits, but mask = ${mask}`
					: `expected signal to have X/Z bits, but mask = 0`,
		};
	},

	toBeAllX(received: unknown) {
		if (!isFourStateRef(received)) {
			throw new TypeError("toBeAllX requires a FourStateRef");
		}
		const [value, mask] = readFourState(received.buffer, received.layout);
		const width = received.layout.width;
		const allOnes = (1n << BigInt(width)) - 1n;
		const pass = mask === allOnes && value === allOnes;
		return {
			pass,
			message: () =>
				pass
					? `expected signal NOT to be all-X`
					: `expected signal to be all-X (v=all-1s, m=all-1s), but v=${value}, m=${mask}`,
		};
	},

	toBeNotX(received: unknown) {
		const [, mask] = getValueAndMask(received);
		const pass = mask === 0n;
		return {
			pass,
			message: () =>
				pass
					? `expected signal to have X/Z bits, but mask = 0`
					: `expected signal NOT to have X/Z bits, but mask = ${mask}`,
		};
	},

	toBeZ(received: unknown) {
		const [value, mask] = getValueAndMask(received);
		// Z bits: mask=1 AND value=0
		const zBits = mask & ~value;
		const pass = zBits !== 0n;
		return {
			pass,
			message: () =>
				pass
					? `expected signal NOT to have Z bits, but found Z`
					: `expected signal to have Z bits, but none found (v=${value}, m=${mask})`,
		};
	},

	toBeAllZ(received: unknown) {
		if (!isFourStateRef(received)) {
			throw new TypeError("toBeAllZ requires a FourStateRef");
		}
		const [value, mask] = readFourState(received.buffer, received.layout);
		const width = received.layout.width;
		const allOnes = (1n << BigInt(width)) - 1n;
		const pass = mask === allOnes && value === 0n;
		return {
			pass,
			message: () =>
				pass
					? `expected signal NOT to be all-Z`
					: `expected signal to be all-Z (v=0, m=all-1s), but v=${value}, m=${mask}`,
		};
	},
};

// ---------------------------------------------------------------------------
// Setup
// ---------------------------------------------------------------------------

/**
 * Register custom matchers with vitest.
 * Call once in a setup file or at the top of your test:
 *
 * ```ts
 * import { setupMatchers } from "@celox-sim/celox/matchers";
 * setupMatchers();
 * ```
 */
export function setupMatchers(): void {
	// Dynamic import to keep vitest as an optional peer dependency
	try {
		// eslint-disable-next-line @typescript-eslint/no-require-imports
		const { expect } = require("vitest");
		expect.extend(customMatchers);
	} catch {
		throw new Error(
			"vitest is required for setupMatchers(). Install it as a dev dependency.",
		);
	}
}
