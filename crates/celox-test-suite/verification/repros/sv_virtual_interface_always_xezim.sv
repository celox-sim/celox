`timescale 1ns/1ns
interface bus_if(input logic clk); endinterface
module Top;
  logic tick = 0;
  always #2 tick = ~tick;
  bus_if ticking(tick);
  virtual bus_if vif = ticking;
  int rises = 0;
  always @(posedge vif.clk) rises++;
  initial begin #7 $display("rises=%0d", rises); $finish; end
endmodule
