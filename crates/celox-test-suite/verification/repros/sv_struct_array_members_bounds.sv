// IEEE 1800-2023 7.4.5: invalid indices ignore writes and return the element default.
// Verilator 5.052 writes rows[1].values[2] through rows[9].values[2], printing
// rows=33,44;245,999 and invalid=0,999. Xezim 0.11.4 fails member-array copies, printing
// saved=0,0 rows=11,55;245,102 instead of saved=33,44 rows=33,44;245,102.
// Standalone probe derived from unpacked_structs::integral_array_members_of_arrays.
module Top(output bit same, output int outer_index, inner_index);
      typedef struct { bit signed [31:0] tag; bit signed [31:0] values[3:2]; } row_t;
      row_t rows[2] = '{'{7, '{11, 22}}, '{8, '{33, 44}}}, copy;
      bit signed [31:0] saved[2];
      int bad = 9;
      initial begin
        inner_index = 2;
        rows[outer_index++].values[inner_index++] = 55;
        copy = rows[0];
        saved = rows[1].values;
        rows[1].values = '{101, 102};
        rows[1].values[3][7:4] = 4'hf;
        rows[1].values[bad] = 777;
        rows[bad].values[2] = 999;
        rows[0].values[3:2] = saved;
        same = copy.tag == 7 && copy.values[3] == 11 && copy.values[2] == 55
          && saved[0] == 33 && saved[1] == 44
          && rows[0].values[3] == 33 && rows[0].values[2] == 44 && rows[0].tag == 7
          && rows[1].values[3] == 245 && rows[1].values[2] == 102 && rows[1].tag == 8
          && rows[0].values[bad] == 0 && rows[bad].values[2] == 0;
        $display("indices=%0d,%0d copy=%0d,%0d saved=%0d,%0d rows=%0d,%0d;%0d,%0d invalid=%0d,%0d same=%b", outer_index, inner_index, copy.values[3], copy.values[2], saved[0], saved[1], rows[0].values[3], rows[0].values[2], rows[1].values[3], rows[1].values[2], rows[0].values[bad], rows[bad].values[2], same);
        $finish;
      end
    endmodule
