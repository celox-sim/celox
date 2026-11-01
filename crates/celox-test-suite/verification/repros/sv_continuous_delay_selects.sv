// IEEE 1800-2023 10.3.3: delays belong to the individual assignments.
// Expected: y=0011 g=01, then y=1111 g=11.
module Top;
  timeunit 1ns;
  timeprecision 100ps;
  bit [3:0] a;
  wire [3:0] y;
  wire [1:0] g;
  assign #2 y[1:0] = a[1:0];
  assign #4 y[3:2] = a[3:2];
  for (genvar i=0; i<2; i++) begin : generated
    localparam int D=i+2;
    assign #(D) g[i]=a[0];
  end
  initial begin
    #4 a=15;
    #2.5 $display("y=%b g=%b",y,g);
    #2 $display("y=%b g=%b",y,g);
    $finish;
  end
endmodule
