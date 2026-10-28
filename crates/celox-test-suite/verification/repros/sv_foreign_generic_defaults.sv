
package p;
 parameter int W=5;
 typedef bit [W-1:0] word_t;
 class Box #(int N=W, type T=word_t);
  static int count=0;
  T value;
  function new(); count++; endfunction
  static function int width(); return $bits(T); endfunction
  static function int number(); return N; endfunction
  static function int from_pkg(); int W; W=77; return p::W; endfunction
 endclass
endpackage
module Top(output int checks);
 parameter int W=9;
 typedef bit [W-1:0] word_t;
 typedef p::Box#(.T(word_t)) Wide;
 typedef p::Box#(.T(word_t),.N(5)) Same;
 typedef Wide::T word_again;
 Wide a; Same b; p::Box#() c;
 initial begin
  a=new(); b=new(); c=new();
  checks=(Wide::number()==5);
  checks+=(Wide::width()==9)*2;
  checks+=(p::Box#()::width()==5)*4;
  checks+=(Wide::N==5)*8;
  checks+=($bits(word_again)==9)*16;
  checks+=(Wide::count==2 && Same::count==2 && p::Box#()::count==1)*32;
  checks+=(Wide::from_pkg()==5)*64;
 end
initial begin #1;
 $display("OBS checks=%0d widths=%0d,%0d,%0d counts=%0d,%0d,%0d", checks, Wide::width(), Same::width(), p::Box#()::width(), Wide::count, Same::count, p::Box#()::count);
 $finish;
end
endmodule