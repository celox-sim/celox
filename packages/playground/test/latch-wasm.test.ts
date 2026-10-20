import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Simulator } from "@celox-sim/celox";
import { describe, expect, test } from "vitest";

const source = `
module Latch(en: input logic, d: input logic<8>, q: output logic<8>) {
    always_comb { if en { q = d; } }
}
`;

const isWasm =
  !!process.env.NAPI_RS_WASI_FLAVOR ||
  process.env.NAPI_RS_FORCE_WASI === "true" ||
  process.env.NAPI_RS_FORCE_WASI === "error";

describe.skipIf(!isWasm)("Inferred latches (WASM bridge)", () => {
  test("rejects by default and preserves latch state with opt-in", () => {
    expect(() => Simulator.fromSource(source, "Latch")).toThrow();
    const sim = Simulator.fromSource<{
      en: bigint;
      d: bigint;
      readonly q: bigint;
    }>(source, "Latch", { allowInferredLatches: true });
    try {
      sim.dut.en = 1n;
      sim.dut.d = 7n;
      expect(sim.dut.q).toBe(7n);
      sim.dut.d = 13n;
      expect(sim.dut.q).toBe(13n);
      sim.dut.en = 0n;
      sim.dut.d = 99n;
      expect(sim.dut.q).toBe(13n);
      sim.dut.en = 1n;
      expect(sim.dut.q).toBe(99n);
    } finally {
      sim.dispose();
    }
  });
  test("uses project TOML diagnostics through the WASM compiler", () => {
    const project = mkdtempSync(join(tmpdir(), "celox-wasm-latch-"));
    try {
      mkdirSync(join(project, "src"));
      // The upstream standard-library cache requires host directory discovery.
      // This standalone WASI fixture has no standard-library dependencies.
      writeFileSync(join(project, "Veryl.toml"), '[project]\nname = "latch_policy"\nversion = "0.1.0"\n[build]\nexclude_std = true\n');
      writeFileSync(join(project, "src/latch.veryl"), source);
      for (const level of ["allow", "warn", "error"]) {
        writeFileSync(join(project, "celox.toml"), `[diagnostics]\ninferred_latch = "${level}"\n`);
        // Explicit project settings take precedence over the compatibility flag.
        const create = () => Simulator.fromProject<{ en: bigint; d: bigint; readonly q: bigint }>(project, "Latch", { allowInferredLatches: true });
        if (level === "error") { expect(create).toThrow(/inferred_latch/); continue; }
        const sim = create();
        try {
          expect(sim.warnings).toHaveLength(level === "warn" ? 1 : 0);
          if (level === "warn") expect(sim.warnings[0]).toContain("inferred_latch");
          sim.dut.en = 1n; sim.dut.d = 7n;
          expect(sim.dut.q).toBe(7n);
          sim.dut.en = 0n; sim.dut.d = 99n;
          expect(sim.dut.q).toBe(7n);
        } finally { sim.dispose(); }
      }
    } finally { rmSync(project, { recursive: true, force: true }); }
  });

});
