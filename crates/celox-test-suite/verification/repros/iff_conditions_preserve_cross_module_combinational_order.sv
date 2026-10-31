module ClockFirst(input bit a);
 bit clk, en;
 always_comb begin clk=a; en=a; end
endmodule
module GateFirst(input bit a);
 bit clk, en;
 always_comb begin en=a; clk=a; end
endmodule
module Top(output int rejected, accepted);
 bit a;
 ClockFirst late_gate(a);
 GateFirst early_gate(a);
 initial forever begin @(posedge late_gate.clk iff late_gate.en); rejected++; end
 initial forever begin @(posedge early_gate.clk iff early_gate.en); accepted++; end
 initial begin #1 a=1; #1 a=0; #1 a=1; end
 initial begin #4; $display("rejected=%0d accepted=%0d", rejected, accepted); $finish; end
endmodule
module Wrapped; Top dut(); endmodule
