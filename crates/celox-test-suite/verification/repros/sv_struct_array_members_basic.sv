// IEEE 1800-2023 7.6: array copies preserve left-to-right element order. Expected
// saved=11,22; xezim 0.11.4 prints saved=22,11.
// Standalone probe derived from unpacked_structs::integral_array_members.
module Top(output bit same);
      typedef struct { int tag; int values[3:2]; bit flag; } row_t;
      row_t s = '{7, '{11, 22}, 1}, copy;
      int index = 2;
      int saved[2];
      initial begin
        copy = s;
        saved = s.values;
        s.values[index] += 3;
        s.values[3][7:0] = 8'h35;
        same = copy.tag == 7 && copy.values[3] == 11 && copy.values[2] == 22
          && copy.flag && saved[0] == 11 && saved[1] == 22
          && s.values[3] == 53 && s.values[2] == 25 && s.tag == 7 && s.flag;
        $display("copy=%0d,%0d saved=%0d,%0d current=%0d,%0d flags=%b,%b same=%b", copy.values[3], copy.values[2], saved[0], saved[1], s.values[3], s.values[2], copy.flag, s.flag, same);
        $finish;
      end
    endmodule
