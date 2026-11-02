module Leaf;
  string filename = "seed";
endmodule
module Top;
  Leaf a();
  initial begin
    #1;
    a.filename.putc(0, 8'h53);
    if (a.filename != "Seed") $fatal(1, "hierarchical string method");
    $finish;
  end
endmodule
