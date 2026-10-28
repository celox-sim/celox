`timescale 1ns/1ns

module Top;
 int ticks;
 Tb tb();

 initial begin ticks=0; forever #2 ticks++; end
 initial #11 begin $display("fallback"); $finish; end
 final $display("final at %0d ticks=%0d", $time, ticks);
endmodule
program Tb;
 process p; initial begin p=process::self(); #3; p.kill(); $display("unreachable"); end
endprogram
module Wrapped; Top t(); endmodule
