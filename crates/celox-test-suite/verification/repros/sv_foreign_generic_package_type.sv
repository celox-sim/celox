
package p;
 class Item;
  static int tag=7;
  int value;
  function new(); value=42; endfunction
 endclass
endpackage
package q;
 class Factory #(type T);
  static int count=0;
  static function T create(); T result; result=new(); count++; return result; endfunction
  static function int type_tag(); return T::tag; endfunction
 endclass
endpackage
module Top(output int result, tag, count);
 p::Item object;
 initial begin
  object=q::Factory#(p::Item)::create(); result=object.value;
  tag=q::Factory#(p::Item)::type_tag(); count=q::Factory#(p::Item)::count;
 end
endmodule
module Observe;
 wire [31:0] result, tag, count;
 Top dut(result, tag, count);
 initial begin #1; $display("OBS package_type=%0d,%0d,%0d", result, tag, count); $finish; end
endmodule
