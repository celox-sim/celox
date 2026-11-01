// xezim 0.11.3 prints "method 0 function 0"; with -s Top, or with the formals
// declared as `output string out[$]`, it prints "method 1 function 1", which
// IEEE 1800-2023 13.5.1 gives: an output is copied to its actual on return.
module Top;
  typedef string names_t[$];
  class Base;
    function void list(output names_t out);
      out.push_back("base");
    endfunction
  endclass
  function automatic void free(output names_t out);
    out.push_back("free");
  endfunction
  Base b;
  string n[$];
  string f[$];
  initial begin
    b = new;
    b.list(n);
    free(f);
    $display("method %0d function %0d", n.size(), f.size());
  end
endmodule
module top;
  Top dut();
endmodule
