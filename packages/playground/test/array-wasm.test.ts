import { Simulator } from "@celox-sim/celox";
import { describe, expect, test } from "vitest";

const ARRAY_SOURCE = `
module ArrayPassThrough (
    data: input  logic<3> [4],
    copy: output logic<3> [4],
) {
    for i in 0..4 :g {
        assign copy[i] = data[i];
    }
}
`;

describe("Array ports (WASM bridge)", () => {
	test("reads and writes unpacked array elements", () => {
		interface Ports {
			data: {
				at(i: number): bigint;
				set(i: number, value: bigint): void;
				readonly length: number;
			};
			readonly copy: {
				at(i: number): bigint;
				readonly length: number;
			};
		}

		const sim = Simulator.fromSource<Ports>(ARRAY_SOURCE, "ArrayPassThrough");

		expect(sim.dut.data.length).toBe(4);
		expect(sim.dut.copy.length).toBe(4);
		for (let i = 0; i < 4; i++) {
			sim.dut.data.set(i, BigInt(i + 1));
		}
		for (let i = 0; i < 4; i++) {
			expect(sim.dut.data.at(i)).toBe(BigInt(i + 1));
			expect(sim.dut.copy.at(i)).toBe(BigInt(i + 1));
		}

		sim.dispose();
	});
});
