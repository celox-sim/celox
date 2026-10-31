`timescale 1ns/1ns
package phase;
 process saved;
 string last_state;
 int visits=0;
 function automatic void visit();
  process p;
  string prior_state, following_state;
  int unsigned a,b;
  bit same, continuity, roundtrip;
  p=process::self(); same=(saved==null || saved==p);
  continuity=(saved==null || p.get_randstate()==last_state);
  if(saved==null) begin saved=p; p.srandom(42); end
  prior_state=p.get_randstate(); a=$urandom; following_state=p.get_randstate();
  p.set_randstate(prior_state); b=$urandom;
  roundtrip=(a==b && p.get_randstate()==following_state);
  last_state=following_state; visits++;
  $display("visit=%0d same=%0d state=%0d continuity=%0d replay=%0d", visits, same, saved.status(), continuity, roundtrip);
 endfunction
endpackage
module Top;
 Child child();
 initial begin phase::saved=null; phase::last_state=""; #1 $finish; end
 final phase::visit();
 final phase::visit();
endmodule
module Child; final phase::visit(); endmodule
module Wrapped; Top t(); endmodule
