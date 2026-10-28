// xezim 0.11.3 reproductions of the unpacked-structure defects retained for
// the unpacked_structs cases (IEEE 1800-2023 7.2, 10.9.2, 11.4.11).
// Expected: "cond 6 a", "member 6 a w", "p 10 3 q 10 4".
module top;
  typedef struct { int level; string name; } row_t;
  typedef struct { row_t r; string tag; } outer_t;
  typedef struct { bit [7:0] a; bit [3:0] b; } pair_t;
  row_t a, b;
  outer_t o;
  pair_t p, q;
  bit [7:0] x = 8'h10;
  assign p = '{a: x, b: 4'h3};
  always_comb begin
    q = p;
    q.b = p.b + 1;
  end
  initial begin
    a = '{6, "a"};
    b = '{1, "b"};
    b = a.level > 5 ? a : b;
    $display("cond %0d %s", b.level, b.name);
    o = '{r: a, tag: "w"};
    $display("member %0d %s %s", o.r.level, o.r.name, o.tag);
    #1 $display("p %h %h q %h %h", p.a, p.b, q.a, q.b);
  end
endmodule
