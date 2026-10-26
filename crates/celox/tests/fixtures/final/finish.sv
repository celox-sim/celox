`timescale 1ns/1ns
// Whichever final runs first must terminate the current and remaining finals.
module Top(output int calls, after_finish);
  initial begin calls = 0; after_finish = 0; #1; $finish; end
  final begin
    calls = calls + 1;
    $display("final calls=%0d", calls);
    $finish;
    after_finish = 1;
    $display("unreachable first");
  end
  final begin
    calls = calls + 1;
    $display("final calls=%0d", calls);
    $finish;
    after_finish = 2;
    $display("unreachable second");
  end
endmodule
