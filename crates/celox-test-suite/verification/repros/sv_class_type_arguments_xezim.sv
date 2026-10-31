package p;
  parameter int W = 6;
  typedef bit signed [W-1:0] word_t;
  class Utils #(type T = int);
    static function int width();
      T v;
      return $bits(v);
    endfunction
    static function int negative();
      T v = '1;
      return v < 0;
    endfunction
  endclass
  typedef Utils#(word_t) word_utils;
  typedef Utils#(bit [W:0]) wide_utils;
endpackage
module Top;
  localparam int W = 29;
  initial $display("word=%0d,%0d wide=%0d,%0d", p::word_utils::width(), p::word_utils::negative(),
                   p::wide_utils::width(), p::wide_utils::negative());
endmodule
module Wrapped; Top dut(); endmodule
