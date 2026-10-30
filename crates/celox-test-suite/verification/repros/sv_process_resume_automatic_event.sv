`timescale 1ns/1ns
module Top(output int hits, event_at);
 process worker;
 initial begin
  automatic bit x=0, y=0;
  hits=0; event_at=0;
  fork
   begin worker=process::self(); @(x ^ y); hits++; event_at=$time; end
   begin #2; x=1; #4; y=1; end
  join_none
  #1; worker.suspend();
  #3; worker.resume();
 end
 initial begin #1; $display("at1 hits=%0d event_at=%0d", hits, event_at); #2; $display("at3 hits=%0d event_at=%0d", hits, event_at); #2; $display("at5 hits=%0d event_at=%0d", hits, event_at); #3; $display("at8 hits=%0d event_at=%0d", hits, event_at); $finish; end
endmodule
module Wrapped; Top t(); endmodule
