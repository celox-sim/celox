// IEEE 1800-2023 21.2.3 / 21.3.2. Select each top independently.
module Transient;
  timeunit 1ns; timeprecision 1ps;
  int value=0;
  initial begin
    $monitor("value=%0d",value);
    #1 value=1; value=0;
    #1 $finish;
  end
endmodule
module Controls;
  timeunit 1ns; timeprecision 1ps;
  int value=0;
  initial begin
    $monitor("old=%0d",value); $monitor("new=%0d",value);
    #1 $monitoroff; value=1;
    #1 $monitoron;
    #1 $monitoron;
    #1 $finish;
  end
endmodule
module FileMonitor;
  timeunit 1ns; timeprecision 1ps;
  int value=0;
  initial begin
    $fmonitor(32'h80000001,"file=%0d",value); $monitoroff;
    #1 value=1;
    #1 $finish;
  end
endmodule
`timescale 1ns/1ps
module TimeQuery;
  int value=0;
  initial begin
    $monitor("t=%0d value=%0d",$time,value);
    #1;
    #1 value=1;
    #1 $finish;
  end
endmodule
module TimeQueryBench;
  timeunit 1ps; timeprecision 1ps;
  TimeQuery dut();
endmodule
