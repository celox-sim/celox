package p;
 class Counter #(type T); static int count=0; static function int tick(); count++; return count; endfunction endclass
endpackage
module Top;
 typedef enum bit [1:0] {A0=0,A1=1} A;
 typedef A Alias;
 typedef enum bit [1:0] {B0=0,B1=1} B;
 int a,b,c;
 initial begin
  a=p::Counter#(A)::tick(); b=p::Counter#(Alias)::tick(); c=p::Counter#(B)::tick();
  $display("OBS nominal=%0d,%0d,%0d",a,b,c); $finish;
 end
endmodule
