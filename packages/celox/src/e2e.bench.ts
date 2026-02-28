/**
 * Testbench performance benchmarks.
 *
 * Mirrors the Rust `crates/celox/benches/simulation.rs` structure
 * but measures the full TypeScript testbench pipeline:
 *   Veryl source → NAPI JIT → SharedArrayBuffer → DUT proxy → tick/read/write
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

describe("testbench", () => {
  bench("testbench_build_top_n1000", () => {
    const sim = Simulator.fromSource<TopPorts>(CODE, "Top");
    sim.dispose();
  });

  const sim = Simulator.fromSource<TopPorts>(CODE, "Top");

  // Reset sequence
  sim.dut.rst = 1;
  sim.tick();
  sim.dut.rst = 0;

  afterAll(() => {
    sim.dispose();
  });

  bench("testbench_tick_top_n1000_x1", () => {
    sim.dut.rst = 0;
    sim.tick();
    // biome-ignore lint: read output to measure full testbench cycle
    sim.dut.cnt[0];
  });

  bench("testbench_tick_top_n1000_x1000000", () => {
    for (let i = 0; i < 1_000_000; i++) {
      sim.dut.rst = 0;
      sim.tick();
      // biome-ignore lint: read output to measure full testbench cycle
      sim.dut.cnt[0];
    }
  });
});
