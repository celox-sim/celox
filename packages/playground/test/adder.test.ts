import { describe, test, expect } from "vitest";
import { Simulator } from "@celox-sim/celox";
import { Adder } from "../src/Adder.veryl";

describe("Adder", () => {
  test("adds two numbers", () => {
    const sim = Simulator.create(Adder);

    sim.dut.a = 100n;
    sim.dut.b = 200n;
    sim.tick();
    expect(sim.dut.sum).toBe(300n);

    sim.dispose();
  });

  test("handles overflow into 17th bit", () => {
    const sim = Simulator.create(Adder);

    sim.dut.a = 0xffffn;
    sim.dut.b = 1n;
    sim.tick();
    expect(sim.dut.sum).toBe(0x10000n);

    sim.dispose();
  });

  test("multiple computations", () => {
    const sim = Simulator.create(Adder);

    for (const [a, b, expected] of [
      [0n, 0n, 0n],
      [1n, 1n, 2n],
      [255n, 255n, 510n],
      [1000n, 2000n, 3000n],
    ] as const) {
      sim.dut.a = a;
      sim.dut.b = b;
      sim.tick();
      expect(sim.dut.sum).toBe(expected);
    }

    sim.dispose();
  });
});
