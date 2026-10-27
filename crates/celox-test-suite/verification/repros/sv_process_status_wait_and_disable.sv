`timescale 1ns/1ns

module Top(output int checks, observed_at, late);
 process child, victim;
 initial begin checks=0; observed_at=0; late=0;
  fork begin child=process::self(); #3; end join_none
  #1; wait(child.status()==process::FINISHED); observed_at=$time;
  checks=(child.status()==process::FINISHED);
  fork begin victim=process::self(); #10; late=1; end join_none
  #1; disable fork;
  checks+=(victim.status()==process::KILLED)*2;
 end

 initial begin #16; $display("checks=%0d observed_at=%0d late=%0d",checks,observed_at,late); $finish; end
endmodule
module Wrapped; Top dut(); endmodule
