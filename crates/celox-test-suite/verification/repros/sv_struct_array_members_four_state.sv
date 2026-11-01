// IEEE 1800-2023 7.4.5: an X index ignores writes and returns the element default.
// Expected row0=10xz,z001 invalid=xxxx,xxxx copy=xxxx,xxxx,x; xezim 0.11.4 prints
// row0=0000,z001 invalid=xxxx,0000 copy=0000,z001,z.
// Standalone probe derived from unpacked_structs::integral_array_members_of_arrays_four_state.
module Top(output bit same);
      typedef struct { logic [3:0] values[2]; logic flag; } row_t;
      row_t rows[2], copy;
      logic index;
      initial begin
        rows[0] = '{'{4'b10xz, 4'bz001}, 1'bz};
        index = 1'bx;
        rows[0].values[index] = 0;
        rows[index].values[0] = 0;
        copy = rows[index];
        same = rows[0].values[0] === 4'b10xz && rows[0].values[1] === 4'bz001
          && rows[0].values[index] === 4'bxxxx && rows[index].values[0] === 4'bxxxx
          && copy.values[0] === 4'bxxxx && copy.values[1] === 4'bxxxx && copy.flag === 1'bx;
        $display("row0=%b,%b invalid=%b,%b copy=%b,%b,%b same=%b", rows[0].values[0], rows[0].values[1], rows[0].values[index], rows[index].values[0], copy.values[0], copy.values[1], copy.flag, same);
        $finish;
      end
    endmodule
