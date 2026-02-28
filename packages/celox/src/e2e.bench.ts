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
  readonly cnt: { readonly [i: number]: number; readonly length: number };
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
});
