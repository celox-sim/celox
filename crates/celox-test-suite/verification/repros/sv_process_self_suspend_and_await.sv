`timescale 1ns/1ns

module Top(output int phase, finished_at, checks);
 process worker;
 initial begin phase=0; finished_at=0;
  fork begin
   worker=process::self(); worker.suspend(); phase=1; #3; phase=2;
  end join_none
  #2; checks=(worker.status()==process::SUSPENDED && phase==0);
  worker.resume(); worker.await(); finished_at=$time;
  checks+=(worker.status()==process::FINISHED && phase==2)*2;
  worker.suspend(); worker.resume(); worker.kill();
  checks+=(worker.status()==process::FINISHED)*4;
 end

 initial begin #7; $display("checks=%0d phase=%0d finished_at=%0d terminal_state=%0d",checks,phase,finished_at,worker.status()); $finish; end
endmodule
module Wrapped; Top dut(); endmodule
