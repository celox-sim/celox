/**
 * SystemVerilog examples for the playground.
 *
 * The constructs used here follow IEEE 1800-2023: ANSI module headers
 * (Clause 23.2.1), `always_comb` for combinational logic (Clause 9.2.2.2)
 * and `always_ff` for sequential logic (Clause 9.2.2.4).
 */
export interface SvExample {
	sourcePath: string;
	source: string;
	testbench: string;
}

const COUNTER_SV = `module Counter (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       en,
    output logic [7:0] count
);
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            count <= 8'd0;
        end else if (en) begin
            count <= count + 8'd1;
        end
    end
endmodule
`;

export const SV_EXAMPLES: Record<string, SvExample> = {
	sv_adder: {
		sourcePath: "src/Adder.sv",
		source: `module Adder (
    input  logic [15:0] a,
    input  logic [15:0] b,
    output logic [16:0] sum
);
    always_comb begin
        sum = a + b;
    end
endmodule
`,
		testbench: `import { describe, it, expect } from "vitest";
import { Simulator } from "@celox-sim/celox";
import { Adder } from "../src/Adder.sv";

describe("Adder (SystemVerilog)", () => {
    it("adds two small numbers", () => {
        const sim = Simulator.create(Adder);
        sim.dut.a = 100n;
        sim.dut.b = 200n;
        expect(sim.dut.sum).toBe(300n);
        sim.dispose();
    });

    it("handles overflow into the 17th bit", () => {
        const sim = Simulator.create(Adder);
        sim.dut.a = 0xFFFFn;
        sim.dut.b = 1n;
        expect(sim.dut.sum).toBe(0x10000n);
        sim.dispose();
    });
});
`,
	},
	sv_counter: {
		sourcePath: "src/Counter.sv",
		source: COUNTER_SV,
		testbench: `import { describe, it, expect } from "vitest";
import { Simulation } from "@celox-sim/celox";
import { Counter } from "../src/Counter.sv";

describe("Counter (SystemVerilog)", () => {
    it("resets and counts with time-based simulation", () => {
        const sim = Simulation.create(Counter);
        sim.addClock("clk", { period: 10 });

        // Assert and release the active-low async reset automatically
        sim.reset("rst_n");

        sim.dut.en = 1n;
        sim.waitForCycles("clk", 5);
        expect(sim.dut.count).toBe(5n);

        sim.dispose();
    });

    it("stays at zero when disabled", () => {
        const sim = Simulation.create(Counter);
        sim.addClock("clk", { period: 10 });
        sim.reset("rst_n");

        sim.dut.en = 0n;
        sim.waitForCycles("clk", 5);
        expect(sim.dut.count).toBe(0n);

        sim.dispose();
    });
});
`,
	},
	sv_counter_vcd: {
		sourcePath: "src/Counter.sv",
		source: COUNTER_SV,
		testbench: `import { describe, it, expect } from "vitest";
import { Simulation } from "@celox-sim/celox";
import { Counter } from "../src/Counter.sv";

describe("Counter (SystemVerilog Waveform)", () => {
    it("counts up with enable", () => {
        const sim = Simulation.create(Counter);
        sim.addClock("clk", { period: 10 });

        sim.dump(sim.time());
        sim.reset("rst_n");
        sim.dump(sim.time());

        sim.dut.en = 1n;
        sim.dump(sim.time());

        for (let i = 0; i < 100; i++) {
            const t = sim.step();
            if (t === null) break;
            sim.dump(t);
        }

        expect(sim.dut.count).toBeGreaterThan(0n);
        sim.dispose();
    });
});
`,
	},
	sv_alu: {
		sourcePath: "src/Alu.sv",
		source: `module Alu #(
    parameter int WIDTH = 8
) (
    input  logic [WIDTH-1:0] a,
    input  logic [WIDTH-1:0] b,
    input  logic [1:0]       op,
    output logic [WIDTH-1:0] y,
    output logic             zero
);
    always_comb begin
        case (op)
            2'b00:   y = a + b;
            2'b01:   y = a - b;
            2'b10:   y = a & b;
            default: y = a | b;
        endcase
    end

    assign zero = (y == '0);
endmodule
`,
		testbench: `import { describe, it, expect } from "vitest";
import { Simulator } from "@celox-sim/celox";
import { Alu } from "../src/Alu.sv";

describe("Alu (SystemVerilog)", () => {
    it("adds, subtracts, ands and ors by opcode", () => {
        const sim = Simulator.create(Alu);
        sim.dut.a = 0x3Cn;
        sim.dut.b = 0x0Fn;

        sim.dut.op = 0n;
        expect(sim.dut.y).toBe(0x4Bn);

        sim.dut.op = 1n;
        expect(sim.dut.y).toBe(0x2Dn);

        sim.dut.op = 2n;
        expect(sim.dut.y).toBe(0x0Cn);

        sim.dut.op = 3n;
        expect(sim.dut.y).toBe(0x3Fn);

        sim.dispose();
    });

    it("flags a zero result", () => {
        const sim = Simulator.create(Alu);
        sim.dut.a = 5n;
        sim.dut.b = 5n;
        sim.dut.op = 1n;

        expect(sim.dut.y).toBe(0n);
        expect(sim.dut.zero).toBe(1n);

        sim.dispose();
    });
});
`,
	},
};
