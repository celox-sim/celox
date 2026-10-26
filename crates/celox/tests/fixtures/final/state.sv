`timescale 1ns/1ns
// IEEE 1800-2023 9.2.3: final observes the last state and runs once.
module Top(output int value, calls);
  initial begin
    value = 3; calls = 0;
    $display("initial calls=%0d", calls);
    #2 value = 7;
    $finish;
  end
  final begin
    calls = calls + 1;
    value = value + 10;
    if (value != 17 || calls != 1) $fatal(1, "bad final state");
    $display("final value=%0d calls=%0d", value, calls);
  end
endmodule
