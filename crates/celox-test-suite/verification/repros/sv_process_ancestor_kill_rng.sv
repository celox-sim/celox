`timescale 1ns/1ns
module Top;
 int checks, late;
 process root, ancestor, child;
 int unsigned reference_first, reference_second, child_draw;
 initial begin
  checks=0; late=0; child_draw=0;
  root=process::self(); root.srandom(42);
  reference_first=$urandom; reference_second=$urandom;
  fork begin
   ancestor=process::self();
   fork begin
    child=process::self(); child.srandom(42);
    child_draw=$urandom; ancestor.kill(); late=1;
   end join_none
   #30;
  end join_none
  #1;
  checks=(child_draw==reference_first);
  checks+=(ancestor.status()==process::KILLED)*2;
  checks+=(child.status()==process::KILLED)*4;
  root.set_randstate(child.get_randstate());
  checks+=($urandom==reference_second)*8;
  checks+=(late==0)*16;
 end
 initial #3 begin
  $display("checks=%0d late=%0d ancestor_state=%0d child_state=%0d", checks, late, ancestor.status(), child.status());
  $finish;
 end
endmodule
module Wrapped; Top t(); endmodule
