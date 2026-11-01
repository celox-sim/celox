// IEEE 1800-2023 11.3.6 requires each assignment expression to return its
// stacked, destination-typed value. The sum below is 12 in either operand order.
// Observed: Verilator 5.052 prints 14; Icarus 13.0 rejects the assignment expressions.
// CONCAT: Verilator rejects a concatenated assignment-expression target.
// SELECTED: Verilator stops in V3Width.cpp with "No dtype expected ... ASSIGN".
module top;
  int a,b,i,mem[2];
  bit [3:0] hi,lo;
  initial begin
`ifdef CONCAT
    b=({hi,lo}=8'h12);
`elsif SELECTED
    b=(mem[i++]=9);
`else
    a=0;
    b=(a+=1);
    $display("compound result %0d (expected 1)",b);
    b=(a=5)+(a=7);
    $display("stacked result %0d (expected 12)",b);
`endif
    $finish;
  end
endmodule
