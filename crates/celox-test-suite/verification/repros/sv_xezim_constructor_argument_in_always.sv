// xezim 0.11.3: an object constructed with `new(s)` in an always procedure
// keeps an empty string property; IEEE 1800-2023 8.7 runs the constructor.
// Expected: "[2] 3".
module top;
  class Node;
    string text;
    function new(string t);
      text = t;
    endfunction
  endclass
  Node node, keep;
  string number;
  int n;
  logic clk = 0;
  always #1 clk = ~clk;
  always @(posedge clk) begin
    number.itoa(n);
    node = new(number);
    keep = node;
    n = n + 1;
  end
  initial begin
    #6 $display("[%s] %0d", keep.text, n);
    $finish;
  end
endmodule
