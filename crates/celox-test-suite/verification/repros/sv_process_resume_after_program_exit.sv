`timescale 1ns/1ns
package handles;
 process victim;
endpackage
module Top(output int late, checks, ended_state);
 Producer producer(late);
 Consumer consumer(checks, ended_state);
 initial begin #9; $display("late=%0d checks=%0d state=%0d", late, checks, ended_state); $finish; end
 final $display("final at %0d", $time);
endmodule
program Producer(output int late);
 import handles::victim;
 initial begin late=0; victim=process::self(); #6; late=1; end
 initial begin #1; victim.suspend(); #1; $exit; end
endprogram
program Consumer(output int checks, ended_state);
 import handles::victim;
 initial begin
  checks=0; ended_state=0;
  #3; ended_state=victim.status(); checks=(ended_state==process::FINISHED);
  victim.resume();
  #5; checks+=(victim.status()==process::FINISHED)*2;
  #2;
 end
endprogram
module Wrapped; Top t(); endmodule
