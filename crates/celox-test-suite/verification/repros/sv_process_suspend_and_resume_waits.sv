`timescale 1ns/1ns

module Top(output int delayed, event_time, level_time, checks);
 process a,b,c;
 bit event_signal, flag;
 initial begin delayed=0; event_time=0; level_time=0;
  fork
   begin a=process::self(); #5; delayed=$time; end
   begin b=process::self(); @(posedge event_signal); event_time=$time; end
   begin c=process::self(); wait(flag); level_time=$time; end
  join_none
  #1; a.suspend(); b.suspend(); c.suspend();
  checks=(a.status()==process::SUSPENDED && b.status()==process::SUSPENDED && c.status()==process::SUSPENDED);
  #2; flag=1; event_signal=1;
  #4; a.resume(); b.resume(); c.resume();
  #1; checks+=(delayed==7 && level_time==7 && event_time==0)*2;
  event_signal=0;
  #1; event_signal=1;
  b.await(); checks+=(b.status()==process::FINISHED)*4;
 end

 initial begin #11; $display("checks=%0d delayed=%0d event_time=%0d level_time=%0d",checks,delayed,event_time,level_time); $finish; end
endmodule
module Wrapped; Top dut(); endmodule
