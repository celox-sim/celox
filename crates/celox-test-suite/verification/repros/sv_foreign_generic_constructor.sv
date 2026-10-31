
package p;
 int source_value=96;
 function automatic int initial_value(); return source_value+3; endfunction
 class Factory #(type T);
  static int count=0;
  static function T create(); T result; result=new(); count++; return result; endfunction
 endclass
endpackage
int value=20;
function automatic int initial_value(); return value+3; endfunction
class User;
 int value;
 function new(int n=initial_value()); value=n; endfunction
endclass
module Top(output int result, count);
 User object;
 initial begin
  object=p::Factory#(User)::create(); result=object.value; count=p::Factory#(User)::count;
 end
endmodule
module Observe;
 wire [31:0] result, count;
 Top dut(result, count);
 initial begin #1; $display("OBS constructor=%0d,%0d", result, count); $finish; end
endmodule
