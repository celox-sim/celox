/**
 * Testbench-language examples: the design is checked by a testbench
 * written in an HDL, not in TypeScript. The TypeScript file only starts
 * the HDL testbench (through `TimedTestbench`) and asserts on its result.
 */
export interface TbExample {
	files: { path: string; content: string }[];
	/** HDL top module that contains the testbench. */
	top: string;
	testbench: string;
}

const VERYL_COUNTER = `module Counter (
    clk:   input  clock,
    rst:   input  reset,
    en:    input  logic,
    count: output logic<8>,
) {
    always_ff {
        if_reset {
            count = 0;
        } else if en {
            count += 1;
        }
    }
}
`;

const SV_TESTBENCH = `// SystemVerilog testbench for the Veryl Counter.
// The Veryl module is instantiated by its emitted name, prj_Counter.
// The testbench generates the clock, drives reset/enable, and checks
// the counts itself; it reports through the errors/done signals.
module Tb;
    timeunit 1ns;
    timeprecision 1ns;

    logic clk;
    logic rst;
    logic en;
    logic [7:0] count;
    int errors;
    bit done;

    prj_Counter dut (.clk(clk), .rst(rst), .en(en), .count(count));

    always #5 clk = ~clk;

    initial begin
        clk = 0;
        rst = 0;
        en = 0;
        errors = 0;
        done = 0;

        #12 rst = 1;
        en = 1;
        repeat (5) @(posedge clk);
        #1;
        if (count !== 8'd5) begin
            errors = errors + 1;
            $display("FAIL: count=%0d, expected 5", count);
        end

        en = 0;
        repeat (2) @(posedge clk);
        #1;
        if (count !== 8'd5) begin
            errors = errors + 1;
            $display("FAIL: count changed while disabled: %0d", count);
        end

        if (errors == 0) $display("PASS: sv_testbench_tests_veryl_counter");
        done = 1;
        $finish;
    end
endmodule
`;

export const TB_EXAMPLES: Record<string, TbExample> = {
	sv_tb_veryl: {
		top: "Tb",
		files: [
			{ path: "src/Counter.veryl", content: VERYL_COUNTER },
			{ path: "src/Tb.sv", content: SV_TESTBENCH },
		],
		testbench: `import { describe, it, expect } from "vitest";

// The checks themselves live in src/Tb.sv (SystemVerilog); this file
// only runs that testbench and asserts on what it reported.
describe("Tb (SystemVerilog testbench, Veryl Counter)", () => {
    it("passes the SystemVerilog testbench", async () => {
        const result = await TimedTestbench.run("Tb");

        expect(result.error).toBeNull();
        expect(result.finished).toBe(true);
        expect(result.signals.errors).toBe("0");
        expect(result.signals.done).toBe("1");
        expect(result.signals.count).toBe("5");
    });
});
`,
	},
};
