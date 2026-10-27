`timescale 1ns/1ns
// The order of final procedures is unspecified. Each emits a distinct
// label so an external check can compare the labels without ordering them.
module Child(output bit done);
  final begin done = 1; $display("child"); end
endmodule
module Top(output bit first, second, child_done, output bit [1:0] generated);
  Child child(child_done);
  initial begin #1; $finish; end
  final begin first = 1; $display("top first"); end
  final begin second = 1; $display("top second"); end
  for (genvar i = 0; i < 2; i++) begin : g
    final begin generated[i] = 1; $display("generate %0d", i); end
  end
endmodule
