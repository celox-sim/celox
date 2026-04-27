/**
 * Test that the WASM simulator bridge works for event-based simulation.
 *
 * This test uses Simulator.fromSource() with the WASM addon to verify
 * that the WASM compilation and execution pipeline works end-to-end.
 * It does NOT depend on the Vite plugin or genTs().
 */
import { describe, test, expect } from "vitest";
import { Simulator } from "@celox-sim/celox";

const ADDER_SOURCE = `\
module Adder (
    clk: input clock,
    rst: input reset,
    a: input logic<16>,
    b: input logic<16>,
    sum: output logic<17>,
) {
    always_comb {
        sum = a + b;
    }
}
`;

// Only run when NAPI_RS_FORCE_WASI is set (i.e., testing the WASM path).
const isWasm = !!process.env.NAPI_RS_FORCE_WASI;

describe.skipIf(!isWasm)("Adder (WASM bridge)", () => {
  test("adds two numbers via WASM", () => {
    const sim = Simulator.fromSource(ADDER_SOURCE, "Adder");

    sim.dut.a = 100n;
    sim.dut.b = 200n;
    expect(sim.dut.sum).toBe(300n);

    sim.dispose();
  });

  test("handles overflow into 17th bit via WASM", () => {
    const sim = Simulator.fromSource(ADDER_SOURCE, "Adder");

    sim.dut.a = 0xffffn;
    sim.dut.b = 1n;
    expect(sim.dut.sum).toBe(0x10000n);

    sim.dispose();
  });

  test("multiple computations via WASM", () => {
    const sim = Simulator.fromSource(ADDER_SOURCE, "Adder");

    for (const [a, b, expected] of [
      [0n, 0n, 0n],
      [1n, 1n, 2n],
      [255n, 255n, 510n],
      [1000n, 2000n, 3000n],
    ] as const) {
      sim.dut.a = a;
      sim.dut.b = b;
      expect(sim.dut.sum).toBe(expected);
    }

    sim.dispose();
  });
});
