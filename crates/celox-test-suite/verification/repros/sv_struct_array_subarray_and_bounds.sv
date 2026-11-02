// IEEE 1800-2023 7.4.5 and 7.6.
// Expected: r2=01 r1=4 line=03 empty=00,0 oob=00 same=1.
// Xezim 0.11.4 instead reads line=00 after copying rows[1] to line[4:4].
// Verilator 5.052 instead reads empty=03,4 and oob=01 for the invalid indices.

    module Top(output bit same);
      typedef struct { bit [7:0] a; bit [3:0] b; } row_t;
      row_t rows[2:1][0:0] = '{'{ '{1, 2} }, '{ '{3, 4} }};
      row_t line[4:4], empty;
      int bad = 0;
      initial begin
        line = rows[1];
        rows[bad][0].a = 99;
        empty = rows[bad][0];
        same = rows[2][0].a == 1 && rows[1][0].b == 4
          && line[4].a == 3 && empty.a == 0 && empty.b == 0
          && rows[2][1].a == 0;
        $display("r2=%h r1=%h line=%h empty=%h,%h oob=%h same=%b", rows[2][0].a, rows[1][0].b, line[4].a, empty.a, empty.b, rows[2][1].a, same);
        $finish;
      end
    endmodule
