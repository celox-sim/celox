module Top;
 class Node; int count; endclass
 int g, seen, b_seen, early, late, local_q, h;
 int arr[4];
 int m[2][3];
 int i, row_read;
 function automatic void bump(ref int x); x = x + 1; seen = g; endfunction
 function automatic void both(ref int a, ref int b); a = 5; b_seen = b; b = a + 1; endfunction
 function automatic int observe(const ref int x); g = 9; return x; endfunction
 function automatic void inc(ref int x = g); x++; endfunction
 function automatic void elem(ref int x); x = x + 10; i = 3; x = x + 1; endfunction
 function automatic int row(ref int r[3]); r[1] = 5; return r[1]; endfunction
 task automatic watch(ref int x); #1 early = x; #4 late = x; x = x + 100; endtask
 task automatic later(ref int x); #2 x = x + 10; endtask
 initial begin
  automatic int local_value = 3;
  automatic Node k = new;
  automatic int o;
  g = 10; bump(g);
  $display("seen=%0d", seen);
  g = 0; both(g, g);
  $display("b_seen=%0d g=%0d", b_seen, g);
  g = 1; o = observe(g);
  $display("observe=%0d", o);
  g = 1; inc();
  $display("default g=%0d", g);
  arr = '{1, 2, 3, 4}; i = 1; elem(arr[i]);
  $display("arr1=%0d arr3=%0d", arr[1], arr[3]);
  row_read = row(m[1]);
  $display("row=%0d read=%0d", m[1][1], row_read);
  fork
   watch(local_value);
   begin #2 local_value = 7; end
  join
  $display("early=%0d late=%0d local=%0d", early, late, local_value);
  k.count = 1;
  fork later(k.count); join_none
  #1 k.count = 100;
  #3 $display("property=%0d", k.count);
  $finish;
 end
endmodule
module Wrapped; Top dut(); endmodule
