import { expect, test } from "vitest";
import { Simulation } from "./simulation.js";
import { Simulator } from "./simulator.js";

test("the addon renders runtime floating-point assertion arguments numerically", () => {
	const sim = Simulation.fromSource<{
		a: bigint;
		b: bigint;
		readonly sum: bigint;
	}>(
		`module Top(a: input f64, b: input f32, sum: output f64) {
            always_comb {
                sum = a + (b as f64);
                $assert(a == 0.0, "a=%.2f b=%.1f sum=%.2f", a, b, a + (b as f64));
            }
        }`,
		"Top",
	);
	try {
		sim.dut.a = 0xbff8000000000000n; // -1.5
		sim.dut.b = 0x40100000n; // 2.25
		expect(() => sim.dut.sum).toThrow("a=-1.50 b=2.2 sum=0.75");
	} finally {
		sim.dispose();
	}
});

test("the native addon preserves FP payloads and numeric conversions", () => {
	const sim = Simulator.fromSource<{
		a: bigint;
		b: bigint;
		readonly sum: bigint;
		readonly rounded: bigint;
	}>(
		`module Top(a: input f64, b: input f32, sum: output f64, rounded: output i32) {
            assign sum = a + (b as f64);
            assign rounded = a as i32;
        }`,
		"Top",
	);
	try {
		// Host ports retain their IEEE payload representation.
		sim.dut.a = 0x3ff8000000000000n; // 1.5
		sim.dut.b = 0x40100000n; // 2.25
		expect(sim.dut.sum).toBe(0x400e000000000000n); // 3.75
		expect(sim.dut.rounded).toBe(2n);
		sim.dut.a = 0xbff8000000000000n; // -1.5
		expect(sim.dut.sum).toBe(0x3fe8000000000000n); // 0.75
		expect(sim.dut.rounded).toBe(0xffff_fffen);
	} finally {
		sim.dispose();
	}
});
