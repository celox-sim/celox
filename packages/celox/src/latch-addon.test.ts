import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, test } from "vitest";
import { Simulator } from "./simulator.js";
import { runTest, runTestFromProject } from "./testbench.js";

const LATCH = `
module Latch(en: input logic, d: input logic<8>, q: output logic<8>) {
    always_comb { if en { q = d; } }
}
`;

describe("inferred latch policy through the native addon", () => {
	test("requires opt-in even after a permissive compilation is cached", () => {
		for (const tier of [false, true]) {
			const sim = Simulator.fromSource<{
				en: bigint;
				d: bigint;
				readonly q: bigint;
			}>(LATCH, "Latch", { allowInferredLatches: true, tier });
			try {
				sim.dut.en = 1n;
				sim.dut.d = 7n;
				expect(sim.dut.q).toBe(7n);
				sim.dut.d = 13n;
				expect(sim.dut.q).toBe(13n);
				sim.dut.en = 0n;
				sim.dut.d = 99n;
				expect(sim.dut.q).toBe(13n);
				sim.dut.d = 55n;
				expect(sim.dut.q).toBe(13n);
				sim.dut.en = 1n;
				expect(sim.dut.q).toBe(55n);
			} finally {
				sim.dispose();
			}
			for (const allowInferredLatches of [undefined, false]) {
				expect(() =>
					Simulator.fromSource(LATCH, "Latch", {
						allowInferredLatches,
						tier,
					}),
				).toThrow();
			}
		}
	});

	test("forwards opt-in to an initial testbench driving a latch instance", () => {
		const source = `${LATCH}
#[test(Top)] module Top {
    inst clk: $tb::clock_gen;
    var en: logic;
    var d: logic<8>;
    var q: logic<8>;
    inst dut: Latch(en, d, q);
    initial {
        en = 1; d = 7;
        clk.next(1);
        $assert(q == 8'd7);
        d = 13;
        clk.next(1);
        $assert(q == 8'd13);
        en = 0; d = 99;
        clk.next(2);
        $assert(q == 8'd13);
        $finish();
    }
}`;
		const result = runTest(
			[{ path: "latch_tb.veryl", content: source }],
			"Top",
			{
				allowInferredLatches: true,
			},
		);
		expect(result.error).toBeUndefined();
		expect(result.passed).toBe(true);
	});
});

describe("project inferred_latch diagnostics", () => {
	test("honors TOML levels, explicit policy precedence and cached warning changes", () => {
		const project = mkdtempSync(join(tmpdir(), "celox-latch-policy-"));
		try {
			mkdirSync(join(project, "src"));
			writeFileSync(
				join(project, "Veryl.toml"),
				'[project]\nname = "latch_policy"\nversion = "0.1.0"\n',
			);
			writeFileSync(join(project, "src/latch.veryl"), LATCH);
			for (const tier of [false, true]) {
				for (const level of ["allow", "warn", "error", undefined]) {
					writeFileSync(
						join(project, "celox.toml"),
						level ? `[diagnostics]\ninferred_latch = "${level}"\n` : "",
					);
					for (const flag of [undefined, false, true]) {
						const create = () =>
							Simulator.fromProject<{
								en: bigint;
								d: bigint;
								readonly q: bigint;
							}>(project, "Latch", { tier, allowInferredLatches: flag });
						if (level === "error" || (level === undefined && !flag)) {
							expect(create).toThrow(/inferred_latch/);
							continue;
						}
						const sim = create();
						try {
							expect(sim.warnings).toHaveLength(level === "warn" ? 1 : 0);
							if (level === "warn")
								expect(sim.warnings[0]).toContain("inferred_latch");
							sim.dut.en = 1n;
							sim.dut.d = 42n;
							expect(sim.dut.q).toBe(42n);
							sim.dut.en = 0n;
							sim.dut.d = 99n;
							expect(sim.dut.q).toBe(42n);
						} finally {
							sim.dispose();
						}
					}
				}
			}
			writeFileSync(
				join(project, "src/top.veryl"),
				`#[test(Top)] module Top {
    inst clk: $tb::clock_gen;
    var en: logic; var d: logic<8>; var q: logic<8>;
    inst dut: Latch(en, d, q);
    initial {
        en = 1; d = 7; clk.next(1); $assert(q == 8'd7);
        en = 0; d = 99; clk.next(1); $assert(q == 8'd7);
        $finish();
    }
}`,
			);
			for (const level of ["warn", "allow"]) {
				writeFileSync(
					join(project, "celox.toml"),
					`[diagnostics]\ninferred_latch = "${level}"\n`,
				);
				const result = runTestFromProject(project, "Top");
				expect(result.error).toBeUndefined();
				expect(result.passed).toBe(true);
			}
		} finally {
			rmSync(project, { recursive: true, force: true });
		}
	});
});
