
package p;
 class Registry #(type T);
  static int count=0;
  static function T create(); T object; object=new(); count++; return object; endfunction
 endclass
endpackage

import p::*;
class FirstUser;
 int value;
 function new(); value=42; endfunction
 typedef Registry#(FirstUser) type_id;
endclass
module First(output int result, count);
 FirstUser object;
 initial begin object=FirstUser::type_id::create(); result=object.value; count=FirstUser::type_id::count; end
endmodule

import p::*;
class SecondUser;
 int padding;
 int value;
 function new(); padding=99; value=73; endfunction
 typedef Registry#(SecondUser) type_id;
endclass
module Second(output int result, count);
 SecondUser object;
 initial begin object=SecondUser::type_id::create(); result=object.value; count=SecondUser::type_id::count; end
endmodule

module Top(output int first, second, first_count, second_count);
 First a(first, first_count); Second b(second, second_count);
endmodule
module Observe;
 wire [31:0] first, second, first_count, second_count;
 Top dut(first,second,first_count,second_count);
 initial begin #1;
  $display("OBS factories=%0d,%0d counts=%0d,%0d", first,second,first_count,second_count);
  $finish;
 end
endmodule
