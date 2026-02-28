import { describe, test, expect, afterEach } from "vitest";
import { Simulation } from "@celox-sim/celox";

interface CounterPorts {
  rst: number;
  en: number;
  readonly count: number;
}

describe("Counter", () => {
  let sim: Simulation | undefined;

  afterEach(() => {
    sim?.dispose();
    sim = undefined;
  });

  test("counts up on each clock edge when enabled", () => {
    sim = Simulation.fromProject<CounterPorts>(".", "Counter");

    // Add a clock with period 10 (toggle every 5 time units)
    sim.addClock("clk", { period: 10 });
    expect(sim.time()).toBe(0);

    // Assert reset
    sim.dut.rst = 1;
    sim.runUntil(20);

    // Release reset and enable counting
    sim.dut.rst = 0;
    sim.dut.en = 1;

    // Run for a while and verify count increments
    sim.runUntil(100);

    const count = sim.dut.count;
    expect(count).toBeGreaterThan(0);
    expect(sim.time()).toBe(100);
  });
});
