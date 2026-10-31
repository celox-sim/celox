module Top(output int first, second);
 bit clk, reset;
 p: assert property (@(posedge clk) disable iff (reset) 1)
    begin first++; reset=1; end else first+=10;
 q: assert property (@(posedge clk) disable iff (reset) 1)
    second++; else second+=10;
 initial #1 clk=1;
 initial begin #4; $display("first=%0d second=%0d", first, second); $finish; end
endmodule
module Wrapped; Top dut(); endmodule
