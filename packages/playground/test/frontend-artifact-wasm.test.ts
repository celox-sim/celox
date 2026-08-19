/** End-to-end coverage for external frontend artifacts in the WASM addon. */
import { Simulator } from "@celox-sim/celox";
import { describe, expect, test } from "vitest";

const FRONTEND_ADDER_ARTIFACT = JSON.stringify({
  format_version: 1,
  module_name: "NetAdder",
  signals: [
    {
      id: 0,
      name: "a",
      direction: "Input",
      value_type: { width: 8, signed: false, four_state: false },
      initial: null,
    },
    {
      id: 1,
      name: "b",
      direction: "Input",
      value_type: { width: 8, signed: false, four_state: false },
      initial: null,
    },
    {
      id: 2,
      name: "y",
      direction: "Output",
      value_type: { width: 8, signed: false, four_state: false },
      initial: null,
    },
  ],
  expressions: [
    {
      id: 0,
      node: { Signal: { signal: 0, lsb: 0, width: 8 } },
      value_type: { width: 8, signed: false, four_state: false },
    },
    {
      id: 1,
      node: { Signal: { signal: 1, lsb: 0, width: 8 } },
      value_type: { width: 8, signed: false, four_state: false },
    },
    {
      id: 2,
      node: { Binary: { op: "Add", lhs: 0, rhs: 1 } },
      value_type: { width: 8, signed: false, four_state: false },
    },
  ],
  assignments: [
    {
      target: { signal: 2, lsb: 0, width: 8 },
      value: 2,
    },
  ],
  registers: [],
  port_order: [0, 1, 2],
});

const INITIALIZED_ARTIFACT = JSON.stringify({
  format_version: 1,
  module_name: "Initialized",
  signals: [
    {
      id: 0,
      name: "q",
      direction: "Output",
      value_type: { width: 8, signed: false, four_state: false },
      initial: {
        payload: [0xa5],
        mask: [],
        value_type: { width: 8, signed: false, four_state: false },
      },
    },
  ],
  expressions: [],
  assignments: [],
  registers: [],
  port_order: [0],
});

const isWasm =
  !!process.env.NAPI_RS_WASI_FLAVOR ||
  process.env.NAPI_RS_FORCE_WASI === "true" ||
  process.env.NAPI_RS_FORCE_WASI === "error";

describe.skipIf(!isWasm)("External frontend artifact (WASM bridge)", () => {
  test("constructs and executes an SDK artifact", () => {
    const sim = Simulator.fromFrontendArtifact<{
      a: bigint;
      b: bigint;
      readonly y: bigint;
    }>(FRONTEND_ADDER_ARTIFACT);

    sim.dut.a = 10n;
    sim.dut.b = 23n;
    expect(sim.dut.y).toBe(33n);
    sim.dispose();
  });

  test("applies artifact initial values to WASM memory", () => {
    const sim = Simulator.fromFrontendArtifact<{ readonly q: bigint }>(
      INITIALIZED_ARTIFACT,
    );

    expect(sim.dut.q).toBe(0xa5n);
    sim.dispose();
  });
});
