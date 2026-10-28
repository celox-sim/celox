module Top(output int accepted, rejected);
 bit first, second;
 initial begin
   automatic bit enable=1;
   fork
     begin @(posedge first iff enable); accepted++; end
     begin @(posedge second iff enable); rejected++; end
     begin #1 first=1; enable=0; second=1; end
   join_none
   #3;
 end
 initial begin #4; $display("accepted=%0d rejected=%0d", accepted, rejected); $finish; end
endmodule
module Wrapped; Top dut(); endmodule
