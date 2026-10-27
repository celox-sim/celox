module Top(output int accepted, rejected);
 bit enable=1;
 initial begin
   automatic bit clock=0;
   fork
     begin @(posedge clock iff enable); accepted++; end
     begin @(negedge clock iff enable); rejected++; end
     begin #1 clock=1; enable=0; clock=0; end
   join_none
   #3;
 end
 initial begin #4; $display("accepted=%0d rejected=%0d", accepted, rejected); $finish; end
endmodule
module Wrapped; Top dut(); endmodule
