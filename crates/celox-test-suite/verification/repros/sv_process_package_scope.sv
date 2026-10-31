`timescale 1ns/1ns
package process;
 parameter int FINISHED=8, RUNNING=9, WAITING=10, SUSPENDED=11, KILLED=12;
 function int self(); return 13; endfunction
endpackage
module Top;
 int checks;
 initial begin
  checks=(process::FINISHED==8)+(process::RUNNING==9)*2+(process::WAITING==10)*4
   +(process::SUSPENDED==11)*8+(process::KILLED==12)*16+(process::self()==13)*32;
  $display("checks=%0d", checks); #1; $finish;
 end
endmodule
module Wrapped; Top t(); endmodule
