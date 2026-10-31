module Top(output int allowed, blocked);
 bit clk;
 task automatic worker(input bit en, input bit which);
   @(posedge clk iff en);
   if (which) allowed++; else blocked++;
 endtask
 initial fork worker(1,1); worker(0,0); join_none
 initial begin #1 clk=1; #1 clk=0; #1 clk=1; end
 initial begin #4; $display("allowed=%0d blocked=%0d", allowed, blocked); $finish; end
endmodule
module Wrapped; Top dut(); endmodule
