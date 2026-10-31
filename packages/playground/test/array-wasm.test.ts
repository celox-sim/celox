import {
	FourState,
	type FourStateSignalValue,
	Simulator,
} from "@celox-sim/celox";
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

test("padded four-state arrays keep values and masks in separate planes", () => {
	const source = `
module PaddedArray (
	data: input logic<20> [6],
	head_index: input logic<3>,
	tail_index: input logic<3>,
	first: output logic<20>,
	last: output logic<20>,
) {
	assign first = data[head_index];
	assign last = data[tail_index];
}
`;
	interface Ports {
		head_index: bigint;
		tail_index: bigint;
		data: { set(i: number, value: FourStateSignalValue): void };
		readonly first: bigint;
		readonly last: bigint;
	}
	const sim = Simulator.fromSource<Ports>(source, "PaddedArray", {
		fourState: true,
	});
	try {
		sim.dut.head_index = 0n;
		sim.dut.tail_index = 5n;
		sim.dut.data.set(0, FourState(0x12345n, 0x10n));
		sim.dut.data.set(5, FourState(0xabcden, 0x10100n));
		expect(sim.fourState("first")).toMatchObject({
			value: 0x12345n,
			mask: 0x10n,
		});
		expect(sim.fourState("last")).toMatchObject({
			value: 0xabcden,
			mask: 0x10100n,
		});
		sim.dut.data.set(5, 6n);
		expect(sim.fourState("last")).toMatchObject({ value: 6n, mask: 0n });
		expect(sim.fourState("first")).toMatchObject({
			value: 0x12345n,
			mask: 0x10n,
		});
	} finally {
		sim.dispose();
	}
});
