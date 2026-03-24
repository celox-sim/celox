import { describe, test, expect } from "vitest";
import { Simulation } from "@celox-sim/celox";
import { Counter } from "../src/Counter.veryl";

// Time-based Simulation is not supported in WASM mode (no NativeSimulationHandle).
// Skip these tests when running with NAPI_RS_FORCE_WASI.
const isWasm = !!process.env.NAPI_RS_FORCE_WASI;

describe.skipIf(isWasm)("Counter", () => {
  test("counts up on each clock edge when enabled", () => {
    const sim = Simulation.create(Counter);

    sim.addClock("clk", { period: 10 });

    // Assert reset
    sim.dut.rst = 0n;
    sim.runUntil(20);
    expect(sim.dut.count).toBe(0n);

    // Release reset, enable counting
    sim.dut.rst = 1n;
    sim.dut.en = 1n;

    sim.runUntil(100);
    expect(sim.dut.count).toBeGreaterThan(0n);

    sim.dispose();
  });

  test("stays at zero when disabled", () => {
    const sim = Simulation.create(Counter);

    sim.addClock("clk", { period: 10 });

    // Reset
    sim.dut.rst = 0n;
    sim.runUntil(20);

    // Release reset but keep en=0
    sim.dut.rst = 1n;
    sim.dut.en = 0n;

    sim.runUntil(100);
    expect(sim.dut.count).toBe(0n);

    sim.dispose();
  });

  test("counts exactly N cycles", () => {
    const sim = Simulation.create(Counter);

    sim.addClock("clk", { period: 10 });

    // Reset
    sim.dut.rst = 0n;
    sim.runUntil(20);

    // Enable
    sim.dut.rst = 1n;
    sim.dut.en = 1n;

    // Run for exactly 5 clock cycles (period=10, so 50 time units)
    sim.runUntil(70); // 20 + 50 = 70

    const count = sim.dut.count;
    expect(count).toBe(5n);

    sim.dispose();
  });
});
