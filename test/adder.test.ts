import { describe, test, expect, afterEach } from "vitest";
import { Simulator } from "@celox-sim/celox";

interface AdderPorts {
  rst: number;
  a: number;
  b: number;
  readonly sum: number;
}

describe("Adder", () => {
  let sim: Simulator | undefined;

  afterEach(() => {
    sim?.dispose();
    sim = undefined;
  });

  test("adds two numbers", () => {
    sim = Simulator.fromProject<AdderPorts>(".", "Adder");

    sim.dut.a = 100;
    sim.dut.b = 200;
    sim.tick();
    expect(sim.dut.sum).toBe(300);
  });

  test("handles overflow into 17th bit", () => {
    sim = Simulator.fromProject<AdderPorts>(".", "Adder");

    sim.dut.a = 0xffff;
    sim.dut.b = 1;
    sim.tick();
    expect(sim.dut.sum).toBe(0x10000);
  });
});
