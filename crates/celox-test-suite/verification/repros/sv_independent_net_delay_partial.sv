// IEEE 1800-2023 6.7.1: statically undriven bits start and remain Z.
// Expect zzxx at 1 ns, zz00 at 4 ns and zz11 at 8 ns.
module Top;
  timeunit 1ns;
  timeprecision 1ns;
  bit a;
  wire [3:0] #3 w;
  assign w[1:0]={a,a};
  initial begin
    #1 $display("w=%b",w);
    #3 $display("w=%b",w);
    a=1;
    #4 $display("w=%b",w);
    $finish;
  end
endmodule
