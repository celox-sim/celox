package p;
  typedef enum {A0, A1} first_e;
  typedef enum {B0, B1} second_e;
  typedef first_e alias_e;
  typedef struct packed { bit [3:0] a; } s1_t;
  typedef struct packed { bit [3:0] a; } s2_t;
  class Box #(type T = int);
    static int count;
    function new();
      count++;
    endfunction
  endclass
  function automatic void run();
    Box#(first_e) a = new();
    Box#(alias_e) b = new();
    Box#(second_e) c = new();
    Box#(s1_t) d = new();
    Box#(s2_t) e = new();
    Box#(s2_t) f = new();
    $display("first=%0d alias=%0d second=%0d s1=%0d s2=%0d", Box#(first_e)::count,
             Box#(alias_e)::count, Box#(second_e)::count, Box#(s1_t)::count, Box#(s2_t)::count);
  endfunction
endpackage
module Top;
  initial p::run();
endmodule
module Wrapped; Top dut(); endmodule
