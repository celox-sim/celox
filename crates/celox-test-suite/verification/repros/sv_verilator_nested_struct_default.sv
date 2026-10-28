module top;
  typedef struct { int level; bit [3:0] l; } row_t;
  typedef struct { row_t r; int tag; } outer_t;
  outer_t o;
  initial begin
    o = '{default: 5, tag: 1};
    $display("%0d %b %0d", o.r.level, o.r.l, o.tag);
  end
endmodule
