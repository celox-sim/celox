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

import type { SignalLayout } from "./types.js";
import { readFourState } from "./dut.js";

// ---------------------------------------------------------------------------
// Matcher declarations (augment vitest's Assertion interface)
// ---------------------------------------------------------------------------

declare module "vitest" {
  interface Assertion {
    /** Assert that the value has any X bits (mask !== 0). */
    toBeX(): void;
    /** Assert that the value is all-X (mask === all-ones). */
    toBeAllX(): void;
    /** Assert that the value has no X bits (mask === 0). */
    toBeNotX(): void;
  }

  interface AsymmetricMatchersContaining {
    toBeX(): void;
    toBeAllX(): void;
    toBeNotX(): void;
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

function getMask(received: unknown): bigint {
  if (!isFourStateRef(received)) {
    throw new TypeError(
      "toBeX/toBeAllX/toBeNotX matchers require a FourStateRef. " +
        "Use sim.fourStateRef(name) to get one.",
    );
  }
  const [, mask] = readFourState(received.buffer, received.layout);
  return mask;
}

const customMatchers = {
  toBeX(received: unknown) {
    const mask = getMask(received);
    const pass = mask !== 0n;
    return {
      pass,
      message: () =>
        pass
          ? `expected signal NOT to have X bits, but mask = ${mask}`
          : `expected signal to have X bits, but mask = 0`,
    };
  },

  toBeAllX(received: unknown) {
    if (!isFourStateRef(received)) {
      throw new TypeError("toBeAllX requires a FourStateRef");
    }
    const [, mask] = readFourState(received.buffer, received.layout);
    const width = received.layout.width;
    const allOnes = (1n << BigInt(width)) - 1n;
    const pass = mask === allOnes;
    return {
      pass,
      message: () =>
        pass
          ? `expected signal NOT to be all-X`
          : `expected signal to be all-X, but mask = ${mask}`,
    };
  },

  toBeNotX(received: unknown) {
    const mask = getMask(received);
    const pass = mask === 0n;
    return {
      pass,
      message: () =>
        pass
          ? `expected signal to have X bits, but mask = 0`
          : `expected signal NOT to have X bits, but mask = ${mask}`,
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
