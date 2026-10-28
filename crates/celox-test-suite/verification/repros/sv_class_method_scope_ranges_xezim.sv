module Top;
  logic [7:0] data;
  class C;
    static function logic [$bits(data)-1:0] f();
      return '1;
    endfunction
  endclass
  logic [$bits(C::f())-1:0] y;
  initial $display("y=%0d f=%0d", $bits(y), $bits(C::f()));
endmodule
module Wrapped; Top dut(); endmodule
