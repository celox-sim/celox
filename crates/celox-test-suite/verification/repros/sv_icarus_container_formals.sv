// Icarus 13.0 aborts compiling this source ("XXXX function argument q type=8?!",
// then "ivl: draw_ufunc.c:73: draw_eval_function_argument: Assertion `0' failed.").
// IEEE 1800-2023 13.5.1 passes the queue by copy and prints "3 2". An output
// queue formal of a function gives "Function arguments must be input ports"
// and a queue formal of a task "assert: elaborate.cc:4342: failed assertion 0".
module top;
  function automatic int size(int q[$]);
    q.push_back(1);
    return q.size();
  endfunction
  int q[$];
  initial begin
    q = '{1, 2};
    $display("%0d %0d", size(q), q.size());
  end
endmodule
