`timescale 1ns/1ns

module Top(output int checks, late);
 process parent, leaf;
 initial begin late=0;
  fork begin
   parent=process::self();
   fork begin leaf=process::self(); #10; late=1; end join_none
   #1;
  end join_none
  #2;
  checks=(parent.status()==process::FINISHED);
  $display("before:parent=%0d leaf=%0d",parent.status(),leaf.status());
  parent.kill();
  checks += (parent.status()==process::FINISHED)*2;
  checks += (leaf.status()==process::KILLED)*4;
  leaf.await(); parent.await();
  #10; checks += (late==0)*8;
 end

 initial begin #13; $display("checks=%0d late=%0d parent_state=%0d leaf_state=%0d",checks,late,parent.status(),leaf.status()); $finish; end
endmodule
module Wrapped; Top dut(); endmodule
