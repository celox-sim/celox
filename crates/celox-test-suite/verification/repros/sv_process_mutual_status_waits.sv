`timescale 1ns/1ns
module Top;
 bit [1:0] checks;
 int observed_at;
 process first, second;
 initial begin
  checks=0; observed_at=0;
  fork
   begin first=process::self(); wait(second!=null);
    wait(second.status()==process::FINISHED); checks[0]=1;
   end
   begin second=process::self(); wait(first!=null);
    wait(first.status()==process::KILLED); checks[1]=1; observed_at=$time;
   end
  join_none
  #3; first.kill();
 end
 initial #6 begin $display("checks=%0d observed_at=%0d", checks, observed_at); $finish; end
endmodule
module Wrapped; Top t(); endmodule
