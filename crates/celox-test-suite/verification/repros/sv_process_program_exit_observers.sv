`timescale 1ns/1ns
package handles; process sibling; endpackage
module Top;
 bit [1:0] checks;
 int awaited_at, observed_at, late;
 Producer producer(late);
 Consumer consumer(checks, awaited_at, observed_at);
 initial #5 begin
  $display("checks=%0d awaited_at=%0d observed_at=%0d late=%0d", checks, awaited_at, observed_at, late);
  $finish;
 end
 final $display("final at %0d", $time);
endmodule
program Producer(output int late);
 initial begin late=0; handles::sibling=process::self(); #30; late=1; end
 initial begin #3; $exit; end
endprogram
program Consumer(output bit [1:0] checks, output int awaited_at, observed_at);
 import handles::sibling;
 initial begin checks[0]=0; awaited_at=0;
  #1; sibling.await(); awaited_at=$time; checks[0]=1; #20;
 end
 initial begin checks[1]=0; observed_at=0;
  #1; wait(sibling.status()==process::FINISHED); observed_at=$time; checks[1]=1; #20;
 end
endprogram
module Wrapped; Top t(); endmodule
