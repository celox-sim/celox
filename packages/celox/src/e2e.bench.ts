/**
 * Performance benchmarks — mirrors `crates/celox/benches/simulation.rs`.
 *
 * Measures the same operations so JS and Rust numbers are directly comparable:
 *   1. Build (JIT compile)
 *   2. Single tick
 *   3. 1M ticks in a loop
 */

import { bench, describe, afterAll } from "vitest";
import { Simulator } from "./simulator.js";
import type { ModuleDefinition } from "./types.js";
import {
  loadNativeAddon,
  createSimulatorBridge,
} from "./napi-helpers.js";

const CODE = `
    module Top #(
        param N: u32 = 1000,
    )(
        clk: input clock,
        rst: input reset,
        cnt: output logic<32>[N],
    ) {
        for i in 0..N: g {
            always_ff (clk, rst) {
                if_reset {
                    cnt[i] = 0;
                } else {
                    cnt[i] += 1;
                }
            }
        }
    }
`;

interface TopPorts {
  rst: number;
  readonly cnt: { at(i: number): number; readonly length: number };
}

describe("simulation", () => {
  bench(
    "simulation_build_top_n1000",
    () => {
      const sim = Simulator.fromSource<TopPorts>(CODE, "Top");
      sim.dispose();
    },
    { iterations: 3, time: 0 },
  );

  const sim = Simulator.fromSource<TopPorts>(CODE, "Top");

  // Reset sequence
  sim.dut.rst = 1;
  sim.tick();
  sim.dut.rst = 0;
  sim.tick();

  afterAll(() => {
    sim.dispose();
  });

  bench("simulation_tick_top_n1000_x1", () => {
    sim.tick();
  });

  bench(
    "simulation_tick_top_n1000_x1000000",
    () => {
      for (let i = 0; i < 1_000_000; i++) {
        sim.tick();
      }
    },
    { iterations: 3, time: 0 },
  );

  // Testbench pattern: write input + tick + read back
  bench("testbench_tick_top_n1000_x1", () => {
    sim.dut.rst = 0;
    sim.tick();
    // biome-ignore lint: read to measure full testbench cycle
    sim.dut.rst;
  });

  bench(
    "testbench_tick_top_n1000_x1000000",
    () => {
      for (let i = 0; i < 1_000_000; i++) {
        sim.dut.rst = 0;
        sim.tick();
        // biome-ignore lint: read to measure full testbench cycle
        sim.dut.rst;
      }
    },
    { iterations: 3, time: 0 },
  );

  // Array access via .at() — use ModuleDefinition with arrayDims
  const addon = loadNativeAddon();
  const TopModule: ModuleDefinition<TopPorts> = {
    __celox_module: true,
    name: "Top",
    source: CODE,
    ports: {
      clk: { direction: "input", type: "clock", width: 1 },
      rst: { direction: "input", type: "reset", width: 1 },
      cnt: { direction: "output", type: "logic", width: 32, arrayDims: [1000] },
    },
    events: ["clk"],
  };
  const simArr = Simulator.create<TopPorts>(TopModule, {
    __nativeCreate: createSimulatorBridge(addon),
  });
  simArr.dut.rst = 1;
  simArr.tick();
  simArr.dut.rst = 0;
  simArr.tick();

  afterAll(() => {
    simArr.dispose();
  });

  bench("testbench_array_tick_top_n1000_x1", () => {
    simArr.dut.rst = 0;
    simArr.tick();
    // biome-ignore lint: read array element to measure .at() overhead
    simArr.dut.cnt.at(0);
  });

  bench(
    "testbench_array_tick_top_n1000_x1000000",
    () => {
      for (let i = 0; i < 1_000_000; i++) {
        simArr.dut.rst = 0;
        simArr.tick();
        // biome-ignore lint: read array element to measure .at() overhead
        simArr.dut.cnt.at(0);
      }
    },
    { iterations: 3, time: 0 },
  );
});
