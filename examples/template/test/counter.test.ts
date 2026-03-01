import { describe, test, expect } from "vitest";
import { Simulation } from "@celox-sim/celox";
import { Counter } from "../src/Counter.veryl";

describe("Counter", () => {
  test("counts up on each clock edge when enabled", () => {
    const sim = Simulation.create(Counter);

    // Add a clock with period 10 (toggle every 5 time units)
    sim.addClock("clk", { period: 10 });
    expect(sim.time()).toBe(0);

    // Assert reset (active-low: 0 = asserted)
    sim.dut.rst = 0n;
    sim.runUntil(20);

    // Release reset and enable counting
    sim.dut.rst = 1n;
    sim.dut.en = 1n;

    // Run for a while and verify count increments
    sim.runUntil(100);

    const count = sim.dut.count;
    expect(count).toBeGreaterThan(0n);
    expect(sim.time()).toBe(100);

    sim.dispose();
  });
});
