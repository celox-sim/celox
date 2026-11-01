// IEEE 1800-2023 10.9.2: replicated structure pattern items fill every corresponding
// member. Expected triple=1,1,1 rows=7,11,22,11,22; xezim 0.11.4 prints triple=1,0,0
// rows=7,22,0,0,0.
// Standalone probe derived from unpacked_structs::integral_array_members_replication.
module Top(output bit same);
      typedef struct { int a; int b[4]; } row_t;
      struct { int x, y, z; } triple = '{3{1}};
      row_t rows[1:0][2:0];
      initial begin
        rows = '{2{'{3{'{7, '{2{11, 22}}}}}}};
        same = triple.x == 1 && triple.y == 1 && triple.z == 1
          && rows[1][2].a == 7 && rows[0][0].b[0] == 11
          && rows[0][0].b[1] == 22 && rows[1][1].b[2] == 11 && rows[1][0].b[3] == 22;
        $display("triple=%0d,%0d,%0d rows=%0d,%0d,%0d,%0d,%0d same=%b", triple.x, triple.y, triple.z, rows[1][2].a, rows[0][0].b[0], rows[0][0].b[1], rows[1][1].b[2], rows[1][0].b[3], same);
        $finish;
      end
    endmodule
