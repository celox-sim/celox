module Top;
  class Box #(type T = int);
    static int count;
    function new();
      count++;
    endfunction
  endclass
  typedef enum {C0, C1} shared_e;
  int counts[2];
  for (genvar i = 0; i < 2; i++) begin : g
    typedef enum {A, B} e_t;
    typedef shared_e alias_e;
    initial begin
      automatic Box#(e_t) local_box = new();
      automatic Box#(alias_e) shared_box = new();
      #1 counts[i] = 10 * Box#(e_t)::count + Box#(alias_e)::count;
    end
  end
  initial #2 $display("counts=%0d %0d", counts[0], counts[1]);
endmodule
module Wrapped; Top dut(); endmodule
