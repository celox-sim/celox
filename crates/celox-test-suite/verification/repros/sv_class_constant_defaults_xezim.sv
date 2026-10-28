package p;
  parameter int WIDTH = 5;
  class Box #(int N = WIDTH, int M = N + 2);
    function int get(); return WIDTH + 10 * N + M; endfunction
  endclass
  typedef Box#() Small;
  typedef Box Implicit;
  typedef Box#(9) Wide;
  function automatic int read();
    Small defaulted = new();
    Implicit normal = new();
    Wide wide = new();
    return defaulted.get() + normal.get() + wide.get();
  endfunction
endpackage
module Top;
  localparam int WIDTH = 29;
  initial $display("sum=%0d", p::read());
endmodule
module Wrapped; Top dut(); endmodule
