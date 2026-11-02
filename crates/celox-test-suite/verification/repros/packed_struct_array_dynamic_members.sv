// Packed variable-array indices select elements; index 4 is outside [3:0].
module Top;
  bit clk = 0, write_en = 0;
  bit [2:0] index = 0;
  bit [7:0] data = 0;
  typedef struct packed { logic [3:0] high, low; } item_t;
  item_t [3:0] items = 32'h12345678;
  logic [7:0] q;
  assign q = {items[index].high, items[index].low};
  always_ff @(posedge clk) if (write_en) begin
    items[index].high <= data[7:4];
    items[index].low <= data[3:0];
  end
  initial begin
    #1; if (q !== 8'h78) $fatal(1, "initial member read");
    index = 1; data = 8'hab; write_en = 1;
    #1 clk = 1;
    #1; if (items !== 32'h1234ab78) $fatal(1, "valid member write");
    clk = 0; index = 4; data = 8'hff;
    #1 clk = 1;
    #1; $display("items=%h", items);
    if (items !== 32'h1234ab78) $fatal(1, "out-of-range write changed storage");
    $finish;
  end
endmodule
