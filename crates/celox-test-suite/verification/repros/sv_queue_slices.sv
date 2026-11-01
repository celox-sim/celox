// IEEE 1800-2023 7.10.1: negative singleton slices are empty;
// q[1:1] has one element; a slice of an empty queue is empty.
// Default: Verilator 5.052 prints 6 then 1; Icarus 13.0 prints 6 then 6.
// With EMPTY_QUEUE: Verilator 5.052 crashes on an invalid empty-queue access;
// Icarus 13.0 rejects the $-bound slice syntax.
module top;
  int q[$], r[$];
  int lo, hi;
  initial begin
`ifdef EMPTY_QUEUE
    r=q[0:$];
    $display("empty slice size=%0d (expected 0)", r.size());
`else
    q={0,1,2,3,4,5};
    lo=-2; hi=-2;
    r=q[lo:hi];
    $display("negative slice size=%0d (expected 0)", r.size());
    lo=1; hi=1;
    r=q[lo:hi];
    $display("single slice size=%0d first=%0d (expected 1, 1)", r.size(), r[0]);
`endif
    $finish;
  end
endmodule
