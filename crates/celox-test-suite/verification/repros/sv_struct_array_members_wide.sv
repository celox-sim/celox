// Verilator 5.052 fails to compile generated C++ for a 129-bit unpacked array member:
// VL_CONST_W_1X receives VlUnpacked<VlWide<5>, 2> where WDataOutP is required. Celox and
// xezim 0.11.4 pass this case.
// Standalone probe derived from unpacked_structs::integral_array_members_wide.
module Top(output bit same);
      typedef struct { bit [2:0] tag; bit [128:0] values[2]; } row_t;
      row_t rows[2] = '{default: '{default: 0}}, copy;
      int outer_index = 1, inner_index = 0;
      initial begin
        rows[outer_index].values[inner_index] = 129'h100000000000000000000000000000001;
        copy = rows[1];
        same = rows[1].values[0][128] && rows[1].values[0][0]
          && rows[0].values[0] == 0 && rows[1].values[1] == 0
          && copy.values[0][128] && copy.values[0][0] && copy.tag == 0;
        $display("same=%b", same);
        $finish;
      end
    endmodule
