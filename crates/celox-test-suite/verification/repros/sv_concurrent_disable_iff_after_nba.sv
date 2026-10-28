module Top(output int passed, failed);
 bit clk, reset;
 always @(posedge clk) reset <= 1;
 p: assert property (@(posedge clk) disable iff (reset) 0)
    passed++; else failed++;
 initial begin #1 clk=1; #1 clk=0; #1 clk=1; end
 initial begin #4; $display("passed=%0d failed=%0d", passed, failed); $finish; end
endmodule
module Wrapped; Top dut(); endmodule
