// IEEE 1800-2023 16.9.3: history functions resolve their own sampled arguments.
module Top;
  bit clk, data, enable, index;
  bit [3:0] bits_value = 4'b1010;
  bit selected;
  int cycle = 0, passed = 0, failed = 0;
`ifdef DISABLED
  always @(posedge clk) begin data <= ~data; cycle <= cycle + 1; end
  p: assert property (@(posedge clk) disable iff (cycle <= 2) $past(data))
    passed++; else failed++;
  initial begin
    repeat (5) begin #1 clk = 1; #1 clk = 0; end
    $display("passed=%0d failed=%0d (expected 2 1)", passed, failed);
    $finish;
  end
`elsif GATED
  always @(posedge clk) selected <= $past(data, 1, enable);
  initial begin #1 clk=1; #1 $finish; end
`else
  always @(posedge clk) selected <= $sampled(bits_value[$past(index)]);
  initial begin
    // First history value is the default bit value, 0: bits_value[0] is 0.
    index = 1;
    #1 clk = 1;
    #1 $display("selected=%0d (expected 0)", selected);
    $finish;
  end
`endif
endmodule
