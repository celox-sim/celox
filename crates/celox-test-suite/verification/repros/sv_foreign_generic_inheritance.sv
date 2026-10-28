
package p;
 class Base #(int N=3, type T=bit[N-1:0]);
  localparam int LOCAL=N+1;
  parameter string LABEL="base";
  typedef T item_t;
 endclass
endpackage
package q;
 import p::*;
 parameter int N=99, LOCAL=23;
 parameter string LABEL="package_q";
 class Child #(int M=5) extends Base#(M);
  bit[LOCAL-1:0] field;
  function int checks();
   return (N==5) + (LOCAL==6)*2 + ($bits(item_t)==5)*4
          + ($bits(field)==6)*8 + (LABEL=="base")*16;
  endfunction
 endclass
endpackage
module Top(output int checks);
 q::Child#(5) child;
 initial begin child=new(); checks=child.checks(); end
endmodule
module Observe;
 wire [31:0] checks;
 Top dut(checks);
 initial begin #1; $display("OBS inheritance=%0d", checks); $finish; end
endmodule
