// IEEE 1800-2023 6.22.2, 7.6 and 10.9.1.
// Expected: list=10xz,0z1x default=z001,x110 and same=1.
// Xezim 0.11.4 instead prints 000x for each displayed member.
module Top(output bit same);
      typedef struct { logic [3:0] x; logic [3:0] y; } row_t;
      row_t source_row[2] = '{'{4'b10xz, 4'bz001}, '{4'bx110, 4'b0z1x}};
      row_t listed[2][2], defaults[2][2];
      initial begin
        listed = '{source_row, source_row};
        defaults = '{default: source_row};
        $display("list=%b,%b default=%b,%b", listed[0][0].x, listed[1][1].y, defaults[0][0].y, defaults[1][1].x);
        same = listed[0][0].x === 4'b10xz && listed[1][1].y === 4'b0z1x
          && defaults[0][0].y === 4'bz001 && defaults[1][1].x === 4'bx110;
        $display("same=%b", same);
        $finish;
      end
    endmodule
