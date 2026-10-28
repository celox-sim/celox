module Top(output int passed, failed, action_value);
 bit clk, data;
 always @(posedge clk) data <= 1;
 p: assert property (@(posedge clk) data == 0)
    begin passed++; action_value = data; end else failed++;
 initial begin #1 clk=1; #1 clk=0; #1 clk=1; end
 initial begin #4; $display("passed=%0d failed=%0d action=%0d", passed, failed, action_value); $finish; end
endmodule
module Wrapped; Top dut(); endmodule
