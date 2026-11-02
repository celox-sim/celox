// IEEE 1800-2023 7.4.5 and 7.6.
// Expected: row0=10xz,11111101 row1=xxxx oob=xxxxxxxx copy=xxxx,xxxxxxxx same=1.
// Xezim 0.11.4 instead aliases the X index to row 0 for both the read and write.

    module Top(output bit same);
      typedef struct { logic [3:0] a; logic signed [7:0] b; } row_t;
      row_t rows[2], copy;
      logic index;
      initial begin
        rows[0] = '{4'b10xz, -8'sd3};
        index = 1'bx;
        rows[index].b = 0;
        copy = rows[index];
        same = rows[0].a === 4'b10xz && rows[0].b === -8'sd3
          && rows[1].a === 4'bxxxx && rows[5].b === 8'bxxxxxxxx
          && copy.a === 4'bxxxx && copy.b === 8'bxxxxxxxx;
        $display("row0=%b,%b row1=%b oob=%b copy=%b,%b same=%b", rows[0].a, rows[0].b, rows[1].a, rows[5].b, copy.a, copy.b, same);
        $finish;
      end
    endmodule
