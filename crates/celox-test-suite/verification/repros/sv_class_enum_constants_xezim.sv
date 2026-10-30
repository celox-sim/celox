package p;
  class C;
    typedef bit [1:0] mode_t;
    typedef enum mode_t {STRICT = 2'b00, HIGHEST = 2'b01, CHECK = 2'b10} mode_e;
    localparam mode_t DEFAULT = CHECK;
    mode_e m = HIGHEST;
    function int f(); return DEFAULT + 10 * m; endfunction
    static function int g(); return C::CHECK; endfunction
  endclass
  class P #(int N = 1);
    typedef enum {A = N, B} e;
    static function int b(); return B; endfunction
  endclass
  typedef P#(5) P5;
  typedef P#() P1;
endpackage
module Top;
  import p::*;
  initial begin
    automatic C c = new();
    $display("f=%0d g=%0d check=%0d default=%0d p5=%0d p1=%0d", c.f(), C::g(), C::CHECK, C::DEFAULT, P5::b(), P1::b());
  end
endmodule
module Wrapped; Top dut(); endmodule
