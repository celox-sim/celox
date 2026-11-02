// IEEE 1800-2023 7.12.2 permits shuffle on the queue stored in rows[7]. The next_row
// receiver index is evaluated once; expected calls=1 size=3 total=15 same=1. Xezim 0.11.4
// instead aborts at time 0, diagnosing method shuffle called on a null class handle.
// Verilator 5.052 and all Celox backends pass the shared case.

module Top(output bit same);
  typedef int queue_t[$];
  queue_t rows[int];
  int calls, total;
  function automatic int next_row(); calls++; return 7; endfunction
  initial begin
    rows[7] = {3, 5, 7};
    rows[next_row()].shuffle();
    #1;
    for (int i = 0; i < rows[7].size(); i++) total += rows[7][i];
    same = calls == 1 && rows[7].size() == 3 && total == 15;
    $display("same=%b calls=%0d size=%0d total=%0d", same, calls, rows[7].size(), total);
    $finish;
  end
endmodule
