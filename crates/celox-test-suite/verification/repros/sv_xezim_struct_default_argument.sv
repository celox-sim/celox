// xezim 0.11.3: a structure assignment pattern as the default of a
// subroutine argument loses its named members (IEEE 1800-2023 13.5.3,
// 10.9.2). Expected: "1 d".
module top;
  typedef struct { int a; string s; } row_t;
  function automatic int f(row_t r = '{default: 0, s: "d"});
    $write("%0d ", r.s.len());
    return r.a;
  endfunction
  initial begin
    void'(f());
    begin row_t r; r = '{default: 0, s: "d"}; $display("%s", r.s); end
  end
endmodule
