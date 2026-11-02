// IEEE 1800-2023 6.22.2, 7.6 and 10.9.1.
// Expected: list=1,-4 default=1,-4 selected=1,-4 and same=1.
// Xezim 0.11.4 prints zero for all rows; Verilator 5.052 rejects the patterns.
module Top(output bit same);
      typedef struct { int x; int y; } row_t;
      row_t source_row[5:4] = '{'{1, -2}, '{3, -4}};
      row_t listed[2][2], defaults[2][2], selected[2][2];
      initial begin
        listed = '{source_row, source_row};
        defaults = '{default: source_row};
        selected = '{listed[0], defaults[1]};
        $display("list=%0d,%0d default=%0d,%0d selected=%0d,%0d", listed[0][0].x, listed[1][1].y, defaults[0][0].x, defaults[1][1].y, selected[0][0].x, selected[1][1].y);
        source_row[5].x = 99;
        listed[0][1].y = 77;
        same = listed[1][0].x == 1 && listed[1][1].y == -4
          && defaults[0][0].x == 1 && defaults[1][1].y == -4
          && selected[0][0].x == 1 && selected[0][1].y == -4
          && selected[1][0].x == 1 && selected[1][1].y == -4;
        $display("same=%b", same);
        $finish;
      end
    endmodule
