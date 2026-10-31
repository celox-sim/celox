`timescale 1ns/1ns
package handles; process victim; endpackage
module Top;
 int ticks;
 Tb tb();
 import handles::victim; initial begin #3; victim.kill(); $display("unreachable"); end
 initial begin ticks=0; forever #2 ticks++; end
 initial #11 begin $display("fallback"); $finish; end
 final $display("final at %0d ticks=%0d", $time, ticks);
endmodule
program Tb;
 initial begin handles::victim=process::self(); #100; end
endprogram
module Wrapped; Top t(); endmodule
