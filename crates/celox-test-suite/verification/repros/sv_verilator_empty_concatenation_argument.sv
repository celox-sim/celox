// With Verilator 5.052, compilation stops with "%Error-UNSUPPORTED: ... Unsupported/Illegal: empty
// queue ('{}') in this context". Passing a value to an input argument is an
// assignment-like context (IEEE 1800-2023 10.8), where {} is an empty queue
// (10.10); the source prints "1".
module top;
  function automatic int size(int q[$]);
    q.push_back(1);
    return q.size();
  endfunction
  initial $display("%0d", size({}));
endmodule
