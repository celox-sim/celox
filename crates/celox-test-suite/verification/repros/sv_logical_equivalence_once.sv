module top;
  int a_count = 0, b_count = 0;
  function automatic bit left(); a_count++; return 0; endfunction
  function automatic bit right(); b_count++; return 1; endfunction
  initial begin
    bit y;
    y = left() <-> right();
    $display("y=%0d a=%0d b=%0d", y, a_count, b_count);
    $finish;
  end
endmodule
