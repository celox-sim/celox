/**
 * Performance benchmarks — mirrors `crates/celox/benches/simulation.rs`
 * and `crates/celox/benches/overhead.rs`.
 *
 * Measures the same operations so JS and Rust numbers are directly comparable:
 *   1. Build (JIT compile)
 *   2. Single tick
 *   3. 1M ticks in a loop
 *   4. Simulator::tick vs Simulation::step overhead
 */

import { readFileSync } from "node:fs";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { bench, describe, afterAll } from "vitest";
import { Simulator } from "./simulator.js";
import { Simulation } from "./simulation.js";
import type { ModuleDefinition, SimulationTimeoutError } from "./types.js";
import {
  loadNativeAddon,
  createSimulatorBridge,
} from "./napi-helpers.js";

const __benchDir = dirname(fileURLToPath(import.meta.url));
const VERYL_STD = resolve(__benchDir, "../../../deps/veryl/crates/std/veryl/src");
function readVeryl(...parts: string[]): string {
  return readFileSync(resolve(VERYL_STD, ...parts), "utf8");
}

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
  rst: bigint;
  readonly cnt: { at(i: number): bigint; readonly length: number };
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
  sim.dut.rst = 1n;
  sim.tick();
  sim.dut.rst = 0n;
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
    sim.dut.rst = 0n;
    sim.tick();
    // biome-ignore lint: read to measure full testbench cycle
    sim.dut.rst;
  });

  bench(
    "testbench_tick_top_n1000_x1000000",
    () => {
      for (let i = 0; i < 1_000_000; i++) {
        sim.dut.rst = 0n;
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
  simArr.dut.rst = 1n;
  simArr.tick();
  simArr.dut.rst = 0n;
  simArr.tick();

  afterAll(() => {
    simArr.dispose();
  });

  bench("testbench_array_tick_top_n1000_x1", () => {
    simArr.dut.rst = 0n;
    simArr.tick();
    // biome-ignore lint: read array element to measure .at() overhead
    simArr.dut.cnt.at(0);
  });

  bench(
    "testbench_array_tick_top_n1000_x1000000",
    () => {
      for (let i = 0; i < 1_000_000; i++) {
        simArr.dut.rst = 0n;
        simArr.tick();
        // biome-ignore lint: read array element to measure .at() overhead
        simArr.dut.cnt.at(0);
      }
    },
    { iterations: 3, time: 0 },
  );
});

/**
 * Overhead comparison — mirrors `crates/celox/benches/overhead.rs`.
 *
 * Compares Simulator.tick() vs Simulation.step() to measure the
 * scheduling overhead of the time-based API.
 */
describe("overhead", () => {
  // Simulator.tick — same as Rust simulator_tick_x10000
  const simTick = Simulator.fromSource<TopPorts>(CODE, "Top");
  simTick.dut.rst = 1n;
  simTick.tick();
  simTick.dut.rst = 0n;
  simTick.tick();

  afterAll(() => {
    simTick.dispose();
  });

  bench(
    "simulator_tick_x10000",
    () => {
      for (let i = 0; i < 10_000; i++) {
        simTick.tick();
      }
    },
    { iterations: 3, time: 0 },
  );

  // Simulation.step — same as Rust simulation_step_x20000
  const simStep = Simulation.fromSource<TopPorts>(CODE, "Top");
  simStep.addClock("clk", { period: 10 });

  afterAll(() => {
    simStep.dispose();
  });

  bench(
    "simulation_step_x20000",
    () => {
      // 20000 steps = 10000 cycles (rising + falling)
      for (let i = 0; i < 20_000; i++) {
        simStep.step();
      }
    },
    { iterations: 3, time: 0 },
  );
});

/**
 * Simulation (time-based) benchmarks — mirrors the simulation describe
 * above but uses the Simulation API instead of Simulator.
 */
describe("simulation-time-based", () => {
  bench(
    "simulation_time_build_top_n1000",
    () => {
      const sim = Simulation.fromSource<TopPorts>(CODE, "Top");
      sim.dispose();
    },
    { iterations: 3, time: 0 },
  );

  const sim = Simulation.fromSource<TopPorts>(CODE, "Top");
  sim.addClock("clk", { period: 10 });

  afterAll(() => {
    sim.dispose();
  });

  bench("simulation_time_step_x1", () => {
    sim.step();
  });

  bench(
    "simulation_time_step_x1000000",
    () => {
      for (let i = 0; i < 1_000_000; i++) {
        sim.step();
      }
    },
    { iterations: 3, time: 0 },
  );

  bench(
    "simulation_time_runUntil_1000000",
    () => {
      const base = sim.time();
      sim.runUntil(base + 1_000_000);
    },
    { iterations: 3, time: 0 },
  );
});

/**
 * Phase 3b: Testbench helpers benchmarks.
 *
 * Compares waitForCycles vs manual step loop, and runUntil with/without
 * maxSteps guard to measure overhead.
 */
describe("testbench-helpers", () => {
  const COUNTER_CODE = `
    module Counter (
        clk: input clock,
        rst: input reset,
        en: input logic,
        count: output logic<8>,
    ) {
        var count_r: logic<8>;

        always_ff (clk, rst) {
            if_reset {
                count_r = 0;
            } else if en {
                count_r = count_r + 1;
            }
        }

        always_comb {
            count = count_r;
        }
    }
  `;

  interface CounterPorts {
    rst: bigint;
    en: bigint;
    readonly count: bigint;
  }

  // waitForCycles benchmark
  const simWait = Simulation.fromSource<CounterPorts>(COUNTER_CODE, "Counter");
  simWait.addClock("clk", { period: 10 });
  simWait.dut.rst = 1n;
  simWait.runUntil(20);
  simWait.dut.rst = 0n;
  simWait.dut.en = 1n;

  afterAll(() => {
    simWait.dispose();
  });

  bench(
    "waitForCycles_x1000",
    () => {
      simWait.waitForCycles("clk", 1000);
    },
    { iterations: 3, time: 0 },
  );

  bench(
    "manual_step_loop_x2000",
    () => {
      for (let i = 0; i < 2000; i++) {
        simWait.step();
      }
    },
    { iterations: 3, time: 0 },
  );

  // runUntil: fast Rust path vs guarded TS path
  const simRun = Simulation.fromSource<CounterPorts>(COUNTER_CODE, "Counter");
  simRun.addClock("clk", { period: 10 });
  simRun.dut.rst = 1n;
  simRun.runUntil(20);
  simRun.dut.rst = 0n;
  simRun.dut.en = 1n;

  afterAll(() => {
    simRun.dispose();
  });

  bench(
    "runUntil_fast_path_100000",
    () => {
      const base = simRun.time();
      simRun.runUntil(base + 100_000);
    },
    { iterations: 3, time: 0 },
  );

  bench(
    "runUntil_guarded_100000",
    () => {
      const base = simRun.time();
      simRun.runUntil(base + 100_000, { maxSteps: 1_000_000 });
    },
    { iterations: 3, time: 0 },
  );
});

/**
 * Phase 3c: Optimize flag benchmarks.
 *
 * Compares build time and tick performance with and without optimization.
 */
describe("optimize-flag", () => {
  bench(
    "build_without_optimize",
    () => {
      const sim = Simulator.fromSource<TopPorts>(CODE, "Top");
      sim.dispose();
    },
    { iterations: 3, time: 0 },
  );

  bench(
    "build_with_optimize",
    () => {
      const sim = Simulator.fromSource<TopPorts>(CODE, "Top", {
        optimize: true,
      });
      sim.dispose();
    },
    { iterations: 3, time: 0 },
  );

  const simNoOpt = Simulator.fromSource<TopPorts>(CODE, "Top");
  simNoOpt.dut.rst = 1n;
  simNoOpt.tick();
  simNoOpt.dut.rst = 0n;
  simNoOpt.tick();

  const simOpt = Simulator.fromSource<TopPorts>(CODE, "Top", {
    optimize: true,
  });
  simOpt.dut.rst = 1n;
  simOpt.tick();
  simOpt.dut.rst = 0n;
  simOpt.tick();

  afterAll(() => {
    simNoOpt.dispose();
    simOpt.dispose();
  });

  bench(
    "tick_x10000_without_optimize",
    () => {
      for (let i = 0; i < 10_000; i++) {
        simNoOpt.tick();
      }
    },
    { iterations: 3, time: 0 },
  );

  bench(
    "tick_x10000_with_optimize",
    () => {
      for (let i = 0; i < 10_000; i++) {
        simOpt.tick();
      }
    },
    { iterations: 3, time: 0 },
  );
});

// ────────────────────────────────────────────────────────────
// Stdlib benchmarks — mirrors crates/celox/benches/simulation.rs
// ────────────────────────────────────────────────────────────

// --- Linear SEC (P=6): Hamming encoder/decoder, combinational ---

const LINEAR_SEC_SRC =
  readVeryl("coding/linear_sec_encoder.veryl") +
  readVeryl("coding/linear_sec_decoder.veryl") +
  `
module Top #(
    param P: u32 = 6,
    const K: u32 = (1 << P) - 1,
    const N: u32 = K - P,
)(
    i_word     : input  logic<N>,
    o_codeword : output logic<K>,
    o_word     : output logic<N>,
    o_corrected: output logic,
) {
    inst u_enc: linear_sec_encoder #(P: P) (i_word, o_codeword);
    inst u_dec: linear_sec_decoder #(P: P) (
        i_codeword: o_codeword,
        o_word,
        o_corrected,
    );
}
`;

interface LinearSecPorts {
  i_word: bigint;
  readonly o_codeword: bigint;
  readonly o_word: bigint;
  readonly o_corrected: bigint;
}

describe("stdlib-linear-sec", () => {
  bench(
    "simulation_build_linear_sec_p6",
    () => {
      const sim = Simulator.fromSource<LinearSecPorts>(LINEAR_SEC_SRC, "Top");
      sim.dispose();
    },
    { iterations: 3, time: 0 },
  );

  const sim = Simulator.fromSource<LinearSecPorts>(LINEAR_SEC_SRC, "Top");

  afterAll(() => {
    sim.dispose();
  });

  bench("simulation_eval_linear_sec_p6_x1", () => {
    sim.dut.i_word = BigInt(Math.floor(Math.random() * 0x1ffffffffffffff));
    // biome-ignore lint: read to measure eval
    sim.dut.o_word;
  });

  bench(
    "simulation_eval_linear_sec_p6_x1000000",
    () => {
      for (let i = 0; i < 1_000_000; i++) {
        sim.dut.i_word = BigInt(i);
        // biome-ignore lint: read to measure eval
        sim.dut.o_word;
      }
    },
    { iterations: 3, time: 0 },
  );

  bench(
    "testbench_eval_linear_sec_p6_x1000000",
    () => {
      for (let i = 0; i < 1_000_000; i++) {
        sim.dut.i_word = BigInt(i);
        // biome-ignore lint: read corrected flag
        sim.dut.o_corrected;
      }
    },
    { iterations: 3, time: 0 },
  );
});

// --- Countones (W=64): recursive combinational popcount tree ---

const COUNTONES_SRC =
  readVeryl("countones/countones.veryl") +
  `
module Top (
    i_data: input  logic<64>,
    o_ones: output logic<7>,
) {
    inst u: countones #(W: 64) (i_data, o_ones);
}
`;

interface CountonesPorts {
  i_data: bigint;
  readonly o_ones: bigint;
}

describe("stdlib-countones", () => {
  bench(
    "simulation_build_countones_w64",
    () => {
      const sim = Simulator.fromSource<CountonesPorts>(COUNTONES_SRC, "Top");
      sim.dispose();
    },
    { iterations: 3, time: 0 },
  );

  const sim = Simulator.fromSource<CountonesPorts>(COUNTONES_SRC, "Top");

  afterAll(() => {
    sim.dispose();
  });

  bench("simulation_eval_countones_w64_x1", () => {
    sim.dut.i_data = BigInt(Math.floor(Math.random() * Number.MAX_SAFE_INTEGER));
    // biome-ignore lint: read to measure eval
    sim.dut.o_ones;
  });

  bench(
    "simulation_eval_countones_w64_x1000000",
    () => {
      for (let i = 0; i < 1_000_000; i++) {
        sim.dut.i_data = BigInt(i);
        // biome-ignore lint: read to measure eval
        sim.dut.o_ones;
      }
    },
    { iterations: 3, time: 0 },
  );
});

// --- std::counter (WIDTH=32): sequential up-counter ---

const STD_COUNTER_SRC =
  readVeryl("counter/counter.veryl") +
  `
module Top (
    clk    : input  clock,
    rst    : input  reset,
    i_up   : input  logic,
    o_count: output logic<32>,
) {
    inst u: counter #(WIDTH: 32) (
        i_clk       : clk,
        i_rst       : rst,
        i_clear     : 1'b0,
        i_set       : 1'b0,
        i_set_value : 32'b0,
        i_up,
        i_down      : 1'b0,
        o_count,
        o_count_next: _,
        o_wrap_around: _,
    );
}
`;

interface StdCounterPorts {
  rst: bigint;
  i_up: bigint;
  readonly o_count: bigint;
}

describe("stdlib-counter", () => {
  bench(
    "simulation_build_std_counter_w32",
    () => {
      const sim = Simulator.fromSource<StdCounterPorts>(STD_COUNTER_SRC, "Top");
      sim.dispose();
    },
    { iterations: 3, time: 0 },
  );

  const sim = Simulator.fromSource<StdCounterPorts>(STD_COUNTER_SRC, "Top");
  sim.dut.rst = 1n;
  sim.dut.i_up = 0n;
  sim.tick();
  sim.dut.rst = 0n;
  sim.dut.i_up = 1n;
  sim.tick();

  afterAll(() => {
    sim.dispose();
  });

  bench("simulation_tick_std_counter_w32_x1", () => {
    sim.tick();
  });

  bench(
    "simulation_tick_std_counter_w32_x1000000",
    () => {
      for (let i = 0; i < 1_000_000; i++) {
        sim.tick();
      }
    },
    { iterations: 3, time: 0 },
  );

  bench(
    "testbench_tick_std_counter_w32_x1000000",
    () => {
      for (let i = 0; i < 1_000_000; i++) {
        sim.tick();
        // biome-ignore lint: read to measure testbench cycle
        sim.dut.o_count;
      }
    },
    { iterations: 3, time: 0 },
  );
});

// --- std::gray_counter (WIDTH=32): Gray-encoded sequential counter ---

const GRAY_COUNTER_SRC =
  readVeryl("counter/counter.veryl") +
  readVeryl("gray/gray_encoder.veryl") +
  readVeryl("gray/gray_counter.veryl") +
  `
module Top (
    clk    : input  clock,
    rst    : input  reset,
    i_up   : input  logic,
    o_count: output logic<32>,
) {
    inst u: gray_counter #(WIDTH: 32) (
        i_clk       : clk,
        i_rst       : rst,
        i_clear     : 1'b0,
        i_set       : 1'b0,
        i_set_value : 32'b0,
        i_up,
        i_down      : 1'b0,
        o_count,
        o_count_next: _,
        o_wrap_around: _,
    );
}
`;

interface GrayCounterPorts {
  rst: bigint;
  i_up: bigint;
  readonly o_count: bigint;
}

describe("stdlib-gray-counter", () => {
  bench(
    "simulation_build_gray_counter_w32",
    () => {
      const sim = Simulator.fromSource<GrayCounterPorts>(GRAY_COUNTER_SRC, "Top");
      sim.dispose();
    },
    { iterations: 3, time: 0 },
  );

  const sim = Simulator.fromSource<GrayCounterPorts>(GRAY_COUNTER_SRC, "Top");
  sim.dut.rst = 1n;
  sim.dut.i_up = 0n;
  sim.tick();
  sim.dut.rst = 0n;
  sim.dut.i_up = 1n;
  sim.tick();

  afterAll(() => {
    sim.dispose();
  });

  bench("simulation_tick_gray_counter_w32_x1", () => {
    sim.tick();
  });

  bench(
    "simulation_tick_gray_counter_w32_x1000000",
    () => {
      for (let i = 0; i < 1_000_000; i++) {
        sim.tick();
      }
    },
    { iterations: 3, time: 0 },
  );

  bench(
    "testbench_tick_gray_counter_w32_x1000000",
    () => {
      for (let i = 0; i < 1_000_000; i++) {
        sim.tick();
        // biome-ignore lint: read to measure testbench cycle
        sim.dut.o_count;
      }
    },
    { iterations: 3, time: 0 },
  );
});
