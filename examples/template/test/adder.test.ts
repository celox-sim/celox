import { describe, test, expect } from "vitest";
import { Simulator } from "@celox-sim/celox";
import { Adder } from "../src/Adder.veryl";

describe("Adder", () => {
  test("adds two numbers", () => {
    const sim = Simulator.create(Adder);

    sim.dut.a = 100;
    sim.dut.b = 200;
    sim.tick();
    expect(sim.dut.sum).toBe(300);

    sim.dispose();
  });

  test("handles overflow into 17th bit", () => {
    const sim = Simulator.create(Adder);

    sim.dut.a = 0xffff;
    sim.dut.b = 1;
    sim.tick();
    expect(sim.dut.sum).toBe(0x10000);

    sim.dispose();
  });
});
