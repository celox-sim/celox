// xezim 0.11.3: an automatic output argument of unpacked structure type that
// the function does not write is copied out as X for a 2-state member; IEEE
// 1800-2023 13.5.2 and 6.8 give the formal its type's default, 0.
// Expected: "0".
module top;
  typedef struct { int a; } row_t;
  function automatic void outz(output row_t r);
    if (0) r.a = 1;
  endfunction
  initial begin
    row_t r;
    r.a = 5;
    outz(r);
    $display("%0d", r.a);
  end
endmodule
