module Leaf;
  string files[2:3];
endmodule
module Top;
  for (genvar i=0; i<2; i++) begin : generated
    Leaf u();
  end
  initial begin
    #1;
    generated[1].u.files[2] = "binary";
    $display("lengths=%0d,%0d", generated[1].u.files[2].len(), generated[1].u.files[3].len());
    if (generated[1].u.files[2].len() != 6 || generated[1].u.files[3].len() != 0)
      $fatal(1,"generated instance string array element methods");
    if (generated[0].u.files[2] != "") $fatal(1,"generated instance isolation");
    $finish;
  end
endmodule
