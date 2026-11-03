// Configured reproduction: verilator --binary --timing --vpi --public-flat-rw
// --assert --coverage-user --x-initial 0 --x-assign 0 --top-module Bench ...
// IEEE 1800-2023 21.2.2: strobe observes the time slot's final values,
// including program writes in Reactive (24.3.1).
program Writer(output logic [31:0] value);
  timeunit 1ns; timeprecision 1ns;
  initial begin value=9; #10 value=10; end
endprogram
module Top(output wire [31:0] value);
  timeunit 1ns; timeprecision 1ns;
  Writer writer(value);
  initial begin
    $strobe("reactive=%0d",value);
    #2 $display("later");
  end
endmodule
module Bench;
  timeunit 1ns; timeprecision 1ns;
  Top dut(.value());
  initial begin
    #1 $display("settled=%0d",dut.value);
    #1 $finish;
  end
endmodule
