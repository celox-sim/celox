// IEEE 1800-2023 10.3.3: an independent net delay preserves each bit's
// pending update. At 10.1 ns expect w=01 selected=01 initialized=00;
// at 12.1 ns all three are 11.
module Top;
  timeunit 1ns;
  timeprecision 100ps;
  bit [1:0] a;
  wire [1:0] #5 w;
  wire [1:0] #5 initialized = a;
  wire [5:4] #5 selected;
  assign w=a;
  assign selected[5]=a[1];
  assign selected[4]=a[0];
  initial begin
    #5 a=1;
    #2 a=3;
    #3.1 $display("w=%b selected=%b initialized=%b",w,selected,initialized);
    #2 $display("w=%b selected=%b initialized=%b",w,selected,initialized);
    $finish;
  end
endmodule
